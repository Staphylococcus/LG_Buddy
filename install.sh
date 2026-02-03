#!/bin/bash

# --- Color Definitions ---
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}=======================================${NC}"
echo -e "${BLUE}     LG Buddy Installation Wizard      ${NC}"
echo -e "${BLUE}=======================================${NC}"

# --- 1. Dependency Check ---
echo -e "\n${GREEN}[1/5] Checking Dependencies...${NC}"

check_cmd() {
    if ! command -v "$1" &> /dev/null; then
        echo -e "${RED}Error: $1 could not be found.${NC} Please install it first."
        exit 1
    fi
}

check_cmd "python3"
check_cmd "wakeonlan"
# Identify bscpylgtvcommand path (check uv, pip, local, global)
# Priority: Custom env var > Local uv > Local bin > Global bin
if [ -z "$BSCPY_CMD" ]; then
    if [ -f "$HOME/.local/bin/bscpylgtvcommand" ]; then
        BSCPY_CMD="$HOME/.local/bin/bscpylgtvcommand"
    elif command -v bscpylgtvcommand &> /dev/null; then
        BSCPY_CMD=$(command -v bscpylgtvcommand)
    else
        echo -e "${RED}Error: bscpylgtvcommand not found!${NC}"
        echo "Please install it using: uv tool install bscpylgtv"
        echo "Or adjust your PATH."
        exit 1
    fi
fi
echo "Found tool at: $BSCPY_CMD"


# --- 2. Auto-Discovery (IP & MAC) ---
echo -e "\n${GREEN}[2/5] Discovering TV...${NC}"

# Try to find current IP
CURRENT_IP=$(grep "^tv_ip=" ./bin/LG_Buddy_Startup | cut -d'"' -f2)
if [ "$CURRENT_IP" == "192.168.X.X" ]; then CURRENT_IP=""; fi

# Discovery attempt via ARP (looking for LG MAC OUI usually starts with various, unreliable to hardcode all)
# Instead, we prompt user but offer discovered neighbors as hints if needed, or just default to current.

# Interactive Prompt for IP
if [ -n "$CURRENT_IP" ]; then
    read -p "Enter TV IP Address [$CURRENT_IP]: " INPUT_IP
    TV_IP=${INPUT_IP:-$CURRENT_IP}
else
    read -p "Enter TV IP Address: " TV_IP
fi

# Validation
if [[ ! $TV_IP =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo -e "${RED}Invalid IP format.${NC}"
    exit 1
fi

# Try to look up MAC from ARP table based on IP
DETECTED_MAC=$(ip neigh show "$TV_IP" | awk '{print $5}')
CURRENT_MAC=$(grep "^tv_mac=" ./bin/LG_Buddy_Startup | cut -d'"' -f2)
[ "$CURRENT_MAC" == "XX:XX:XX:XX:XX:XX" ] && CURRENT_MAC=""

SUGGESTED_MAC=${DETECTED_MAC:-$CURRENT_MAC}

if [ -n "$SUGGESTED_MAC" ]; then
    read -p "Enter TV MAC Address [$SUGGESTED_MAC]: " INPUT_MAC
    TV_MAC=${INPUT_MAC:-$SUGGESTED_MAC}
else
    read -p "Enter TV MAC Address: " TV_MAC
fi


# --- 3. Input Selection ---
echo -e "\n${GREEN}[3/5] Configuring Input...${NC}"
CURRENT_INPUT=$(grep "^input=" ./bin/LG_Buddy_Startup | cut -d'"' -f2)
read -p "Enter PC Input (e.g., HDMI_1, HDMI_2) [$CURRENT_INPUT]: " INPUT_VAL
TV_INPUT=${INPUT_VAL:-$CURRENT_INPUT}


# --- 4. Apply Configuration ---
echo -e "\n${GREEN}[4/5] Applying Configuration to Scripts...${NC}"

# Function to safely replace config in files
update_config() {
    FILE=$1
    echo "Updating $FILE..."
    
    # Update IP
    sed -i "s|^tv_ip=.*|tv_ip=\"$TV_IP\"|" "$FILE"
    
    # Update Command Path logic (This is tricky because it's inside command substitution $())
    # We replace the command path if it looks like a path or command name before arguments
    # Simplest reliable way: replace specific previous known strings or use a specific marker?
    # Actually, we can replace the whole line if we know the structure, but that's brittle.
    # Better: Update regex for the specific lines we know exists.
    
    # Replace the command path everywhere it occurs
    # We match: $(/path/to/cmd ... ) or /path/to/cmd ...
    # We will search for 'bscpylgtvcommand' and replace the path preceding it.
    sed -i "s|[^ \t\"']*/bin/bscpylgtvcommand|$BSCPY_CMD|g" "$FILE"
    sed -i "s|[^ \t\"']*/bscpylgtvcommand|$BSCPY_CMD|g" "$FILE"

    # Specific Variable Updates
    if [[ "$FILE" == *"Startup"* ]]; then
        sed -i "s|^tv_mac=.*|tv_mac=\"$TV_MAC\"|" "$FILE"
        sed -i "s|^input=.*|input=\"$TV_INPUT\"|" "$FILE"
    fi
     # Specific Variable Updates for Brightness
    if [[ "$FILE" == *"Brightness"* ]]; then
        sed -i "s|^TV_IP=.*|TV_IP=\"$TV_IP\"|" "$FILE"
        sed -i "s|^CMD=.*|CMD=\"$BSCPY_CMD\"|" "$FILE"
    fi
}

update_config "./bin/LG_Buddy_Startup"
update_config "./bin/LG_Buddy_Shutdown"
update_config "./bin/LG_Buddy_sleep"
update_config "./bin/LG_Buddy_Brightness"

echo "Configuration applied."


# --- 5. Installation ---
echo -e "\n${GREEN}[5/5] Installing Files...${NC}"

echo "Installing scripts to /usr/bin/..."
sudo cp ./bin/LG_Buddy_Startup /usr/bin/
sudo cp ./bin/LG_Buddy_Shutdown /usr/bin/
sudo cp ./bin/LG_Buddy_Brightness /usr/bin/

sudo chmod +x /usr/bin/LG_Buddy_Startup
sudo chmod +x /usr/bin/LG_Buddy_Shutdown
sudo chmod +x /usr/bin/LG_Buddy_Brightness

echo "Installing NetworkManager script..."
sudo cp ./bin/LG_Buddy_sleep /etc/NetworkManager/dispatcher.d/pre-down.d/
sudo chmod +x /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep

echo "Installing Systemd Services..."
sudo cp ./systemd/LG_Buddy.service /etc/systemd/system/
sudo cp ./systemd/LG_Buddy_wake.service /etc/systemd/system/

echo "Installing Desktop Shortcuts..."
sudo cp ./LG_Buddy_Brightness.desktop /usr/share/applications/

echo "Enabling Services..."
sudo systemctl daemon-reload
sudo systemctl enable LG_Buddy.service
sudo systemctl enable LG_Buddy_wake.service

# Check status but don't fail script if inactive
sudo systemctl is-enabled LG_Buddy.service
sudo systemctl is-enabled LG_Buddy_wake.service

echo -e "\n${GREEN}Success! Installation Complete.${NC}"
echo "You can now run 'LG_Buddy_Brightness' from your menu."
