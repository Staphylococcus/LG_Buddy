# Native public-module qualification; src may be a pinned flake reference.
{
  src ? ../.,
  desktop ? "gnome",
}:
let
  self = builtins.getFlake (toString src);
  pkgs = import self.inputs.nixpkgs { system = "x86_64-linux"; };
in
import ../nix/tests/desktop.nix {
  inherit self pkgs desktop;
  home-manager = self.inputs.home-manager;
  package = self.packages.x86_64-linux.lg-buddy;
}
