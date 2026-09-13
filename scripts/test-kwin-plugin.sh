#!/bin/bash
# Real KWin loader/ABI smoke for every CI prebuilt, inside a disposable container.
set -euo pipefail
artifact_root="${1:?artifact directory required}"
metadata="$(find "$artifact_root" -type f -name metadata.tsv -print -quit)"
IFS=$'\t' read -r version qt arch source_id digest < "$metadata"
plugin_id="lg_buddy_inhibition_$(id -u)_$digest"
plugin_file="/usr/lib64/qt6/plugins/kwin/plugins/$plugin_id.so"
install -m 644 "${metadata%/*}/plugin.so" "$plugin_file"
export XDG_RUNTIME_DIR="$(mktemp -d)"
chmod 700 "$XDG_RUNTIME_DIR"
export KWIN_COMPOSE=O2 LIBGL_ALWAYS_SOFTWARE=1
# Fedora's executable carries CAP_SYS_NICE, which Docker does not grant. A
# disposable copy drops file capabilities; the virtual backend needs none.
cp /usr/bin/kwin_wayland "$XDG_RUNTIME_DIR/kwin_wayland"
"$XDG_RUNTIME_DIR/kwin_wayland" --virtual --no-lockscreen --no-global-shortcuts > "$XDG_RUNTIME_DIR/kwin.log" 2>&1 &
kwin_pid=$!
trap 'kill "$kwin_pid" 2>/dev/null || true; wait "$kwin_pid" 2>/dev/null || true; cat "$XDG_RUNTIME_DIR/kwin.log"; rm -f "$plugin_file"; rm -rf "$XDG_RUNTIME_DIR"' EXIT
ready=0
for _ in {1..150}; do
    kill -0 "$kwin_pid" 2>/dev/null || break
    if busctl --user --timeout=1 get-property org.kde.KWin /Plugins org.kde.KWin.Plugins LoadedPlugins >/dev/null 2>&1; then ready=1; break; fi
    sleep 0.1
done
[ "$ready" -eq 1 ]
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
