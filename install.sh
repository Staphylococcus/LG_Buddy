#!/bin/bash

# Exit on any error
set -e

if [ "$(id -u)" -eq 0 ]; then
    echo "Error: Do not run this script with sudo. It will prompt for sudo when needed."
    exit 1
fi

echo "Starting LG Buddy Installation"

# 1. CHECK PREREQUISITES
echo ""
echo "Checking prerequisites..."

MISSING_PKGS=()
MISSING_MANUAL_DEPS=()
SCREEN_MONITOR_AVAILABLE=0
SCREEN_MONITOR_CONFIGURED_BACKEND="auto"
SCREEN_MONITOR_RUNTIME_BACKEND=""
SYSTEM_CONFIG_OVERRIDE_TMP=""
CONFIG_POINTER_TMP=""
NM_SLEEP_HOOK_TMP=""

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

check_manual_dep() {
    local label="$1"
    local check_cmd="$2"
    local guidance="$3"

    if eval "$check_cmd" &>/dev/null; then
        echo "  [OK]      $label"
    else
        echo "  [MISSING] $label"
        MISSING_MANUAL_DEPS+=("$guidance")
    fi
}

check_dep "python3-venv" "python3-venv" "python3 -c 'import venv'"
check_dep "python3-pip" "python3-pip" "python3 -m pip --version"
check_dep "zenity" "zenity" "command -v zenity"
check_manual_dep "cargo" "command -v cargo" "Install a Rust toolchain so the 'cargo' command is available, then re-run install.sh."

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
        INSTALL_CMD="sudo apt install -y"
    elif command -v dnf &>/dev/null; then
        PM="dnf"
        INSTALL_CMD="sudo dnf install -y"
    elif command -v pacman &>/dev/null; then
        PM="pacman"
        INSTALL_CMD="sudo pacman -S --noconfirm"
    else
        PM=""
    fi

    if [ -n "$PM" ]; then
        read -p "Install missing packages with $PM now? (y/N) " AUTO_INSTALL
        case "$AUTO_INSTALL" in
            [Yy]*)
                $INSTALL_CMD "${MISSING_PKGS[@]}"
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

if [ ${#MISSING_MANUAL_DEPS[@]} -gt 0 ]; then
    echo ""
    printf '%s\n' "${MISSING_MANUAL_DEPS[@]}"
    exit 1
fi

# 2. CONFIGURE SCRIPTS
echo ""
echo "Running configuration script..."
# Make sure configure.sh is executable
if [ ! -x "./configure.sh" ]; then
    chmod +x ./configure.sh
fi
./configure.sh
CONFIG_FILE="$(bash ./bin/LG_Buddy_Common --user-config-path)"
SCREEN_MONITOR_CONFIGURED_BACKEND="$(sed -n 's/^screen_backend=//p' "$CONFIG_FILE" | tail -n1)"
SCREEN_MONITOR_CONFIGURED_BACKEND="${SCREEN_MONITOR_CONFIGURED_BACKEND:-auto}"
echo "Using configuration file at $CONFIG_FILE"
echo "Configuration complete."

# 3. BUILD RUST RUNTIME
echo ""
echo "Building Rust runtime..."
cargo build --release -p lg-buddy
echo "Done."

echo ""
echo "Checking screen idle/resume backend for configured mode ($SCREEN_MONITOR_CONFIGURED_BACKEND)..."
case "$SCREEN_MONITOR_CONFIGURED_BACKEND" in
    gnome)
        if command -v gdbus &>/dev/null; then
            echo "  [OK]      gdbus (GNOME backend)"
            SCREEN_MONITOR_AVAILABLE=1
            SCREEN_MONITOR_RUNTIME_BACKEND="gnome"
        else
            echo "  [MISSING] gdbus (required for GNOME backend)"
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
        if command -v gdbus &>/dev/null; then
            echo "  [OK]      gdbus (GNOME backend)"
            SCREEN_MONITOR_AVAILABLE=1
        else
            echo "  [OPTIONAL] gdbus (required for GNOME backend)"
        fi

        if command -v swayidle &>/dev/null; then
            echo "  [OK]      swayidle (wlroots/COSMIC backend)"
            SCREEN_MONITOR_AVAILABLE=1
        else
            echo "  [OPTIONAL] swayidle (required for wlroots/COSMIC backend)"
        fi

        SCREEN_MONITOR_RUNTIME_BACKEND="$(./target/release/lg-buddy detect-backend 2>/dev/null || true)"
        if [ -n "$SCREEN_MONITOR_RUNTIME_BACKEND" ]; then
            echo "  [OK]      current session backend: $SCREEN_MONITOR_RUNTIME_BACKEND"
        else
            echo "  [INFO]    no supported backend detected in the current session"
        fi
        ;;
esac

# 4. CREATE VIRTUAL ENVIRONMENT
echo "Creating Python virtual environment at /usr/bin/LG_Buddy_PIP..."
sudo python3 -m venv /usr/bin/LG_Buddy_PIP
echo "Done."

# 5. INSTALL BSCPYLGTV
echo "Installing bscpylgtv into the virtual environment..."
sudo /usr/bin/LG_Buddy_PIP/bin/pip install bscpylgtv
echo "Done."

# 6. INSTALL RUST RUNTIME AND SUPPORT FILES
echo "Installing Rust runtime and support files..."
sudo install -m 755 ./target/release/lg-buddy /usr/bin/lg-buddy
sudo rm -f /usr/bin/LG_Buddy_Startup
sudo rm -f /usr/bin/LG_Buddy_Shutdown
sudo rm -f /usr/bin/LG_Buddy_Screen_On
sudo rm -f /usr/bin/LG_Buddy_Screen_Off
sudo rm -f /usr/bin/LG_Buddy_Screen_Monitor
sudo rm -f /usr/bin/LG_Buddy_sleep_pre
sudo rm -f /usr/bin/LG_Buddy_Brightness
sudo rm -f /usr/lib/lg-buddy/common.sh
sudo rm -f /usr/lib/lg-buddy/config-path
sudo rmdir /usr/lib/lg-buddy 2>/dev/null || true
sudo rm -f /usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook
sudo install -d /usr/lib/lg-buddy
CONFIG_POINTER_TMP="$(mktemp)"
write_config_pointer "$CONFIG_POINTER_TMP" "$CONFIG_FILE"
sudo install -m 644 "$CONFIG_POINTER_TMP" /usr/lib/lg-buddy/config-path
rm -f "$CONFIG_POINTER_TMP"
CONFIG_POINTER_TMP=""
sudo mkdir -p /etc/NetworkManager/dispatcher.d/pre-down.d
echo "Installing brightness control desktop entry..."
sudo mkdir -p /usr/share/applications
sudo cp ./LG_Buddy_Brightness.desktop /usr/share/applications/
cp ./LG_Buddy_Brightness.desktop ~/Desktop/ 2>/dev/null || true
echo "Done."

# 7. SETUP SYSTEMD SERVICES
echo "Copying and enabling systemd services..."
sudo cp ./systemd/LG_Buddy.service /etc/systemd/system/
sudo cp ./systemd/LG_Buddy_wake.service /etc/systemd/system/
sudo cp ./systemd/LG_Buddy_sleep.service /etc/systemd/system/
sudo cp ./systemd/lg_buddy.conf /etc/tmpfiles.d/
sudo install -d /etc/systemd/system/LG_Buddy.service.d
sudo install -d /etc/systemd/system/LG_Buddy_wake.service.d
sudo install -d /etc/systemd/system/LG_Buddy_sleep.service.d
SYSTEM_CONFIG_OVERRIDE_TMP="$(mktemp)"
write_config_override "$SYSTEM_CONFIG_OVERRIDE_TMP" "$CONFIG_FILE"
sudo install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" /etc/systemd/system/LG_Buddy.service.d/config.conf
sudo install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" /etc/systemd/system/LG_Buddy_wake.service.d/config.conf
sudo install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" /etc/systemd/system/LG_Buddy_sleep.service.d/config.conf
rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
SYSTEM_CONFIG_OVERRIDE_TMP=""

sudo systemctl daemon-reload
sudo systemctl enable LG_Buddy.service
sudo systemctl enable LG_Buddy_wake.service
sudo systemctl enable LG_Buddy_sleep.service
echo "Done."

# 8. INSTALL SCREEN MONITOR USER SERVICE
echo "Installing screen monitor user service..."
mkdir -p ~/.config/systemd/user/
cp ./systemd/LG_Buddy_screen.service ~/.config/systemd/user/
mkdir -p ~/.config/systemd/user/LG_Buddy_screen.service.d
write_config_override ~/.config/systemd/user/LG_Buddy_screen.service.d/config.conf "$CONFIG_FILE"
systemctl --user daemon-reload

if [ "$SCREEN_MONITOR_AVAILABLE" -eq 1 ]; then
    read -p "Enable the screen idle/resume monitor now? [Y/n] " ENABLE_SCREEN_MONITOR
    case "$ENABLE_SCREEN_MONITOR" in
        [Nn]*)
            echo "Leaving LG_Buddy_screen.service installed but disabled."
            ;;
        *)
            systemctl --user enable LG_Buddy_screen.service
            if [ -n "$SCREEN_MONITOR_RUNTIME_BACKEND" ]; then
                systemctl --user restart LG_Buddy_screen.service
                echo "LG_Buddy_screen.service enabled and started using the $SCREEN_MONITOR_RUNTIME_BACKEND backend."
            else
                echo "LG_Buddy_screen.service enabled."
                echo "It will start automatically the next time a supported graphical session is available."
            fi
            ;;
    esac
else
    echo "No supported screen idle backend detected for the configured mode ($SCREEN_MONITOR_CONFIGURED_BACKEND)."
    case "$SCREEN_MONITOR_CONFIGURED_BACKEND" in
        gnome)
            echo "Install gdbus, then enable LG_Buddy_screen.service later."
            ;;
        swayidle)
            echo "Install swayidle, then enable LG_Buddy_screen.service later."
            ;;
        *)
            echo "Install gdbus for GNOME or swayidle for wlroots/COSMIC, then enable LG_Buddy_screen.service later."
            ;;
    esac
fi

# 9. INSTALL NETWORKMANAGER SLEEP HOOK
NM_SLEEP_HOOK_TMP="$(mktemp)"
write_nm_sleep_hook "$NM_SLEEP_HOOK_TMP" "$CONFIG_FILE"
sudo install -m 755 "$NM_SLEEP_HOOK_TMP" /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
rm -f "$NM_SLEEP_HOOK_TMP"
NM_SLEEP_HOOK_TMP=""

# 10. ASK TO DISABLE SUSPEND/RESUME FUNCTIONALITY
echo "Do you want to disable automatic TV power on/off during system sleep/wake? (y/N) "
read -r REPLY
case "$REPLY" in
    [Yy]*)
        echo "Disabling sleep/wake TV control..."
        sudo systemctl disable LG_Buddy_wake.service
        sudo systemctl disable LG_Buddy_sleep.service
        sudo rm -f /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
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
