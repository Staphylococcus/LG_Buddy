#!/bin/bash

# Exit on any error
set -e

echo "Starting LG Buddy Installation"

# 1. INSTALL PREREQUISITES
echo "Installing prerequisites (python3-venv, python3-pip, wakeonlan, swayidle)..."
sudo apt-get update
sudo apt-get install -y python3-venv python3-pip wakeonlan swayidle
echo "Done."

# 2. CONFIGURE SCRIPTS
echo "Running configuration script..."
# Make sure configure.sh is executable
if [ ! -x "./configure.sh" ]; then
    chmod +x ./configure.sh
fi
./configure.sh
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
sudo cp ./bin/LG_Buddy_Startup /usr/bin/
sudo cp ./bin/LG_Buddy_Shutdown /usr/bin/
sudo cp ./bin/LG_Buddy_Screen_On /usr/bin/
sudo cp ./bin/LG_Buddy_Screen_Off /usr/bin/
sudo cp ./bin/LG_Buddy_Screen_Monitor /usr/bin/
sudo chmod +x /usr/bin/LG_Buddy_Startup
sudo chmod +x /usr/bin/LG_Buddy_Shutdown
sudo chmod +x /usr/bin/LG_Buddy_Screen_On
sudo chmod +x /usr/bin/LG_Buddy_Screen_Off
sudo chmod +x /usr/bin/LG_Buddy_Screen_Monitor

# Ensure the NetworkManager dispatcher directory exists
sudo mkdir -p /etc/NetworkManager/dispatcher.d/pre-down.d
sudo cp ./bin/LG_Buddy_sleep /etc/NetworkManager/dispatcher.d/pre-down.d/
sudo chmod +x /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
echo "Done."

# 6. SETUP SYSTEMD SERVICES
echo "Copying and enabling systemd services..."
sudo cp ./systemd/LG_Buddy.service /etc/systemd/system/
sudo cp ./systemd/LG_Buddy_wake.service /etc/systemd/system/

sudo systemctl daemon-reload
sudo systemctl enable LG_Buddy.service
sudo systemctl enable LG_Buddy_wake.service
echo "Done."

# 7. SETUP SCREEN MONITOR USER SERVICE
echo "Setting up screen monitor user service..."
mkdir -p ~/.config/systemd/user/
cp ./systemd/LG_Buddy_screen.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable LG_Buddy_screen.service
systemctl --user start LG_Buddy_screen.service
echo "Done."

# 8. ASK TO DISABLE SUSPEND/RESUME FUNCTIONALITY
echo "Do you want to disable the TV power management on system suspend/resume? (y/N) "
read -r REPLY
case "$REPLY" in
    [Yy]*)
        echo "Disabling suspend/resume services..."
        sudo systemctl disable LG_Buddy.service
        sudo systemctl disable LG_Buddy_wake.service
        sudo rm /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
        echo "Suspend/resume services disabled."
        ;;
    *)
        echo "Leaving suspend/resume services enabled."
        ;;
esac


echo "Installation complete!"
echo "The screen monitor service has been started."
echo "Please restart your computer for all changes to take full effect."
echo "NOTE: On first use, you may need to accept a prompt on your TV to allow this application to connect."
