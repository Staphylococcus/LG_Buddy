{
  runCommand,
  lib,
  bash,
  dbus,
  systemd,
  kdePackages,
  mesa,
  package,
  bridge,
}:
runCommand "lg-buddy-kwin-check"
  {
    nativeBuildInputs = [
      bash
      dbus
      systemd
    ];
  }
  ''
    mkdir -p artifact
    installed=${bridge}/lib/qt-6/plugins/kwin/plugins/lg_buddy_inhibition.so
    IFS=$'\t' read -r version qt arch source_id < ${bridge}/share/lg-buddy/kwin/build-info.tsv
    expected=$(cd ${package}/lib/lg-buddy/kwin/source && sha256sum CMakeLists.txt main.cpp metadata.json | sha256sum)
    test "$source_id" = "''${expected%% *}"
    digest=$(sha256sum "$installed")
    cp "$installed" artifact/plugin.so
    printf '%s\t%s\t%s\t%s\t%s\n' "$version" "$qt" "$arch" "$source_id" "''${digest%% *}" > artifact/metadata.tsv
    export LG_BUDDY_TEST_KWIN=${kdePackages.kwin}/bin/kwin_wayland
    export LG_BUDDY_TEST_QT=${kdePackages.qtbase.version}
    export LIBGL_DRIVERS_PATH=${lib.getLib mesa}/lib/dri
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- \
      bash ${../scripts/test-kwin-plugin.sh} "$PWD/artifact"
    touch "$out"
  ''
