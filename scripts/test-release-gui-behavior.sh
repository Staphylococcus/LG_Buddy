#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 <installed-lg-buddy> <config-file> <tv-fixture> [update-archive]"
    exit 1
}

RUNTIME_BINARY="${1:-}"
CONFIG_FILE="${2:-}"
TV_FIXTURE="${3:-${LG_BUDDY_GUI_TV_FIXTURE:-}}"
UPDATE_ARCHIVE="${4:-${LG_BUDDY_GUI_UPDATE_ARCHIVE:-}}"
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPOSITORY_ROOT="$(dirname "$SCRIPT_DIR")"
WORK_DIR="$(mktemp -d)"
STATE_FILE="$WORK_DIR/behavior-tv/state.json"
GET_BRIGHTNESS="ssap://settings/getSystemSettings"
SET_BRIGHTNESS="ssap://system.notifications/createAlert"
SET_VOLUME="ssap://audio/setVolume"
SET_MUTE="ssap://audio/setMute"
GET_MODEL="ssap://system/getSystemInfo"
WINDOW_TITLE="LG Buddy"
GUI_PID=""
WINDOW_ID=""
ACCESSIBILITY_BUS_PID=""
ACCESSIBILITY_REGISTRY_PID=""
ACCESSIBILITY_PYTHON=""
TV_FIXTURE_PID=""
GITHUB_FIXTURE_PID=""
SYSTEMD_CONFIG_FIXTURE_PID=""

fail() {
    echo "$1" >&2
    exit 1
}

cleanup() {
    local status=$?
    if [ "$status" -ne 0 ] && [ -f "$WORK_DIR/gui.output" ]; then
        cat "$WORK_DIR/gui.output" >&2
    fi
    if [ -n "$GUI_PID" ] && kill -0 "$GUI_PID" 2>/dev/null; then
        kill "$GUI_PID"
        wait "$GUI_PID" 2>/dev/null || true
    fi
    for fixture_pid in "$TV_FIXTURE_PID" "$GITHUB_FIXTURE_PID" "$SYSTEMD_CONFIG_FIXTURE_PID"; do
        if [ -n "$fixture_pid" ] && kill -0 "$fixture_pid" 2>/dev/null; then
            kill "$fixture_pid" 2>/dev/null || true
            wait "$fixture_pid" 2>/dev/null || true
        fi
    done
    if [ -n "$ACCESSIBILITY_REGISTRY_PID" ] && kill -0 "$ACCESSIBILITY_REGISTRY_PID" 2>/dev/null; then
        kill "$ACCESSIBILITY_REGISTRY_PID"
        wait "$ACCESSIBILITY_REGISTRY_PID" 2>/dev/null || true
    fi
    if [ -n "$ACCESSIBILITY_BUS_PID" ] && kill -0 "$ACCESSIBILITY_BUS_PID" 2>/dev/null; then
        kill "$ACCESSIBILITY_BUS_PID"
        wait "$ACCESSIBILITY_BUS_PID" 2>/dev/null || true
    fi
    if [ "${LG_BUDDY_KEEP_GUI_SMOKE:-0}" = 1 ]; then
        echo "GUI smoke evidence: $WORK_DIR" >&2
    else
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

[ -x "$RUNTIME_BINARY" ] || usage
[ -r "$CONFIG_FILE" ] || usage
[ -n "${DISPLAY:-}" ] || fail "DISPLAY is required for GUI behavior smoke."
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || fail "A D-Bus session is required for GUI behavior smoke."
command -v xdotool >/dev/null || fail "xdotool is required for GUI behavior smoke."
[ -x "$TV_FIXTURE" ] || fail "Native TV fixture is required for GUI behavior smoke: $TV_FIXTURE"

cp "$CONFIG_FILE" "$WORK_DIR/config.env"
CONFIG_FILE="$WORK_DIR/config.env"
# Operational GUI tests use the supported backend and only a loopback TV.
sed -i '/^tvs_primary_platform=/d; /^tvs_primary_ip=/d' "$CONFIG_FILE"
printf '%s\n' 'tvs_primary_platform=lg_webos' 'tvs_primary_ip=127.0.0.1' >> "$CONFIG_FILE"
export LG_BUDDY_CONFIG="$CONFIG_FILE"

stop_behavior_tv() {
    if [ -n "$TV_FIXTURE_PID" ]; then
        printf 'stop\n' > "$WORK_DIR/behavior-tv/command.tmp"
        mv "$WORK_DIR/behavior-tv/command.tmp" "$WORK_DIR/behavior-tv/command"
        wait "$TV_FIXTURE_PID" || fail "Native TV fixture failed."
        TV_FIXTURE_PID=""
    fi
}

reset_tv_state() {
    stop_behavior_tv
    mkdir -p "$WORK_DIR/behavior-tv" "$WORK_DIR/tvs/primary"
    printf '%s\n' '{"access_token":"webos-test-access-token"}' > "$WORK_DIR/tvs/primary/access-token.json"
    chmod 600 "$WORK_DIR/tvs/primary/access-token.json"
    rm -f "$STATE_FILE"
    python3 - "$WORK_DIR/behavior-tv/initial-state.json" "$@" <<'PY_STATE'
import json, sys
from pathlib import Path
path, backlight, volume, muted, *fault = sys.argv[1:]
state = {"backlight": int(backlight), "volume": int(volume), "muted": muted == "true"}
if fault:
    uri, delay, reject = fault
    state["fault"] = {"uri": uri, "delay_ms": int(delay), "reject": reject == "true"}
Path(path).write_text(json.dumps(state))
PY_STATE
    "$TV_FIXTURE" "$WORK_DIR/behavior-tv" > "$WORK_DIR/behavior-tv.output" 2>&1 &
    TV_FIXTURE_PID=$!
    for ((attempt = 0; attempt < 100; attempt++)); do
        [ ! -e "$STATE_FILE" ] || return 0
        kill -0 "$TV_FIXTURE_PID" 2>/dev/null || fail "Native TV fixture exited before becoming ready."
        sleep 0.05
    done
    fail "Native TV fixture did not become ready."
}

start_gui() {
    local accessibility="${1:-disabled}"
    local color_scheme="${2:-}"
    local scale="${3:-}"
    local entrypoint="${4:-brightness}"
    local -a gui_arguments=()
    if [ "$entrypoint" = "brightness" ]; then
        gui_arguments=(brightness)
    elif [ "$entrypoint" != "normal" ]; then
        fail "Unknown GUI smoke entrypoint: $entrypoint"
    fi
    local -a gui_environment=(
        ADW_DISABLE_PORTAL=1
        GDK_BACKEND=x11
        GDK_DEBUG=no-portals
    )
    [ -z "$color_scheme" ] || gui_environment+=("ADW_DEBUG_COLOR_SCHEME=$color_scheme")
    [ -z "$scale" ] || gui_environment+=("GDK_SCALE=$scale")
    WINDOW_ID=""
    if [ "$accessibility" = "enabled" ]; then
        env -u NO_AT_BRIDGE "${gui_environment[@]}" \
            "$RUNTIME_BINARY" "${gui_arguments[@]}" >"$WORK_DIR/gui.output" 2>&1 &
    else
        env "${gui_environment[@]}" NO_AT_BRIDGE=1 \
            "$RUNTIME_BINARY" "${gui_arguments[@]}" >"$WORK_DIR/gui.output" 2>&1 &
    fi
    GUI_PID=$!
    for ((attempt = 0; attempt < 300; attempt++)); do
        WINDOW_ID="$(xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" 2>/dev/null | head -n1 || true)"
        if [ -n "$WINDOW_ID" ]; then
            xdotool windowfocus --sync "$WINDOW_ID"
            return 0
        fi
        kill -0 "$GUI_PID" 2>/dev/null || fail "GUI exited before presenting its window."
        sleep 0.1
    done
    fail "GUI did not present its window."
}

wait_for_requests() {
    local command="$1"
    local count="$2"
    for ((attempt = 0; attempt < 300; attempt++)); do
        if python3 - "$STATE_FILE" "$command" "$count" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.exists():
    raise SystemExit(1)
state = json.loads(path.read_text(encoding="utf-8"))
observed = state.get("request_uris", []).count(sys.argv[2])
raise SystemExit(0 if observed >= int(sys.argv[3]) else 1)
PY
        then
            return 0
        fi
        sleep 0.1
    done
    fail "Timed out waiting for $count $command native requests."
}

finish_gui() {
    local scenario="${1:-closing action}"
    local status=0

    for ((attempt = 0; attempt < 300; attempt++)); do
        kill -0 "$GUI_PID" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$GUI_PID" 2>/dev/null; then
        if xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" >/dev/null 2>&1; then
            echo "Overview remained visible; the closing action was not observed." >&2
        else
            echo "Overview closed, but the GUI process remained alive." >&2
        fi
        if [ -s "$WORK_DIR/gui.output" ]; then
            echo "GUI output for $scenario:" >&2
            sed 's/^/  /' "$WORK_DIR/gui.output" >&2
        fi
        fail "GUI did not exit after $scenario."
    fi
    wait "$GUI_PID" || status=$?
    GUI_PID=""
    [ "$status" -eq 0 ] || fail "GUI exited with status $status."
}

send_closing_mnemonic() {
    # The key-down event closes the window, so xdotool may see BadWindow while
    # sending key-up. The following behavior and bounded process wait verify it.
    xdotool key --window "$WINDOW_ID" "$1" 2>/dev/null || true
}

start_accessibility_bus() {
    local launcher=""
    local registry=""
    local candidate=""

    for candidate in \
        "$(command -v python3 2>/dev/null || true)" \
        /usr/bin/python3; do
        if [ -n "$candidate" ] && [ -x "$candidate" ] && \
            "$candidate" -c 'import pyatspi' >/dev/null 2>&1; then
            ACCESSIBILITY_PYTHON="$candidate"
            break
        fi
    done
    [ -n "$ACCESSIBILITY_PYTHON" ] || fail "Python AT-SPI bindings are required for GUI state verification."

    for candidate in \
        "$(command -v at-spi-bus-launcher 2>/dev/null || true)" \
        /usr/libexec/at-spi-bus-launcher \
        /usr/lib/at-spi-bus-launcher \
        /usr/lib/at-spi2-core/at-spi-bus-launcher; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            launcher="$candidate"
            break
        fi
    done
    [ -n "$launcher" ] || fail "at-spi-bus-launcher is required for accessibility verification."

    for candidate in \
        "$(command -v at-spi2-registryd 2>/dev/null || true)" \
        /usr/libexec/at-spi2-registryd \
        /usr/lib/at-spi2-registryd \
        /usr/lib/at-spi2-core/at-spi2-registryd; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            registry="$candidate"
            break
        fi
    done
    [ -n "$registry" ] || fail "at-spi2-registryd is required for accessibility verification."

    "$launcher" --launch-immediately >"$WORK_DIR/at-spi-bus.output" 2>&1 &
    ACCESSIBILITY_BUS_PID=$!
    sleep 0.2
    "$registry" --use-gnome-session >"$WORK_DIR/at-spi-registry.output" 2>&1 &
    ACCESSIBILITY_REGISTRY_PID=$!
    sleep 0.2
}

observe_gui_state() {
    "$ACCESSIBILITY_PYTHON" "$SCRIPT_DIR/test-release-gui-accessibility.py" \
        --timeout 30 "$@"
}

if [ "${LG_BUDDY_GUI_JOURNEY_ONLY:-0}" = 1 ]; then
    start_accessibility_bus
    source "$SCRIPT_DIR/test-release-gui-journey.sh"
    run_installed_gui_journey
    exit 0
fi

# A plain installed launch opens Overview. An explicit brightness activation
# from TVs returns to the same window and focuses the slider after the read.
reset_tv_state 50 20 true "$GET_BRIGHTNESS" 2000 false
start_accessibility_bus
cp "$CONFIG_FILE" "$WORK_DIR/current-config.env"
sed -i 's/^tvs_primary_platform=lg_webos$/tvs_primary_platform=bscpylgtv/' "$CONFIG_FILE"
cp "$CONFIG_FILE" "$WORK_DIR/stale-config.env"
start_gui enabled "" "" normal
observe_gui_state --expected-text "saved TV configuration needs migration"
observe_gui_state --select-page Settings
observe_gui_state --expected-settings-state ready
send_closing_mnemonic Escape
finish_gui "migration gate with Settings still available"
cmp "$CONFIG_FILE" "$WORK_DIR/stale-config.env" || fail "Migration gate changed configuration."
python3 - "$STATE_FILE" <<'PY_STALE'
import json, sys
state = json.load(open(sys.argv[1]))
assert state["connection_count"] == 0 and not state["request_uris"], state
PY_STALE
cp "$WORK_DIR/current-config.env" "$CONFIG_FILE"
start_gui enabled "" "" normal
NORMAL_GUI_PID="$GUI_PID"
NORMAL_WINDOW_ID="$WINDOW_ID"
observe_gui_state --select-page TVs
env -u NO_AT_BRIDGE ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals \
    "$RUNTIME_BINARY" brightness
kill -0 "$NORMAL_GUI_PID" 2>/dev/null || fail "Brightness activation replaced the running GUI process."
REACTIVATED_WINDOW_ID="$(xdotool search --onlyvisible --name "$WINDOW_TITLE" 2>/dev/null | head -n1 || true)"
[ "$REACTIVATED_WINDOW_ID" = "$NORMAL_WINDOW_ID" ] || fail "Brightness activation replaced the Overview window."
observe_gui_state --expected-state ready --expected-slider-value 50 --require-brightness-focus
observe_gui_state --select-page Settings
env -u NO_AT_BRIDGE ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals \
    "$RUNTIME_BINARY"
kill -0 "$NORMAL_GUI_PID" 2>/dev/null || fail "Normal activation replaced the running GUI process."
observe_gui_state --expected-state ready --expected-slider-value 50
send_closing_mnemonic Escape
finish_gui "brightness and normal activation from other views"

# Read current state, edit the initially focused brightness slider through the
# keyboard. Movement submits automatically and keeps Overview open.
reset_tv_state 50 20 true
start_gui enabled
wait_for_requests "$GET_BRIGHTNESS" 1
xdotool windowfocus --sync "$WINDOW_ID"
observe_gui_state --expected-state ready --expected-slider-value 50 \
    --expected-volume 20 --expected-muted true --require-brightness-focus
xdotool key --window "$WINDOW_ID" Right
wait_for_requests "$SET_BRIGHTNESS" 1
observe_gui_state --expected-state ready --expected-slider-value 55
# Volume uses the CLI's set-then-unmute behavior. Mute remains independently
# available, and both audio operations leave the brightness control usable.
observe_gui_state --expected-volume 20 --expected-muted true
observe_gui_state --focus-control "TV Volume" --window-id "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" Right
wait_for_requests "$SET_VOLUME" 1
wait_for_requests "$SET_MUTE" 1
observe_gui_state --expected-slider-value 55 --expected-volume 21 --expected-muted false
observe_gui_state --activate-control "Mute TV"
wait_for_requests "$SET_MUTE" 2
observe_gui_state --expected-slider-value 55 --expected-volume 21 --expected-muted true
# Native tab navigation shows the configured profile and preserves live controls.
observe_gui_state --select-page TVs
TV_ADDRESS="$(sed -n 's/^tvs_primary_ip=//p' "$CONFIG_FILE" | tail -n1)"
observe_gui_state --expected-tvs-state configured --expected-tv-address "$TV_ADDRESS" --expected-tv-name OLED42C2
# Settings reads the shared store without changing it, including invalid values.
cp "$CONFIG_FILE" "$WORK_DIR/before-settings.env"
printf '%s\n' 'screen_idle_timeout=600' 'updates_channel=not-a-channel' >> "$CONFIG_FILE"
cp "$CONFIG_FILE" "$WORK_DIR/settings-snapshot.env"
observe_gui_state --select-page Settings
observe_gui_state --expected-settings-state invalid --expected-settings-timeout 600 --expected-integration automatic
xdotool windowsize --sync "$WINDOW_ID" 700 780
observe_gui_state --focus-control "Restore policy" --window-id "$WINDOW_ID"
observe_gui_state --expected-settings-state invalid --expected-settings-timeout 600
cmp "$CONFIG_FILE" "$WORK_DIR/settings-snapshot.env" || fail "Inspecting Settings changed configuration."
# Returning to Settings reloads changes made outside the GUI.
observe_gui_state --select-page TVs
printf '%s\n' 'screen_idle_timeout=120' 'updates_channel=stable' >> "$CONFIG_FILE"
observe_gui_state --select-page Settings
observe_gui_state --expected-settings-state ready --expected-settings-timeout 120 --expected-integration automatic
# The timeout draft remains local until Enter.
cp "$CONFIG_FILE" "$WORK_DIR/before-settings-edit.env"
observe_gui_state --edit-settings-timeout 720 --window-id "$WINDOW_ID"
cmp "$CONFIG_FILE" "$WORK_DIR/before-settings-edit.env" || fail "Typing a timeout saved before finalization."
xdotool key --window "$WINDOW_ID" Return
observe_gui_state --expected-settings-state ready --expected-settings-timeout 720
[ "$("$RUNTIME_BINARY" settings get screen.idle_timeout)" = "720" ] || fail "Finalized timeout was not saved."
observe_gui_state --edit-settings-timeout invalid --window-id "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" Return
observe_gui_state --expected-settings-state ready --expected-settings-timeout 720
[ "$("$RUNTIME_BINARY" settings get screen.idle_timeout)" = "720" ] || fail "Invalid timeout changed configuration."
# Every saved legacy override has one explicit transition. Cancellation leaves
# the exact file intact; disabled idle monitoring needs no native idle provider.
for legacy_backend in gnome wayland swayidle; do
    observe_gui_state --select-page TVs
    cp "$WORK_DIR/before-settings.env" "$CONFIG_FILE"
    printf '%s\n' "screen_backend=$legacy_backend" 'screen_idle_blank=disabled' \
        'screen_idle_timeout=731' 'screen_restore_policy=aggressive' \
        'screen_honor_idle_inhibitors=enabled' >> "$CONFIG_FILE"
    cp "$CONFIG_FILE" "$WORK_DIR/legacy-transition.env"
    observe_gui_state --select-page Settings
    observe_gui_state --expected-settings-state ready --expected-integration legacy
    observe_gui_state --activate-control "Use automatic"
    observe_gui_state --activate-control "Cancel"
    cmp "$CONFIG_FILE" "$WORK_DIR/legacy-transition.env" || fail "Cancelling automatic integration changed $legacy_backend settings."
    observe_gui_state --activate-control "Use automatic"
    observe_gui_state --activate-control "Use automatic integration"
    observe_gui_state --expected-settings-state ready --expected-integration automatic
    [ "$("$RUNTIME_BINARY" settings get screen.backend)" = auto ] || fail "Automatic integration did not persist."
    sed "s/^screen_backend=$legacy_backend$/screen_backend=auto/" \
        "$WORK_DIR/legacy-transition.env" > "$WORK_DIR/automatic-transition.env"
    cmp "$CONFIG_FILE" "$WORK_DIR/automatic-transition.env" || fail "Automatic integration changed behavior settings."
done
cp "$WORK_DIR/before-settings.env" "$CONFIG_FILE"
observe_gui_state --select-page Overview
observe_gui_state --expected-slider-value 55 --expected-volume 21 --expected-muted true
xdotool windowfocus --sync "$WINDOW_ID"
send_closing_mnemonic Escape
finish_gui "cancellation after successful apply"
python3 - "$STATE_FILE" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
assert state["backlight"] != 50, state
assert "ssap://system.notifications/createAlert" in state["request_uris"], state
assert state["volume"] == 21 and state["muted"] is True, state
audio_calls = [uri for uri in state["request_uris"] if uri in ("ssap://audio/setVolume", "ssap://audio/setMute")]
assert audio_calls == ["ssap://audio/setVolume", "ssap://audio/setMute", "ssap://audio/setMute"], audio_calls
PY

# TV management uses native controls and the real local persistence backend.
cp "$CONFIG_FILE" "$WORK_DIR/before-management.env"
mkdir -p "$WORK_DIR/tvs/primary"
printf '%s\n' '{"access_token":"webos-test-access-token"}' > "$WORK_DIR/tvs/primary/access-token.json"
chmod 600 "$WORK_DIR/tvs/primary/access-token.json"
start_gui enabled
observe_gui_state --select-page TVs
observe_gui_state --expected-tvs-state configured --expected-tv-address "$TV_ADDRESS" --expected-tv-name OLED42C2
observe_gui_state --focus-control "HDMI input" --window-id "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" --delay 60 space Home Down Down Return
for ((attempt = 0; attempt < 100; attempt++)); do
    [ "$("$RUNTIME_BINARY" settings get tv.input)" = "HDMI_3" ] && break
    sleep 0.1
done
[ "$("$RUNTIME_BINARY" settings get tv.input)" = "HDMI_3" ] || fail "Input selection was not saved."
cp "$CONFIG_FILE" "$WORK_DIR/before-unpair.env"
observe_gui_state --activate-control "Unpair TV…"
observe_gui_state --expected-tvs-state unpair
xdotool key --window "$WINDOW_ID" Escape
observe_gui_state --expected-tvs-state configured --expected-tv-address "$TV_ADDRESS" --expected-tv-name OLED42C2
cmp -s "$CONFIG_FILE" "$WORK_DIR/before-unpair.env" || fail "Cancelling Unpair changed the configuration."
[ -f "$WORK_DIR/tvs/primary/access-token.json" ] || fail "Cancelling Unpair removed the credential."
observe_gui_state --activate-control "Unpair TV…"
observe_gui_state --expected-tvs-state unpair
observe_gui_state --activate-control Unpair
observe_gui_state --expected-tvs-state empty
[ ! -e "$WORK_DIR/tvs/primary/access-token.json" ] || fail "Unpair left the native credential."
observe_gui_state --activate-control "Pair a TV"
observe_gui_state --expected-tvs-state pairing
observe_gui_state --activate-control Cancel
observe_gui_state --expected-tvs-state empty
send_closing_mnemonic Escape
finish_gui "input editing and confirmed unpairing"
cp "$WORK_DIR/before-management.env" "$CONFIG_FILE"

# An absent profile has a standard empty state and never contacts the TV.
export LG_BUDDY_CONFIG="$WORK_DIR/no-config.env"
cp "$STATE_FILE" "$WORK_DIR/before-empty.json"
start_gui enabled "" "" normal
observe_gui_state --expected-tvs-state empty
observe_gui_state --activate-control "Pair a TV"
observe_gui_state --expected-tvs-state pairing
xdotool key --window "$WINDOW_ID" Return
observe_gui_state --expected-tvs-state pairing-invalid
xdotool key --window "$WINDOW_ID" Escape
observe_gui_state --expected-tvs-state empty
# A second opening starts with a fresh form and uses the header's Cancel button.
observe_gui_state --activate-control "Pair a TV"
observe_gui_state --expected-tvs-state pairing
observe_gui_state --activate-control Cancel
observe_gui_state --expected-tvs-state empty
send_closing_mnemonic Escape
finish_gui "empty TVs view"
python3 - "$WORK_DIR/before-empty.json" "$STATE_FILE" <<'PY_EMPTY'
import json, sys
before, after = [json.load(open(path)) for path in sys.argv[1:]]
for key in ("request_uris", "connection_count"):
    assert before[key] == after[key], f"Empty profile performed TV work: {key}"
PY_EMPTY
export LG_BUDDY_CONFIG="$CONFIG_FILE"

# A failed optional model read retains the local TV details.
reset_tv_state 50 20 false "$GET_MODEL" 0 true
start_gui enabled
wait_for_requests "$GET_MODEL" 1
observe_gui_state --select-page TVs
observe_gui_state --expected-tvs-state configured --expected-tv-address "$TV_ADDRESS"
send_closing_mnemonic Escape
finish_gui "unavailable TV model"

# A slow write must not disable the slider or discard subsequent movement.
reset_tv_state 50 20 false "$SET_BRIGHTNESS" 500 false
start_gui enabled
xdotool windowfocus --sync "$WINDOW_ID"
observe_gui_state --expected-slider-value 50 --expected-volume 20 --require-brightness-focus
xdotool key --window "$WINDOW_ID" --repeat 5 --delay 20 Right
observe_gui_state --expected-slider-value 75
wait_for_requests "$SET_BRIGHTNESS" 2
send_closing_mnemonic Escape
finish_gui "rapid slider movement"
python3 - "$STATE_FILE" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
assert state["backlight"] == 75, state
assert state["request_uris"].count("ssap://system.notifications/createAlert") == 2, state
PY

# A failed read stays visible and Retry performs a fresh read. Observe each
# rendered presentation before sending the action that depends on it.
reset_tv_state 64 20 false "$GET_BRIGHTNESS" 0 true
start_gui enabled
wait_for_requests "$GET_BRIGHTNESS" 1
observe_gui_state --expected-state read-failed --expected-volume 20 --expected-muted false
xdotool windowfocus --sync "$WINDOW_ID"
observe_gui_state --focus-control "TV Volume" --window-id "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" Right
wait_for_requests "$SET_VOLUME" 1
observe_gui_state --expected-state read-failed --expected-volume 21 --expected-muted false
xdotool windowfocus --sync "$WINDOW_ID"
observe_gui_state --activate-control "Retry OLED Pixel Brightness"
wait_for_requests "$GET_BRIGHTNESS" 2
observe_gui_state --expected-state ready --expected-slider-value 64
xdotool windowfocus --sync "$WINDOW_ID"
send_closing_mnemonic Escape
finish_gui "read-failure cancellation"

# Volume succeeded but unmuting failed: show the changed level and recover the
# remaining mute operation without repeating the successful volume write.
reset_tv_state 50 20 true "$SET_MUTE" 0 true
start_gui enabled
observe_gui_state --expected-volume 20 --expected-muted true
xdotool windowfocus --sync "$WINDOW_ID"
observe_gui_state --focus-control "TV Volume" --window-id "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" Right
wait_for_requests "$SET_MUTE" 1
observe_gui_state --expected-volume 21 --expected-muted true --require-audio-retry
observe_gui_state --activate-control "Retry Audio"
wait_for_requests "$SET_MUTE" 2
observe_gui_state --expected-volume 21 --expected-muted false
send_closing_mnemonic Escape
finish_gui "audio recovery cancellation"
python3 - "$STATE_FILE" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
assert state["volume"] == 21 and state["muted"] is False, state
assert state["request_uris"].count("ssap://audio/setVolume") == 1, state
PY

# Cancelling the loading window never writes a value.
reset_tv_state 37 20 false "$GET_BRIGHTNESS" 2000 false
start_gui
xdotool windowfocus --sync "$WINDOW_ID"
send_closing_mnemonic Escape
finish_gui "loading cancellation"
# Wait for the delayed brightness read before checking for writes. Other read
# workers may still be finishing when the next scenario resets the state.
wait_for_requests "$GET_BRIGHTNESS" 1
python3 - "$STATE_FILE" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if path.exists():
    state = json.loads(path.read_text(encoding="utf-8"))
    assert "ssap://system.notifications/createAlert" not in state["request_uris"], state
PY

stop_behavior_tv
if [ -n "$TV_FIXTURE" ]; then
    source "$SCRIPT_DIR/test-release-gui-journey.sh"
    run_installed_gui_journey
fi

if [ "${LG_BUDDY_TEST_PLATFORM_CONTRACT:-0}" = "1" ]; then
    command -v xwd >/dev/null || fail "xwd is required for theme verification."

    capture_platform_state() {
        local label="$1"
        local color_scheme="$2"
        local scale="$3"
        local geometry=""
        local screenshot="$WORK_DIR/$label.xwd"

        reset_tv_state 50 20 false
        start_gui enabled "$color_scheme" "$scale"
        wait_for_requests "$GET_BRIGHTNESS" 1
        observe_gui_state --expected-state ready --expected-slider-value 50
        geometry="$(xdotool getwindowgeometry --shell "$WINDOW_ID")"
        PLATFORM_WIDTH="$(printf '%s\n' "$geometry" | sed -n 's/^WIDTH=//p')"
        PLATFORM_HEIGHT="$(printf '%s\n' "$geometry" | sed -n 's/^HEIGHT=//p')"
        xwd -silent -id "$WINDOW_ID" -out "$screenshot"
        PLATFORM_MEAN="$(python3 "$SCRIPT_DIR/xwd_mean.py" "$screenshot")"

        observe_gui_state --select-page Settings
        observe_gui_state --expected-settings-state ready

        xdotool windowfocus --sync "$WINDOW_ID"
        send_closing_mnemonic Escape
        finish_gui "$label platform-state cancellation"
    }

    capture_platform_state light prefer-light 1
    LIGHT_WIDTH="$PLATFORM_WIDTH"
    LIGHT_HEIGHT="$PLATFORM_HEIGHT"
    LIGHT_MEAN="$PLATFORM_MEAN"
    capture_platform_state dark prefer-dark 2
    DARK_WIDTH="$PLATFORM_WIDTH"
    DARK_HEIGHT="$PLATFORM_HEIGHT"
    DARK_MEAN="$PLATFORM_MEAN"

    python3 - "$LIGHT_WIDTH" "$LIGHT_HEIGHT" "$LIGHT_MEAN" "$DARK_WIDTH" "$DARK_HEIGHT" "$DARK_MEAN" <<'PY'
import sys

light_width, light_height = map(int, sys.argv[1:3])
light_mean = float(sys.argv[3])
dark_width, dark_height = map(int, sys.argv[4:6])
dark_mean = float(sys.argv[6])

if dark_width < light_width * 1.5 or dark_height < light_height * 1.5:
    raise SystemExit(
        f"2x scale did not materially enlarge the window: "
        f"{light_width}x{light_height} -> {dark_width}x{dark_height}"
    )
if light_mean < dark_mean + 0.15:
    raise SystemExit(
        f"light and dark themes were not visibly distinct: {light_mean:.3f} vs {dark_mean:.3f}"
    )
PY
fi

stop_behavior_tv
echo "Release GUI behavior smoke passed."
