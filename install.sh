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
SCREEN_MONITOR_AVAILABLE=0
SCREEN_MONITOR_CURRENT_BACKEND=""

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

check_dep "python3-venv"    "python3-venv"  "python3 -c 'import venv'"
check_dep "python3-pip"     "python3-pip"   "python3 -m pip --version"
check_dep "wakeonlan / wol" "wakeonlan"     "command -v wakeonlan || command -v wol"
check_dep "zenity"          "zenity"        "command -v zenity"

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

echo ""
echo "Checking screen idle/resume backends..."
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

SCREEN_MONITOR_CURRENT_BACKEND="$(bash ./bin/LG_Buddy_Screen_Monitor --detect-backend 2>/dev/null || true)"
if [ -n "$SCREEN_MONITOR_CURRENT_BACKEND" ]; then
    echo "  [OK]      current session backend: $SCREEN_MONITOR_CURRENT_BACKEND"
else
    echo "  [INFO]    no supported backend detected in the current session"
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
echo "Using configuration file at $CONFIG_FILE"
echo "Configuration complete."

# 3. CREATE VIRTUAL ENVIRONMENT
echo "Creating Python virtual environment at /usr/bin/LG_Buddy_PIP..."
sudo python3 -m venv /usr/bin/LG_Buddy_PIP
echo "Done."

# 4. INSTALL BSCPYLGTV
echo "Installing bscpylgtv into the virtual environment..."
sudo /usr/bin/LG_Buddy_PIP/bin/pip install bscpylgtv
echo "Done."

# 5. COPY SCRIPTS AND MAKE EXECUTABLE
echo "Copying scripts to system directories and making executable..."
sudo install -d /usr/lib/lg-buddy
sudo install -m 755 ./bin/LG_Buddy_Common /usr/lib/lg-buddy/common.sh
printf '%s\n' "$CONFIG_FILE" | sudo tee /usr/lib/lg-buddy/config-path >/dev/null
sudo chmod 644 /usr/lib/lg-buddy/config-path

sudo cp ./bin/LG_Buddy_Startup /usr/bin/
sudo cp ./bin/LG_Buddy_Shutdown /usr/bin/
sudo cp ./bin/LG_Buddy_Screen_On /usr/bin/
sudo cp ./bin/LG_Buddy_Screen_Off /usr/bin/
sudo cp ./bin/LG_Buddy_Screen_Monitor /usr/bin/
sudo cp ./bin/LG_Buddy_sleep_pre /usr/bin/
sudo cp ./bin/LG_Buddy_Brightness /usr/bin/
sudo mkdir -p /etc/NetworkManager/dispatcher.d/pre-down.d
sudo cp ./bin/LG_Buddy_sleep /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
sudo chmod +x /usr/bin/LG_Buddy_Startup
sudo chmod +x /usr/bin/LG_Buddy_Shutdown
sudo chmod +x /usr/bin/LG_Buddy_Screen_On
sudo chmod +x /usr/bin/LG_Buddy_Screen_Off
sudo chmod +x /usr/bin/LG_Buddy_Screen_Monitor
sudo chmod +x /usr/bin/LG_Buddy_sleep_pre
sudo chmod +x /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
sudo chmod +x /usr/bin/LG_Buddy_Brightness

sudo mkdir -p /run/lg_buddy
sudo chmod 777 /run/lg_buddy

sudo rm -f /usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook

echo "Installing brightness control desktop entry..."
sudo mkdir -p /usr/share/applications
sudo cp ./LG_Buddy_Brightness.desktop /usr/share/applications/
cp ./LG_Buddy_Brightness.desktop ~/Desktop/ 2>/dev/null || true
echo "Done."

# 6. SETUP SYSTEMD SERVICES
echo "Copying and enabling systemd services..."
sudo cp ./systemd/LG_Buddy.service /etc/systemd/system/
sudo cp ./systemd/LG_Buddy_wake.service /etc/systemd/system/
sudo cp ./systemd/LG_Buddy_sleep.service /etc/systemd/system/
sudo cp ./systemd/lg_buddy.conf /etc/tmpfiles.d/

sudo systemctl daemon-reload
sudo systemctl enable LG_Buddy.service
sudo systemctl enable LG_Buddy_wake.service
sudo systemctl enable LG_Buddy_sleep.service
echo "Done."

# 7. INSTALL SCREEN MONITOR USER SERVICE
echo "Installing screen monitor user service..."
mkdir -p ~/.config/systemd/user/
cp ./systemd/LG_Buddy_screen.service ~/.config/systemd/user/
systemctl --user daemon-reload

if [ "$SCREEN_MONITOR_AVAILABLE" -eq 1 ]; then
    read -p "Enable the screen idle/resume monitor now? [Y/n] " ENABLE_SCREEN_MONITOR
    case "$ENABLE_SCREEN_MONITOR" in
        [Nn]*)
            echo "Leaving LG_Buddy_screen.service installed but disabled."
            ;;
        *)
            systemctl --user enable LG_Buddy_screen.service
            if [ -n "$SCREEN_MONITOR_CURRENT_BACKEND" ]; then
                systemctl --user restart LG_Buddy_screen.service
                echo "LG_Buddy_screen.service enabled and started using the $SCREEN_MONITOR_CURRENT_BACKEND backend."
            else
                echo "LG_Buddy_screen.service enabled."
                echo "It will start automatically the next time a supported graphical session is available."
            fi
            ;;
    esac
else
    echo "No supported screen idle backend detected."
    echo "Install gdbus for GNOME or swayidle for wlroots/COSMIC, then enable LG_Buddy_screen.service later."
fi

# 8. ASK TO DISABLE SUSPEND/RESUME FUNCTIONALITY
echo "Do you want to disable automatic TV power on/off during system sleep/wake? (y/N) "
read -r REPLY
case "$REPLY" in
    [Yy]*)
        echo "Disabling sleep/wake TV control..."
        sudo systemctl disable LG_Buddy_wake.service
        sudo systemctl disable LG_Buddy_sleep.service
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
