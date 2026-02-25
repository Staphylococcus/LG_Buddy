#!/bin/bash

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
rm -f ~/.config/systemd/user/LG_Buddy_screen.service
echo "Done."

echo "Removing scripts"
sudo rm -f /usr/bin/LG_Buddy_Startup
sudo rm -f /usr/bin/LG_Buddy_Shutdown
sudo rm -f /usr/bin/LG_Buddy_Screen_On
sudo rm -f /usr/bin/LG_Buddy_Screen_Off
sudo rm -f /usr/bin/LG_Buddy_Screen_Monitor
sudo rm -f /usr/bin/LG_Buddy_sleep_pre
sudo rm -f /etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep
sudo rm -f /usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook
sudo rm -f /etc/tmpfiles.d/lg_buddy.conf
sudo rm -rf /run/lg_buddy

echo "Removing python virtual environment"
sudo rm -rf /usr/bin/LG_Buddy_PIP

echo "Done."
