{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  gtk4,
  libadwaita,
  bash,
  dbus,
  python3,
  src,
  buildCommit ? "unknown",
}:

let
  cleanSrc = lib.cleanSourceWith {
    inherit src;
    filter = path: _type: !(builtins.elem (baseNameOf path) [
      ".git" ".codex" ".tmp" ".worktrees" "target" "dist"
    ]);
  };
  cargoToml = builtins.fromTOML (builtins.readFile "${cleanSrc}/crates/lg-buddy/Cargo.toml");
in
rustPlatform.buildRustPackage {
  pname = "lg-buddy";
  version = cargoToml.package.version;
  src = cleanSrc;

  cargoLock.lockFile = "${cleanSrc}/Cargo.lock";
  cargoBuildFlags = [ "--package" "lg-buddy" "--package" "lg-buddy-gui" ];
  cargoTestFlags = [ "--package" "lg-buddy" "--lib" ];
  # Process and timeout fixtures should not compete with one another.
  dontUseCargoParallelTests = true;

  nativeBuildInputs = [ pkg-config wrapGAppsHook4 ];
  nativeCheckInputs = [ dbus python3 ];
  buildInputs = [ bash gtk4 libadwaita ];
  LG_BUDDY_BUILD_COMMIT = buildCommit;

  postInstall = ''
    kwin_dir="$out/lib/lg-buddy/kwin"
    mkdir -p "$kwin_dir"
    cp -r data/kwin/. "$kwin_dir/"
    chmod 755 "$kwin_dir/setup.sh" "$kwin_dir/build.sh"
    substituteInPlace "$kwin_dir/setup.sh" \
      --replace-fail 'runtime=/usr/bin/lg-buddy' "runtime=$out/bin/lg-buddy"
    patchShebangs --host "$kwin_dir"

    desktop_file="$out/share/applications/io.github.staphylococcus.LGBuddy.desktop"
    install -Dm644 io.github.staphylococcus.LGBuddy.desktop "$desktop_file"
    substituteInPlace "$desktop_file" \
      --replace-fail 'Exec=/usr/bin/lg-buddy' "Exec=$out/bin/lg-buddy"
    install -Dm644 \
      data/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg \
      "$out/share/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg"

    extension_uuid=lg-buddy-brightness@staphylococcus.github.io
    extension_dir="$out/share/gnome-shell/extensions/$extension_uuid"
    install -Dm644 "data/gnome-shell/$extension_uuid/metadata.json" "$extension_dir/metadata.json"
    install -Dm644 "data/gnome-shell/$extension_uuid/extension.js" "$extension_dir/extension.js"
    substituteInPlace "$extension_dir/extension.js" \
      --replace-fail "['lg-buddy', ...args]" "['$out/bin/lg-buddy', ...args]"
  '';

  # The CLI discovers its companion beside its real executable. Keep it as
  # an ELF and wrap only the GTK executable with its own runtime resources.
  dontWrapGApps = true;
  preFixup = ''
    wrapGApp "$out/bin/lg-buddy-gui"
  '';

  meta = {
    description = "Coordinate an LG TV with Linux desktop and system lifecycle events";
    homepage = "https://github.com/Staphylococcus/LG_Buddy";
    license = lib.licenses.gpl3Only;
    mainProgram = "lg-buddy";
    platforms = [ "x86_64-linux" "aarch64-linux" ];
  };
}
