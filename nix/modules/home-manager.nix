{ source }:
{
  config,
  lib,
  pkgs,
  osConfig ? null,
  ...
}:
let
  cfg = config.services.lg-buddy;
  common = import ./common.nix { inherit lib; };
  runtime = lib.getExe cfg.package;
  settings = lib.listToAttrs (
    lib.concatLists (
      lib.mapAttrsToList (
        group: values:
        lib.mapAttrsToList (name: value: {
          name = "${group}.${name}";
          inherit value;
        }) (lib.filterAttrs (_: value: value != null) values)
      ) cfg.settings
    )
  );
  generation = pkgs.writeText "lg-buddy-settings.json" (
    builtins.toJSON {
      version = 1;
      source = "nix";
      inherit settings;
    }
  );
  manifest = pkgs.writeText "lg-buddy-activation.json" (
    builtins.toJSON {
      inherit (cfg) configPath;
      inherit runtime;
    }
  );
in
{
  options.services.lg-buddy = {
    enable = lib.mkEnableOption "LG Buddy user session integration";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ../package.nix {
        src = source;
        buildCommit = source.rev or source.dirtyRev or "unknown";
      };
      defaultText = lib.literalExpression "LG Buddy built with the user's package set";
      description = "Matching CLI/GUI package; must match the NixOS module's package.";
    };
    configPath = lib.mkOption {
      type = common.configPathType;
      default = "${config.xdg.configHome}/lg-buddy/config.env";
      defaultText = lib.literalExpression ''"''${config.xdg.configHome}/lg-buddy/config.env"'';
      description = "Canonical absolute writable configuration path owned by this user. Match the machine module.";
    };
    settings = lib.mkOption {
      type = lib.types.submodule { options = import ./settings.nix { inherit lib; }; };
      default = { };
      description = "Declared read-only behavior settings. Null leaves a key editable; credentials and TV profiles are never generated.";
    };
  };

  config = lib.mkMerge [
    {
      # Keep the cleanup hook when this module is disabled. Its receipt lives in
      # the HM generation, never in mutable preferences or credential storage.
      home.activation.lg-buddy-publish =
        lib.hm.dag.entryBetween [ "linkGeneration" ] [ "writeBoundary" ]
          ''
            export XDG_CONFIG_HOME=${lib.escapeShellArg config.xdg.configHome}
            ${
              if cfg.enable then
                ''
                  run ${runtime} config-binding publish-user ${lib.escapeShellArg cfg.configPath} ${
                    if settings == { } then "-" else lib.escapeShellArg generation
                  }
                ''
              else
                ''
                  if [[ -n ''${oldGenPath:-} && -f "$oldGenPath/lg-buddy-activation.json" ]]; then
                    lgBuddyOldConfig=$(${pkgs.jq}/bin/jq -er .configPath "$oldGenPath/lg-buddy-activation.json")
                    lgBuddyOldRuntime=$(${pkgs.jq}/bin/jq -er .runtime "$oldGenPath/lg-buddy-activation.json")
                    run "$lgBuddyOldRuntime" config-binding publish-user "$lgBuddyOldConfig" -
                    unset lgBuddyOldConfig lgBuddyOldRuntime
                  fi
                ''
            }
          '';
      home.activation.lg-buddy-stop-removed = lib.mkIf (!cfg.enable) (
        lib.hm.dag.entryAfter [ "reloadSystemd" "lg-buddy-publish" ] ''
          export XDG_RUNTIME_DIR="''${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
          if [[ -n ''${oldGenPath:-} && -f "$oldGenPath/lg-buddy-activation.json" && -S "$XDG_RUNTIME_DIR/bus" ]]; then
            run ${pkgs.systemd}/bin/systemctl --user daemon-reload
            # sd-switch may already have removed and unloaded it (exit 5).
            run ${pkgs.systemd}/bin/systemctl --user stop LG_Buddy_screen.service || {
              lgBuddyStopStatus=$?
              [[ $lgBuddyStopStatus == 5 ]] || exit "$lgBuddyStopStatus"
              unset lgBuddyStopStatus
            }
          fi
        ''
      );
    }
    (lib.mkIf cfg.enable {
      assertions = [
        {
          assertion = pkgs.stdenv.isLinux && config.systemd.user.enable;
          message = "services.lg-buddy requires a Linux systemd user manager.";
        }
        {
          assertion =
            osConfig == null
            || !(osConfig.services ? lg-buddy)
            || !osConfig.services.lg-buddy.enable
            || (
              osConfig.services.lg-buddy.user == config.home.username
              && osConfig.services.lg-buddy.configPath == cfg.configPath
              && osConfig.services.lg-buddy.package == cfg.package
            );
          message = "The NixOS and Home Manager LG Buddy modules must select the same user, configPath and package.";
        }
      ];
      home.packages = [ cfg.package ];
      home.extraBuilderCommands = ''
        ln -s ${manifest} "$out/lg-buddy-activation.json"
      '';
      systemd.user.services.LG_Buddy_screen = {
        Unit = {
          Description = "LG Buddy screen monitor";
          After = [ "graphical-session.target" ];
          PartOf = [ "graphical-session.target" ];
          # Existing monitor restarts use the validated conditional helper below.
          X-SwitchMethod = "keep-old";
        };
        Service = {
          ExecStart = "${runtime} monitor";
          Environment = map common.systemdQuote [
            "LG_BUDDY_CONFIG=${cfg.configPath}"
            "PATH=${
              lib.makeBinPath [
                cfg.package
                # Setup inspection invokes the installed KWin launcher through
                # bash, including its NotApplicable check on GNOME.
                pkgs.bash
                pkgs.systemd
              ]
            }"
          ];
          Restart = "on-failure";
          RestartSec = 10;
        };
        Install.WantedBy = [ "graphical-session.target" ];
      };
      # Higher-priority user masks also cover retained downstream global units.
      # Existing unmanaged files require HM's normal backup/collision handling.
      xdg.configFile =
        lib.genAttrs
          (map (suffix: "systemd/user/LG_Buddy_update_check.${suffix}") [
            "service"
            "timer"
          ])
          (_: {
            source = config.lib.file.mkOutOfStoreSymlink "/dev/null";
          });
      home.activation.lg-buddy-refresh = lib.hm.dag.entryAfter [ "reloadSystemd" "lg-buddy-publish" ] ''
        export XDG_RUNTIME_DIR="''${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
        if [[ -S "$XDG_RUNTIME_DIR/bus" ]]; then
          run ${pkgs.systemd}/bin/systemctl --user daemon-reload
          run ${pkgs.systemd}/bin/systemctl --user stop LG_Buddy_update_check.timer LG_Buddy_update_check.service
          # Masking a loaded legacy timer can mark it failed when its trigger
          # disappears. Clear only failures of the update units we just stopped;
          # otherwise a later NixOS switch may try to revive that failed timer.
          for lgBuddyUnit in LG_Buddy_update_check.timer LG_Buddy_update_check.service; do
            if ${pkgs.systemd}/bin/systemctl --user --quiet is-failed "$lgBuddyUnit"; then
              run ${pkgs.systemd}/bin/systemctl --user reset-failed "$lgBuddyUnit"
            fi
          done
          unset lgBuddyUnit
          run ${pkgs.coreutils}/bin/env PATH="${lib.makeBinPath [ pkgs.systemd ]}:$PATH" \
            ${runtime} config-binding refresh-user ${lib.escapeShellArg cfg.configPath}
        else
          echo "LG Buddy: user manager unavailable; policy will load at the next graphical session."
        fi
      '';
    })
  ];
}
