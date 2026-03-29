# LG Buddy
Inspired by LG Companion for Windows (https://github.com/JPersson77/LGTVCompanion), LG Buddy is a set of scripts and service using https://github.com/chros73/bscpylgtv to turn LG WebOS TV's on and off automatically on startup, shutdown (but not reboot), sleep, and wake in Linux.

## Features

- **Startup/Shutdown:** Automatically turn TV on at boot and off at shutdown
- **Sleep/Wake:** Turn TV off on suspend, back on at wake
- **Screen Idle/Resume:** Blank the TV panel when your GNOME or wlroots-based Wayland session goes idle, unblank it when you return
- **Brightness Control:** Interactive slider to adjust OLED pixel brightness (via `zenity`)
- **Interactive Setup:** `configure.sh` prompts for TV and screen settings and writes them to your home directory

## Prerequisites

You will need the following packages installed:

- `python3`, `python3-venv`, `python3-pip` — for bscpylgtv
- `wakeonlan` (or `wol`) — to wake TV from standby
- `gdbus` — for GNOME screen idle detection (usually installed with GNOME/GLib)
- `swayidle` — for wlroots/COSMIC screen idle detection (optional)
- `zenity` — for OLED Pixel Brightness Control (optional)

**Debian/Ubuntu/Pop!_OS:**
```bash
sudo apt install python3-venv python3-pip wakeonlan zenity
```

**Fedora:**
```bash
sudo dnf install python3 python3-pip wol zenity
```

**Arch:**
```bash
sudo pacman -S python python-pip wakeonlan zenity
```

If `gdbus` is missing on GNOME, install your distro's GLib utilities package (`libglib2.0-bin` on Debian/Ubuntu, `glib2` on Fedora/Arch).

> **Note:** The screen monitor now supports multiple backends. On GNOME it uses `org.gnome.ScreenSaver` plus `org.gnome.Mutter.IdleMonitor` over D-Bus. On wlroots/COSMIC desktops it uses `swayidle`.

## Installation

1. Clone or download the latest release of LG Buddy.

2. Make `install.sh` executable and run it:
```bash
chmod +x ./install.sh
./install.sh
```

3. The installer will:
   - Install prerequisites (`python3-venv`, `wakeonlan` or `wol`)
   - Run `configure.sh` to create `~/.config/lg-buddy/config.env`
   - Create the Python virtual environment and install bscpylgtv
   - Copy scripts to system locations and set up systemd services
   - Install the screen monitor user service and optionally enable it during install

4. Restart your computer.

5. On first use, you may need to accept a prompt on your TV to allow this application to connect.
   See: https://github.com/chros73/bscpylgtv/blob/master/docs/guides/first_use.md

## Screen Idle/Resume (Wayland)

The screen monitor auto-detects a supported backend:
- **GNOME:** Uses `org.gnome.ScreenSaver` for idle transitions and `org.gnome.Mutter.IdleMonitor` for early wake-on-activity, while still following GNOME's own blank/lock timing
- **wlroots/COSMIC:** Uses `swayidle`

In a normal GNOME Shell session, Mutter is part of the GNOME stack. No separate Mutter package needs to be installed for this backend.

When idle:
- **LG_Buddy_Screen_Off** blanks the TV panel (if it's on the configured HDMI input)
- **LG_Buddy_Screen_On** unblanks the panel when you move the mouse or press a key

Startup, shutdown, suspend, and resume still use full TV power transitions.

The `swayidle` backend defaults to a 300 second (5 minute) timeout. You can change it with `./configure.sh`. The installer and configurator only prompt for that timeout when `swayidle` is the selected backend.

**Check status:**
```bash
systemctl --user status LG_Buddy_screen.service
```

**Preferred way to change backend or timeout:**
```bash
./configure.sh
```

**Temporary backend override for testing:**
```bash
systemctl --user edit LG_Buddy_screen.service
```

Then add:
```ini
[Service]
Environment=LG_BUDDY_SCREEN_BACKEND=gnome
```

Supported values are `auto`, `gnome`, and `swayidle`.

**Test the GNOME backend manually:**
```bash
gdbus monitor --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver
```

**Test the swayidle backend manually:**
```bash
swayidle -w timeout 10 'echo IDLE' resume 'echo RESUMED'
```

## Configuration

To reconfigure your TV settings after installation, run:
```bash
./configure.sh
```

This updates `~/.config/lg-buddy/config.env`. It does not rewrite the installed scripts, and it does not require `sudo`.

The config file currently contains:
- `tv_ip`
- `tv_mac`
- `input`
- `screen_backend`
- `screen_idle_timeout`

## File Layout

| File | Purpose |
|------|---------|
| `crates/lg-buddy/src/commands.rs` | Runtime commands for startup, shutdown, sleep, brightness, and screen control |
| `crates/lg-buddy/src/session/runner.rs` | User-session monitor loop for GNOME and swayidle |
| `crates/lg-buddy/src/tv.rs` | TV transport boundary and device-facing facade |
| `bin/LG_Buddy_Common` | Shared shell config helper used by setup and uninstall scripts |
| `configure.sh` | Interactive configuration tool |
| `install.sh` | Automated installer |
| `uninstall.sh` | Uninstaller |
| `LG_Buddy_Brightness.desktop` | OLED Brightness Control app |
| `systemd/LG_Buddy.service` | Boot startup and shutdown systemd service |
| `systemd/LG_Buddy_wake.service` | Wake restore systemd service |
| `systemd/LG_Buddy_sleep.service` | Pre-sleep power-off systemd service |
| `systemd/LG_Buddy_screen.service` | Screen monitor user service |

## Author

Rob Grieves aka https://github.com/Faceless3882 aka r/TheFacIessMan

## Credits

- https://github.com/chros73 for the bscpylgtv software that makes this possible.
- https://github.com/JPersson77 for the inspiration and pointing me in the right direction.
