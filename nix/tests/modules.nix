{
  self,
  home-manager,
  pkgs,
  package,
}:
let
  inherit (pkgs) lib;
  configPath = "/home/alice/TV settings%$/config.env";
  # Exercise real publication/settings and real systemd managers. TV/session
  # operations are recording peers, so this test cannot contact a physical TV.
  peer =
    revision:
    pkgs.writeShellScriptBin "lg-buddy" ''
      set -eu
      case "$1" in
        config-binding|settings) exec ${package}/bin/lg-buddy "$@" ;;
        monitor)
          printf '%s\n' ${revision} >> "$HOME/monitor-starts"
          exec ${pkgs.coreutils}/bin/sleep infinity ;;
        lifecycle) exec ${pkgs.coreutils}/bin/sleep infinity ;;
        *) printf '%s\t%s\n' "$*" "$LG_BUDDY_CONFIG" >> /run/lg-buddy-machine-actions ;;
      esac
    '';
  first = peer "first";
  second = peer "second";
in
pkgs.testers.runNixOSTest {
  name = "lg-buddy-public-modules";
  nodes.machine = { ... }: {
    imports = [
      self.nixosModules.lg-buddy
      home-manager.nixosModules.home-manager
    ];
    virtualisation.memorySize = 2048;
    virtualisation.cores = 2;
    networking.networkmanager.enable = true;
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
    };
    users.users.bob = {
      isNormalUser = true;
      uid = 1001;
    };
    services.lg-buddy = {
      enable = true;
      user = "alice";
      package = first;
      inherit configPath;
    };
    home-manager = {
      useGlobalPkgs = true;
      useUserPackages = true;
      backupFileExtension = "lg-buddy-test-backup";
      users.alice = {
        imports = [ self.homeManagerModules.lg-buddy ];
        home.stateVersion = "26.05";
        services.lg-buddy = {
          enable = true;
          package = first;
          inherit configPath;
          settings.screen.idle_timeout = 300;
        };
      };
      users.bob = {
        imports = [ self.homeManagerModules.lg-buddy ];
        home.stateVersion = "26.05";
        services.lg-buddy.enable = false;
      };
    };
    # Retained downstream global units must be shadowed for Alice alone.
    systemd.user.services.LG_Buddy_update_check.serviceConfig = {
      ExecStart = "${pkgs.coreutils}/bin/sleep infinity";
    };
    systemd.user.timers.LG_Buddy_update_check = {
      timerConfig = {
        OnActiveSec = "1h";
        Unit = "LG_Buddy_update_check.service";
      };
      wantedBy = [ "default.target" ];
    };
    # Pull in the dependency-only graphical target as a desktop session would.
    systemd.user.targets.test-desktop = {
      bindsTo = [ "graphical-session.target" ];
      after = [ "graphical-session-pre.target" ];
    };
    specialisation.changed.configuration = {
      services.lg-buddy.package = lib.mkForce second;
      home-manager.users.alice.services.lg-buddy = {
        package = lib.mkForce second;
        settings.screen.idle_timeout = lib.mkForce 120;
      };
    };
    specialisation.removed.configuration.home-manager.users.alice.services.lg-buddy.settings.screen.idle_timeout =
      lib.mkForce null;
    specialisation.disabled.configuration.home-manager.users.alice.services.lg-buddy.enable =
      lib.mkForce false;
  };
  testScript = ''
    import shlex
    start_all()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("home-manager-alice.service")
    machine.wait_for_unit("home-manager-bob.service")
    machine.wait_for_file("/run/user/1000/bus")
    config = ${builtins.toJSON configPath}
    token = "/home/alice/TV settings%$/tvs/primary/access-token.json"
    qconfig = shlex.quote(config)
    user = "runuser -u alice -- env XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
    ctl = user + "systemctl --user "
    cli = user + "${package}/bin/lg-buddy "
    hook = "/etc/NetworkManager/dispatcher.d/pre-down.d/03userscript0001"
    base = machine.succeed("readlink -f /run/current-system").strip()

    with subtest("fresh activation publishes regular selectors without user config or TV actions"):
        machine.succeed("test -f /etc/lg-buddy/config-path && test ! -L /etc/lg-buddy/config-path")
        assert machine.succeed("cat /etc/lg-buddy/config-path").strip() == config
        machine.succeed("test -f /home/alice/.config/lg-buddy/config-path && test ! -L /home/alice/.config/lg-buddy/config-path")
        machine.fail("test -e " + qconfig)
        machine.fail("test -e /run/lg-buddy-machine-actions")
        machine.succeed(hook + " eth0 pre-down")
        machine.fail("test -e /run/lg-buddy-machine-actions")
        machine.succeed("systemctl is-active LG_Buddy.service LG_Buddy_lifecycle.service")
        machine.succeed(ctl + "is-enabled LG_Buddy_screen.service")
        machine.fail(ctl + "is-active LG_Buddy_screen.service")
        machine.fail("test -e /etc/systemd/user/LG_Buddy_screen.service")
        machine.fail("test -e /home/bob/.config/lg-buddy/config-path")
        for suffix in ["timer", "service"]:
            assert machine.succeed(ctl + "is-enabled LG_Buddy_update_check." + suffix + " || true").strip() == "masked"
            machine.fail(ctl + "start LG_Buddy_update_check." + suffix)

    with subtest("first writable state retains credentials while declared keys are protected"):
        machine.succeed(user + "sh -c " + shlex.quote("umask 077; printf 'screen_idle_timeout=600\\n' > " + qconfig))
        machine.succeed(user + "sh -c " + shlex.quote("umask 077; mkdir -p " + shlex.quote(token.rsplit("/", 1)[0]) + "; printf '{\"access_token\":\"private-fixture-token\"}' > " + shlex.quote(token)))
        assert machine.succeed(cli + "settings get screen.idle_timeout").strip() == "300"
        machine.fail(cli + "settings set screen.idle_timeout 450")
        saved = machine.succeed("sha256sum " + qconfig + " " + shlex.quote(token))
        machine.succeed(hook + " eth0 up")
        machine.fail("test -e /run/lg-buddy-machine-actions")
        machine.succeed(hook + " eth0 pre-down")
        assert machine.succeed("cat /run/lg-buddy-machine-actions").splitlines() == ["nm-pre-down\t" + config]
        machine.succeed("rm /run/lg-buddy-machine-actions")
        machine.succeed(ctl + "start test-desktop.target")
        machine.wait_until_succeeds(ctl + "is-active LG_Buddy_screen.service", timeout=60)
        machine.wait_for_file("/home/alice/monitor-starts")
        assert machine.succeed("cat /home/alice/monitor-starts").splitlines() == ["first"]

    with subtest("rebuild publishes then refreshes the active matching monitor without TV power actions"):
        legacy = {
            "service": "[Service]\nExecStart=${pkgs.coreutils}/bin/sleep infinity\n",
            "timer": "[Timer]\nOnActiveSec=1h\nUnit=LG_Buddy_update_check.service\n",
        }
        for suffix, text in legacy.items():
            unit = "/home/alice/.config/systemd/user/LG_Buddy_update_check." + suffix
            machine.succeed(user + "sh -c " + shlex.quote("rm " + unit + "; printf %s " + shlex.quote(text) + " > " + unit))
        machine.succeed(ctl + "daemon-reload")
        machine.succeed(ctl + "start LG_Buddy_update_check.timer LG_Buddy_update_check.service")
        machine.succeed(ctl + "is-active LG_Buddy_update_check.timer LG_Buddy_update_check.service")
        machine.succeed(base + "/specialisation/changed/bin/switch-to-configuration test")
        machine.wait_until_succeeds("test $(wc -l < /home/alice/monitor-starts) -eq 2", timeout=60)
        assert machine.succeed("cat /home/alice/monitor-starts").splitlines() == ["first", "second"]
        assert machine.succeed(cli + "settings get screen.idle_timeout").strip() == "120"
        assert machine.succeed("sha256sum " + qconfig + " " + shlex.quote(token)) == saved
        machine.fail("test -e /run/lg-buddy-machine-actions")
        for suffix in ["timer", "service"]:
            assert machine.succeed(ctl + "is-enabled LG_Buddy_update_check." + suffix + " || true").strip() == "masked"
            machine.fail(ctl + "is-active LG_Buddy_update_check." + suffix)
            machine.fail(ctl + "is-failed LG_Buddy_update_check." + suffix)
            assert machine.succeed("cat /home/alice/.config/systemd/user/LG_Buddy_update_check." + suffix + ".lg-buddy-test-backup") == legacy[suffix]

    with subtest("rollback refreshes active policy and removal restores saved preferences"):
        machine.succeed(base + "/bin/switch-to-configuration test")
        machine.wait_until_succeeds("test $(wc -l < /home/alice/monitor-starts) -eq 3", timeout=60)
        assert machine.succeed(cli + "settings get screen.idle_timeout").strip() == "300"
        machine.succeed(base + "/specialisation/removed/bin/switch-to-configuration test")
        assert machine.succeed(cli + "settings get screen.idle_timeout").strip() == "600"
        machine.fail("test -L " + shlex.quote(config + ".declarative.json"))

    with subtest("an inactive session stays stopped across activation; disable removes policy and user units"):
        machine.succeed(ctl + "stop test-desktop.target")
        machine.wait_until_fails(ctl + "is-active LG_Buddy_screen.service", timeout=60)
        count = machine.succeed("wc -l < /home/alice/monitor-starts")
        machine.succeed(base + "/bin/switch-to-configuration test")
        machine.fail(ctl + "is-active LG_Buddy_screen.service")
        assert machine.succeed("wc -l < /home/alice/monitor-starts") == count
        machine.succeed(base + "/specialisation/disabled/bin/switch-to-configuration test")
        machine.fail("test -L " + shlex.quote(config + ".declarative.json"))
        assert machine.succeed(cli + "settings get screen.idle_timeout").strip() == "600"
        machine.fail("test -e /home/alice/.config/systemd/user/LG_Buddy_screen.service")
        assert machine.succeed("sha256sum " + qconfig + " " + shlex.quote(token)) == saved
        machine.fail("test -e /run/lg-buddy-machine-actions")

    with subtest("shutdown uses state paired after the boot oneshot began"):
        machine.succeed("systemctl stop LG_Buddy.service")
        assert machine.succeed("cat /run/lg-buddy-machine-actions").splitlines() == ["shutdown\t" + config]
  '';
}
