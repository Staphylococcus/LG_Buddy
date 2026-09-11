#!/bin/bash

set -euo pipefail
umask 0022

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPOSITORY_ROOT="$(dirname "$SCRIPT_DIR")"
WORK_DIR="$(mktemp -d)"
BUNDLE="$WORK_DIR/bundle"
STUB_DIR="$WORK_DIR/stubs"

cleanup() {
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

[ "$(id -u)" -ne 0 ] || {
    echo "First-run installer smoke must run as a regular user."
    exit 1
}

mkdir -p "$BUNDLE/docs" "$BUNDLE/systemd" "$BUNDLE/bin" "$STUB_DIR"
cp "$REPOSITORY_ROOT/install.sh" "$BUNDLE/install.sh"
cp "$REPOSITORY_ROOT/bin/LG_Buddy_Common" "$BUNDLE/bin/LG_Buddy_Common"
cp "$REPOSITORY_ROOT/io.github.staphylococcus.LGBuddy.desktop" "$BUNDLE/io.github.staphylococcus.LGBuddy.desktop"
cp "$REPOSITORY_ROOT/systemd/LG_Buddy.service" "$BUNDLE/systemd/LG_Buddy.service"
cp "$REPOSITORY_ROOT/systemd/LG_Buddy_lifecycle.service" "$BUNDLE/systemd/LG_Buddy_lifecycle.service"
cp "$REPOSITORY_ROOT/systemd/LG_Buddy_screen.service" "$BUNDLE/systemd/LG_Buddy_screen.service"
cp "$REPOSITORY_ROOT/systemd/LG_Buddy_update_check.service" "$BUNDLE/systemd/LG_Buddy_update_check.service"
cp "$REPOSITORY_ROOT/systemd/LG_Buddy_update_check.timer" "$BUNDLE/systemd/LG_Buddy_update_check.timer"
cp "$REPOSITORY_ROOT/systemd/lg_buddy.conf" "$BUNDLE/systemd/lg_buddy.conf"
cp "$REPOSITORY_ROOT/data/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg" \
    "$BUNDLE/docs/io.github.staphylococcus.LGBuddy.svg"

cat >"$BUNDLE/lg-buddy" <<'EOF'
#!/bin/sh
set -eu

saved_value() {
    key="$1"
    [ -r "${LG_BUDDY_CONFIG:?}" ] || exit 1
    awk -v key="$key" '
        function trim(value) {
            sub(/^[[:space:]]+/, "", value)
            sub(/[[:space:]]+$/, "", value)
            return value
        }
        {
            line = trim($0)
            separator = index(line, "=")
            if (separator > 0 && trim(substr(line, 1, separator - 1)) == key) {
                value = substr(line, separator + 1)
                sub(/#.*/, "", value)
                result = trim(value)
            }
        }
        END {
            if (result == "") exit 1
            print result
        }
    ' "$LG_BUDDY_CONFIG"
}

case "${1:-}" in
    --version)
        printf '%s\n' '1.7.0'
        ;;
    settings)
        case "${3:-}" in
            tv.ip) saved_value tvs_primary_ip ;;
            tv.mac) saved_value tvs_primary_mac ;;
            tv.input) saved_value tvs_primary_input ;;
            tv.platform) printf '%s\n' bscpylgtv ;;
            screen.idle_blank) printf '%s\n' disabled ;;
            screen.backend) printf '%s\n' auto ;;
            system.sleep_wake_policy) printf '%s\n' disabled ;;
            updates.auto_check) printf '%s\n' disabled ;;
            updates.channel) printf '%s\n' prerelease ;;
            *) exit 1 ;;
        esac
        ;;
    "")
        if [ "${LG_BUDDY_HANDOFF_STATUS:-0}" -ne 0 ]; then
            exit "$LG_BUDDY_HANDOFF_STATUS"
        fi
        installed_dir="$(dirname "$0")"
        [ -x "$installed_dir/lg-buddy-gui" ]
        [ -f "$installed_dir/../lib/lg-buddy/config-path" ]
        [ -f "$installed_dir/../../etc/systemd/system/LG_Buddy.service" ]
        printf '%s\n' "$0:${LG_BUDDY_CONFIG:?}" >"${LG_BUDDY_HANDOFF_MARKER:?}"
        ;;
    *)
        exit 1
        ;;
esac
EOF
cat >"$BUNDLE/docs/lg-buddy-gui-x86_64-unknown-linux-gnu" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = --version ] || exit 1
printf '%s\n' '1.7.0'
EOF
cat >"$BUNDLE/systemd-placeholder" <<'EOF'
placeholder
EOF
chmod 755 "$BUNDLE/install.sh" "$BUNDLE/lg-buddy" "$BUNDLE/docs/lg-buddy-gui-x86_64-unknown-linux-gnu"
chmod 644 "$BUNDLE/io.github.staphylococcus.LGBuddy.desktop" "$BUNDLE/docs/io.github.staphylococcus.LGBuddy.svg"

cat >"$STUB_DIR/python3" <<'EOF'
#!/bin/sh
set -eu

if [ "${1:-}" = -m ] && [ "${2:-}" = venv ]; then
    target=""
    for argument do
        target="$argument"
    done
    mkdir -p "$target/bin"
    : >"$target/pyvenv.cfg"
    cat >"$target/bin/pip" <<'PIP'
#!/bin/sh
case "${1:-}" in
    --version) exit 0 ;;
    *) exit 0 ;;
esac
PIP
    cat >"$target/bin/python" <<'PYTHON'
#!/bin/sh
exit 0
PYTHON
    chmod 755 "$target/bin/pip" "$target/bin/python"
    exit 0
fi

exit 1
EOF
cat >"$STUB_DIR/gui-runtime-probe" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$STUB_DIR/systemd-tmpfiles" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"${LG_BUDDY_SYSTEMCTL_LOG:?}"
EOF
cat >"$STUB_DIR/systemctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"${LG_BUDDY_SYSTEMCTL_LOG:?}"
exit 0
EOF
cat >"$STUB_DIR/pkexec" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$STUB_DIR/python3" "$STUB_DIR/gui-runtime-probe" \
    "$STUB_DIR/systemd-tmpfiles" "$STUB_DIR/systemctl" "$STUB_DIR/pkexec"

run_install() {
    local scenario="$1"
    local status=0
    local root="$WORK_DIR/$scenario/root"
    local home="$WORK_DIR/$scenario/home"
    local output="$WORK_DIR/$scenario/output"
    local handoff="$WORK_DIR/$scenario/handoff"
    local systemctl_log="$WORK_DIR/$scenario/systemctl.log"
    local command_path="$STUB_DIR:$PATH"
    local path_entry=""
    local filtered_path=""
    local scenario_stub_dir=""
    local path_index=0
    local command_path_entry=""
    local mirror_dir=""

    mkdir -p "$root" "$home/Desktop" "$WORK_DIR/$scenario"
    : >"$systemctl_log"
    if [ "${LG_BUDDY_TEST_WITHOUT_PKEXEC:-0}" = "1" ]; then
        scenario_stub_dir="$WORK_DIR/$scenario/without-pkexec"
        mkdir -p "$scenario_stub_dir"
        while IFS= read -r path_entry; do
            [ -n "$path_entry" ] || continue
            mirror_dir="$scenario_stub_dir/path-$path_index"
            mkdir -p "$mirror_dir"
            for command_path_entry in "$path_entry"/*; do
                [ -e "$command_path_entry" ] || continue
                [ "$(basename "$command_path_entry")" = pkexec ] && continue
                ln -s "$command_path_entry" "$mirror_dir/$(basename "$command_path_entry")"
            done
            filtered_path="${filtered_path:+$filtered_path:}$mirror_dir"
            path_index=$((path_index + 1))
        done < <(printf '%s\n' "$PATH" | tr ':' '\n')
        command_path="$scenario_stub_dir:$filtered_path"
        for command in python3 gui-runtime-probe systemd-tmpfiles systemctl; do
            ln -s "$STUB_DIR/$command" "$scenario_stub_dir/$command"
        done
    fi
    set +e
    PATH="$command_path" \
    HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" \
    LG_BUDDY_INSTALL_ROOT="$root" \
    LG_BUDDY_SUDO_CMD=none \
    LG_BUDDY_NONINTERACTIVE=1 \
    LG_BUDDY_SKIP_PIP_INSTALL=1 \
    LG_BUDDY_GUI_RUNTIME_PROBE="$STUB_DIR/gui-runtime-probe" \
    LG_BUDDY_SYSTEMCTL_LOG="$systemctl_log" \
    LG_BUDDY_HANDOFF_MARKER="$handoff" \
    LG_BUDDY_HANDOFF_STATUS="${LG_BUDDY_HANDOFF_STATUS:-0}" \
        bash "$BUNDLE/install.sh" >"$output" 2>&1
    status=$?
    set -e
    RUN_STATUS="$status"
    RUN_ROOT="$root"
    RUN_HOME="$home"
    RUN_OUTPUT="$output"
    RUN_HANDOFF="$handoff"
    RUN_SYSTEMCTL_LOG="$systemctl_log"
}

unset LG_BUDDY_CONFIG LG_BUDDY_HANDOFF_STATUS
run_install fresh
[ "$RUN_STATUS" -eq 0 ] || { cat "$RUN_OUTPUT"; exit 1; }
CONFIG_FILE="$RUN_HOME/.config/lg-buddy/config.env"
[ -f "$CONFIG_FILE" ] && [ ! -s "$CONFIG_FILE" ]
[ "$(stat -c '%a' "$CONFIG_FILE")" = 600 ]
[ -f "$RUN_HANDOFF" ]
grep -F -q ":$CONFIG_FILE" "$RUN_HANDOFF"
[ -x "$RUN_ROOT/usr/bin/lg-buddy" ]
[ -x "$RUN_ROOT/usr/bin/lg-buddy-gui" ]
[ -f "$RUN_ROOT/etc/systemd/system/LG_Buddy.service" ]
grep -F -q 'Prepared an empty user configuration for first-run TV pairing.' "$RUN_OUTPUT"
grep -F -q 'Opening LG Buddy to pair your first TV...' "$RUN_OUTPUT"
grep -F -q 'Pairing will attempt the default Idle Blanking and TV Sleep & Wake behaviors.' "$RUN_OUTPUT"
grep -F -q 'If a behavior is declined or unavailable, it stays off until retried in Settings.' "$RUN_OUTPUT"
grep -F -q 'System sleep/wake integration installed; pairing will attempt TV Sleep & Wake.' "$RUN_OUTPUT"
grep -F -q 'If authorization or activation fails, TV Sleep & Wake stays off until retried in Settings.' "$RUN_OUTPUT"
grep -F -q 'LG_Buddy_screen.service enabled and started for session notifications; idle blanking is disabled by config.' "$RUN_OUTPUT"
grep -F -q 'LG_Buddy_update_check.timer enabled and started.' "$RUN_OUTPUT"
! grep -F -q 'System sleep/wake TV control enabled via' "$RUN_OUTPUT"
! grep -F -q 'Running configuration script' "$RUN_OUTPUT"
grep -F -q 'enable LG_Buddy.service' "$RUN_SYSTEMCTL_LOG"
grep -F -q 'enable LG_Buddy_lifecycle.service' "$RUN_SYSTEMCTL_LOG"
! grep -F -q 'restart LG_Buddy_lifecycle.service' "$RUN_SYSTEMCTL_LOG"
! grep -F -q 'start LG_Buddy_lifecycle.service' "$RUN_SYSTEMCTL_LOG"
grep -F -q -- '--user enable LG_Buddy_screen.service' "$RUN_SYSTEMCTL_LOG"
grep -F -q -- '--user restart LG_Buddy_screen.service' "$RUN_SYSTEMCTL_LOG"
grep -F -q -- '--user enable LG_Buddy_update_check.timer' "$RUN_SYSTEMCTL_LOG"
grep -F -q -- '--user start LG_Buddy_update_check.timer' "$RUN_SYSTEMCTL_LOG"

export LG_BUDDY_HANDOFF_STATUS=77
run_install failed-handoff
[ "$RUN_STATUS" -eq 77 ] || { cat "$RUN_OUTPUT"; exit 1; }
[ ! -e "$RUN_HOME/.config/lg-buddy/config.env.setup-pending" ]
[ ! -s "$RUN_HOME/.config/lg-buddy/config.env" ]
unset LG_BUDDY_HANDOFF_STATUS

export LG_BUDDY_TEST_WITHOUT_PKEXEC=1
run_install missing-pkexec
[ "$RUN_STATUS" -ne 0 ] || { cat "$RUN_OUTPUT"; exit 1; }
grep -F -q '[MISSING] pkexec (required for TV Sleep & Wake)' "$RUN_OUTPUT"
[ -z "$(find "$RUN_ROOT" -mindepth 1 -print -quit)" ]
[ ! -e "$RUN_HOME/.config" ]
unset LG_BUDDY_TEST_WITHOUT_PKEXEC

SYMLINK_CONFIG_HOME="$WORK_DIR/config-symlink/home"
SYMLINK_CONFIG_TARGET="$WORK_DIR/config-symlink-target"
mkdir -p "$SYMLINK_CONFIG_HOME/.config/lg-buddy" "$SYMLINK_CONFIG_HOME/Desktop"
: >"$SYMLINK_CONFIG_TARGET"
ln -s "$SYMLINK_CONFIG_TARGET" "$SYMLINK_CONFIG_HOME/.config/lg-buddy/config.env"
run_install config-symlink
[ "$RUN_STATUS" -ne 0 ] || { cat "$RUN_OUTPUT"; exit 1; }
grep -F -q 'configuration file is a symbolic link' "$RUN_OUTPUT"
[ -L "$SYMLINK_CONFIG_HOME/.config/lg-buddy/config.env" ]
[ ! -s "$SYMLINK_CONFIG_TARGET" ]

UNREADABLE_HOME="$WORK_DIR/unreadable-config/home"
mkdir -p "$UNREADABLE_HOME/Desktop" "$UNREADABLE_HOME/.config/lg-buddy"
printf '%s\n' retained >"$UNREADABLE_HOME/.config/lg-buddy/config.env"
chmod 000 "$UNREADABLE_HOME/.config/lg-buddy/config.env"
run_install unreadable-config
[ "$RUN_STATUS" -ne 0 ] || { cat "$RUN_OUTPUT"; exit 1; }
grep -F -q 'configuration file is not readable' "$RUN_OUTPUT"
[ "$(stat -c '%a' "$UNREADABLE_HOME/.config/lg-buddy/config.env")" = 0 ]

if unshare -Ur true >/dev/null 2>&1 ||
    { command -v sudo >/dev/null 2>&1 && sudo -n -u root true >/dev/null 2>&1; }; then
    ROOT_OUTPUT="$WORK_DIR/root-invocation.output"
    ROOT_HOME="$WORK_DIR/root-invocation/home"
    ROOT_INSTALL_ROOT="$WORK_DIR/root-invocation/root"
    mkdir -p "$ROOT_HOME/Desktop"
    ROOT_RUNNER=(sudo -n -u root)
    if unshare -Ur true >/dev/null 2>&1; then
        ROOT_RUNNER=(unshare -Ur)
    fi
    if "${ROOT_RUNNER[@]}" env \
            HOME="$ROOT_HOME" \
            XDG_CONFIG_HOME="$ROOT_HOME/.config" \
            LG_BUDDY_INSTALL_ROOT="$ROOT_INSTALL_ROOT" \
            LG_BUDDY_SUDO_CMD=none \
            LG_BUDDY_SKIP_PIP_INSTALL=1 \
            LG_BUDDY_GUI_RUNTIME_PROBE="$STUB_DIR/gui-runtime-probe" \
            bash "$BUNDLE/install.sh" >"$ROOT_OUTPUT" 2>&1; then
        cat "$ROOT_OUTPUT"
        exit 1
    fi
    grep -F -q 'Do not run this script with sudo' "$ROOT_OUTPUT"
    [ ! -e "$ROOT_HOME/.config" ]
    [ ! -e "$ROOT_INSTALL_ROOT" ]
fi

mkdir -p "$WORK_DIR/configured/home/.config/lg-buddy"
cat >"$WORK_DIR/configured/home/.config/lg-buddy/config.env" <<'EOF'
  tvs_primary_ip = 192.0.2.10 # existing profile
  tvs_primary_mac = 02:00:00:00:00:10
  tvs_primary_input = HDMI_2
  tvs_primary_platform = bscpylgtv
screen_idle_blank=disabled
screen_backend=auto
screen_idle_timeout=900
screen_restore_policy=aggressive
system_sleep_wake_policy=disabled
updates_auto_check=disabled
updates_channel=prerelease
EOF
cp "$WORK_DIR/configured/home/.config/lg-buddy/config.env" "$WORK_DIR/configured.expected"
run_install configured
[ "$RUN_STATUS" -eq 0 ] || { cat "$RUN_OUTPUT"; exit 1; }
cmp -s "$WORK_DIR/configured.expected" "$RUN_HOME/.config/lg-buddy/config.env"
! grep -F -q 'Opening LG Buddy to pair your first TV...' "$RUN_OUTPUT"
grep -F -q 'Preserving existing TV profile and policy settings.' "$RUN_OUTPUT"
grep -F -q 'restart LG_Buddy_lifecycle.service' "$RUN_SYSTEMCTL_LOG"
grep -F -q -- '--user enable LG_Buddy_screen.service' "$RUN_SYSTEMCTL_LOG"
grep -F -q -- '--user restart LG_Buddy_screen.service' "$RUN_SYSTEMCTL_LOG"

echo "First-run installer smoke passed: fresh handoff, failure preservation, configured preservation."
