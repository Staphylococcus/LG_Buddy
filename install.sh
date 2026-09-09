#!/bin/bash

# Exit on any error
set -e

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
ORIGINAL_SCRIPT_DIR="$SCRIPT_DIR"
INSTALL_ROOT="${LG_BUDDY_INSTALL_ROOT:-}"
INSTALL_ROOT="${INSTALL_ROOT%/}"
SUDO_CMD="${LG_BUDDY_SUDO_CMD:-sudo}"
NONINTERACTIVE="${LG_BUDDY_NONINTERACTIVE:-0}"
SKIP_SYSTEMD_ACTIONS="${LG_BUDDY_SKIP_SYSTEMD_ACTIONS:-0}"
SKIP_PIP_INSTALL="${LG_BUDDY_SKIP_PIP_INSTALL:-0}"
DEFAULT_RUNTIME_BINARY="$SCRIPT_DIR/lg-buddy"
GUI_TARGET="x86_64-unknown-linux-gnu"
DEFAULT_GUI_BINARY="$SCRIPT_DIR/docs/lg-buddy-gui-$GUI_TARGET"
APP_ICON_NAME="io.github.staphylococcus.LGBuddy.svg"
DEFAULT_APP_ICON="$SCRIPT_DIR/data/icons/hicolor/scalable/apps/$APP_ICON_NAME"
if [ ! -f "$DEFAULT_APP_ICON" ]; then
    DEFAULT_APP_ICON="$SCRIPT_DIR/docs/$APP_ICON_NAME"
fi
RUNTIME_BINARY="$DEFAULT_RUNTIME_BINARY"
GUI_BINARY="$DEFAULT_GUI_BINARY"
APP_ICON="$DEFAULT_APP_ICON"
RUNTIME_BINARY_OVERRIDDEN=0
GUI_BINARY_OVERRIDDEN=0
UPGRADE_MODE=0
SYSTEM_UPGRADE_MODE=0
MUTATION_STARTED=0
UPGRADE_COMPLETED=0
GUI_BINARY_STAGED_TMP=""
SYSTEM_UPGRADE_INSTALL_ROOT=""
SYSTEM_UPGRADE_CANDIDATE_ROOT=""
SYSTEM_UPGRADE_CONFIG_OVERRIDE=""
SYSTEM_UPGRADE_NM_HOOK=""
SYSTEM_UPGRADE_REPAIR_PYTHON="0"
SYSTEM_UPGRADE_SKIP_PIP="0"
CONFIG_FILE=""
SETUP_PENDING_PATH=""
FRESH_SETUP_MODE=0
SYSTEM_UPGRADE_SKIP_SYSTEMD="0"

usage() {
    cat <<EOF
Usage: $0 [--upgrade] [--runtime-binary /path/to/lg-buddy] [--gui-binary /path/to/lg-buddy-gui]

Install LG Buddy from existing runtime and GUI binaries.

Options:
  --upgrade         Upgrade an existing compatible release-bundle installation

Defaults:
  --runtime-binary defaults to ./lg-buddy next to install.sh
  --gui-binary defaults to ./docs/lg-buddy-gui-x86_64-unknown-linux-gnu
               in an official release bundle
EOF
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --runtime-binary)
            RUNTIME_BINARY="${2:-}"
            [ -n "$RUNTIME_BINARY" ] || usage
            RUNTIME_BINARY_OVERRIDDEN=1
            shift 2
            ;;
        --gui-binary)
            GUI_BINARY="${2:-}"
            [ -n "$GUI_BINARY" ] || usage
            GUI_BINARY_OVERRIDDEN=1
            shift 2
            ;;
        --upgrade)
            [ "$UPGRADE_MODE" -eq 0 ] || usage
            UPGRADE_MODE=1
            shift
            ;;
        --system-upgrade)
            [ "$SYSTEM_UPGRADE_MODE" -eq 0 ] || usage
            SYSTEM_UPGRADE_MODE=1
            UPGRADE_MODE=1
            shift
            [ "$#" -eq 7 ] || usage
            SYSTEM_UPGRADE_INSTALL_ROOT="$1"
            SYSTEM_UPGRADE_CANDIDATE_ROOT="$2"
            SYSTEM_UPGRADE_CONFIG_OVERRIDE="$3"
            SYSTEM_UPGRADE_NM_HOOK="$4"
            SYSTEM_UPGRADE_REPAIR_PYTHON="$5"
            SYSTEM_UPGRADE_SKIP_PIP="$6"
            SYSTEM_UPGRADE_SKIP_SYSTEMD="$7"
            shift 7
            ;;
        -h|--help)
            usage
            ;;
        *)
            usage
            ;;
    esac
done

if [ "$UPGRADE_MODE" -eq 1 ] && { [ "$RUNTIME_BINARY_OVERRIDDEN" -eq 1 ] || [ "$GUI_BINARY_OVERRIDDEN" -eq 1 ]; }; then
    echo "Error: --upgrade uses the verified lg-buddy and lg-buddy-gui binaries from this release bundle."
    exit 1
fi

if [ "$SYSTEM_UPGRADE_MODE" -eq 0 ] && [ -n "$INSTALL_ROOT" ]; then
    case "$INSTALL_ROOT" in
        /*) ;;
        *)
            echo "Error: LG_BUDDY_INSTALL_ROOT must be an absolute path."
            exit 1
            ;;
    esac
fi

if [ "$SYSTEM_UPGRADE_MODE" -eq 0 ] && [ "$(id -u)" -eq 0 ]; then
    echo "Error: Do not run this script with sudo. It will prompt for sudo when needed."
    exit 1
fi

if [ "$SYSTEM_UPGRADE_MODE" -eq 0 ] && [ "$UPGRADE_MODE" -eq 1 ]; then
    echo "Starting LG Buddy Upgrade"
elif [ "$SYSTEM_UPGRADE_MODE" -eq 0 ]; then
    echo "Starting LG Buddy Installation"
fi
if [ "$SYSTEM_UPGRADE_MODE" -eq 0 ] && [ -n "$INSTALL_ROOT" ]; then
    echo "Install root override: $INSTALL_ROOT"
fi

MISSING_PKGS=()
SCREEN_IDLE_BLANK="enabled"
SYSTEM_CONFIG_OVERRIDE_TMP=""
CONFIG_POINTER_TMP=""
NM_HOOK_TMP=""
SYSTEM_UPGRADE_OUTPUT_TMP=""
PM=""
INSTALL_CMD=()

prefix_path() {
    local path="$1"

    if [ -n "$INSTALL_ROOT" ]; then
        printf '%s%s\n' "$INSTALL_ROOT" "$path"
    else
        printf '%s\n' "$path"
    fi
}

run_privileged() {
    if [ "$SUDO_CMD" = "none" ]; then
        "$@"
    elif [ "$SUDO_CMD" = "pkexec" ]; then
        pkexec --disable-internal-agent "$@"
    else
        "$SUDO_CMD" "$@"
    fi
}

prefix_install_root_path() {
    local install_root="$1"
    local path="$2"

    if [ -n "$install_root" ]; then
        printf '%s%s\n' "${install_root%/}" "$path"
    else
        printf '%s\n' "$path"
    fi
}

initialize_install_paths() {
    SYSTEM_BIN_DIR="$(prefix_path "/usr/bin")"
    RUNTIME_INSTALL_PATH="${SYSTEM_BIN_DIR}/lg-buddy"
    GUI_INSTALL_PATH="${SYSTEM_BIN_DIR}/lg-buddy-gui"
    VENV_DIR="${SYSTEM_BIN_DIR}/LG_Buddy_PIP"
    SYSTEM_LIB_DIR="$(prefix_path "/usr/lib/lg-buddy")"
    CONFIG_POINTER_PATH="${SYSTEM_LIB_DIR}/config-path"
    COMMON_HELPER_PATH="${SYSTEM_LIB_DIR}/common.sh"
    SYSTEM_SLEEP_HOOK_PATH="$(prefix_path "/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook")"
    SYSTEMD_SYSTEM_DIR="$(prefix_path "/etc/systemd/system")"
    SYSTEMD_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy.service"
    SYSTEMD_LIFECYCLE_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_lifecycle.service"
    SYSTEMD_WAKE_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_wake.service"
    SYSTEMD_SLEEP_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_sleep.service"
    SYSTEMD_SERVICE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy.service.d"
    SYSTEMD_LIFECYCLE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_lifecycle.service.d"
    SYSTEMD_WAKE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_wake.service.d"
    SYSTEMD_SLEEP_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_sleep.service.d"
    TMPFILES_CONF_DIR="$(prefix_path "/etc/tmpfiles.d")"
    TMPFILES_CONF_PATH="${TMPFILES_CONF_DIR}/lg_buddy.conf"
    NM_PRE_DOWN_DIR="$(prefix_path "/etc/NetworkManager/dispatcher.d/pre-down.d")"
    NM_SLEEP_HOOK_PATH="${NM_PRE_DOWN_DIR}/LG_Buddy_sleep"
    NM_LIFECYCLE_HOOK_PATH="${NM_PRE_DOWN_DIR}/LG_Buddy_lifecycle"
    APPLICATIONS_DIR="$(prefix_path "/usr/share/applications")"
    DESKTOP_ENTRY_NAME="io.github.staphylococcus.LGBuddy.desktop"
    DESKTOP_ENTRY_PATH="${APPLICATIONS_DIR}/${DESKTOP_ENTRY_NAME}"
    LEGACY_DESKTOP_ENTRY_PATH="${APPLICATIONS_DIR}/LG_Buddy_Brightness.desktop"
    DESKTOP_ENTRY_SOURCE="${SCRIPT_DIR}/${DESKTOP_ENTRY_NAME}"
    if [ ! -f "$DESKTOP_ENTRY_SOURCE" ]; then
        # Release archives retain this internal name for compatibility with
        # updaters shipped before the application-ID filename was adopted.
        DESKTOP_ENTRY_SOURCE="${SCRIPT_DIR}/LG_Buddy_Brightness.desktop"
    fi
    APP_ICON_DIR="$(prefix_path "/usr/share/icons/hicolor/scalable/apps")"
    APP_ICON_PATH="${APP_ICON_DIR}/${APP_ICON_NAME}"
    USER_DESKTOP_ENTRY_PATH="${HOME}/Desktop/${DESKTOP_ENTRY_NAME}"
    LEGACY_USER_DESKTOP_ENTRY_PATH="${HOME}/Desktop/LG_Buddy_Brightness.desktop"
    USER_SYSTEMD_DIR="${HOME}/.config/systemd/user"
    USER_SCREEN_SERVICE_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service"
    USER_SCREEN_OVERRIDE_DIR="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service.d"
    USER_UPDATE_CHECK_SERVICE_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.service"
    USER_UPDATE_CHECK_TIMER_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.timer"
    USER_UPDATE_CHECK_OVERRIDE_DIR="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.service.d"
}

initialize_install_paths

if [ ! -r "$SCRIPT_DIR/bin/LG_Buddy_Common" ]; then
    echo "LG Buddy common helper is not readable: $SCRIPT_DIR/bin/LG_Buddy_Common"
    exit 1
fi
. "$SCRIPT_DIR/bin/LG_Buddy_Common"

if [ "$SYSTEM_UPGRADE_MODE" -eq 0 ]; then
    CONFIG_FILE="$(lg_buddy_user_config_path)"
    SETUP_PENDING_PATH="${CONFIG_FILE}.setup-pending"
fi

config_has_saved_tv_profile() {
    local ip=""
    local mac=""
    local input=""

    ip="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get tv.ip 2>/dev/null)" || return 1
    mac="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get tv.mac 2>/dev/null)" || return 1
    input="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get tv.input 2>/dev/null)" || return 1
    [ -n "$ip" ] && [ -n "$mac" ] && [ -n "$input" ]
}

ensure_user_file_if_absent() {
    local path="$1"
    local description="$2"

    if [ -L "$path" ]; then
        echo "LG Buddy $description is a symbolic link; refusing to replace it: $path" >&2
        return 1
    fi
    if [ -e "$path" ]; then
        if [ ! -f "$path" ]; then
            echo "LG Buddy $description is not a regular file: $path" >&2
            return 1
        fi
        if [ ! -r "$path" ]; then
            echo "LG Buddy $description is not readable: $path" >&2
            return 1
        fi
        return 0
    fi

    # noclobber makes the redirection itself the exclusive create operation;
    # the second check handles a concurrent publisher that won the race.
    if (umask 077; set -C; : >"$path") 2>/dev/null; then
        return 0
    fi
    if [ -L "$path" ]; then
        echo "LG Buddy $description became a symbolic link; refusing to replace it: $path" >&2
        return 1
    fi
    if [ -f "$path" ]; then
        return 0
    fi
    echo "Could not create the LG Buddy $description: $path" >&2
    return 1
}

create_empty_config_if_absent() {
    local config_dir=""

    config_dir="$(dirname "$CONFIG_FILE")"
    mkdir -p "$config_dir"
    chmod 700 "$config_dir"
    ensure_user_file_if_absent "$CONFIG_FILE" "configuration file"
}

create_setup_pending_marker() {
    local marker_dir=""

    marker_dir="$(dirname "$SETUP_PENDING_PATH")"
    mkdir -p "$marker_dir"
    chmod 700 "$marker_dir"
    ensure_user_file_if_absent "$SETUP_PENDING_PATH" "setup marker"
}

check_dep() {
    local label="$1"
    local pkg="$2"
    local check_cmd="$3"
    if eval "$check_cmd" &>/dev/null; then
        echo "  [OK]      $label"
    else
        echo "  [MISSING] $label"
        MISSING_PKGS+=("$pkg")
    fi
}

check_python3_venv() {
    local tmp_venv_dir=""
    tmp_venv_dir="$(mktemp -d)" || return 1

    if python3 -m venv "$tmp_venv_dir" >/dev/null 2>&1 &&
        "$tmp_venv_dir/bin/pip" --version >/dev/null 2>&1; then
        rm -rf "$tmp_venv_dir"
        return 0
    fi

    rm -rf "$tmp_venv_dir"
    return 1
}

detect_package_manager() {
    if command -v apt &>/dev/null; then
        PM="apt"
        INSTALL_CMD=(apt install -y)
    elif command -v dnf &>/dev/null; then
        PM="dnf"
        INSTALL_CMD=(dnf install -y)
    elif command -v pacman &>/dev/null; then
        PM="pacman"
        INSTALL_CMD=(pacman -S --noconfirm)
    else
        PM=""
        INSTALL_CMD=()
    fi
}

pkexec_package() {
    case "$PM" in
        dnf|pacman) printf '%s\n' polkit ;;
        *) printf '%s\n' pkexec ;;
    esac
}

pkexec_available() {
    local path=""

    path="$(command -v pkexec 2>/dev/null || true)"
    [ -n "$path" ] && [ -x "$path" ]
}

require_first_run_pkexec() {
    [ "$FRESH_SETUP_MODE" -eq 1 ] || return 0
    if pkexec_available; then
        return 0
    fi

    echo "pkexec is required to activate LG Buddy's installed system services after pairing."
    MISSING_PKGS=("$(pkexec_package)")
    print_manual_install_command
    return 1
}

gui_runtime_package() {
    local requirement="$1"

    case "$PM:$requirement" in
        apt:gtk) printf '%s\n' "libgtk-4-1" ;;
        apt:libadwaita) printf '%s\n' "libadwaita-1-0" ;;
        dnf:gtk|pacman:gtk) printf '%s\n' "gtk4" ;;
        dnf:libadwaita|pacman:libadwaita) printf '%s\n' "libadwaita" ;;
        *:gtk) printf '%s\n' "GTK 4.14 or newer" ;;
        *:libadwaita) printf '%s\n' "libadwaita 1.5 or newer" ;;
    esac
}

gui_runtime_version_at_least() {
    local library="$1"
    local symbol_prefix="$2"
    local required_major="$3"
    local required_minor="$4"

    if [ -n "${LG_BUDDY_GUI_RUNTIME_PROBE:-}" ]; then
        "$LG_BUDDY_GUI_RUNTIME_PROBE" \
            "$library" "$symbol_prefix" "$required_major" "$required_minor"
        return
    fi

    python3 - "$library" "$symbol_prefix" "$required_major" "$required_minor" <<'PY'
import ctypes
import sys

library, prefix, required_major, required_minor = sys.argv[1:]
try:
    runtime = ctypes.CDLL(library)
    major = getattr(runtime, f"{prefix}_get_major_version")
    minor = getattr(runtime, f"{prefix}_get_minor_version")
    major.argtypes = []
    minor.argtypes = []
    major.restype = ctypes.c_uint
    minor.restype = ctypes.c_uint
    installed = (major(), minor())
except (AttributeError, OSError):
    raise SystemExit(1)

required = (int(required_major), int(required_minor))
raise SystemExit(0 if installed >= required else 1)
PY
}

check_gui_runtime_prerequisites() {
    check_dep \
        "GTK 4.14 or newer" \
        "$(gui_runtime_package gtk)" \
        "gui_runtime_version_at_least libgtk-4.so.1 gtk 4 14"
    check_dep \
        "libadwaita 1.5 or newer" \
        "$(gui_runtime_package libadwaita)" \
        "gui_runtime_version_at_least libadwaita-1.so.0 adw 1 5"
}

verify_gui_runtime_prerequisites() {
    local missing=0

    if ! gui_runtime_version_at_least libgtk-4.so.1 gtk 4 14; then
        echo "Error: GTK 4.14 or newer is still unavailable."
        missing=1
    fi
    if ! gui_runtime_version_at_least libadwaita-1.so.0 adw 1 5; then
        echo "Error: libadwaita 1.5 or newer is still unavailable."
        missing=1
    fi

    [ "$missing" -eq 0 ] || return 1
}

print_manual_install_command() {
    case "$PM" in
        apt) echo "  sudo apt install ${MISSING_PKGS[*]}" ;;
        dnf) echo "  sudo dnf install ${MISSING_PKGS[*]}" ;;
        pacman) echo "  sudo pacman -S ${MISSING_PKGS[*]}" ;;
        *) echo "Install these requirements with your system package manager: ${MISSING_PKGS[*]}" ;;
    esac
}

write_config_override() {
    local override_file="$1"
    local config_path="$2"
    local escaped_config_path=""

    escaped_config_path="${config_path//\\/\\\\}"
    escaped_config_path="${escaped_config_path//\"/\\\"}"

    cat >"$override_file" <<EOF
[Service]
Environment="LG_BUDDY_CONFIG=$escaped_config_path"
EOF
}

write_config_pointer() {
    local pointer_file="$1"
    local config_path="$2"

    printf '%s\n' "$config_path" >"$pointer_file"
}

write_nm_pre_down_hook() {
    local hook_file="$1"

    cat >"$hook_file" <<EOF
#!/bin/sh
set -eu

if [ "\${2:-}" != "pre-down" ]; then
    exit 0
fi

exec /usr/bin/lg-buddy nm-pre-down
EOF
}

cleanup_legacy_sleep_wake_handlers() {
    if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
        echo "Skipping legacy sleep/wake systemctl cleanup because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
    else
        run_privileged systemctl disable LG_Buddy_wake.service 2>/dev/null || true
        run_privileged systemctl disable LG_Buddy_sleep.service 2>/dev/null || true
        run_privileged systemctl stop LG_Buddy_wake.service 2>/dev/null || true
        run_privileged systemctl stop LG_Buddy_sleep.service 2>/dev/null || true
    fi

    run_privileged rm -f "$SYSTEMD_WAKE_SERVICE_PATH"
    run_privileged rm -f "$SYSTEMD_SLEEP_SERVICE_PATH"
    run_privileged rm -f "${SYSTEMD_WAKE_OVERRIDE_DIR}/config.conf"
    run_privileged rm -f "${SYSTEMD_SLEEP_OVERRIDE_DIR}/config.conf"
    run_privileged rmdir "$SYSTEMD_WAKE_OVERRIDE_DIR" 2>/dev/null || true
    run_privileged rmdir "$SYSTEMD_SLEEP_OVERRIDE_DIR" 2>/dev/null || true
    run_privileged rm -f "$NM_SLEEP_HOOK_PATH"
    run_privileged rm -f "$SYSTEM_SLEEP_HOOK_PATH"
}

SYSTEM_UPGRADE_HELPER_MODE=0
SYSTEM_UPGRADE_MUTATION_EMITTED=0

system_upgrade_message() {
    if [ "$SYSTEM_UPGRADE_HELPER_MODE" -eq 1 ]; then
        echo "$*" >&2
    else
        echo "$*"
    fi
}

system_upgrade_status() {
    printf 'LG_BUDDY_INSTALL_STATUS=%s\n' "$1"
}

run_system_mutation_command() {
    if [ "$SYSTEM_UPGRADE_HELPER_MODE" -eq 1 ]; then
        if [ "$SYSTEM_UPGRADE_MUTATION_EMITTED" -eq 0 ]; then
            system_upgrade_status mutation_started
            SYSTEM_UPGRADE_MUTATION_EMITTED=1
        fi
        "$@" >&2
    else
        run_privileged "$@"
    fi
}

perform_privileged_runtime_installation() {
    if [ "$UPGRADE_MODE" -eq 0 ] || [ "$REPAIR_PYTHON_ENVIRONMENT" -eq 1 ]; then
        MUTATION_STARTED=1
        system_upgrade_message "Creating Python virtual environment at $VENV_DIR..."
        # Recreate the helper venv so OS Python minor-version upgrades do not leave
        # bscpylgtv installed under an interpreter-specific site-packages directory
        # that the new `/usr/bin/python3` no longer reads.
        run_system_mutation_command python3 -m venv --clear "$VENV_DIR"
        system_upgrade_message "Done."

        if [ "$SKIP_PIP_INSTALL" = "1" ]; then
            system_upgrade_message "Skipping bscpylgtv installation because LG_BUDDY_SKIP_PIP_INSTALL=1."
        else
            system_upgrade_message "Installing bscpylgtv into the virtual environment..."
            run_system_mutation_command "$VENV_DIR/bin/pip" install bscpylgtv
            system_upgrade_message "Done."
        fi
    fi

    MUTATION_STARTED=1
    system_upgrade_message "Installing Rust runtime and support files..."
    run_system_mutation_command install -m 755 "$RUNTIME_BINARY" "$RUNTIME_INSTALL_PATH"
    run_system_mutation_command install -m 755 "$GUI_BINARY" "$GUI_INSTALL_PATH"
    if [ "$UPGRADE_MODE" -eq 0 ]; then
        run_system_mutation_command rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Startup"
        run_system_mutation_command rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Shutdown"
        run_system_mutation_command rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_On"
        run_system_mutation_command rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Off"
        run_system_mutation_command rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Monitor"
        run_system_mutation_command rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_sleep_pre"
        run_system_mutation_command rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Brightness"
        run_system_mutation_command rm -f "$COMMON_HELPER_PATH"
        run_system_mutation_command rm -f "$CONFIG_POINTER_PATH"
        run_system_mutation_command rmdir "$SYSTEM_LIB_DIR" 2>/dev/null || true
    fi
    if [ "$UPGRADE_MODE" -eq 0 ]; then
        run_system_mutation_command install -d "$SYSTEM_LIB_DIR"
        run_system_mutation_command install -m 644 "$CONFIG_POINTER_TMP" "$CONFIG_POINTER_PATH"
    fi
    system_upgrade_message "Installing LG Buddy desktop entry..."
    run_system_mutation_command install -d "$APPLICATIONS_DIR"
    run_system_mutation_command install -m 644 "$DESKTOP_ENTRY_SOURCE" "$DESKTOP_ENTRY_PATH"
    run_system_mutation_command rm -f "$LEGACY_DESKTOP_ENTRY_PATH"
    run_system_mutation_command install -d "$APP_ICON_DIR"
    run_system_mutation_command install -m 644 "$APP_ICON" "$APP_ICON_PATH"
    system_upgrade_message "Done."

}

perform_privileged_services_installation() {
    system_upgrade_message "Copying and enabling systemd services..."
    run_system_mutation_command install -d "$SYSTEMD_SYSTEM_DIR"
    run_system_mutation_command install -d "$TMPFILES_CONF_DIR"
    run_system_mutation_command install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy.service" "$SYSTEMD_SERVICE_PATH"
    run_system_mutation_command install -m 644 "$SCRIPT_DIR/systemd/lg_buddy.conf" "$TMPFILES_CONF_PATH"
    run_system_mutation_command install -d "$SYSTEMD_SERVICE_OVERRIDE_DIR"
    run_system_mutation_command install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_SERVICE_OVERRIDE_DIR}/config.conf"

    if [ "$UPGRADE_MODE" -eq 0 ]; then
        cleanup_legacy_sleep_wake_handlers
    fi

    run_system_mutation_command install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_lifecycle.service" "$SYSTEMD_LIFECYCLE_SERVICE_PATH"
    run_system_mutation_command install -d "$SYSTEMD_LIFECYCLE_OVERRIDE_DIR"
    run_system_mutation_command install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_LIFECYCLE_OVERRIDE_DIR}/config.conf"
    run_system_mutation_command install -d "$NM_PRE_DOWN_DIR"
    run_system_mutation_command install -m 755 "$NM_HOOK_TMP" "$NM_LIFECYCLE_HOOK_PATH"

    if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
        system_upgrade_message "Skipping systemd tmpfiles and enable actions because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
    else
        run_system_mutation_command systemd-tmpfiles --create "$TMPFILES_CONF_PATH"
        run_system_mutation_command systemctl daemon-reload
        run_system_mutation_command systemctl enable LG_Buddy.service
        run_system_mutation_command systemctl enable LG_Buddy_lifecycle.service
        if [ "$FRESH_SETUP_MODE" -eq 1 ]; then
            system_upgrade_message "System services enabled; lifecycle start is deferred until the first TV is paired."
        else
            run_system_mutation_command systemctl restart LG_Buddy_lifecycle.service
        fi
    fi
    system_upgrade_message "Done."
}

perform_privileged_installation() {
    perform_privileged_runtime_installation
    perform_privileged_services_installation
}

run_system_upgrade_helper() {
    [ "$(id -u)" -eq 0 ] || exit 126
    [ "$#" -eq 7 ] || exit 2

    INSTALL_ROOT="$1"
    SCRIPT_DIR="$2"
    SYSTEM_CONFIG_OVERRIDE_TMP="$3"
    NM_HOOK_TMP="$4"
    REPAIR_PYTHON_ENVIRONMENT="$5"
    SKIP_PIP_INSTALL="$6"
    SKIP_SYSTEMD_ACTIONS="$7"

    case "$INSTALL_ROOT" in
        ""|/*) ;;
        *) exit 2 ;;
    esac
    [ "$SCRIPT_DIR" = "$ORIGINAL_SCRIPT_DIR" ] || exit 2
    [ -d "$SCRIPT_DIR" ] || exit 2
    case "$SCRIPT_DIR:$SYSTEM_CONFIG_OVERRIDE_TMP:$NM_HOOK_TMP" in
        /*:/*:/*) ;;
        *) exit 2 ;;
    esac
    case "$REPAIR_PYTHON_ENVIRONMENT:$SKIP_PIP_INSTALL:$SKIP_SYSTEMD_ACTIONS" in
        0:0:0|0:0:1|0:1:0|0:1:1|1:0:0|1:0:1|1:1:0|1:1:1) ;;
        *) exit 2 ;;
    esac

    PATH=/run/current-system/sw/bin:/run/wrappers/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
    export PATH
    SYSTEM_UPGRADE_HELPER_MODE=1
    UPGRADE_MODE=1
    RUNTIME_BINARY="$SCRIPT_DIR/lg-buddy"
    GUI_BINARY="$SCRIPT_DIR/docs/lg-buddy-gui-$GUI_TARGET"
    APP_ICON="$SCRIPT_DIR/docs/$APP_ICON_NAME"
    DESKTOP_ENTRY_SOURCE="$SCRIPT_DIR/$DESKTOP_ENTRY_NAME"
    if [ ! -f "$DESKTOP_ENTRY_SOURCE" ]; then
        DESKTOP_ENTRY_SOURCE="$SCRIPT_DIR/LG_Buddy_Brightness.desktop"
    fi
    for source in \
        "$RUNTIME_BINARY" "$GUI_BINARY" "$APP_ICON" "$DESKTOP_ENTRY_SOURCE" \
        "$SYSTEM_CONFIG_OVERRIDE_TMP" "$NM_HOOK_TMP"; do
        [ -f "$source" ] || exit 2
        [ ! -L "$source" ] || exit 2
        [ -r "$source" ] || exit 2
    done
    [ -x "$RUNTIME_BINARY" ] || exit 2
    [ -x "$GUI_BINARY" ] || exit 2
    initialize_install_paths
    SYSTEM_UPGRADE_MUTATION_EMITTED=0

    system_upgrade_status authorized
    perform_privileged_installation
    system_upgrade_status root_complete
}

resolve_runtime_binary() {
    if [ ! -f "$RUNTIME_BINARY" ]; then
        echo "LG Buddy runtime binary not found at: $RUNTIME_BINARY"
        echo "Build lg-buddy separately first, or use an official release bundle."
        exit 1
    fi

    if [ ! -x "$RUNTIME_BINARY" ]; then
        echo "LG Buddy runtime binary is not executable: $RUNTIME_BINARY"
        echo "Run chmod +x on the binary or provide a valid executable path."
        exit 1
    fi

    echo "Using lg-buddy runtime binary: $RUNTIME_BINARY"
}

resolve_gui_binary() {
    if [ -L "$GUI_BINARY" ]; then
        echo "LG Buddy GUI binary must be a regular file, not a symbolic link: $GUI_BINARY"
        exit 1
    fi

    if [ ! -f "$GUI_BINARY" ]; then
        echo "LG Buddy GUI binary not found at: $GUI_BINARY"
        echo "Build lg-buddy-gui separately first, or use an official release bundle."
        exit 1
    fi

    if [ ! -r "$GUI_BINARY" ]; then
        echo "LG Buddy GUI binary is not readable: $GUI_BINARY"
        exit 1
    fi

    if find "$GUI_BINARY" -prune -perm /022 | grep -q .; then
        echo "LG Buddy GUI binary is writable by its group or by other users: $GUI_BINARY"
        echo "Remove unsafe write permissions before installing."
        exit 1
    fi

    if [ ! -x "$GUI_BINARY" ]; then
        if [ "$GUI_BINARY_OVERRIDDEN" -eq 1 ]; then
            echo "LG Buddy GUI binary is not executable: $GUI_BINARY"
            echo "Provide a regular executable lg-buddy-gui binary."
            exit 1
        fi
        GUI_BINARY_STAGED_TMP="$(mktemp)"
        install -m 700 "$GUI_BINARY" "$GUI_BINARY_STAGED_TMP"
        GUI_BINARY="$GUI_BINARY_STAGED_TMP"
    fi

    if [ "$GUI_BINARY" -ef "$RUNTIME_BINARY" ]; then
        echo "LG Buddy GUI binary resolves to the lg-buddy runtime binary: $GUI_BINARY"
        exit 1
    fi

    echo "Using lg-buddy GUI binary: $GUI_BINARY"
}

resolve_app_icon() {
    if [ -L "$APP_ICON" ] || [ ! -f "$APP_ICON" ]; then
        echo "LG Buddy application icon is missing or is not a regular file: $APP_ICON"
        exit 1
    fi

    if [ ! -r "$APP_ICON" ]; then
        echo "LG Buddy application icon is not readable: $APP_ICON"
        exit 1
    fi

    echo "Using LG Buddy application icon: $APP_ICON"
}

validate_candidate_binary_identity() {
    if ! CANDIDATE_VERSION_OUTPUT="$("$RUNTIME_BINARY" --version)"; then
        echo "LG Buddy runtime binary could not report its version identity: $RUNTIME_BINARY"
        exit 1
    fi
    if ! GUI_VERSION_OUTPUT="$("$GUI_BINARY" --version)"; then
        echo "LG Buddy GUI binary could not report its version identity: $GUI_BINARY"
        echo "Ensure GTK 4.14 or newer and libadwaita 1.5 or newer are installed."
        exit 1
    fi
    if [ "$GUI_VERSION_OUTPUT" != "$CANDIDATE_VERSION_OUTPUT" ]; then
        echo "LG Buddy GUI candidate identity does not match the runtime candidate."
        exit 1
    fi
}

install_missing_prerequisites() {
    if [ ${#MISSING_PKGS[@]} -eq 0 ]; then
        echo "All prerequisites satisfied."
        return
    fi

    echo ""
    echo "Missing: ${MISSING_PKGS[*]}"

    if [ -n "$PM" ]; then
        AUTO_INSTALL="${LG_BUDDY_AUTO_INSTALL_DEPS:-}"
        if [ -z "$AUTO_INSTALL" ] && [ "$NONINTERACTIVE" != "1" ]; then
            read -p "Install missing packages with $PM now? (y/N) " AUTO_INSTALL
        fi
        case "$AUTO_INSTALL" in
            [Yy]*)
                if ! run_privileged "${INSTALL_CMD[@]}" "${MISSING_PKGS[@]}"; then
                    echo "Failed to install the missing packages. Install them manually and re-run install.sh."
                    print_manual_install_command
                    exit 1
                fi
                ;;
            *)
                echo "Please install the missing packages manually and re-run install.sh."
                print_manual_install_command
                exit 1
                ;;
        esac
    else
        echo "Could not detect a supported package manager (apt/dnf/pacman)."
        echo "Please install the missing packages manually and re-run install.sh."
        print_manual_install_command
        exit 1
    fi
}

check_install_prerequisites() {
    echo ""
    echo "Checking prerequisites..."
    MISSING_PKGS=()
    detect_package_manager
    check_gui_runtime_prerequisites
    if [ "$UPGRADE_MODE" -eq 0 ]; then
        check_dep "python3-venv" "python3-venv" "check_python3_venv"
        check_dep "zenity" "zenity" "command -v zenity"
        if [ "$FRESH_SETUP_MODE" -eq 1 ]; then
            check_dep "pkexec (required for first-run activation)" "$(pkexec_package)" "pkexec_available"
        fi
    fi
    install_missing_prerequisites
    require_first_run_pkexec
    if ! verify_gui_runtime_prerequisites; then
        echo "The installed packages do not satisfy the GUI runtime requirements."
        print_manual_install_command
        exit 1
    fi
}

require_python_repair_prerequisites() {
    echo "Checking Python compatibility-platform repair prerequisites..."
    MISSING_PKGS=()
    check_dep "python3-venv" "python3-venv" "check_python3_venv"
    if [ ${#MISSING_PKGS[@]} -gt 0 ]; then
        echo "Upgrade requires Python environment repair, but these prerequisites are missing: ${MISSING_PKGS[*]}"
        echo "Install them manually and rerun the upgrade. No installation files were changed."
        exit 1
    fi
}

python_environment_healthy() {
    local python_version=""
    local site_packages=""

    python_version="$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')" || return 1
    site_packages="$VENV_DIR/lib/python$python_version/site-packages"

    [ -f "$VENV_DIR/pyvenv.cfg" ] &&
        [ -x "$VENV_DIR/bin/python" ] &&
        [ -x "$VENV_DIR/bin/pip" ] &&
        [ -x "$VENV_DIR/bin/bscpylgtvcommand" ] &&
        { [ -d "$site_packages/bscpylgtv" ] || [ -f "$site_packages/bscpylgtv.py" ]; }
}

load_upgrade_configuration() {
    CONFIG_FILE="$(sed -n '/[^[:space:]]/{p;q;}' "$CONFIG_POINTER_PATH")"
    [ -n "$CONFIG_FILE" ] || {
        echo "Installed config pointer is empty: $CONFIG_POINTER_PATH"
        exit 1
    }

    TV_PLATFORM="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get tv.platform)"
    SCREEN_IDLE_BLANK="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get screen.idle_blank)"
    SYSTEM_SLEEP_WAKE_POLICY="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get system.sleep_wake_policy)"
    UPDATE_AUTO_CHECK="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get updates.auto_check)"
    UPDATE_CHANNEL="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get updates.channel)"
    echo "Using existing configuration file at $CONFIG_FILE"
    echo "Preserving update channel: $UPDATE_CHANNEL"
}

load_existing_configuration() {
    TV_PLATFORM="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get tv.platform)"
    SCREEN_IDLE_BLANK="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get screen.idle_blank)"
    SYSTEM_SLEEP_WAKE_POLICY="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get system.sleep_wake_policy)"
    UPDATE_AUTO_CHECK="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get updates.auto_check)"
    UPDATE_CHANNEL="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get updates.channel)"
    echo "Using existing configuration file at $CONFIG_FILE"
    echo "Preserving existing TV profile and policy settings."
}

prepare_installation_files() {
    if [ "$UPGRADE_MODE" -eq 0 ]; then
        CONFIG_POINTER_TMP="$(mktemp)"
        write_config_pointer "$CONFIG_POINTER_TMP" "$CONFIG_FILE"
    fi
    SYSTEM_CONFIG_OVERRIDE_TMP="$(mktemp)"
    write_config_override "$SYSTEM_CONFIG_OVERRIDE_TMP" "$CONFIG_FILE"
    NM_HOOK_TMP="$(mktemp)"
    write_nm_pre_down_hook "$NM_HOOK_TMP"
}

cleanup() {
    local status=$?

    if [ -n "$SYSTEM_CONFIG_OVERRIDE_TMP" ]; then
        rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
    fi

    if [ -n "$CONFIG_POINTER_TMP" ]; then
        rm -f "$CONFIG_POINTER_TMP"
    fi

    if [ -n "$NM_HOOK_TMP" ]; then
        rm -f "$NM_HOOK_TMP"
    fi

    if [ -n "$SYSTEM_UPGRADE_OUTPUT_TMP" ]; then
        rm -f "$SYSTEM_UPGRADE_OUTPUT_TMP"
    fi

    if [ -n "$GUI_BINARY_STAGED_TMP" ]; then
        rm -f "$GUI_BINARY_STAGED_TMP"
    fi

    if [ "$status" -ne 0 ] && [ "$UPGRADE_MODE" -eq 1 ] && [ "$MUTATION_STARTED" -eq 1 ] && [ "$UPGRADE_COMPLETED" -eq 0 ]; then
        echo "LG Buddy upgrade did not complete after installation changes began." >&2
        echo "The installation may be partial; rerun this verified bundle with --upgrade after correcting the reported failure." >&2
    fi

    trap - EXIT
    exit "$status"
}

if [ "$SYSTEM_UPGRADE_MODE" -eq 1 ]; then
    run_system_upgrade_helper \
        "$SYSTEM_UPGRADE_INSTALL_ROOT" \
        "$SYSTEM_UPGRADE_CANDIDATE_ROOT" \
        "$SYSTEM_UPGRADE_CONFIG_OVERRIDE" \
        "$SYSTEM_UPGRADE_NM_HOOK" \
        "$SYSTEM_UPGRADE_REPAIR_PYTHON" \
        "$SYSTEM_UPGRADE_SKIP_PIP" \
        "$SYSTEM_UPGRADE_SKIP_SYSTEMD"
    exit $?
fi

trap cleanup EXIT

resolve_runtime_binary
REPAIR_PYTHON_ENVIRONMENT=0

if [ "$UPGRADE_MODE" -eq 0 ] && ! config_has_saved_tv_profile; then
    FRESH_SETUP_MODE=1
fi

if [ "$UPGRADE_MODE" -eq 1 ]; then
    echo ""
    echo "Running candidate upgrade preflight..."
    "$RUNTIME_BINARY" upgrade-preflight "$SCRIPT_DIR"
    resolve_gui_binary
    resolve_app_icon
    check_install_prerequisites
    validate_candidate_binary_identity
    load_upgrade_configuration

    if [ "$TV_PLATFORM" = "lg_webos" ]; then
        echo "Native TV platform selected; preserving the existing Python environment unchanged."
    elif python_environment_healthy; then
        echo "Python compatibility environment is healthy; preserving it unchanged."
    else
        REPAIR_PYTHON_ENVIRONMENT=1
        "$RUNTIME_BINARY" upgrade-preflight "$SCRIPT_DIR" --repair-python
        require_python_repair_prerequisites
    fi
else
    resolve_gui_binary
    resolve_app_icon
    check_install_prerequisites
    validate_candidate_binary_identity

if [ "$FRESH_SETUP_MODE" -eq 1 ]; then
    create_empty_config_if_absent
    create_setup_pending_marker
    SCREEN_IDLE_BLANK="enabled"
    SYSTEM_SLEEP_WAKE_POLICY="enabled"
    UPDATE_AUTO_CHECK="enabled"
    echo "Prepared an empty user configuration for first-run TV pairing."
    echo "First-run setup will use the application defaults; no behavior choices are required."
else
    load_existing_configuration
fi
fi

prepare_installation_files

if [ "$UPGRADE_MODE" -eq 1 ] && [ "$SUDO_CMD" = "pkexec" ]; then
    echo "Requesting graphical authorization for system installation changes..."
    SYSTEM_UPGRADE_OUTPUT_TMP="$(mktemp)"
    HELPER_STATUS=0
    set +e
    run_privileged "$BASH" "$SCRIPT_DIR/install.sh" --system-upgrade \
        "$INSTALL_ROOT" \
        "$SCRIPT_DIR" \
        "$SYSTEM_CONFIG_OVERRIDE_TMP" \
        "$NM_HOOK_TMP" \
        "$REPAIR_PYTHON_ENVIRONMENT" \
        "$SKIP_PIP_INSTALL" \
        "$SKIP_SYSTEMD_ACTIONS" | tee "$SYSTEM_UPGRADE_OUTPUT_TMP"
    HELPER_STATUS="${PIPESTATUS[0]}"
    set -e

    if [ "$HELPER_STATUS" -ne 0 ]; then
        if grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=mutation_started' "$SYSTEM_UPGRADE_OUTPUT_TMP"; then
            MUTATION_STARTED=1
        elif [ "$HELPER_STATUS" -eq 126 ]; then
            echo "Graphical authorization was cancelled; no installation changes were made." >&2
        elif [ "$HELPER_STATUS" -eq 127 ]; then
            echo "Graphical authorization failed or no authentication agent was available; no installation changes were made." >&2
        else
            echo "The authorized system installation helper failed before installation changes began." >&2
        fi
        exit "$HELPER_STATUS"
    fi

    if grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=mutation_started' "$SYSTEM_UPGRADE_OUTPUT_TMP"; then
        MUTATION_STARTED=1
    fi
    if ! grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=root_complete' "$SYSTEM_UPGRADE_OUTPUT_TMP"; then
        echo "The authorized system installation helper did not report completion." >&2
        exit 1
    fi
    MUTATION_STARTED=1
fi

# 4. CREATE VIRTUAL ENVIRONMENT
if [ "$UPGRADE_MODE" -ne 1 ] || [ "$SUDO_CMD" != "pkexec" ]; then
    perform_privileged_runtime_installation
fi

# The desktop file on the user's desktop belongs to the installing user.
if [ "$UPGRADE_MODE" -eq 0 ]; then
    cp "$DESKTOP_ENTRY_SOURCE" "$USER_DESKTOP_ENTRY_PATH" 2>/dev/null || true
    rm -f "$LEGACY_USER_DESKTOP_ENTRY_PATH"
elif [ -f "$USER_DESKTOP_ENTRY_PATH" ] || [ -f "$LEGACY_USER_DESKTOP_ENTRY_PATH" ]; then
    cp "$DESKTOP_ENTRY_SOURCE" "$USER_DESKTOP_ENTRY_PATH"
    rm -f "$LEGACY_USER_DESKTOP_ENTRY_PATH"
fi
echo "Done."

if [ "$UPGRADE_MODE" -ne 1 ] || [ "$SUDO_CMD" != "pkexec" ]; then
    perform_privileged_services_installation
fi

# 8. INSTALL USER SERVICES
echo "Installing background update check user timer..."
mkdir -p "$USER_SYSTEMD_DIR"
install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_update_check.service" "$USER_UPDATE_CHECK_SERVICE_PATH"
install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_update_check.timer" "$USER_UPDATE_CHECK_TIMER_PATH"
mkdir -p "$USER_UPDATE_CHECK_OVERRIDE_DIR"
install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${USER_UPDATE_CHECK_OVERRIDE_DIR}/config.conf"
echo "Done."

echo "Installing screen monitor user service..."
install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_screen.service" "$USER_SCREEN_SERVICE_PATH"
mkdir -p "$USER_SCREEN_OVERRIDE_DIR"
install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${USER_SCREEN_OVERRIDE_DIR}/config.conf"
if [ "$SKIP_SYSTEMD_ACTIONS" != "1" ]; then
    systemctl --user daemon-reload
fi

if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
    echo "Skipping user service enable/start because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
elif [ "$FRESH_SETUP_MODE" -eq 1 ]; then
    echo "User services installed; activation is deferred until the first TV is paired."
else
    systemctl --user enable LG_Buddy_screen.service
    systemctl --user restart LG_Buddy_screen.service
    if [ "$SCREEN_IDLE_BLANK" = "disabled" ]; then
        echo "LG_Buddy_screen.service enabled and started for session notifications; idle blanking is disabled by config."
    else
        echo "LG_Buddy_screen.service enabled and started for session notifications."
        echo "It will retry idle blanking until a compatible screen backend is available."
    fi

    if [ "$UPDATE_AUTO_CHECK" = "enabled" ]; then
        systemctl --user enable LG_Buddy_update_check.timer
        if systemctl --user is-active --quiet graphical-session.target; then
            systemctl --user start LG_Buddy_update_check.timer
            echo "LG_Buddy_update_check.timer enabled and started."
        else
            echo "LG_Buddy_update_check.timer enabled; it will start with the graphical session."
        fi
    else
        systemctl --user disable --now LG_Buddy_update_check.timer 2>/dev/null || true
        echo "LG_Buddy_update_check.timer installed but disabled by config."
    fi
fi

if [ "$FRESH_SETUP_MODE" -eq 1 ]; then
    echo "System sleep/wake integration installed; activation is deferred until the first TV is paired."
elif [ "$SYSTEM_SLEEP_WAKE_POLICY" = "enabled" ]; then
    echo "System sleep/wake TV control enabled via LG_Buddy_lifecycle.service and NetworkManager pre-down gate."
else
    echo "System sleep/wake TV control disabled by config. Lifecycle integration is installed and will no-op until re-enabled."
fi

INSTALLED_GUI_VERSION_OUTPUT="$("$GUI_INSTALL_PATH" --version)"
if ! cmp -s "$GUI_BINARY" "$GUI_INSTALL_PATH" || [ "$INSTALLED_GUI_VERSION_OUTPUT" != "$CANDIDATE_VERSION_OUTPUT" ]; then
    echo "Installed GUI identity does not match the verified candidate." >&2
    if [ "$UPGRADE_MODE" -eq 1 ]; then
        echo "Rerun this verified bundle with --upgrade to repair the partial installation." >&2
    fi
    exit 1
fi

if [ "$UPGRADE_MODE" -eq 1 ]; then
    INSTALLED_VERSION_OUTPUT="$("$RUNTIME_INSTALL_PATH" --version)"
    if ! cmp -s "$RUNTIME_BINARY" "$RUNTIME_INSTALL_PATH" || [ "$INSTALLED_VERSION_OUTPUT" != "$CANDIDATE_VERSION_OUTPUT" ]; then
        echo "Installed binary identity does not match the verified candidate." >&2
        echo "Rerun this verified bundle with --upgrade to repair the partial installation." >&2
        exit 1
    fi
    UPGRADE_COMPLETED=1
    echo "Upgrade complete!"
    echo "$INSTALLED_VERSION_OUTPUT"
    if [ "$SUDO_CMD" = "pkexec" ]; then
        system_upgrade_status complete
    fi
else
    if [ "$FRESH_SETUP_MODE" -eq 1 ]; then
        require_first_run_pkexec
        echo "Installation complete!"
        echo "Opening LG Buddy to pair your first TV..."
        LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_INSTALL_PATH"
    else
        echo "Installation complete!"
        echo "The user-session service has been installed."
        echo "Please restart your computer for all changes to take full effect."
        echo "NOTE: On first use, you may need to accept a prompt on your TV to allow this application to connect."
    fi
fi
