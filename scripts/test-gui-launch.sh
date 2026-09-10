#!/bin/bash

set -euo pipefail
umask 0022

LAUNCH_BINARY="${1:-./target/debug/lg-buddy}"
GUI_BINARY="${2:-$(dirname "$LAUNCH_BINARY")/lg-buddy-gui}"
APPLICATION_ID="io.github.staphylococcus.LGBuddy"
WINDOW_TITLE="LG Buddy"
GUI_PID=""
REPLACEMENT_PID=""
WINDOW_IDS=()

fail() {
    echo "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "$GUI_PID" ] && kill -0 "$GUI_PID" 2>/dev/null; then
        kill "$GUI_PID"
        wait "$GUI_PID" 2>/dev/null || true
    fi
    if [ -n "$REPLACEMENT_PID" ] && kill -0 "$REPLACEMENT_PID" 2>/dev/null; then
        kill "$REPLACEMENT_PID"
        wait "$REPLACEMENT_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

[ -x "$LAUNCH_BINARY" ] || fail "GUI launcher is not executable: $LAUNCH_BINARY"
[ -x "$GUI_BINARY" ] || fail "GUI binary is not executable: $GUI_BINARY"
[ -n "${DISPLAY:-}" ] || fail "DISPLAY is required for the GUI launch smoke test."
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || fail "A D-Bus session is required for the GUI launch smoke test."
command -v gapplication >/dev/null || fail "gapplication is required for the GUI launch smoke test."
command -v xdotool >/dev/null || fail "xdotool is required for the GUI launch smoke test."

ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals NO_AT_BRIDGE=1 \
    "$LAUNCH_BINARY" &
GUI_PID=$!

for ((attempt = 0; attempt < 300; attempt++)); do
    if ! kill -0 "$GUI_PID" 2>/dev/null; then
        status=0
        wait "$GUI_PID" || status=$?
        fail "GUI process exited before presenting a window with status $status."
    fi

    mapfile -t WINDOW_IDS < <(
        xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" 2>/dev/null || true
    )
    [ "${#WINDOW_IDS[@]}" -le 1 ] || fail "GUI presented duplicate Overview windows."
    [ "${#WINDOW_IDS[@]}" -eq 0 ] || break
    sleep 0.1
done

[ "${#WINDOW_IDS[@]}" -eq 1 ] || fail "GUI did not present Overview."

ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals NO_AT_BRIDGE=1 \
    "$LAUNCH_BINARY" brightness
kill -0 "$GUI_PID" 2>/dev/null || fail "Reactivation replaced the running GUI process."
mapfile -t WINDOW_IDS < <(
    xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" 2>/dev/null || true
)
[ "${#WINDOW_IDS[@]}" -eq 1 ] || fail "Reactivation did not preserve one Overview window."

ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals NO_AT_BRIDGE=1 \
    "$GUI_BINARY" --gapplication-replace &
REPLACEMENT_PID=$!

for ((attempt = 0; attempt < 300; attempt++)); do
    if ! kill -0 "$REPLACEMENT_PID" 2>/dev/null; then
        status=0
        wait "$REPLACEMENT_PID" || status=$?
        fail "GApplication replacement exited before taking over the primary instance with status $status."
    fi
    if ! kill -0 "$GUI_PID" 2>/dev/null; then
        break
    fi
    sleep 0.1
done

if kill -0 "$GUI_PID" 2>/dev/null; then
    fail "GApplication replacement did not displace the incumbent GUI instance."
fi
wait "$GUI_PID" || fail "The incumbent GUI did not exit cleanly after replacement."
GUI_PID=""
for ((attempt = 0; attempt < 300; attempt++)); do
    mapfile -t WINDOW_IDS < <(
        xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" 2>/dev/null || true
    )
    [ "${#WINDOW_IDS[@]}" -eq 1 ] && break
    sleep 0.1
done
[ "${#WINDOW_IDS[@]}" -eq 1 ] || fail "GApplication replacement did not preserve one Overview window."

gapplication action "$APPLICATION_ID" quit
wait "$REPLACEMENT_PID"
REPLACEMENT_PID=""
