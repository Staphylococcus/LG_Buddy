#!/bin/bash
# Sourced by test-release-gui-behavior.sh; reuse its installed app, isolated
# configuration, accessibility observer, and process lifecycle helpers.

journey_setting() {
    local key="$1" expected="$2"
    for ((attempt = 0; attempt < 100; attempt++)); do
        [ "$("$RUNTIME_BINARY" settings get "$key" 2>/dev/null)" = "$expected" ] && return
        sleep 0.1
    done
    fail "The installed GUI did not persist $key=$expected."
}

journey_tv_scenario() {
    printf '%s\n' "$1" > "$WORK_DIR/native-tv/command.tmp"
    mv "$WORK_DIR/native-tv/command.tmp" "$WORK_DIR/native-tv/command"
    for ((attempt = 0; attempt < 100; attempt++)); do
        if python3 - "$WORK_DIR/native-tv/state.json" "$1" <<'PY'
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
raise SystemExit(0 if path.exists() and json.loads(path.read_text()).get("scenario") == sys.argv[2] else 1)
PY
        then return; fi
        sleep 0.1
    done
    fail "Native TV fixture did not enter $1."
}

journey_pair() {
    observe_gui_state --activate-control "Pair a TV"
    observe_gui_state --expected-tvs-state pairing
    observe_gui_state --edit-pairing-address 127.0.0.1 --edit-pairing-mac 02:00:00:00:00:10 --window-id "$WINDOW_ID"
    observe_gui_state --focus-control "HDMI input" --window-id "$WINDOW_ID"
    xdotool key --window "$WINDOW_ID" --delay 60 space Home Down Down Return
    observe_gui_state --activate-control Pair
}

journey_close() {
    gapplication action io.github.staphylococcus.LGBuddy quit
    finish_gui "$1"
}

journey_diagnostics() {
    local label="$1"
    observe_gui_state --focus-control "Main Menu" --window-id "$WINDOW_ID"
    # Activate the first menu entry through normal keyboard navigation. GTK
    # 4.14 does not expose Gio menu-item labels through AT-SPI.
    xdotool key --window "$WINDOW_ID" Return
    xdotool key Home Return
    observe_gui_state --expected-diagnostics-state report
    observe_gui_state --read-diagnostics "$WORK_DIR/$label-report.txt"
    observe_gui_state --focus-control Copy --window-id "$WINDOW_ID"
    observe_gui_state --copy-diagnostics "$WORK_DIR/$label-copy.txt"
    observe_gui_state --save-diagnostics "$WORK_DIR/$label-save.txt" --window-id "$WINDOW_ID"
    cmp "$WORK_DIR/$label-report.txt" "$WORK_DIR/$label-copy.txt" || fail "Diagnostics Copy differs from its visible report."
    cmp "$WORK_DIR/$label-report.txt" "$WORK_DIR/$label-save.txt" || fail "Diagnostics Save differs from its visible report."
    if grep -E 'webos-test-access-token|diagnostics-secret-canary' "$WORK_DIR/$label-report.txt"; then
        fail "Diagnostics exported a credential or raw failure payload."
    fi
    grep -q 'no accessible entries' "$WORK_DIR/$label-report.txt" || fail "Diagnostics did not explain unavailable observations."
    observe_gui_state --activate-control Refresh
    observe_gui_state --expected-diagnostics-state report
    observe_gui_state --activate-control Close
}

run_installed_gui_journey() {
    [ -x "$TV_FIXTURE" ] || fail "Native TV fixture is not executable: $TV_FIXTURE"
    [ -n "${LG_BUDDY_INSTALL_ROOT:-}" ] && [ "$LG_BUDDY_INSTALL_ROOT" != / ] || fail "GUI journey requires an isolated installation root."
    local old_path="$PATH"
    local old_skip="${LG_BUDDY_SKIP_SYSTEMD_ACTIONS:-0}"
    local CONFIG_FILE LG_BUDDY_CONFIG
    CONFIG_FILE="$(cat "$LG_BUDDY_INSTALL_ROOT/usr/lib/lg-buddy/config-path")"
    LG_BUDDY_CONFIG="$CONFIG_FILE"
    export LG_BUDDY_CONFIG
    local token="$(dirname "$CONFIG_FILE")/tvs/primary/access-token.json"
    if [ -f "$token" ]; then cp "$token" "$WORK_DIR/before-journey.token"; fi
    cp "$CONFIG_FILE" "$WORK_DIR/before-journey.env"
    mkdir -p "$WORK_DIR/journey-bin" "$WORK_DIR/services" "$WORK_DIR/native-tv"
    export LG_BUDDY_GUI_SERVICE_FIXTURE="$WORK_DIR/services"
    printf 'accept\n' > "$WORK_DIR/services/auth-mode"
    cat > "$WORK_DIR/journey-bin/systemctl" <<'SH'
#!/bin/bash
set -eu
dir="$LG_BUDDY_GUI_SERVICE_FIXTURE"
scope=system
if [ "${1:-}" = --user ]; then scope=user; shift; fi
action="${1:-}"; shift || true
unit="${*: -1}"
printf '%s %s %s\n' "$scope" "$action" "$unit" >> "$dir/calls"
case "$action" in
    cat|show-environment|daemon-reload) exit 0 ;;
    is-active)
        [ "$unit" = graphical-session.target ] || [ -e "$dir/active-$scope-$unit" ] ;;
    is-enabled) [ -e "$dir/enabled-$scope-$unit" ] ;;
    start|restart)
        if [ "$unit" = LG_Buddy_screen.service ] && [ -e "$dir/screen-fails" ]; then
            echo 'screen service failed' >&2; exit 1
        fi
        touch "$dir/active-$scope-$unit"
        printf '%s %s %s\n' "$scope" "$action" "$unit" >> "$dir/successful-calls" ;;
    enable)
        if [[ " $* " == *" --now "* ]]; then
            if [ "$unit" = LG_Buddy_screen.service ] && [ -e "$dir/screen-fails" ]; then
                echo 'screen service failed' >&2; exit 1
            fi
            touch "$dir/active-$scope-$unit"
        fi
        touch "$dir/enabled-$scope-$unit" ;;
    disable) rm -f "$dir/enabled-$scope-$unit" "$dir/active-$scope-$unit" ;;
    show)
        active=inactive; enabled=disabled
        [ ! -e "$dir/active-$scope-$unit" ] || active=active
        [ ! -e "$dir/enabled-$scope-$unit" ] || enabled=enabled
        printf 'LoadState=loaded\nActiveState=%s\nSubState=dead\nUnitFileState=%s\n' "$active" "$enabled" ;;
    *) echo "Unexpected systemctl request: $action $*" >&2; exit 2 ;;
esac
SH
    cat > "$WORK_DIR/journey-bin/pkexec" <<'SH'
#!/bin/bash
set -eu
dir="$LG_BUDDY_GUI_SERVICE_FIXTURE"
[ "${1:-}" = --disable-internal-agent ] || exit 2
shift
printf '%s\n' "$*" >> "$dir/authorizations"
[ "$(cat "$dir/auth-mode")" != decline ] || exit 126
if { [ "${1:-}" = /usr/bin/systemctl ] || [ "${1:-}" = /run/current-system/sw/bin/systemctl ]; } &&
    [ "${2:-}" = start ] && [ "${3:-}" = LG_Buddy_lifecycle.service ]; then
    shift
    exec "$LG_BUDDY_SYSTEMCTL" "$@"
fi
exec unshare -Ur "$@"
SH
    cat > "$WORK_DIR/journey-bin/journalctl" <<'SH'
#!/bin/sh
echo 'token=diagnostics-secret-canary' >&2
exit 1
SH
    chmod 755 "$WORK_DIR/journey-bin/"*
    export PATH="$WORK_DIR/journey-bin:$PATH"
    export LG_BUDDY_SYSTEMCTL="$WORK_DIR/journey-bin/systemctl"
    export LG_BUDDY_JOURNALCTL="$WORK_DIR/journey-bin/journalctl"
    export LG_BUDDY_SKIP_SYSTEMD_ACTIONS=0
    "$TV_FIXTURE" "$WORK_DIR/native-tv" > "$WORK_DIR/native-tv.output" 2>&1 &
    TV_FIXTURE_PID=$!
    journey_tv_scenario stateful

    # An installed app with no TV offers pairing and diagnostics, without tabs.
    : > "$CONFIG_FILE"
    rm -f "$token"
    start_gui enabled "" "" normal
    observe_gui_state --expected-tvs-state empty
    journey_diagnostics before-pairing
    [ ! -s "$CONFIG_FILE" ] || fail "Diagnostics changed the fresh configuration."
    observe_gui_state --activate-control "Pair a TV"
    observe_gui_state --expected-tvs-state pairing
    observe_gui_state --activate-control Cancel
    observe_gui_state --expected-tvs-state empty

    journey_tv_scenario pairing-rejected
    journey_pair
    observe_gui_state --expected-text "Connection declined on TV"
    observe_gui_state --activate-control Cancel
    observe_gui_state --expected-tvs-state empty
    [ ! -s "$CONFIG_FILE" ] && [ ! -e "$token" ] || fail "Rejected pairing saved a profile or credential."

    journey_tv_scenario stall
    journey_pair
    observe_gui_state --expected-text "Verifying TV Access"
    journey_close "interrupted first-run pairing"
    [ ! -s "$CONFIG_FILE" ] && [ ! -e "$token" ] || fail "Interrupted pairing saved incomplete state."
    journey_tv_scenario stateful
    start_gui enabled "" "" normal
    observe_gui_state --expected-tvs-state empty
    journey_pair
    journey_setting tv.ip 127.0.0.1
    journey_setting screen.idle_blank enabled
    journey_setting system.sleep_wake_policy enabled
    observe_gui_state --select-page Settings
    observe_gui_state --expected-toggle 'Idle blanking=on' --expected-toggle 'TV sleep & wake=on'
    "$LG_BUDDY_SYSTEMCTL" --user is-active LG_Buddy_screen.service || fail "Default idle blanking did not activate its service."
    "$LG_BUDDY_SYSTEMCTL" is-active LG_Buddy_lifecycle.service || fail "Default sleep/wake did not activate its service."
    cp "$CONFIG_FILE" "$WORK_DIR/paired-config.snapshot"
    cp "$token" "$WORK_DIR/paired-token.snapshot"
    journey_diagnostics paired
    cmp "$CONFIG_FILE" "$WORK_DIR/paired-config.snapshot" || fail "Diagnostics changed configuration."
    cmp "$token" "$WORK_DIR/paired-token.snapshot" || fail "Diagnostics changed credentials."

    # Re-pairing preserves preferences; failed activation leaves honest toggles
    # and can be retried from Settings without repeating TV pairing.
    touch "$WORK_DIR/services/screen-fails"
    observe_gui_state --edit-settings-timeout 720 --window-id "$WINDOW_ID"
    xdotool key --window "$WINDOW_ID" Return
    journey_setting screen.idle_timeout 720
    observe_gui_state --expected-text 'Retry apply'
    rm "$WORK_DIR/services/screen-fails"
    # A saved value and the earlier activation do not prove this retry worked.
    : > "$WORK_DIR/services/successful-calls"
    observe_gui_state --activate-control 'Retry apply Idle timeout'
    for ((attempt = 0; attempt < 100; attempt++)); do
        grep -qx 'user restart LG_Buddy_screen.service' "$WORK_DIR/services/successful-calls" && break
        sleep 0.1
    done
    grep -qx 'user restart LG_Buddy_screen.service' "$WORK_DIR/services/successful-calls" || fail "Retry apply did not successfully restart the screen service."
    observe_gui_state --expected-settings-state ready --expected-settings-timeout 720 --expected-absent-text 'Retry apply'
    observe_gui_state --select-page TVs
    observe_gui_state --activate-control 'Unpair TV…'
    observe_gui_state --expected-tvs-state unpair
    observe_gui_state --activate-control Unpair
    observe_gui_state --expected-tvs-state empty
    journey_setting screen.idle_timeout 720
    rm -f "$WORK_DIR/services/active-system-LG_Buddy_lifecycle.service" "$WORK_DIR/services/active-user-LG_Buddy_screen.service"
    touch "$WORK_DIR/services/screen-fails"
    printf 'decline\n' > "$WORK_DIR/services/auth-mode"
    journey_pair
    journey_setting tv.ip 127.0.0.1
    observe_gui_state --select-page Settings
    observe_gui_state --expected-toggle 'Idle blanking=off' --expected-toggle 'TV sleep & wake=off'
    journey_setting screen.idle_blank disabled
    journey_setting system.sleep_wake_policy disabled
    journey_close "paired TV with declined activation"
    cp "$WORK_DIR/services/authorizations" "$WORK_DIR/authorizations.snapshot"
    start_gui enabled "" "" normal
    observe_gui_state --select-page Settings
    observe_gui_state --expected-toggle 'Idle blanking=off' --expected-toggle 'TV sleep & wake=off'
    cmp "$WORK_DIR/services/authorizations" "$WORK_DIR/authorizations.snapshot" || fail "Relaunch unexpectedly requested activation again."
    rm "$WORK_DIR/services/screen-fails"
    printf 'accept\n' > "$WORK_DIR/services/auth-mode"
    observe_gui_state --activate-control 'Idle blanking'
    journey_setting screen.idle_blank enabled
    observe_gui_state --activate-control 'TV sleep & wake'
    journey_setting system.sleep_wake_policy enabled
    journey_setting screen.idle_timeout 720
    observe_gui_state --expected-toggle 'Idle blanking=on' --expected-toggle 'TV sleep & wake=on'
    "$LG_BUDDY_SYSTEMCTL" --user is-active LG_Buddy_screen.service || fail "Settings retry did not activate idle blanking."
    "$LG_BUDDY_SYSTEMCTL" is-active LG_Buddy_lifecycle.service || fail "Settings retry did not activate sleep/wake."
    journey_close "successful activation from Settings"

    # An offline saved TV remains a configured application after relaunch.
    kill "$TV_FIXTURE_PID"
    wait "$TV_FIXTURE_PID" 2>/dev/null || true
    TV_FIXTURE_PID=""
    start_gui enabled prefer-dark 2 normal
    observe_gui_state --select-page TVs
    observe_gui_state --expected-tvs-state configured --expected-tv-address 127.0.0.1
    journey_diagnostics offline
    journey_close "offline configured TV"

    if [ -n "$UPDATE_ARCHIVE" ]; then journey_updates; fi

    cp "$WORK_DIR/before-journey.env" "$CONFIG_FILE"
    if [ -f "$WORK_DIR/before-journey.token" ]; then
        cp "$WORK_DIR/before-journey.token" "$token"
    else
        rm -f "$token"
    fi
    export PATH="$old_path" LG_BUDDY_SKIP_SYSTEMD_ACTIONS="$old_skip"
    unset LG_BUDDY_SYSTEMCTL LG_BUDDY_JOURNALCTL LG_BUDDY_GUI_SERVICE_FIXTURE
    echo "Installed GUI pairing, activation, and diagnostics journey passed."
}

journey_update_mode() {
    printf '{"mode":"%s","delay_seconds":%s}\n' "$1" "${2:-0}" > "$WORK_DIR/github-state.tmp"
    mv "$WORK_DIR/github-state.tmp" "$WORK_DIR/github-state.json"
}

journey_update_unchanged() {
    cmp "$CONFIG_FILE" "$WORK_DIR/update-config.snapshot" || fail "Update changed user settings."
    cmp "$update_token" "$WORK_DIR/update-token.snapshot" || fail "Update changed pairing credentials."
    cmp "$RUNTIME_BINARY" "$WORK_DIR/update-runtime.snapshot" || fail "Unfinished update replaced the runtime."
    cmp "$(dirname "$RUNTIME_BINARY")/lg-buddy-gui" "$WORK_DIR/update-gui.snapshot" || fail "Unfinished update replaced the GUI."
}

journey_update_confirmation() {
    observe_gui_state --activate-control 'Install update…'
    observe_gui_state --expected-updater-state confirmation
    observe_gui_state --expected-text "Install LG Buddy $expected_update_version?"
}

journey_updates() {
    [ -f "$UPDATE_ARCHIVE" ] || fail "Candidate update archive is missing: $UPDATE_ARCHIVE"
    local installed_gui="$(dirname "$RUNTIME_BINARY")/lg-buddy-gui"
    local update_token="$(dirname "$CONFIG_FILE")/tvs/primary/access-token.json"
    local expected_update_version
    expected_update_version="$(tar -xOf "$UPDATE_ARCHIVE" --wildcards '*/release-manifest.json' | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')"
    for binary in "$RUNTIME_BINARY" "$installed_gui"; do
        LC_ALL=C grep -aq LG_BUDDY_TEST_GITHUB_ADDRESS "$binary" || fail "Update smoke requires debug binaries built with gui-test-fixtures."
    done
    journey_update_mode up-to-date 1
    python3 "$REPOSITORY_ROOT/tools/mock_github_gui.py" \
        --archive "$UPDATE_ARCHIVE" --state "$WORK_DIR/github-state.json" \
        --ready "$WORK_DIR/github-ready.json" --requests "$WORK_DIR/github-requests.jsonl" \
        --current-version 0.0.0 > "$WORK_DIR/github.output" 2>&1 &
    GITHUB_FIXTURE_PID=$!
    for ((attempt = 0; attempt < 100; attempt++)); do
        [ ! -f "$WORK_DIR/github-ready.json" ] || break
        kill -0 "$GITHUB_FIXTURE_PID" 2>/dev/null || fail "GitHub fixture exited before becoming ready."
        sleep 0.1
    done
    export LG_BUDDY_TEST_GITHUB_ADDRESS
    LG_BUDDY_TEST_GITHUB_ADDRESS="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["address"])' "$WORK_DIR/github-ready.json")"
    export LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1
    start_gui enabled prefer-dark 2 normal
    local update_gui_pid
    update_gui_pid="$(pgrep -P "$GUI_PID" -f lg-buddy-gui)"
    observe_gui_state --select-page Settings
    observe_gui_state --activate-control 'Automatic update checks'
    journey_setting updates.auto_check disabled
    [ ! -s "$WORK_DIR/github-requests.jsonl" ] || fail "Opening Settings unexpectedly checked for updates."

    observe_gui_state --activate-control 'Check for updates'
    observe_gui_state --expected-updater-state checking
    observe_gui_state --expected-updater-state idle --expected-text 'Already up to date'
    journey_update_mode error
    observe_gui_state --activate-control 'Check for updates'
    observe_gui_state --expected-text 'Could not check for updates'
    observe_gui_state --expected-text 'Copy details'

    journey_update_mode available
    : > "$WORK_DIR/github-requests.jsonl"
    observe_gui_state --activate-control 'Check for updates'
    observe_gui_state --expected-updater-state available
    grep -q '/releases/latest' "$WORK_DIR/github-requests.jsonl" || fail "Stable check ignored the saved channel."
    if grep -q '/releases/assets/' "$WORK_DIR/github-requests.jsonl"; then
        fail "Checking for updates downloaded installation assets."
    fi
    observe_gui_state --focus-control 'Update channel' --window-id "$WINDOW_ID"
    xdotool key --window "$WINDOW_ID" --delay 60 space Home Down Return
    journey_setting updates.channel prerelease
    observe_gui_state --expected-updater-state idle
    observe_gui_state --activate-control 'Check for updates'
    observe_gui_state --expected-updater-state available
    grep -q '/releases?per_page=1' "$WORK_DIR/github-requests.jsonl" || fail "Prerelease check ignored the saved channel."

    cp "$CONFIG_FILE" "$WORK_DIR/update-config.snapshot"
    cp "$update_token" "$WORK_DIR/update-token.snapshot"
    cp "$RUNTIME_BINARY" "$WORK_DIR/update-runtime.snapshot"
    cp "$installed_gui" "$WORK_DIR/update-gui.snapshot"
    cp "$WORK_DIR/services/authorizations" "$WORK_DIR/update-authorizations.snapshot"
    local discovery_count
    discovery_count="$(grep -c '/releases?per_page=1' "$WORK_DIR/github-requests.jsonl")"
    journey_update_confirmation
    [ "$(grep -c '/releases?per_page=1' "$WORK_DIR/github-requests.jsonl")" -gt "$discovery_count" ] || fail "Install dialog did not discover a fresh release."
    observe_gui_state --activate-control Cancel
    observe_gui_state --expected-updater-state available
    journey_update_unchanged
    cmp "$WORK_DIR/services/authorizations" "$WORK_DIR/update-authorizations.snapshot" || fail "Cancelled confirmation requested installation authorization."

    journey_update_mode corrupt-archive
    journey_update_confirmation
    observe_gui_state --activate-control 'Install and restart'
    observe_gui_state --expected-text 'Could not install update'
    journey_update_unchanged
    cmp "$WORK_DIR/services/authorizations" "$WORK_DIR/update-authorizations.snapshot" || fail "Corrupt download reached the installer."

    journey_update_mode available
    printf 'decline\n' > "$WORK_DIR/services/auth-mode"
    journey_update_confirmation
    observe_gui_state --activate-control 'Install and restart'
    observe_gui_state --expected-text 'Could not install update'
    journey_update_unchanged
    if cmp -s "$WORK_DIR/services/authorizations" "$WORK_DIR/update-authorizations.snapshot"; then
        fail "Authorization-decline scenario did not reach the installer."
    fi

    printf 'accept\n' > "$WORK_DIR/services/auth-mode"
    journey_update_mode available 1
    journey_update_confirmation
    observe_gui_state --activate-control 'Install and restart'
    observe_gui_state --expected-updater-state downloading
    for ((attempt = 0; attempt < 600; attempt++)); do
        if ! cmp -s "$installed_gui" "$WORK_DIR/update-gui.snapshot" && cmp -s "/proc/$update_gui_pid/exe" "$installed_gui"; then
            break
        fi
        kill -0 "$GUI_PID" 2>/dev/null || fail "GUI exited instead of handing off to the installed update."
        sleep 0.1
    done
    ! cmp -s "$installed_gui" "$WORK_DIR/update-gui.snapshot" || fail "GUI update did not replace installed executables."
    cmp -s "/proc/$update_gui_pid/exe" "$installed_gui" || fail "GUI did not relaunch the verified installed executable."
    mkdir "$WORK_DIR/update-candidate"
    tar -xzf "$UPDATE_ARCHIVE" -C "$WORK_DIR/update-candidate" --strip-components=1
    cmp "$RUNTIME_BINARY" "$WORK_DIR/update-candidate/lg-buddy" || fail "Installed runtime differs from the candidate."
    local gui_target
    gui_target="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["gui_target"])' "$WORK_DIR/update-candidate/release-manifest.json")"
    cmp "$installed_gui" "$WORK_DIR/update-candidate/docs/lg-buddy-gui-$gui_target" || fail "Installed GUI differs from the candidate."
    "$RUNTIME_BINARY" --version
    "$installed_gui" --version
    cmp "$CONFIG_FILE" "$WORK_DIR/update-config.snapshot" || fail "Successful update changed settings."
    cmp "$update_token" "$WORK_DIR/update-token.snapshot" || fail "Successful update changed pairing credentials."
    observe_gui_state --select-page TVs
    observe_gui_state --expected-tvs-state configured --expected-tv-address 127.0.0.1
    journey_close 'updated installed GUI'
    kill "$GITHUB_FIXTURE_PID"
    wait "$GITHUB_FIXTURE_PID" 2>/dev/null || true
    GITHUB_FIXTURE_PID=""
    unset LG_BUDDY_TEST_GITHUB_ADDRESS
    echo 'Installed GUI update check, confirmation, failure, installation, and relaunch journey passed.'
}
