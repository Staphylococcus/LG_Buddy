# LG Buddy
Inspired by LG Companion for Windows (https://github.com/JPersson77/LGTVCompanion), LG Buddy is a set of scripts and service using https://github.com/chros73/bscpylgtv to turn LG WebOS TV's on and off automatically on startup, shutdown (but not reboot), sleep, and wake in Linux.

## Features

- **Startup/Shutdown:** Automatically turn TV on at boot and off at shutdown
- **Sleep/Wake:** Turn TV off on suspend, back on at wake
- **Screen Idle/Resume:** Turn TV off when screen goes idle, back on when you return (Wayland)
- **Brightness Control:** Interactive slider to adjust OLED pixel brightness (via `zenity`)
- **Interactive Setup:** `configure.sh` prompts for TV settings and updates all scripts automatically

## Prerequisites

You will need the following packages installed:

- `python3`, `python3-venv`, `python3-pip` — for bscpylgtv
- `wakeonlan` (or `wol`) — to wake TV from standby
- `swayidle` — for screen idle detection (Wayland only, optional)
- `zenity` — for OLED Pixel Brightness Control (optional)

**Debian/Ubuntu/Pop!_OS:**
```bash
sudo apt install python3-venv python3-pip wakeonlan swayidle zenity
```

**Fedora:**
```bash
sudo dnf install python3 python3-pip wol swayidle zenity
```

**Arch:**
```bash
sudo pacman -S python python-pip wakeonlan swayidle zenity
```

> **Note:** `swayidle` is only needed if you want screen idle/resume support. It uses the `ext-idle-notify-v1` Wayland protocol and works on COSMIC, sway, and other Wayland compositors.

## Installation

1. Clone or download the latest release of LG Buddy.

2. Make `install.sh` executable and run it:
```bash
chmod +x ./install.sh
./install.sh
```

3. The installer will:
   - Install prerequisites (`python3-venv`, `wakeonlan` or `wol`, `swayidle`)
   - Run `configure.sh` to set your TV's IP, MAC address, and HDMI input
   - Create the Python virtual environment and install bscpylgtv
   - Copy scripts to `/usr/bin/` and set up systemd services
   - Set up the screen monitor user service (optional — you can disable it during install)

4. Restart your computer.

5. On first use, you may need to accept a prompt on your TV to allow this application to connect.
   See: https://github.com/chros73/bscpylgtv/blob/master/docs/guides/first_use.md

## Screen Idle/Resume (Wayland)

The screen monitor uses `swayidle` to detect when your screen goes idle. When idle:
- **LG_Buddy_Screen_Off** turns the TV off (if it's on the configured HDMI input)
- **LG_Buddy_Screen_On** turns the TV back on when you move the mouse or press a key

The idle timeout defaults to 300 seconds (5 minutes). You can change it in `bin/LG_Buddy_Screen_Monitor`.

**Check status:**
```bash
systemctl --user status LG_Buddy_screen.service
```

**Test swayidle manually:**
```bash
swayidle -w timeout 10 'echo IDLE' resume 'echo RESUMED'
```

## Configuration

To reconfigure your TV settings after installation, run:
```bash
./configure.sh
```

This updates the IP, MAC, and HDMI input in all scripts at once. It also detects your user ID automatically for scripts that run as root.

## File Layout

| File | Purpose |
|------|---------|
| `bin/LG_Buddy_Startup` | Turn TV on at boot/wake |
| `bin/LG_Buddy_Shutdown` | Turn TV off at shutdown |
| `bin/LG_Buddy_sleep` | Turn TV off on suspend |
| `bin/LG_Buddy_Screen_Monitor` | swayidle daemon for idle detection |
| `bin/LG_Buddy_Screen_Off` | Turn TV off on screen idle |
| `bin/LG_Buddy_Screen_On` | Turn TV back on on resume |
| `bin/LG_Buddy_Brightness` | Interactive brightness control |
| `configure.sh` | Interactive configuration tool |
| `install.sh` | Automated installer |
| `systemd/LG_Buddy.service` | Shutdown systemd service |
| `systemd/LG_Buddy_wake.service` | Startup systemd service |
| `systemd/LG_Buddy_screen.service` | Screen monitor user service |

## Author

Rob Grieves aka https://github.com/Faceless3882 aka r/TheFacIessMan

## Credits

- https://github.com/chros73 for the bscpylgtv software that makes this possible.
- https://github.com/JPersson77 for the inspiration and pointing me in the right direction.
