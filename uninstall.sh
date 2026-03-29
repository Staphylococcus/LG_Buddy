#!/bin/bash

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

if [ -r "$SCRIPT_DIR/bin/LG_Buddy_Common" ]; then
    . "$SCRIPT_DIR/bin/LG_Buddy_Common"
elif [ -r "/usr/lib/lg-buddy/common.sh" ]; then
    . "/usr/lib/lg-buddy/common.sh"
fi

if declare -F lg_buddy_effective_config_path >/dev/null 2>&1; then
    CONFIG_FILE="$(lg_buddy_effective_config_path 2>/dev/null || true)"
fi

CONFIG_FILE="${CONFIG_FILE:-${LG_BUDDY_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/lg-buddy/config.env}}"
CONFIG_DIR="$(dirname "$CONFIG_FILE")"

echo "Disabling & removing services..."
echo "(This might turn off your TV)"
sleep 3
sudo systemctl disable LG_Buddy.service
sudo systemctl disable LG_Buddy_wake.service
sudo systemctl disable LG_Buddy_sleep.service
systemctl --user disable LG_Buddy_screen.service
sudo systemctl stop LG_Buddy.service
sudo systemctl stop LG_Buddy_wake.service
sudo systemctl stop LG_Buddy_sleep.service
systemctl --user stop LG_Buddy_screen.service
sudo rm -f /etc/systemd/system/LG_Buddy.service
sudo rm -f /etc/systemd/system/LG_Buddy_wake.service
sudo rm -f /etc/systemd/system/LG_Buddy_sleep.service
sudo rm -f /etc/systemd/system/LG_Buddy.service.d/config.conf
sudo rm -f /etc/systemd/system/LG_Buddy_wake.service.d/config.conf
sudo rm -f /etc/systemd/system/LG_Buddy_sleep.service.d/config.conf
sudo rmdir /etc/systemd/system/LG_Buddy.service.d 2>/dev/null || true
sudo rmdir /etc/systemd/system/LG_Buddy_wake.service.d 2>/dev/null || true
sudo rmdir /etc/systemd/system/LG_Buddy_sleep.service.d 2>/dev/null || true
rm -f ~/.config/systemd/user/LG_Buddy_screen.service
rm -rf ~/.config/systemd/user/LG_Buddy_screen.service.d
sudo systemctl daemon-reload
systemctl --user daemon-reload
echo "Done."

echo "Removing scripts"
sudo rm -f /usr/bin/LG_Buddy_Startup
sudo rm -f /usr/bin/LG_Buddy_Shutdown
sudo rm -f /usr/bin/LG_Buddy_Screen_On
sudo rm -f /usr/bin/LG_Buddy_Screen_Off
sudo rm -f /usr/bin/LG_Buddy_Screen_Monitor
sudo rm -f /usr/bin/LG_Buddy_sleep_pre
sudo rm -f /usr/bin/LG_Buddy_Brightness
sudo rm -f /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
sudo rm -f /usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook
sudo rm -f /etc/tmpfiles.d/lg_buddy.conf
sudo rm -f /usr/lib/lg-buddy/common.sh
sudo rm -f /usr/lib/lg-buddy/config-path
sudo rmdir /usr/lib/lg-buddy 2>/dev/null || true
sudo rm -rf /run/lg_buddy

echo "Removing desktop entries"
sudo rm -f /usr/share/applications/LG_Buddy_Brightness.desktop
rm -f ~/Desktop/LG_Buddy_Brightness.desktop

echo "Removing python virtual environment"
sudo rm -rf /usr/bin/LG_Buddy_PIP

if [ -f "$CONFIG_FILE" ]; then
    read -p "Remove user configuration at $CONFIG_FILE? [y/N] " REMOVE_CONFIG
    case "$REMOVE_CONFIG" in
        [Yy]*)
            rm -f "$CONFIG_FILE"
            rmdir "$CONFIG_DIR" 2>/dev/null || true
            echo "Removed user configuration."
            ;;
        *)
            echo "Keeping user configuration at $CONFIG_FILE"
            ;;
    esac
fi

echo "Done."
