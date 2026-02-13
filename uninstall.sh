#!/bin/bash

echo "Disabling & removing services..."
echo "(This might turn off your TV)"
sleep 3
sudo systemctl disable LG_Buddy.service
sudo systemctl disable LG_Buddy_wake.service
sudo systemctl disable LG_Buddy_screen.service
sudo systemctl stop LG_Buddy.service
sudo systemctl stop LG_Buddy_wake.service
sudo systemctl stop LG_Buddy_screen.service
sudo rm /etc/systemd/system/LG_Buddy.service
sudo rm /etc/systemd/system/LG_Buddy_wake.service
sudo rm /etc/systemd/system/LG_Buddy_screen.service
echo "Done."

echo "Removing scripts"
sudo rm /usr/bin/LG_Buddy_Startup
sudo rm /usr/bin/LG_Buddy_Shutdown
sudo rm /usr/bin/LG_Buddy_Screen_On
sudo rm /usr/bin/LG_Buddy_Screen_Off
sudo rm /usr/bin/LG_Buddy_Screen_Monitor
sudo rm /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
echo "Done."
