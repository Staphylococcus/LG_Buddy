#!/bin/bash

set -euo pipefail
umask 0022

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPOSITORY_ROOT="$(dirname "$SCRIPT_DIR")"
RUNTIME_BINARY="${1:-$REPOSITORY_ROOT/target/debug/lg-buddy}"
GUI_BINARY="${2:-$REPOSITORY_ROOT/target/debug/lg-buddy-gui}"
TV_FIXTURE="${3:-${LG_BUDDY_GUI_TV_FIXTURE:-}}"
UPDATE_ARCHIVE="${4:-${LG_BUDDY_GUI_UPDATE_ARCHIVE:-}}"
WORK_DIR="$(mktemp -d)"
INSTALL_ROOT="$WORK_DIR/root"
HOME_DIR="$WORK_DIR/home"
FRESH_GUI_INSTALL_PID=""
FRESH_GUI_PID=""
FRESH_ACCESSIBILITY_BUS_PID=""
FRESH_ACCESSIBILITY_REGISTRY_PID=""
FRESH_ACCESSIBILITY_PYTHON=""

fail() {
    echo "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "$FRESH_GUI_INSTALL_PID" ] && kill -0 "$FRESH_GUI_INSTALL_PID" 2>/dev/null; then
        kill "$FRESH_GUI_INSTALL_PID" 2>/dev/null || true
        wait "$FRESH_GUI_INSTALL_PID" 2>/dev/null || true
    fi
    if [ -n "$FRESH_GUI_PID" ] && kill -0 "$FRESH_GUI_PID" 2>/dev/null; then
        kill "$FRESH_GUI_PID" 2>/dev/null || true
        wait "$FRESH_GUI_PID" 2>/dev/null || true
    fi
    if [ -n "$FRESH_ACCESSIBILITY_REGISTRY_PID" ] && kill -0 "$FRESH_ACCESSIBILITY_REGISTRY_PID" 2>/dev/null; then
        kill "$FRESH_ACCESSIBILITY_REGISTRY_PID" 2>/dev/null || true
        wait "$FRESH_ACCESSIBILITY_REGISTRY_PID" 2>/dev/null || true
    fi
    if [ -n "$FRESH_ACCESSIBILITY_BUS_PID" ] && kill -0 "$FRESH_ACCESSIBILITY_BUS_PID" 2>/dev/null; then
        kill "$FRESH_ACCESSIBILITY_BUS_PID" 2>/dev/null || true
        wait "$FRESH_ACCESSIBILITY_BUS_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

[ "$(id -u)" -ne 0 ] || fail "Installed GUI smoke must run as a regular user."
[ -x "$RUNTIME_BINARY" ] || fail "Runtime binary is not executable: $RUNTIME_BINARY"
[ -x "$GUI_BINARY" ] || fail "GUI binary is not executable: $GUI_BINARY"
[ -n "${DISPLAY:-}" ] || fail "DISPLAY is required for the installed GUI smoke test."
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || fail "A D-Bus session is required for the installed GUI smoke test."
command -v pgrep >/dev/null || fail "pgrep is required for the installed GUI smoke test."

RUNTIME_BINARY="$(realpath "$RUNTIME_BINARY")"
GUI_BINARY="$(realpath "$GUI_BINARY")"
mkdir -p "$INSTALL_ROOT" "$HOME_DIR/Desktop" "$HOME_DIR/.config/lg-buddy"
cat >"$HOME_DIR/.config/lg-buddy/config.env" <<'EOF'
tvs_primary_ip=192.0.2.10
tvs_primary_mac=02:00:00:00:00:10
tvs_primary_input=HDMI_1
tvs_primary_platform=bscpylgtv
screen_idle_blank=enabled
screen_backend=auto
screen_idle_timeout=300
screen_restore_policy=conservative
system_sleep_wake_policy=enabled
updates_auto_check=enabled
updates_channel=stable
EOF

export HOME="$HOME_DIR"
export XDG_CONFIG_HOME="$HOME_DIR/.config"
export LG_BUDDY_INSTALL_ROOT="$INSTALL_ROOT"
export LG_BUDDY_SUDO_CMD="none"
export LG_BUDDY_NONINTERACTIVE="1"
export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
export LG_BUDDY_SKIP_PIP_INSTALL="1"
export LG_BUDDY_TV_IP="192.0.2.10"
export LG_BUDDY_TV_MAC="02:00:00:00:00:10"
export LG_BUDDY_INPUT="HDMI_1"
export LG_BUDDY_TV_PLATFORM="bscpylgtv"
export LG_BUDDY_SCREEN_BACKEND="auto"
export LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY="enabled"

start_fresh_accessibility_bus() {
    local launcher=""
    local registry=""
    local candidate=""

    for candidate in \
        "$(command -v python3 2>/dev/null || true)" \
        /usr/bin/python3; do
        if [ -n "$candidate" ] && [ -x "$candidate" ] && \
            "$candidate" -c 'import pyatspi' >/dev/null 2>&1; then
            FRESH_ACCESSIBILITY_PYTHON="$candidate"
            break
        fi
    done
    [ -n "$FRESH_ACCESSIBILITY_PYTHON" ] || fail "Python AT-SPI bindings are required for fresh GUI state verification."

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
    [ -n "$launcher" ] || fail "at-spi-bus-launcher is required for fresh GUI state verification."

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
    [ -n "$registry" ] || fail "at-spi2-registryd is required for fresh GUI state verification."

    "$launcher" --launch-immediately >"$WORK_DIR/fresh-at-spi-bus.output" 2>&1 &
    FRESH_ACCESSIBILITY_BUS_PID=$!
    sleep 0.2
    "$registry" --use-gnome-session >"$WORK_DIR/fresh-at-spi-registry.output" 2>&1 &
    FRESH_ACCESSIBILITY_REGISTRY_PID=$!
    sleep 0.2
}

stop_fresh_accessibility_bus() {
    if [ -n "$FRESH_ACCESSIBILITY_REGISTRY_PID" ] && kill -0 "$FRESH_ACCESSIBILITY_REGISTRY_PID" 2>/dev/null; then
        kill "$FRESH_ACCESSIBILITY_REGISTRY_PID" 2>/dev/null || true
        wait "$FRESH_ACCESSIBILITY_REGISTRY_PID" 2>/dev/null || true
    fi
    FRESH_ACCESSIBILITY_REGISTRY_PID=""
    if [ -n "$FRESH_ACCESSIBILITY_BUS_PID" ] && kill -0 "$FRESH_ACCESSIBILITY_BUS_PID" 2>/dev/null; then
        kill "$FRESH_ACCESSIBILITY_BUS_PID" 2>/dev/null || true
        wait "$FRESH_ACCESSIBILITY_BUS_PID" 2>/dev/null || true
    fi
    FRESH_ACCESSIBILITY_BUS_PID=""
}

run_fresh_gui_launch_smoke() {
    local fresh_root="$WORK_DIR/fresh-root"
    local fresh_home="$WORK_DIR/fresh-home"
    local fresh_output="$WORK_DIR/fresh-install.output"
    local status=0

    mkdir -p "$fresh_root" "$fresh_home/Desktop"
    start_fresh_accessibility_bus
    (
        unset LG_BUDDY_CONFIG
        export HOME="$fresh_home"
        export XDG_CONFIG_HOME="$fresh_home/.config"
        export LG_BUDDY_INSTALL_ROOT="$fresh_root"
        export LG_BUDDY_SUDO_CMD="none"
        export LG_BUDDY_NONINTERACTIVE="1"
        export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
        export LG_BUDDY_SKIP_PIP_INSTALL="1"
        unset LG_BUDDY_GUI_RUNTIME_PROBE
        env -u NO_AT_BRIDGE ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals \
            bash "$REPOSITORY_ROOT/install.sh" \
                --runtime-binary "$RUNTIME_BINARY" \
                --gui-binary "$GUI_BINARY" >"$fresh_output" 2>&1
    ) &
    FRESH_GUI_INSTALL_PID=$!

    for ((attempt = 0; attempt < 300; attempt++)); do
        FRESH_GUI_PID="$(pgrep -n -f "$fresh_root/usr/bin/lg-buddy" || true)"
        [ -n "$FRESH_GUI_PID" ] && break
        if ! kill -0 "$FRESH_GUI_INSTALL_PID" 2>/dev/null; then
            cat "$fresh_output" >&2
            fail "Fresh installer exited before opening the GUI."
        fi
        sleep 0.1
    done
    env -u NO_AT_BRIDGE ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals \
        "$FRESH_ACCESSIBILITY_PYTHON" "$SCRIPT_DIR/test-release-gui-accessibility.py" \
        --timeout 30 --expected-tvs-state empty

    gapplication action io.github.staphylococcus.LGBuddy quit
    wait "$FRESH_GUI_INSTALL_PID" || {
        status=$?
        cat "$fresh_output" >&2
        fail "Fresh installer handoff exited with status $status."
    }
    FRESH_GUI_INSTALL_PID=""
    stop_fresh_accessibility_bus

    [ -f "$fresh_home/.config/lg-buddy/config.env" ] || fail "Fresh install did not create a user configuration."
    [ ! -s "$fresh_home/.config/lg-buddy/config.env" ] || fail "Fresh install wrote TV settings before pairing."
    [ ! -e "$fresh_home/.config/lg-buddy/config.env.setup-pending" ] || fail "Fresh install created an unexpected setup marker."
}

run_fresh_gui_launch_smoke

bash "$REPOSITORY_ROOT/install.sh" \
    --runtime-binary "$RUNTIME_BINARY" \
    --gui-binary "$GUI_BINARY"

INSTALLED_RUNTIME="$INSTALL_ROOT/usr/bin/lg-buddy"
INSTALLED_GUI="$INSTALL_ROOT/usr/bin/lg-buddy-gui"
DESKTOP_ENTRY="$INSTALL_ROOT/usr/share/applications/io.github.staphylococcus.LGBuddy.desktop"
LEGACY_DESKTOP_ENTRY="$INSTALL_ROOT/usr/share/applications/LG_Buddy_Brightness.desktop"
APP_ICON="$INSTALL_ROOT/usr/share/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg"
CONFIG_FILE="$XDG_CONFIG_HOME/lg-buddy/config.env"
NATIVE_TOKEN="$XDG_CONFIG_HOME/lg-buddy/tvs/primary/access-token.json"

[ -x "$INSTALLED_RUNTIME" ] || fail "Installed runtime is missing."
[ -x "$INSTALLED_GUI" ] || fail "Installed GUI is missing."
[ "$(stat -c '%a' "$INSTALLED_GUI")" = "755" ] || fail "Installed GUI mode is not 755."
[ "$(stat -c '%u' "$INSTALLED_GUI")" = "$(id -u)" ] || fail "Installed GUI has the wrong owner."
cmp -s "$RUNTIME_BINARY" "$INSTALLED_RUNTIME" || fail "Installed runtime bytes differ from the candidate."
cmp -s "$GUI_BINARY" "$INSTALLED_GUI" || fail "Installed GUI bytes differ from the candidate."
grep -F -x -q 'Exec=/usr/bin/lg-buddy' "$DESKTOP_ENTRY" || fail "Desktop entry does not use the normal Overview launcher."
grep -F -x -q 'Icon=io.github.staphylococcus.LGBuddy' "$DESKTOP_ENTRY" || fail "Desktop entry does not use the installed application icon."
grep -F -x -q 'Terminal=false' "$DESKTOP_ENTRY" || fail "Desktop entry would open a terminal."
[ -f "$APP_ICON" ] || fail "Installed application icon is missing."
[ ! -e "$LEGACY_DESKTOP_ENTRY" ] || fail "Install left the legacy desktop entry behind."
cmp -s "$REPOSITORY_ROOT/data/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg" "$APP_ICON" || fail "Installed application icon differs from the source asset."

export LG_BUDDY_CONFIG="$CONFIG_FILE"
bash "$SCRIPT_DIR/test-gui-launch.sh" "$INSTALLED_RUNTIME" "$INSTALLED_GUI"
bash "$SCRIPT_DIR/test-release-gui-behavior.sh" "$INSTALLED_RUNTIME" "$CONFIG_FILE" "$TV_FIXTURE" "$UPDATE_ARCHIVE"

mkdir -p "$(dirname "$NATIVE_TOKEN")"
printf '%s\n' '{"access_token":"installed-gui-smoke-token"}' >"$NATIVE_TOKEN"
chmod 600 "$NATIVE_TOKEN"
CONFIG_SNAPSHOT="$WORK_DIR/config.snapshot"
TOKEN_SNAPSHOT="$WORK_DIR/token.snapshot"
cp "$CONFIG_FILE" "$CONFIG_SNAPSHOT"
cp "$NATIVE_TOKEN" "$TOKEN_SNAPSHOT"

unset LG_BUDDY_REMOVE_CONFIG
bash "$REPOSITORY_ROOT/uninstall.sh"

[ ! -e "$INSTALLED_RUNTIME" ] || fail "Uninstall left the runtime installed."
[ ! -e "$INSTALLED_GUI" ] || fail "Uninstall left the GUI installed."
[ ! -e "$DESKTOP_ENTRY" ] || fail "Uninstall left the desktop entry installed."
[ ! -e "$LEGACY_DESKTOP_ENTRY" ] || fail "Uninstall left the legacy desktop entry installed."
[ ! -e "$APP_ICON" ] || fail "Uninstall left the application icon installed."
cmp -s "$CONFIG_SNAPSHOT" "$CONFIG_FILE" || fail "Uninstall changed the user configuration."
cmp -s "$TOKEN_SNAPSHOT" "$NATIVE_TOKEN" || fail "Uninstall changed the native credential."
