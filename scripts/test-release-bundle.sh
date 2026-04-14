#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 --archive <path-to-release-tar.gz> [--work-dir <dir>] [--skip-pip-install]"
    exit 1
}

assert_file() {
    local path="$1"

    if [ ! -f "$path" ]; then
        echo "Expected file not found: $path"
        exit 1
    fi
}

assert_executable() {
    local path="$1"

    if [ ! -x "$path" ]; then
        echo "Expected executable not found: $path"
        exit 1
    fi
}

validate_archive_paths() {
    local archive="$1"
    local entry=""

    while IFS= read -r entry; do
        case "$entry" in
            /*)
                echo "Archive contains an absolute path: $entry"
                exit 1
                ;;
        esac

        if printf '%s\n' "$entry" | grep -Eq '(^|/)\.\.(/|$)'; then
            echo "Archive contains a parent-directory traversal path: $entry"
            exit 1
        fi
    done < <(tar -tzf "$archive")
}

ARCHIVE=""
WORK_DIR=""
SKIP_PIP_INSTALL=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --archive)
            ARCHIVE="${2:-}"
            shift 2
            ;;
        --work-dir)
            WORK_DIR="${2:-}"
            shift 2
            ;;
        --skip-pip-install)
            SKIP_PIP_INSTALL=1
            shift
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$ARCHIVE" ] || usage
[ -f "$ARCHIVE" ] || {
    echo "Archive not found: $ARCHIVE"
    exit 1
}

CLEANUP_WORK_DIR=0
if [ -z "$WORK_DIR" ]; then
    WORK_DIR="$(mktemp -d)"
    CLEANUP_WORK_DIR=1
fi

cleanup() {
    if [ "$CLEANUP_WORK_DIR" -eq 1 ]; then
        rm -rf "$WORK_DIR"
    fi
}

trap cleanup EXIT

EXTRACT_DIR="$WORK_DIR/extracted"
INSTALL_ROOT="$WORK_DIR/root"
HOME_DIR="$WORK_DIR/home"
XDG_CONFIG_HOME="$HOME_DIR/.config"

mkdir -p "$EXTRACT_DIR" "$INSTALL_ROOT" "$HOME_DIR"

validate_archive_paths "$ARCHIVE"
tar -C "$EXTRACT_DIR" -xzf "$ARCHIVE"
BUNDLE_DIR="$(find "$EXTRACT_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"

[ -n "$BUNDLE_DIR" ] || {
    echo "Release archive did not contain a top-level bundle directory."
    exit 1
}

assert_executable "$BUNDLE_DIR/install.sh"
assert_executable "$BUNDLE_DIR/configure.sh"
assert_executable "$BUNDLE_DIR/uninstall.sh"
assert_executable "$BUNDLE_DIR/lg-buddy"
assert_executable "$BUNDLE_DIR/bin/LG_Buddy_Common"
assert_file "$BUNDLE_DIR/LG_Buddy_Brightness.desktop"
assert_file "$BUNDLE_DIR/README.md"
assert_file "$BUNDLE_DIR/LICENSE"
assert_file "$BUNDLE_DIR/docs/user-guide.md"
assert_file "$BUNDLE_DIR/docs/development.md"
assert_file "$BUNDLE_DIR/docs/release-process.md"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy.service"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy_screen.service"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy_sleep.service"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy_wake.service"

HELP_OUTPUT="$("$BUNDLE_DIR/lg-buddy" 2>&1 || true)"
printf '%s\n' "$HELP_OUTPUT" | grep -q "lg-buddy"

export HOME="$HOME_DIR"
export XDG_CONFIG_HOME="$XDG_CONFIG_HOME"
export LG_BUDDY_INSTALL_ROOT="$INSTALL_ROOT"
export LG_BUDDY_SUDO_CMD="none"
export LG_BUDDY_NONINTERACTIVE="1"
export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
export LG_BUDDY_TV_IP="192.168.1.10"
export LG_BUDDY_TV_MAC="aa:bb:cc:dd:ee:ff"
export LG_BUDDY_INPUT="HDMI_2"
export LG_BUDDY_SCREEN_BACKEND="auto"
export LG_BUDDY_ENABLE_SCREEN_MONITOR="0"
export LG_BUDDY_DISABLE_SLEEP_WAKE="0"
export PIP_DISABLE_PIP_VERSION_CHECK="1"
export PIP_NO_PYTHON_VERSION_WARNING="1"

if [ "$SKIP_PIP_INSTALL" -eq 1 ]; then
    export LG_BUDDY_SKIP_PIP_INSTALL="1"
fi

(
    cd "$BUNDLE_DIR"
    ./install.sh
)

CONFIG_FILE="$XDG_CONFIG_HOME/lg-buddy/config.env"
INSTALLED_BINARY="$INSTALL_ROOT/usr/bin/lg-buddy"
INSTALLED_VENV_PIP="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/bin/pip"
INSTALLED_BSCPYLGTV="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/bin/bscpylgtvcommand"
INSTALLED_POINTER="$INSTALL_ROOT/usr/lib/lg-buddy/config-path"
SYSTEM_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy.service"
USER_SCREEN_SERVICE="$HOME/.config/systemd/user/LG_Buddy_screen.service"
DESKTOP_ENTRY="$INSTALL_ROOT/usr/share/applications/LG_Buddy_Brightness.desktop"
NM_SLEEP_HOOK="$INSTALL_ROOT/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep"

assert_file "$CONFIG_FILE"
assert_executable "$INSTALLED_BINARY"
assert_executable "$INSTALLED_VENV_PIP"
assert_file "$INSTALLED_POINTER"
assert_file "$SYSTEM_SERVICE"
assert_file "$USER_SCREEN_SERVICE"
assert_file "$DESKTOP_ENTRY"
assert_file "$NM_SLEEP_HOOK"

grep -q '^tv_ip=192.168.1.10$' "$CONFIG_FILE"
grep -q '^tv_mac=aa:bb:cc:dd:ee:ff$' "$CONFIG_FILE"
grep -q '^input=HDMI_2$' "$CONFIG_FILE"
grep -q '^screen_backend=auto$' "$CONFIG_FILE"
grep -q "$CONFIG_FILE" "$INSTALLED_POINTER"

if [ "$SKIP_PIP_INSTALL" -eq 0 ]; then
    assert_executable "$INSTALLED_BSCPYLGTV"
fi

INSTALLED_HELP_OUTPUT="$("$INSTALLED_BINARY" 2>&1 || true)"
printf '%s\n' "$INSTALLED_HELP_OUTPUT" | grep -q "lg-buddy"

export LG_BUDDY_REMOVE_CONFIG="1"
(
    cd "$BUNDLE_DIR"
    ./uninstall.sh
)

[ ! -e "$INSTALLED_BINARY" ] || {
    echo "Installed binary still present after uninstall: $INSTALLED_BINARY"
    exit 1
}
[ ! -e "$INSTALLED_VENV_PIP" ] || {
    echo "Installed Python virtual environment still present after uninstall: $INSTALLED_VENV_PIP"
    exit 1
}
[ ! -e "$INSTALLED_POINTER" ] || {
    echo "Config pointer still present after uninstall: $INSTALLED_POINTER"
    exit 1
}
[ ! -e "$SYSTEM_SERVICE" ] || {
    echo "System service still present after uninstall: $SYSTEM_SERVICE"
    exit 1
}
[ ! -e "$USER_SCREEN_SERVICE" ] || {
    echo "User screen service still present after uninstall: $USER_SCREEN_SERVICE"
    exit 1
}
[ ! -e "$DESKTOP_ENTRY" ] || {
    echo "Desktop entry still present after uninstall: $DESKTOP_ENTRY"
    exit 1
}
[ ! -e "$NM_SLEEP_HOOK" ] || {
    echo "NetworkManager sleep hook still present after uninstall: $NM_SLEEP_HOOK"
    exit 1
}
[ ! -e "$CONFIG_FILE" ] || {
    echo "User config still present after uninstall: $CONFIG_FILE"
    exit 1
}

echo "Release bundle smoke test passed for $ARCHIVE"
