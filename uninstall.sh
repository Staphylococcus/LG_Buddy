#!/bin/bash

echo "Disabling & removing services..."
echo "(This might turn off your TV)"
sleep 3
sudo systemctl disable LG_Buddy.service
sudo systemctl disable LG_Buddy_wake.service
sudo systemctl stop LG_Buddy.service
sudo systemctl stop LG_Buddy_wake.service
sudo rm /etc/systemd/system/LG_Buddy.service
sudo rm /etc/systemd/system/LG_Buddy_wake.service
echo "Done."

echo "Removing Startup, Shutdown, and Sleep scripts"
sudo rm /usr/bin/LG_Buddy_Startup
sudo rm /usr/bin/LG_Buddy_Shutdown
sudo rm /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
echo "Done."
