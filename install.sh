#!/bin/bash

# Exit on any error
set -e

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
INSTALL_ROOT="${LG_BUDDY_INSTALL_ROOT:-}"
INSTALL_ROOT="${INSTALL_ROOT%/}"
SUDO_CMD="${LG_BUDDY_SUDO_CMD:-sudo}"
NONINTERACTIVE="${LG_BUDDY_NONINTERACTIVE:-0}"
SKIP_SYSTEMD_ACTIONS="${LG_BUDDY_SKIP_SYSTEMD_ACTIONS:-0}"
SKIP_PIP_INSTALL="${LG_BUDDY_SKIP_PIP_INSTALL:-0}"
DEFAULT_RUNTIME_BINARY="$SCRIPT_DIR/lg-buddy"
RUNTIME_BINARY="$DEFAULT_RUNTIME_BINARY"

usage() {
    cat <<EOF
Usage: $0 [--runtime-binary /path/to/lg-buddy]

Install LG Buddy from an existing runtime binary.

Defaults:
  --runtime-binary defaults to ./lg-buddy next to install.sh
EOF
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --runtime-binary)
            RUNTIME_BINARY="${2:-}"
            [ -n "$RUNTIME_BINARY" ] || usage
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            usage
            ;;
    esac
done

if [ "$(id -u)" -eq 0 ]; then
    echo "Error: Do not run this script with sudo. It will prompt for sudo when needed."
    exit 1
fi

echo "Starting LG Buddy Installation"
if [ -n "$INSTALL_ROOT" ]; then
    echo "Install root override: $INSTALL_ROOT"
fi

# 1. CHECK PREREQUISITES
echo ""
echo "Checking prerequisites..."

MISSING_PKGS=()
SCREEN_MONITOR_AVAILABLE=0
SCREEN_MONITOR_CONFIGURED_BACKEND="auto"
SCREEN_MONITOR_RUNTIME_BACKEND=""
SYSTEM_CONFIG_OVERRIDE_TMP=""
CONFIG_POINTER_TMP=""
NM_SLEEP_HOOK_TMP=""
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
    else
        "$SUDO_CMD" "$@"
    fi
}

SYSTEM_BIN_DIR="$(prefix_path "/usr/bin")"
RUNTIME_INSTALL_PATH="${SYSTEM_BIN_DIR}/lg-buddy"
VENV_DIR="${SYSTEM_BIN_DIR}/LG_Buddy_PIP"
SYSTEM_LIB_DIR="$(prefix_path "/usr/lib/lg-buddy")"
CONFIG_POINTER_PATH="${SYSTEM_LIB_DIR}/config-path"
COMMON_HELPER_PATH="${SYSTEM_LIB_DIR}/common.sh"
SYSTEM_SLEEP_HOOK_PATH="$(prefix_path "/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook")"
SYSTEMD_SYSTEM_DIR="$(prefix_path "/etc/systemd/system")"
SYSTEMD_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy.service"
SYSTEMD_WAKE_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_wake.service"
SYSTEMD_SLEEP_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_sleep.service"
SYSTEMD_SERVICE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy.service.d"
SYSTEMD_WAKE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_wake.service.d"
SYSTEMD_SLEEP_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_sleep.service.d"
TMPFILES_CONF_DIR="$(prefix_path "/etc/tmpfiles.d")"
TMPFILES_CONF_PATH="${TMPFILES_CONF_DIR}/lg_buddy.conf"
NM_PRE_DOWN_DIR="$(prefix_path "/etc/NetworkManager/dispatcher.d/pre-down.d")"
NM_SLEEP_HOOK_PATH="${NM_PRE_DOWN_DIR}/LG_Buddy_sleep"
APPLICATIONS_DIR="$(prefix_path "/usr/share/applications")"
DESKTOP_ENTRY_PATH="${APPLICATIONS_DIR}/LG_Buddy_Brightness.desktop"
USER_SYSTEMD_DIR="${HOME}/.config/systemd/user"
USER_SCREEN_SERVICE_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service"
USER_SCREEN_OVERRIDE_DIR="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service.d"

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

    if python3 -m venv "$tmp_venv_dir" >/dev/null 2>&1; then
        rm -rf "$tmp_venv_dir"
        return 0
    fi

    rm -rf "$tmp_venv_dir"
    return 1
}

check_dep "python3-venv" "python3-venv" "check_python3_venv"
check_dep "python3-pip" "python3-pip" "/usr/bin/python3 -m pip --version"
check_dep "zenity" "zenity" "command -v zenity"

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

write_nm_sleep_hook() {
    local hook_file="$1"
    local config_path="$2"
    local quoted_config_path=""

    quoted_config_path="$(printf '%q' "$config_path")"

    cat >"$hook_file" <<EOF
#!/bin/bash
export LG_BUDDY_CONFIG=$quoted_config_path
exec /usr/bin/lg-buddy sleep
EOF
}

write_config_pointer() {
    local pointer_file="$1"
    local config_path="$2"

    printf '%s\n' "$config_path" >"$pointer_file"
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

cleanup() {
    if [ -n "$SYSTEM_CONFIG_OVERRIDE_TMP" ]; then
        rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
    fi

    if [ -n "$CONFIG_POINTER_TMP" ]; then
        rm -f "$CONFIG_POINTER_TMP"
    fi

    if [ -n "$NM_SLEEP_HOOK_TMP" ]; then
        rm -f "$NM_SLEEP_HOOK_TMP"
    fi
}

trap cleanup EXIT

if [ ${#MISSING_PKGS[@]} -gt 0 ]; then
    echo ""
    echo "Missing: ${MISSING_PKGS[*]}"

    # Detect package manager
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
    fi

    if [ -n "$PM" ]; then
        AUTO_INSTALL="${LG_BUDDY_AUTO_INSTALL_DEPS:-}"
        if [ -z "$AUTO_INSTALL" ] && [ "$NONINTERACTIVE" != "1" ]; then
            read -p "Install missing packages with $PM now? (y/N) " AUTO_INSTALL
        fi
        case "$AUTO_INSTALL" in
            [Yy]*)
                run_privileged "${INSTALL_CMD[@]}" "${MISSING_PKGS[@]}"
                ;;
            *)
                echo "Please install the missing packages manually and re-run install.sh."
                exit 1
                ;;
        esac
    else
        echo "Could not detect a supported package manager (apt/dnf/pacman)."
        echo "Please install the missing packages manually and re-run install.sh."
        exit 1
    fi
else
    echo "All prerequisites satisfied."
fi

# 2. RESOLVE RUST RUNTIME
resolve_runtime_binary

# 3. CONFIGURE SCRIPTS
echo ""
echo "Running configuration script..."
# Make sure configure.sh is executable
if [ ! -x "$SCRIPT_DIR/configure.sh" ]; then
    chmod +x "$SCRIPT_DIR/configure.sh"
fi
"$SCRIPT_DIR/configure.sh"
CONFIG_FILE="$(bash "$SCRIPT_DIR/bin/LG_Buddy_Common" --user-config-path)"
SCREEN_MONITOR_CONFIGURED_BACKEND="$(sed -n 's/^screen_backend=//p' "$CONFIG_FILE" | tail -n1)"
SCREEN_MONITOR_CONFIGURED_BACKEND="${SCREEN_MONITOR_CONFIGURED_BACKEND:-auto}"
echo "Using configuration file at $CONFIG_FILE"
echo "Configuration complete."

echo ""
echo "Checking screen idle/resume backend for configured mode ($SCREEN_MONITOR_CONFIGURED_BACKEND)..."
case "$SCREEN_MONITOR_CONFIGURED_BACKEND" in
    gnome)
        SCREEN_MONITOR_AVAILABLE=1
        SCREEN_MONITOR_RUNTIME_BACKEND="$(LG_BUDDY_SCREEN_BACKEND=gnome "$RUNTIME_BINARY" detect-backend 2>/dev/null || true)"
        if [ "$SCREEN_MONITOR_RUNTIME_BACKEND" = "gnome" ]; then
            echo "  [OK]      current session satisfies the GNOME backend contract"
        else
            SCREEN_MONITOR_RUNTIME_BACKEND=""
            echo "  [INFO]    current session did not verify the full GNOME backend contract"
            echo "            GNOME requires GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor."
            echo "            The user service will start automatically in a compatible GNOME session."
        fi
        ;;
    swayidle)
        if command -v swayidle &>/dev/null; then
            echo "  [OK]      swayidle (configured backend)"
            SCREEN_MONITOR_AVAILABLE=1
            SCREEN_MONITOR_RUNTIME_BACKEND="swayidle"
        else
            echo "  [MISSING] swayidle (required for the configured backend)"
        fi
        ;;
    *)
        if command -v swayidle &>/dev/null; then
            echo "  [OK]      swayidle (wlroots/COSMIC backend)"
            SCREEN_MONITOR_AVAILABLE=1
        else
            echo "  [OPTIONAL] swayidle (required for wlroots/COSMIC backend)"
        fi

        SCREEN_MONITOR_RUNTIME_BACKEND="$("$RUNTIME_BINARY" detect-backend 2>/dev/null || true)"
        if [ -n "$SCREEN_MONITOR_RUNTIME_BACKEND" ]; then
            SCREEN_MONITOR_AVAILABLE=1
            echo "  [OK]      current session backend: $SCREEN_MONITOR_RUNTIME_BACKEND"
        else
            echo "  [INFO]    no supported backend detected in the current session"
        fi
        ;;
esac

# 4. CREATE VIRTUAL ENVIRONMENT
echo "Creating Python virtual environment at $VENV_DIR..."
run_privileged python3 -m venv "$VENV_DIR"
echo "Done."

# 5. INSTALL BSCPYLGTV
if [ "$SKIP_PIP_INSTALL" = "1" ]; then
    echo "Skipping bscpylgtv installation because LG_BUDDY_SKIP_PIP_INSTALL=1."
else
    echo "Installing bscpylgtv into the virtual environment..."
    run_privileged "$VENV_DIR/bin/pip" install bscpylgtv
    echo "Done."
fi

# 6. INSTALL RUST RUNTIME AND SUPPORT FILES
echo "Installing Rust runtime and support files..."
run_privileged install -m 755 "$RUNTIME_BINARY" "$RUNTIME_INSTALL_PATH"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Startup"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Shutdown"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_On"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Off"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Monitor"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_sleep_pre"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Brightness"
run_privileged rm -f "$COMMON_HELPER_PATH"
run_privileged rm -f "$CONFIG_POINTER_PATH"
run_privileged rmdir "$SYSTEM_LIB_DIR" 2>/dev/null || true
run_privileged rm -f "$SYSTEM_SLEEP_HOOK_PATH"
run_privileged install -d "$SYSTEM_LIB_DIR"
CONFIG_POINTER_TMP="$(mktemp)"
write_config_pointer "$CONFIG_POINTER_TMP" "$CONFIG_FILE"
run_privileged install -m 644 "$CONFIG_POINTER_TMP" "$CONFIG_POINTER_PATH"
rm -f "$CONFIG_POINTER_TMP"
CONFIG_POINTER_TMP=""
run_privileged mkdir -p "$NM_PRE_DOWN_DIR"
echo "Installing brightness control desktop entry..."
run_privileged mkdir -p "$APPLICATIONS_DIR"
run_privileged cp "$SCRIPT_DIR/LG_Buddy_Brightness.desktop" "$DESKTOP_ENTRY_PATH"
cp "$SCRIPT_DIR/LG_Buddy_Brightness.desktop" ~/Desktop/ 2>/dev/null || true
echo "Done."

# 7. SETUP SYSTEMD SERVICES
echo "Copying and enabling systemd services..."
run_privileged install -d "$SYSTEMD_SYSTEM_DIR"
run_privileged install -d "$TMPFILES_CONF_DIR"
run_privileged cp "$SCRIPT_DIR/systemd/LG_Buddy.service" "$SYSTEMD_SERVICE_PATH"
run_privileged cp "$SCRIPT_DIR/systemd/LG_Buddy_wake.service" "$SYSTEMD_WAKE_SERVICE_PATH"
run_privileged cp "$SCRIPT_DIR/systemd/LG_Buddy_sleep.service" "$SYSTEMD_SLEEP_SERVICE_PATH"
run_privileged cp "$SCRIPT_DIR/systemd/lg_buddy.conf" "$TMPFILES_CONF_PATH"
run_privileged install -d "$SYSTEMD_SERVICE_OVERRIDE_DIR"
run_privileged install -d "$SYSTEMD_WAKE_OVERRIDE_DIR"
run_privileged install -d "$SYSTEMD_SLEEP_OVERRIDE_DIR"
SYSTEM_CONFIG_OVERRIDE_TMP="$(mktemp)"
write_config_override "$SYSTEM_CONFIG_OVERRIDE_TMP" "$CONFIG_FILE"
run_privileged install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_SERVICE_OVERRIDE_DIR}/config.conf"
run_privileged install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_WAKE_OVERRIDE_DIR}/config.conf"
run_privileged install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_SLEEP_OVERRIDE_DIR}/config.conf"
rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
SYSTEM_CONFIG_OVERRIDE_TMP=""

if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
    echo "Skipping systemd tmpfiles and enable actions because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
else
    run_privileged systemd-tmpfiles --create "$TMPFILES_CONF_PATH"
    run_privileged systemctl daemon-reload
    run_privileged systemctl enable LG_Buddy.service
    run_privileged systemctl enable LG_Buddy_wake.service
    run_privileged systemctl enable LG_Buddy_sleep.service
fi
echo "Done."

# 8. INSTALL SCREEN MONITOR USER SERVICE
echo "Installing screen monitor user service..."
mkdir -p "$USER_SYSTEMD_DIR"
cp "$SCRIPT_DIR/systemd/LG_Buddy_screen.service" "$USER_SCREEN_SERVICE_PATH"
mkdir -p "$USER_SCREEN_OVERRIDE_DIR"
write_config_override "${USER_SCREEN_OVERRIDE_DIR}/config.conf" "$CONFIG_FILE"
if [ "$SKIP_SYSTEMD_ACTIONS" != "1" ]; then
    systemctl --user daemon-reload
fi

if [ "$SCREEN_MONITOR_AVAILABLE" -eq 1 ]; then
    ENABLE_SCREEN_MONITOR="${LG_BUDDY_ENABLE_SCREEN_MONITOR:-}"
    if [ -z "$ENABLE_SCREEN_MONITOR" ] && [ "$NONINTERACTIVE" != "1" ]; then
        read -p "Enable the screen idle/resume monitor now? [Y/n] " ENABLE_SCREEN_MONITOR
    fi
    case "$ENABLE_SCREEN_MONITOR" in
        [Nn]*|0|false|FALSE|False|no|NO|No)
            echo "Leaving LG_Buddy_screen.service installed but disabled."
            ;;
        *)
            if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
                echo "Skipping user service enable/restart because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
            else
                systemctl --user enable LG_Buddy_screen.service
                if [ -n "$SCREEN_MONITOR_RUNTIME_BACKEND" ]; then
                    systemctl --user restart LG_Buddy_screen.service
                    echo "LG_Buddy_screen.service enabled and started using the $SCREEN_MONITOR_RUNTIME_BACKEND backend."
                else
                    echo "LG_Buddy_screen.service enabled."
                    echo "It will start automatically the next time a supported graphical session is available."
                fi
            fi
            ;;
    esac
else
    echo "No supported screen idle backend detected for the configured mode ($SCREEN_MONITOR_CONFIGURED_BACKEND)."
    case "$SCREEN_MONITOR_CONFIGURED_BACKEND" in
        gnome)
            echo "Use a GNOME session with GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor."
            echo "Then enable LG_Buddy_screen.service later."
            ;;
        swayidle)
            echo "Install swayidle, then enable LG_Buddy_screen.service later."
            ;;
        *)
            echo "Use a compatible GNOME session or install swayidle for wlroots/COSMIC, then enable LG_Buddy_screen.service later."
            ;;
    esac
fi

# 9. INSTALL NETWORKMANAGER SLEEP HOOK
NM_SLEEP_HOOK_TMP="$(mktemp)"
write_nm_sleep_hook "$NM_SLEEP_HOOK_TMP" "$CONFIG_FILE"
run_privileged install -m 755 "$NM_SLEEP_HOOK_TMP" "$NM_SLEEP_HOOK_PATH"
rm -f "$NM_SLEEP_HOOK_TMP"
NM_SLEEP_HOOK_TMP=""

# 10. ASK TO DISABLE SUSPEND/RESUME FUNCTIONALITY
REPLY="${LG_BUDDY_DISABLE_SLEEP_WAKE:-}"
if [ "$NONINTERACTIVE" != "1" ]; then
    echo "Do you want to disable automatic TV power on/off during system sleep/wake? (y/N) "
    read -r REPLY
fi
case "$REPLY" in
    [Yy]*|1|true|TRUE|True|yes|YES|Yes)
        echo "Disabling sleep/wake TV control..."
        if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
            echo "Skipping systemctl disable actions because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
        else
            run_privileged systemctl disable LG_Buddy_wake.service
            run_privileged systemctl disable LG_Buddy_sleep.service
        fi
        run_privileged rm -f "$NM_SLEEP_HOOK_PATH"
        echo "Sleep/wake TV control disabled. Startup/shutdown will still work."
        ;;
    *)
        echo "Leaving all services enabled (startup, shutdown, sleep, wake)."
        ;;
esac


echo "Installation complete!"
echo "The screen monitor service has been installed."
echo "Please restart your computer for all changes to take full effect."
echo "NOTE: On first use, you may need to accept a prompt on your TV to allow this application to connect."
