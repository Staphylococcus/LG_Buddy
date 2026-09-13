#!/bin/bash
# Real KWin loader/ABI smoke for every CI prebuilt, inside an isolated build environment or disposable container.
set -euo pipefail
artifact_root="${1:?artifact directory required}"
metadata="$(find "$artifact_root" -type f -name metadata.tsv -print -quit)"
[ -n "$metadata" ]
[ "$(find "$artifact_root" -type f -name metadata.tsv | wc -l)" -eq 1 ]
IFS=$'\t' read -r version qt arch source_id digest < "$metadata"
plugin_id="lg_buddy_inhibition_$(id -u)_$digest"
[ "$(sha256sum "${metadata%/*}/plugin.so" | cut -d ' ' -f1)" = "$digest" ]
kwin_executable="${LG_BUDDY_TEST_KWIN:-/usr/bin/kwin_wayland}"
export XDG_RUNTIME_DIR="$(mktemp -d)"
chmod 700 "$XDG_RUNTIME_DIR"
cleanup() {
    if [ -n "${kwin_pid:-}" ]; then
        kill "$kwin_pid" 2>/dev/null || true
        wait "$kwin_pid" 2>/dev/null || true
    fi
    [ ! -f "$XDG_RUNTIME_DIR/kwin.log" ] || cat "$XDG_RUNTIME_DIR/kwin.log"
    rm -r -- "$XDG_RUNTIME_DIR"
}
trap cleanup EXIT
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET QT_QPA_PLATFORM
export HOME="$XDG_RUNTIME_DIR/home"
export XDG_CONFIG_HOME="$HOME/config" XDG_CACHE_HOME="$HOME/cache" XDG_DATA_HOME="$HOME/data"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME"
plugin_file="$XDG_RUNTIME_DIR/plugins/kwin/plugins/$plugin_id.so"
install -D -m 644 "${metadata%/*}/plugin.so" "$plugin_file"
export QT_PLUGIN_PATH="$XDG_RUNTIME_DIR/plugins${QT_PLUGIN_PATH:+:$QT_PLUGIN_PATH}"
if [ -n "${LG_BUDDY_TEST_LIBRARY_PATH:-}" ]; then
    export LD_LIBRARY_PATH="$LG_BUDDY_TEST_LIBRARY_PATH${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
export KWIN_COMPOSE=O2 LIBGL_ALWAYS_SOFTWARE=1
# Fedora's executable carries CAP_SYS_NICE, which Docker does not grant. A
# disposable copy drops file capabilities; the virtual backend needs none.
cp "$kwin_executable" "$XDG_RUNTIME_DIR/kwin_wayland"
[ "$("$XDG_RUNTIME_DIR/kwin_wayland" --version | awk '{print $NF}')" = "$version" ]
"$XDG_RUNTIME_DIR/kwin_wayland" --virtual --no-lockscreen --no-global-shortcuts > "$XDG_RUNTIME_DIR/kwin.log" 2>&1 &
kwin_pid=$!
ready=0
for _ in {1..150}; do
    kill -0 "$kwin_pid" 2>/dev/null || break
    if busctl --user --timeout=1 get-property org.kde.KWin /Plugins org.kde.KWin.Plugins LoadedPlugins >/dev/null 2>&1; then ready=1; break; fi
    sleep 0.1
done
[ "$ready" -eq 1 ]
if [ -n "${LG_BUDDY_TEST_QT:-}" ]; then
    busctl --user call org.kde.KWin /KWin org.kde.KWin supportInformation | grep -F "Qt Version: $LG_BUDDY_TEST_QT" >/dev/null
fi
[ "$(busctl --user call org.kde.KWin /Plugins org.kde.KWin.Plugins LoadPlugin s "$plugin_id")" = 'b true' ]
service=io.github.staphylococcus.LGBuddy.KWinInhibition
path=/io/github/staphylococcus/LGBuddy/KWinInhibition
interface=io.github.staphylococcus.LGBuddy.KWinInhibition1
[ "$(busctl --user call "$service" "$path" "$interface" BuildVersion)" = "s \"$version\"" ]
[ "$(busctl --user call "$service" "$path" "$interface" BuildId)" = "s \"$source_id\"" ]
[ "$(busctl --user call "$service" "$path" "$interface" IsInhibited)" = 'b false' ]
busctl --user call org.kde.KWin /Plugins org.kde.KWin.Plugins UnloadPlugin s "$plugin_id"
[ "$(busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus NameHasOwner s "$service")" = 'b false' ]
kill -0 "$kwin_pid"
echo "PASS: KWin $version / Qt $qt / $arch loaded and queried $digest without restarting."
