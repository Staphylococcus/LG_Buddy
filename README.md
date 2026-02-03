# LG Buddy
Inspired by LG Companion for Windows (https://github.com/JPersson77/LGTVCompanion), LG Buddy is a set of scripts and service using https://github.com/chros73/bscpylgtv to turn LG WebOS TV's on and off automatically on startup, shutdown (but not reboot), sleep, and wake in Linux.

# PREREQUISITES #

Before installation, ensure you have the necessary system dependencies. For example, on Ubuntu/Debian:
```bash
sudo apt install python3 wakeonlan zenity
```

Then, install the TV control library using **uv** (do **not** use sudo for this):

1. **Install uv** (if you don't have it):
   Refer to [uv installation guide](https://github.com/astral-sh/uv).

2. **Install bscpylgtv tool**:
   ```bash
   uv tool install bscpylgtv
   ```

*(Alternatively, you can install `bscpylgtv` using `pip`, but ensure the `bscpylgtvcommand` binary is in your PATH).*
# INSTALLATION #

1. Download the latest release of LG Buddy.

2. Set install.sh as executable:
```bash
chmod +x ./install.sh
```

3. Run the interactive installer:
```bash
./install.sh
```
The installer will now guide you through:
*   **Auto-detecting** your `bscpylgtv` installation.
*   **Discovering** your TV's IP and MAC address.
*   **Configuring** your custom HDMI input.
*   **Installing** a GUI Brightness Control app.

4. Enter your sudo password when prompted to finalize the system integration.

# FEATURES #

### TV Brightness Control
LG Buddy now includes a desktop integration for controlling your OLED Pixel Brightness. 
* Search for **"TV Brightness"** in your application menu.
* Move the slider to adjust your TV's backlight instantly.

Restart your computer

Take note of the first time use instructions: https://github.com/chros73/bscpylgtv/blob/master/docs/guides/first_use.md

Author:
Rob Grieves aka https://github.com/Faceless3882 aka r/TheFacIessMan

Credit and Thanks:
https://github.com/chros73 for the bscpylgtv software that makes this possible.

https://github.com/JPersson77 for the inspiration and pointing me in the right direction.
