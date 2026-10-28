{
  lib,
  stdenv,
  cmake,
  pkg-config,
  kdePackages,
  package,
}:
stdenv.mkDerivation {
  pname = "lg-buddy-kwin";
  version = kdePackages.kwin.version;
  src = "${package}/lib/lg-buddy/kwin/source";
  nativeBuildInputs = [
    cmake
    pkg-config
    kdePackages.extra-cmake-modules
    kdePackages.wrapQtAppsHook
  ];
  buildInputs = [
    kdePackages.kwin
    kdePackages.qtbase
  ];
  strictDeps = true;
  dontWrapQtApps = true;
  # Match the runtime's manifest hash, including the unchanged metadata. Enable
  # through KWin configuration instead of altering the source being identified.
  preConfigure = ''
    source_id=$(sha256sum CMakeLists.txt main.cpp metadata.json | sha256sum)
    cmakeFlagsArray+=("-DLG_BUDDY_KWIN_BUILD_ID=''${source_id%% *}")
  '';
  postInstall = ''
    install -Dm644 build-info.tsv "$out/share/lg-buddy/kwin/build-info.tsv"
  '';
  meta = {
    description = "LG Buddy native inhibition bridge for the host's KWin and Qt";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux;
  };
}
