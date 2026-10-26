{
  runCommand,
  package,
  bash,
  dbus,
  glib,
  xdotool,
  xvfb-run,
}:

runCommand "lg-buddy-package-check" {
  nativeBuildInputs = [ bash dbus glib xdotool xvfb-run ];
} ''
  export HOME="$TMPDIR/home"
  export XDG_CONFIG_HOME="$HOME/.config"
  export XDG_DATA_HOME="$HOME/.local/share"
  export XDG_CACHE_HOME="$HOME/.cache"
  export XDG_STATE_HOME="$HOME/.local/state"
  export XDG_RUNTIME_DIR="$TMPDIR/runtime"
  mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
  chmod 700 "$XDG_RUNTIME_DIR"
  unset LG_BUDDY_CONFIG LG_BUDDY_GUI DBUS_SESSION_BUS_ADDRESS DISPLAY

  ${package}/bin/lg-buddy --version > cli-version
  ${package}/bin/lg-buddy-gui --version > gui-version
  cmp cli-version gui-version
  grep -Fx 'version: ${package.version}' cli-version

  test -x ${package}/bin/.lg-buddy-gui-wrapped
  grep -Fx 'Exec=${package}/bin/lg-buddy' \
    ${package}/share/applications/io.github.staphylococcus.LGBuddy.desktop
  test -s ${package}/share/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg
  grep -F "['${package}/bin/lg-buddy', ...args]" \
    ${package}/share/gnome-shell/extensions/lg-buddy-brightness@staphylococcus.github.io/extension.js
  cmp ${../data/gnome-shell}/lg-buddy-brightness@staphylococcus.github.io/brightness.js \
    ${package}/share/gnome-shell/extensions/lg-buddy-brightness@staphylococcus.github.io/brightness.js
  grep -Fx 'runtime=${package}/bin/lg-buddy' ${package}/lib/lg-buddy/kwin/setup.sh
  # Packaging the helper must not change the source fingerprint expected by setup.
  for source in CMakeLists.txt main.cpp metadata.json; do
    cmp ${../data/kwin/source}/"$source" ${package}/lib/lg-buddy/kwin/source/"$source"
  done

  # Exercise discovery from a profile symlink and the real GTK wrapper without
  # inheriting a desktop bus, user config, or host GTK resource environment.
  mkdir -p profile/bin
  ln -s ${package}/bin/lg-buddy profile/bin/lg-buddy
  dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- xvfb-run -a \
    bash ${../scripts/test-gui-launch.sh} "$PWD/profile/bin/lg-buddy" ${package}/bin/lg-buddy-gui
  dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- xvfb-run -a \
    bash ${../scripts/test-gui-launch.sh} ${package}/bin/lg-buddy-gui

  touch "$out"
''
