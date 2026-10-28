{
  description = "LG Buddy package and declarative NixOS/Home Manager integration";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  inputs.home-manager = {
    url = "github:nix-community/home-manager/release-26.05";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      home-manager,
    }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      nixosModules.lg-buddy = import ./nix/modules/nixos.nix { source = self; };
      nixosModules.default = self.nixosModules.lg-buddy;
      homeManagerModules.lg-buddy = import ./nix/modules/home-manager.nix { source = self; };
      homeManagerModules.default = self.homeManagerModules.lg-buddy;

      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          lg-buddy = pkgs.callPackage ./nix/package.nix {
            src = self;
            buildCommit = self.rev or self.dirtyRev or "unknown";
          };
        in
        {
          inherit lg-buddy;
          lg-buddy-kwin = pkgs.callPackage ./nix/kwin.nix { package = lg-buddy; };
          default = lg-buddy;
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          package = pkgs.callPackage ./nix/check-package.nix {
            package = self.packages.${system}.lg-buddy;
          };
          module-evaluation = import ./nix/tests/evaluation.nix {
            inherit
              self
              nixpkgs
              home-manager
              pkgs
              ;
          };
          kwin = pkgs.callPackage ./nix/check-kwin.nix {
            package = self.packages.${system}.lg-buddy;
            bridge = self.packages.${system}.lg-buddy-kwin;
          };
        }
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
          modules = import ./nix/tests/modules.nix {
            inherit self home-manager pkgs;
            package = self.packages.${system}.lg-buddy;
          };
        }
      );
    };
}
