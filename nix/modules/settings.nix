{ lib }:
let
  setting =
    type: description:
    lib.mkOption {
      type = lib.types.nullOr type;
      default = null;
      inherit description;
    };
  toggle = lib.types.enum [
    "enabled"
    "disabled"
  ];
in
{
  screen = {
    backend = setting (lib.types.enum [
      "auto"
      "gnome"
      "wayland"
    ]) "Legacy backend preference; normally leave automatic selection unmanaged.";
    idle_blank = setting toggle "Blank the TV when idle or locked.";
    honor_idle_inhibitors = setting toggle "Honor application keep-awake requests.";
    idle_timeout = setting (lib.types.ints.between 1 86400) "User inactivity timeout in seconds.";
    restore_policy = setting (lib.types.enum [
      "conservative"
      "aggressive"
    ]) "When activity may restore the TV.";
  };
  system.sleep_wake_policy = setting toggle "Power off before sleep and restore after waking.";
  updates = {
    auto_check = setting toggle "Retained preference; Nix-owned installations do not run application update checks.";
    channel = setting (lib.types.enum [
      "stable"
      "prerelease"
    ]) "Retained preference; Nix selects the installed version.";
  };
}
