# The declarative package/module are supplied by the caller, not copied or mutated.
{ pkgs ? import <nixpkgs> {}, packageFile, serviceModule, src ? ../., buildCommit ? "native-validation" }:
let
  package = (pkgs.callPackage packageFile { inherit src buildCommit; }).overrideAttrs (old: {
    nativeCheckInputs = (old.nativeCheckInputs or []) ++ [ pkgs.util-linux ];
    postBuild = (old.postBuild or "") + ''
      cargo build --offline --release --target x86_64-unknown-linux-gnu -p lg-buddy --example gui_journey_tv
    '';
    postInstall = old.postInstall + ''
      install -Dm755 target/x86_64-unknown-linux-gnu/release/examples/gui_journey_tv $out/bin/gui_journey_tv
    '';
  });
  configFile = pkgs.writeText "lg-buddy-vm.env" ''
    tvs_primary_ip=127.0.0.1
    tvs_primary_mac=02:11:22:33:44:55
    tvs_primary_input=HDMI_1
    tvs_primary_platform=lg_webos
    screen_idle_blank=disabled
    system_sleep_wake_policy=disabled
    updates_auto_check=disabled
  '';
  token = pkgs.writeText "lg-buddy-vm-token.json" ''{"access_token":"webos-test-access-token"}'';
  python = pkgs.python3.withPackages (p: [ p.pyatspi p.dbus-python p.pygobject3 ]);
  helpers = pkgs.runCommand "lg-buddy-native-ui-helpers" {} ''
    mkdir -p $out
    cp ${pkgs.writeText "native-recovery.py" (builtins.readFile ./test-native-setup-recovery.py)} $out/test-native-setup-recovery.py
    cp ${pkgs.writeText "a11y.py" (builtins.readFile ./test-release-gui-accessibility.py)} $out/test-release-gui-accessibility.py
  '';
  gtkCheck = pkgs.writeText "native-gtk-check.py" ''
    import sys, importlib, json
    sys.path.insert(0, "${helpers}")
    ui = importlib.import_module("test-native-setup-recovery")
    if sys.argv[1] == "blocked":
        ui.wait("setup splash", lambda: "Complete setup" in ui.names())
        assert not ui.functional()
        ui.activate("Complete setup")
        ui.wait("managed external recovery", lambda: "Recheck" in ui.names())
        assert any("NixOS" in name for name in ui.names())
        assert "Install build tools" not in ui.names()
    else:
        ui.activate("Recheck")
        ui.wait("verified functional UI", ui.functional)
        assert "Complete setup" not in ui.names()
    print(json.dumps(sorted(ui.names())))
  '';
in pkgs.testers.runNixOSTest {
  name = "lg-buddy-native-setup-recovery";
  nodes.machine = { lib, ... }: {
    imports = [ serviceModule ];
    _module.args.inputs.lg-buddy-src = { outPath = src; rev = buildCommit; };
    virtualisation.memorySize = 8192;
    virtualisation.cores = 6;
    services.xserver.enable = true;
    services.displayManager.gdm.enable = true;
    services.desktopManager.gnome.enable = true;
    services.displayManager.autoLogin = { enable = true; user = "lgtest"; };
    networking.networkmanager.enable = true;
    users.users.lgtest = { isNormalUser = true; uid = 1000; password = "disposable"; };
    vas.services.lgBuddy = { enable = true; user = "lgtest"; inherit package; enableSwayidle = false; };
    systemd.tmpfiles.rules = [
      "d /home/lgtest/.config 0700 lgtest users -"
      "d /home/lgtest/.config/lg-buddy 0700 lgtest users -"
      "C /home/lgtest/.config/lg-buddy/config.env 0600 lgtest users - ${configFile}"
      "d /home/lgtest/.config/lg-buddy/tvs 0700 lgtest users -"
      "d /home/lgtest/.config/lg-buddy/tvs/primary 0700 lgtest users -"
      "C /home/lgtest/.config/lg-buddy/tvs/primary/access-token.json 0600 lgtest users - ${token}"
    ];
    systemd.user.services.LG_Buddy_update_check.environment.LG_BUDDY_CONFIG = lib.mkOverride 90 "/home/lgtest/wrong.env";
    specialisation.repaired.configuration.systemd.user.services.LG_Buddy_update_check.environment.LG_BUDDY_CONFIG = lib.mkForce "/home/lgtest/.config/lg-buddy/config.env";
    systemd.user.services.lg-buddy-tv-fixture = {
      wantedBy = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      serviceConfig.ExecStart = "${package}/bin/gui_journey_tv /home/lgtest/tv-fixture";
    };
    environment.systemPackages = [ python pkgs.at-spi2-core pkgs.glib ];
  };
  testScript = ''
    import json
    start_all()
    machine.wait_for_unit("graphical.target")
    machine.wait_for_file("/run/user/1000/bus")
    prefix = "runuser -u lgtest -- env XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus GI_TYPELIB_PATH=${pkgs.at-spi2-core}/lib/girepository-1.0:${pkgs.gobject-introspection}/lib/girepository-1.0:${pkgs.glib}/lib/girepository-1.0 "
    call = "busctl --user --json=short call io.github.Staphylococcus.LGBuddy /io/github/Staphylococcus/LGBuddy/Session io.github.Staphylococcus.LGBuddy.Session1 "
    machine.wait_until_succeeds(prefix + call + "GetSetupAssessment")
    before = json.loads(json.loads(machine.succeed(prefix + call + "GetSetupAssessment"))["data"][0])
    assert before["status"] == "Incomplete", before
    assert any(r["recovery"]["cause"] == "managed_installation" for r in before["requirements"]), before
    assert all(not r["actionable"] for r in before["requirements"]), before
    machine.succeed(prefix + "gsettings set org.gnome.desktop.interface toolkit-accessibility true")
    machine.succeed(prefix + "systemd-run --user --unit=native-setup-gui ${package}/bin/lg-buddy-gui")
    print("NATIVE MANAGED GATE:", machine.succeed(prefix + "${python}/bin/python ${gtkCheck} blocked"))
    machine.screenshot("managed-gate")
    machine.succeed("/run/current-system/specialisation/repaired/bin/switch-to-configuration test")
    print("NATIVE RECHECK ADMISSION:", machine.succeed(prefix + "${python}/bin/python ${gtkCheck} recheck"))
    after = json.loads(json.loads(machine.succeed(prefix + call + "GetSetupAssessment"))["data"][0])
    assert after["status"] == "Complete", after
    assert before["instance"] == after["instance"] and after["revision"] > before["revision"], (before, after)
    print("INSTALLED IDENTITY:", machine.succeed("sha256sum ${package}/bin/lg-buddy ${package}/bin/.lg-buddy-gui-wrapped"))
    print("LIVE SERVICES:", machine.succeed(prefix + "systemctl --user show LG_Buddy_screen.service -p MainPID -p ExecStart -p ActiveState"))
    machine.screenshot("declarative-repair")
    machine.succeed(prefix + "gapplication action io.github.staphylococcus.LGBuddy quit")
    machine.wait_until_fails(prefix + "systemctl --user is-active native-setup-gui.service")
    print("NATIVE SETUP BEFORE:", before)
    print("NATIVE SETUP AFTER:", after)
  '';
}
