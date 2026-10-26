{
  description = "LG Buddy package";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          lg-buddy = pkgs.callPackage ./nix/package.nix {
            src = self;
            buildCommit = self.rev or self.dirtyRev or "unknown";
          };
        in
        {
          inherit lg-buddy;
          default = lg-buddy;
        });

      checks = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          package = pkgs.callPackage ./nix/check-package.nix {
            package = self.packages.${system}.lg-buddy;
          };
        });
    };
}
