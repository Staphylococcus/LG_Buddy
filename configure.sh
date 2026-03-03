#!/bin/bash

# --- TV IP Detection ---
echo "Scanning for LG TV on local network..."

# Known LG Electronics MAC OUI prefixes
DETECTED_IPS=$(ip neigh show \
  | grep -iE "a8:23:fe|fc:f1:52|f8:b9:5a|c4:36:6c|50:c7:bf|40:b0:76" \
  | awk '{print $1}')
IP_COUNT=$(echo "$DETECTED_IPS" | grep -c . 2>/dev/null || echo 0)

if [ "$IP_COUNT" -eq 1 ]; then
    SUGGESTED_IP="$DETECTED_IPS"
    read -p "Found LG device at $SUGGESTED_IP. Use this address? [Y/n]: " USE_IT
    case "$USE_IT" in
        [Nn]*) SUGGESTED_IP="" ;;
    esac
elif [ "$IP_COUNT" -gt 1 ]; then
    echo "Found multiple LG devices:"
    i=1
    while IFS= read -r ip; do
        echo "  $i) $ip"
        ((i++))
    done <<< "$DETECTED_IPS"
    read -p "Enter the number of your TV, or press Enter to type manually: " CHOICE
    if [[ "$CHOICE" =~ ^[0-9]+$ ]] && [ "$CHOICE" -ge 1 ] && [ "$CHOICE" -le "$IP_COUNT" ]; then
        SUGGESTED_IP=$(echo "$DETECTED_IPS" | sed -n "${CHOICE}p")
    else
        SUGGESTED_IP=""
    fi
else
    echo "  No LG devices found in ARP table (is the TV on?)."
    SUGGESTED_IP=""
fi

# Use confirmed IP directly, or prompt with validation if not yet known
if [ -n "$SUGGESTED_IP" ]; then
    tv_ip="$SUGGESTED_IP"
else
    while true; do
        read -p "Enter your TV's IP address (e.g. 192.168.1.100): " tv_ip
        [[ $tv_ip =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] && break
        echo "  Invalid format. Expected: 192.168.1.100"
    done
fi

# Soft reachability check — warn but don't block (TV may be in standby)
if ! ping -c 1 -W 2 "$tv_ip" &>/dev/null; then
    echo "  Warning: TV not responding at $tv_ip (may be in standby). Continuing."
fi

# --- TV MAC Detection ---
DETECTED_MAC=$(ip neigh show "$tv_ip" | awk 'NR==1{print $5}')

# Use detected MAC directly, or prompt with validation if not found
if [ -n "$DETECTED_MAC" ]; then
    tv_mac="$DETECTED_MAC"
else
    while true; do
        read -p "Enter your TV's MAC address (e.g. aa:bb:cc:dd:ee:ff): " tv_mac
        [[ $tv_mac =~ ^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$ ]] && break
        echo "  Invalid format. Expected: aa:bb:cc:dd:ee:ff"
    done
fi

# --- HDMI Input ---
echo "Which HDMI input is your PC connected to?"
echo "  1) HDMI_1"
echo "  2) HDMI_2"
echo "  3) HDMI_3"
echo "  4) HDMI_4"
while true; do
    read -p "Enter number (1-4): " HDMI_CHOICE
    case "$HDMI_CHOICE" in
        1) pc_input="HDMI_1"; break ;;
        2) pc_input="HDMI_2"; break ;;
        3) pc_input="HDMI_3"; break ;;
        4) pc_input="HDMI_4"; break ;;
        *) echo "  Please enter a number between 1 and 4." ;;
    esac
done

# --- Summary + Confirm ---
echo ""
echo "Configuration to apply:"
echo "  TV IP:    $tv_ip"
echo "  TV MAC:   $tv_mac"
echo "  PC Input: $pc_input"
echo ""
read -p "Apply this configuration? [Y/n]: " CONFIRM
case "$CONFIRM" in
    [Nn]*)
        echo "Aborted. Re-run configure.sh to try again."
        exit 1
        ;;
esac

echo "Updating configuration files..."

sed -i "s/tv_ip=\"[^\"]*\"/tv_ip=\"$tv_ip\"/" bin/LG_Buddy_Startup
sed -i "s/tv_mac=\"[^\"]*\"/tv_mac=\"$tv_mac\"/" bin/LG_Buddy_Startup
sed -i "s/input=\"[^\"]*\"/input=\"$pc_input\"/" bin/LG_Buddy_Startup

sed -i "s/tv_ip=\"[^\"]*\"/tv_ip=\"$tv_ip\"/" bin/LG_Buddy_Shutdown
sed -i "s/input=\"[^\"]*\"/input=\"$pc_input\"/" bin/LG_Buddy_Shutdown

sed -i "s/tv_ip=\"[^\"]*\"/tv_ip=\"$tv_ip\"/" bin/LG_Buddy_sleep_pre
sed -i "s/input=\"[^\"]*\"/input=\"$pc_input\"/" bin/LG_Buddy_sleep_pre

sed -i "s/tv_ip=\"[^\"]*\"/tv_ip=\"$tv_ip\"/" bin/LG_Buddy_sleep
sed -i "s/input=\"[^\"]*\"/input=\"$pc_input\"/" bin/LG_Buddy_sleep

sed -i "s/tv_ip=\"[^\"]*\"/tv_ip=\"$tv_ip\"/" bin/LG_Buddy_Screen_Off
sed -i "s/input=\"[^\"]*\"/input=\"$pc_input\"/" bin/LG_Buddy_Screen_Off

sed -i "s/tv_ip=\"[^\"]*\"/tv_ip=\"$tv_ip\"/" bin/LG_Buddy_Screen_On
sed -i "s/tv_mac=\"[^\"]*\"/tv_mac=\"$tv_mac\"/" bin/LG_Buddy_Screen_On
sed -i "s/input=\"[^\"]*\"/input=\"$pc_input\"/" bin/LG_Buddy_Screen_On

sed -i "s/tv_ip=\"[^\"]*\"/tv_ip=\"$tv_ip\"/" bin/LG_Buddy_Brightness

echo "Configuration updated successfully."
