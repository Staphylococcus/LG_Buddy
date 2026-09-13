# CI-only toolchains. The resulting plugin has no Nix runtime dependency.
{ kwinVersion, qtMinor }:
let
  manifest = builtins.fromJSON (builtins.readFile ./targets.json);
  env = manifest.environments.${qtMinor};
  kdeEnv = manifest.kde_environments.${builtins.head (builtins.match "(6[.][0-9]+)[.][0-9]+" kwinVersion)};
  target = builtins.head (builtins.filter (row: row.version == kwinVersion) manifest.kwin);
  source = pin: builtins.fetchTarball {
    url = "https://api.github.com/repos/NixOS/nixpkgs/tarball/${pin.rev}";
    sha256 = pin.hash;
  };
  kdePkgs = import (source kdeEnv) { system = "x86_64-linux"; };
  importKdeRecipes = kdeEnv.rev != env.qt_nixpkgs.rev;
  needsPlasmaUpdate = builtins.compareVersions kwinVersion kdePkgs.kdePackages.kwin.version > 0;
  pkgs = import (source env.qt_nixpkgs) {
    system = "x86_64-linux";
    # Import the matching KDE recipes into one complete Qt/system package set.
    # Mixing prebuilt Qt libraries into a different base mixes libc/glib too.
    overlays = if !importKdeRecipes && !needsPlasmaUpdate then [ ] else [ (final: prev: {
      libgbm = prev.libgbm or prev.mesa;
      kdePackages = (if importKdeRecipes then final.callPackage ((source kdeEnv) + "/pkgs/kde") { }
        else prev.kdePackages).overrideScope (kdeFinal: kdePrev: {
        # Later maintenance releases require Plasma libraries at least as new
        # as themselves. Keep the proven Qt/Frameworks baseline and update the
        # Plasma sources only for targets beyond the recipe snapshot.
        sources = kdePrev.sources // prev.lib.optionalAttrs needsPlasmaUpdate
          (prev.lib.mapAttrs (name: pin: (prev.fetchzip {
            url = "https://api.github.com/repos/KDE/${name}/tarball/${pin.rev}";
            extension = "tar.gz";
            sha256 = pin.sha256;
          }) // { version = kdeEnv.plasma.version; }) kdeEnv.plasma.sources);
      } // prev.lib.optionalAttrs importKdeRecipes {
        lib = kdePkgs.lib;
        libsForQt5 = final.libsForQt5 // {
          __internalKF5 = final.libsForQt5.__internalKF5 or final.libsForQt5;
        };
        # Protocol XML has no runtime ABI; use the newer pinned headers.
        wayland-protocols = if prev.lib.versionOlder prev.wayland-protocols.version kdePkgs.wayland-protocols.version
          then kdePkgs.wayland-protocols else prev.wayland-protocols;
        mkKdeDerivation = args: kdePrev.mkKdeDerivation (args // {
          hasPythonBindings = false;
          extraCmakeFlags = (args.extraCmakeFlags or [ ])
            ++ prev.lib.optional (args.hasPythonBindings or false) "-DBUILD_PYTHON_BINDINGS=OFF";
        });
      });
    }) ];
  };
  inherit (pkgs) lib;
  kwin = pkgs.kdePackages.kwin.overrideAttrs {
    version = target.version;
    src = pkgs.fetchzip {
      url = "https://api.github.com/repos/KDE/kwin/tarball/${target.rev}";
      extension = "tar.gz";
      sha256 = target.sha256;
    };
  };
  plugin = pkgs.stdenv.mkDerivation {
    pname = "lg-buddy-kwin-bridge";
    version = "${kwinVersion}-qt${env.qt_version}";
    src = ../../data/kwin/source;
    nativeBuildInputs = [ pkgs.cmake pkgs.ninja pkgs.pkg-config pkgs.patchelf ];
    buildInputs = [ kwin.dev pkgs.kdePackages.extra-cmake-modules pkgs.qt6.qtbase ];
    dontWrapQtApps = true;
    NIX_DONT_SET_RPATH = true;
    cmakeFlags = [ "-DCMAKE_SKIP_RPATH=ON" ];
    preConfigure = ''
      source_id="$(sha256sum CMakeLists.txt main.cpp metadata.json | sha256sum | cut -d ' ' -f1)"
      cmakeFlagsArray+=("-DLG_BUDDY_KWIN_BUILD_ID=$source_id")
    '';
    postConfigure = ''
      IFS=$'\t' read -r actual_kwin actual_qt arch actual_source < build-info.tsv
      test "$actual_kwin" = '${kwinVersion}'
      test "$actual_qt" = '${env.qt_version}'
      test "$arch" = x86_64
      test "$actual_source" = "$source_id"
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p "$out"
      cp lg_buddy_inhibition.so "$out/plugin.so"
      cp build-info.tsv "$out/build-info.tsv"
      runHook postInstall
    '';
    postFixup = ''
      # Installed machines resolve their own Qt/KWin/system libraries.
      patchelf --remove-rpath "$out/plugin.so"
      strip --strip-unneeded "$out/plugin.so"
      test -z "$(patchelf --print-rpath "$out/plugin.so")"
      if grep -a -q /nix/store/ "$out/plugin.so"; then
        echo 'Portable plugin retains a Nix store reference.' >&2
        strings "$out/plugin.so" | grep /nix/store/ >&2
        exit 1
      fi
      digest="$(sha256sum "$out/plugin.so" | cut -d ' ' -f1)"
      IFS=$'\t' read -r actual_kwin actual_qt arch actual_source < "$out/build-info.tsv"
      mkdir "$out/$actual_kwin-$actual_qt-$arch-$digest"
      mv "$out/plugin.so" "$out/$actual_kwin-$actual_qt-$arch-$digest/"
      printf '%s\t%s\t%s\t%s\t%s\n' "$actual_kwin" "$actual_qt" "$arch" "$actual_source" "$digest" \
        > "$out/$actual_kwin-$actual_qt-$arch-$digest/metadata.tsv"
      rm "$out/build-info.tsv"
    '';
    allowedReferences = [ ];
  };
in
assert builtins.elem qtMinor target.qt_minors;
assert pkgs.qt6.qtbase.version == env.qt_version;
{
  inherit kwin plugin;
  testEnvironment = pkgs.mkShell {
    nativeBuildInputs = [ pkgs.mesa.llvmpipeHook ];
    packages = [
      pkgs.bash pkgs.coreutils pkgs.findutils pkgs.gnugrep pkgs.gnused pkgs.gawk
      pkgs.dbus pkgs.systemd pkgs.binutils pkgs.patchelf
    ];
    LG_BUDDY_TEST_KWIN = "${kwin}/bin/kwin_wayland";
    LG_BUDDY_TEST_QT = env.qt_version;
    LG_BUDDY_TEST_LIBRARY_PATH = lib.makeLibraryPath [ kwin pkgs.qt6.qtbase pkgs.stdenv.cc.cc.lib ];
    QT_PLUGIN_PATH = "${kwin}/lib/qt-6/plugins:${pkgs.qt6.qtbase}/lib/qt-6/plugins";
    QT_QPA_PLATFORM_PLUGIN_PATH = "${kwin}/lib/qt-6/plugins/platforms";
  };
}
