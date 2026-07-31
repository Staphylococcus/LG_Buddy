{
  lib,
  rustPlatform,
  makeWrapper,
  writeShellScriptBin,
  dbus,
  iputils,
  networkmanager,
  python3,
  systemd,
  xdg-utils,
  zenity,
  bscpylgtv,
  buildCommit ? null,
  source ? ../.,
}:

let
  crate = builtins.fromTOML (builtins.readFile ../crates/lg-buddy/Cargo.toml);
  testDbusDaemon = writeShellScriptBin "dbus-daemon" ''
    set -eu

    forwarded=()
    for argument in "$@"; do
      if [ "$argument" != "--session" ]; then
        forwarded+=("$argument")
      fi
    done

    exec ${lib.getExe' dbus "dbus-daemon"} \
      --config-file=${dbus}/share/dbus-1/session.conf \
      "''${forwarded[@]}"
  '';
in
rustPlatform.buildRustPackage {
  pname = "lg-buddy";
  inherit (crate.package) version;

  src = lib.cleanSourceWith {
    src = source;
    filter =
      path: type:
      let
        relative = lib.removePrefix "${toString source}/" (toString path);
        included =
          builtins.elem relative [
            "Cargo.toml"
            "Cargo.lock"
            "LG_Buddy_Brightness.desktop"
            "crates"
            "tools"
          ]
          || lib.hasPrefix "crates/" relative
          || lib.hasPrefix "tools/" relative;
      in
      lib.cleanSourceFilter path type && included;
  };
  cargoLock.lockFile = ../Cargo.lock;
  env = lib.optionalAttrs (buildCommit != null) {
    LG_BUDDY_BUILD_COMMIT = buildCommit;
  };

  nativeBuildInputs = [ makeWrapper ];
  nativeCheckInputs = [ python3 ];

  preCheck = ''
    # The test harness expects a conventional host --session configuration.
    # Put a test-only adapter first in PATH so it uses the immutable Nix store
    # equivalent instead of looking under /etc.
    export PATH="${testDbusDaemon}/bin:$PATH"
  '';

  postInstall = ''
    wrapProgram "$out/bin/lg-buddy" \
      --set-default LG_BUDDY_BSCPYLGTV_COMMAND "${bscpylgtv}/bin/bscpylgtvcommand" \
      --set-default LG_BUDDY_JOURNALCTL "${systemd}/bin/journalctl" \
      --set-default LG_BUDDY_NM_ONLINE "${networkmanager}/bin/nm-online" \
      --set-default LG_BUDDY_PING "${iputils}/bin/ping" \
      --set-default LG_BUDDY_SYSTEMCTL "${systemd}/bin/systemctl" \
      --set-default LG_BUDDY_XDG_OPEN "${xdg-utils}/bin/xdg-open" \
      --set-default LG_BUDDY_ZENITY "${zenity}/bin/zenity"

    install -Dm644 LG_Buddy_Brightness.desktop \
      "$out/share/applications/LG_Buddy_Brightness.desktop"
    substituteInPlace "$out/share/applications/LG_Buddy_Brightness.desktop" \
      --replace-fail "Exec=/usr/bin/lg-buddy brightness" \
      "Exec=$out/bin/lg-buddy brightness"
  '';

  meta = {
    description = "Make an LG webOS TV behave like a monitor for a Linux PC";
    homepage = "https://github.com/Staphylococcus/LG_Buddy";
    license = lib.licenses.gpl3Only;
    mainProgram = "lg-buddy";
    platforms = lib.platforms.linux;
  };
}
