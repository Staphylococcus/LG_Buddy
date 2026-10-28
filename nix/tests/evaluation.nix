{
  self,
  nixpkgs,
  home-manager,
  pkgs,
}:
let
  inherit (pkgs) lib;
  package = pkgs.writeShellScriptBin "lg-buddy" "exit 0";
  system =
    extra:
    nixpkgs.lib.nixosSystem {
      inherit (pkgs.stdenv.hostPlatform) system;
      modules = [
        self.nixosModules.lg-buddy
        {
          system.stateVersion = "26.05";
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "/dev/test-root";
            fsType = "ext4";
          };
          networking.networkmanager.enable = true;
          users.users.alice = {
            isNormalUser = true;
            home = "/home/alice";
          };
          services.lg-buddy = {
            enable = true;
            user = "alice";
            inherit package;
          };
        }
        extra
      ];
    };
  homeWith =
    extraSpecialArgs: extra:
    home-manager.lib.homeManagerConfiguration {
      inherit pkgs extraSpecialArgs;
      modules = [
        self.homeManagerModules.lg-buddy
        {
          home = {
            username = "alice";
            homeDirectory = "/home/alice";
            stateVersion = "26.05";
          };
          services.lg-buddy = {
            enable = true;
            inherit package;
          };
        }
        extra
      ];
    };
  home = homeWith { };
  integrated =
    overrides:
    homeWith {
      osConfig.services.lg-buddy = {
        enable = true;
        user = "alice";
        configPath = "/home/alice/.config/lg-buddy/config.env";
        inherit package;
      }
      // overrides;
    } { };
  machine = (system { }).config;
  user = (home { }).config;
  disabledMachine = (system { services.lg-buddy.enable = lib.mkForce false; }).config;
  disabledHome = (home { services.lg-buddy.enable = lib.mkForce false; }).config;
  fails = value: !(builtins.tryEval (builtins.deepSeq value true)).success;
  check = condition: message: if condition then true else throw message;
  checks = [
    (check (
      machine.services.lg-buddy.configPath == user.services.lg-buddy.configPath
    ) "Default machine/user config must agree")
    (check (
      !machine.systemd.services.LG_Buddy.restartIfChanged
      && !machine.systemd.services.LG_Buddy.stopIfChanged
    ) "Rebuilds must not power-cycle the TV")
    (check (
      !(machine.systemd.user.services ? LG_Buddy_screen)
    ) "Machine module must not install global user units")
    (check (
      !(disabledMachine.systemd.services ? LG_Buddy)
    ) "Disabled machine module must have no services")
    (check (
      !(disabledHome.systemd.user.services ? LG_Buddy_screen)
    ) "Disabled home module must have no monitor")
    (check (
      !(user.systemd.user.services.LG_Buddy_screen.Unit ? ConditionPathExists)
    ) "First pairing needs a running monitor before config exists")
    (check (
      user.systemd.user.services.LG_Buddy_screen.Unit.X-SwitchMethod == "keep-old"
    ) "Refresh owns existing monitor transitions")
    (check (fails
      (home { services.lg-buddy.settings.screen.idle_timeout = 0; }).config.services.lg-buddy.settings
    ) "Invalid timeout must fail")
    (check (fails
      (home { services.lg-buddy.settings.tv.ip = "127.0.0.1"; }).config.services.lg-buddy.settings
    ) "TV profiles must not enter store declarations")
    (check (fails
      (home { services.lg-buddy.configPath = "/nix/store/config.env"; })
      .config.services.lg-buddy.configPath
    ) "Mutable config must not enter store")
    (check (fails
      (home { services.lg-buddy.configPath = "/home/alice/../config.env"; })
      .config.services.lg-buddy.configPath
    ) "Noncanonical config must fail")
    (check (builtins.all (
      a: a.assertion
    ) machine.assertions) "Valid machine configuration must pass assertions")
    (check (builtins.all (a: a.assertion)
      (integrated { }).config.assertions
    ) "Matching integrated home must pass assertions")
    (check (builtins.all (overrides: fails (integrated overrides).activationPackage) [
      { user = "bob"; }
      { configPath = "/home/alice/other.env"; }
      { package = pkgs.writeShellScriptBin "lg-buddy" "exit 1"; }
    ]) "Mismatched user, config or package must fail")
    (check (builtins.any (a: !a.assertion)
      (system { networking.networkmanager.enable = lib.mkForce false; }).config.assertions
    ) "Missing NetworkManager must fail")
  ];
in
assert builtins.all (x: x) checks;
pkgs.runCommand "lg-buddy-module-evaluation" { } ''touch "$out"''
