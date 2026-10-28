{ source }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.lg-buddy;
  common = import ./common.nix { inherit lib; };
  runtime = lib.getExe cfg.package;
  selected = lib.escapeShellArg cfg.configPath;
  publish = pkgs.writeShellScript "lg-buddy-publish-machine" ''
    exec ${runtime} config-binding publish-machine ${selected}
  '';
  machinePath = [
    cfg.package
    pkgs.systemd
    pkgs.networkmanager
    pkgs.iputils
  ];
  # The system-wide installation is Nix-owned. Retained global update units
  # must also lose their enablement links, or NixOS will try to start a mask.
  noUpdateUnit = {
    enable = false;
    wantedBy = lib.mkForce [ ];
    requiredBy = lib.mkForce [ ];
    upheldBy = lib.mkForce [ ];
  };
  # Keep the oneshot active before pairing, so its later ExecStop can use the
  # newly paired state. Before pairing there is no TV operation to perform.
  guarded =
    name: command:
    pkgs.writeShellScript name ''
      if [ -f ${selected} ]; then
        exec ${runtime} ${command}
      fi
    '';
  lifecycle = pkgs.writeShellScript "lg-buddy-lifecycle" ''
    # A fresh declaration precedes pairing. Stay active while waiting for the
    # user's atomic config publication, then keep this PID for the real daemon.
    while [ ! -f ${selected} ]; do
      ${pkgs.coreutils}/bin/sleep 1
    done
    exec ${runtime} lifecycle
  '';
in
{
  options.services.lg-buddy = {
    enable = lib.mkEnableOption "LG Buddy machine lifecycle integration";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ../package.nix {
        src = source;
        buildCommit = source.rev or source.dirtyRev or "unknown";
      };
      defaultText = lib.literalExpression "LG Buddy built with the host package set";
      description = "Matching LG Buddy CLI/GUI package. Use the same package in Home Manager.";
    };
    user = lib.mkOption {
      type = lib.types.str;
      description = "Configured desktop user who owns the writable configuration and credentials.";
    };
    configPath = lib.mkOption {
      type = common.configPathType;
      default = "${config.users.users.${cfg.user}.home or "/var/empty"}/.config/lg-buddy/config.env";
      defaultText = lib.literalExpression ''"''${config.users.users.<user>.home}/.config/lg-buddy/config.env"'';
      description = "Canonical absolute writable user config path, also selected by Home Manager.";
    };
    plasma.enable = lib.mkEnableOption "the optional native inhibition bridge built with this host's KWin/Qt";
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.users.users ? ${cfg.user} && config.users.users.${cfg.user}.isNormalUser;
        message = "services.lg-buddy.user must name a configured normal NixOS user.";
      }
      {
        assertion = config.networking.networkmanager.enable;
        message = "services.lg-buddy requires NetworkManager for lifecycle readiness and pre-down handling.";
      }
      {
        assertion =
          !cfg.plasma.enable
          || (
            config.services.desktopManager.plasma6.enable
            && lib.versionAtLeast pkgs.kdePackages.kwin.version "6.4"
          );
        message = "services.lg-buddy.plasma.enable requires this host's Plasma 6.4 or newer package set.";
      }
    ];
    environment.systemPackages = [
      cfg.package
    ]
    ++ lib.optional cfg.plasma.enable (pkgs.callPackage ../kwin.nix { package = cfg.package; });
    # A user's explicit kwinrc override still wins. Source metadata stays intact.
    environment.etc."xdg/kwinrc" = lib.mkIf cfg.plasma.enable {
      text = ''
        [Plugins]
        lg_buddy_inhibitionEnabled=true
      '';
    };
    systemd.tmpfiles.rules = [ "d /run/lg_buddy 0755 root root -" ];
    systemd.user.services.LG_Buddy_update_check = noUpdateUnit;
    systemd.user.timers.LG_Buddy_update_check = noUpdateUnit;

    system.activationScripts.lg-buddy-binding = {
      deps = [
        "users"
        "etc"
      ];
      text = ''
        ${publish}
      '';
    };

    systemd.services.LG_Buddy = {
      description = "LG Buddy boot/shutdown TV control";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      path = machinePath;
      # Ordinary switches/removal must not execute a TV power cycle.
      restartIfChanged = false;
      stopIfChanged = false;
      environment.LG_BUDDY_CONFIG = cfg.configPath;
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = guarded "lg-buddy-boot" "startup boot";
        ExecStop = guarded "lg-buddy-shutdown" "shutdown";
      };
    };

    systemd.services.LG_Buddy_lifecycle = {
      description = "LG Buddy system sleep/wake monitor";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      path = machinePath;
      environment.LG_BUDDY_CONFIG = cfg.configPath;
      serviceConfig = {
        Type = "simple";
        ExecStart = lifecycle;
        Restart = "on-failure";
        RestartSec = 10;
      };
    };

    networking.networkmanager.dispatcherScripts = [
      {
        type = "pre-down";
        source = pkgs.writeShellScript "lg-buddy-networkmanager-pre-down" ''
          set -eu
          [ "''${2:-}" = pre-down ] || exit 0
          [ -r ${selected} ] || exit 0
          export LG_BUDDY_CONFIG=${selected}
          export PATH=${lib.makeBinPath machinePath}
          exec ${runtime} nm-pre-down
        '';
      }
    ];
  };
}
