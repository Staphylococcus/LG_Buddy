{
  description = "Reproducible Nix packaging and declarative integration for LG Buddy";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          bscpylgtv = pkgs.python3Packages.callPackage ./nix/bscpylgtv.nix { };
          lg-buddy = pkgs.callPackage ./nix/package.nix {
            inherit bscpylgtv;
            buildCommit = self.rev or (self.dirtyRev or null);
            source = self;
          };
        in
        {
          inherit bscpylgtv lg-buddy;
          default = lg-buddy;
        }
      );

      nixosModules.default =
        {
          lib,
          pkgs,
          ...
        }:
        {
          imports = [ ./nix/module.nix ];
          services.lg-buddy.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };

      formatter = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        pkgs.nixfmt-tree
      );

      checks = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          evaluated = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                boot.isContainer = true;
                system.stateVersion = "26.05";

                networking.networkmanager.enable = true;
                users.users.lg-buddy-test = {
                  isNormalUser = true;
                };

                services.lg-buddy = {
                  enable = true;
                  user = "lg-buddy-test";
                  tv = {
                    ip = "192.0.2.10";
                    mac = "02:00:00:00:00:10";
                    input = "HDMI_2";
                  };
                };
              }
            ];
          };
          moduleSummary = {
            package = evaluated.config.services.lg-buddy.package.name;
            systemService = evaluated.config.systemd.services.LG_Buddy.serviceConfig.ExecStart;
            lifecycleService = evaluated.config.systemd.services.LG_Buddy_lifecycle.serviceConfig.ExecStart;
            sessionService = evaluated.config.systemd.user.services.LG_Buddy_screen.serviceConfig.ExecStart;
            networkManagerHooks = builtins.length evaluated.config.networking.networkmanager.dispatcherScripts;
          };
        in
        {
          package = self.packages.${system}.default;
          module-evaluation = pkgs.writeText "lg-buddy-module-evaluation.json" (
            builtins.toJSON moduleSummary
          );
        }
      );
    };
}
