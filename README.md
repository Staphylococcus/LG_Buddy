<p align="center">
  <img src="data/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg" alt="LG Buddy app icon" width="128" height="128">
</p>
<h1 align="center">LG Buddy</h1>
<p align="center">Make your LG webOS TV feel at home on your Linux desktop.</p>
<p align="center">
  <a href="https://github.com/Staphylococcus/LG_Buddy/releases">Download</a> ·
  <a href="docs/user-guide.md">User guide</a> ·
  <a href="CONTRIBUTING.md">Contribute</a>
</p>

Use your LG webOS TV as a Linux PC display with less reaching for the remote.

- **Start and stop with your PC.** Turn the TV on at boot and wake, and off at
  shutdown and before system sleep.
- **Give the panel a break.** Blank it when you step away, restore it when you
  return, and power off after five more minutes without activity.
- **Keep playing.** Supported gamepad activity keeps the panel awake on GNOME
  and compatible native Wayland sessions.
- **Get comfortable.** Adjust brightness and sound from your desktop.

![LG Buddy Overview with a connected TV, brightness and volume sliders, and connection status](docs/screenshots/overview.png)

Move a slider to apply its value, or click the sound icon to toggle mute.
[Adjust brightness and sound →](docs/user-guide.md#adjust-brightness-and-sound)
Screenshots use sample TV data.

## Desktop Compatibility

| What you can do | GNOME | Compatible native Wayland | Wayland with `swayidle` | Other Linux sessions |
| --- | --- | --- | --- | --- |
| Turn the TV on and off with your PC | ✅ | ✅ | ✅ | ✅ |
| Blank the panel while away and restore it on return | ✅ | ✅ | ✅ | ❌ |
| Keep the panel awake while using a gamepad | ✅ | ✅ | ❌ | ❌ |
| Adjust brightness and sound in the desktop app | ✅ | ✅ | ✅ | ✅ |
| Control the TV, change settings, and update from a terminal | ✅ | ✅ | ✅ | ✅ |

Most modern Linux desktop environments are supported. Automatic desktop detection is the default; most users
can leave it selected. If the TV does not blank or restore as expected, follow
the [screen behavior and troubleshooting guide](docs/user-guide.md#automatic-screen-blanking).

## Before You Install

LG Buddy manages one TV. Connect it to the same network as your PC and enable
**TV On With Mobile / Wake-on-LAN**. A static IP address and **Always Ready**
are strongly recommended so the saved address stays valid and the TV remains
ready to respond.

Official bundles contain prebuilt binaries. They require GTK 4.14,
libadwaita 1.5, and glibc 2.39 or newer—the Ubuntu 24.04 runtime baseline.
The release installer also needs Python 3 with `venv`/`pip` support and Zenity
for its compatibility tools. Install the prerequisites for your distribution:

<details>
<summary>Dependency commands for Debian/Ubuntu, Fedora, and Arch</summary>

### Debian, Ubuntu, and Pop!_OS

```bash
sudo apt install python3-venv python3-pip zenity libgtk-4-1 libadwaita-1-0
```

### Fedora

```bash
sudo dnf install python3 python3-pip python3-virtualenv zenity gtk4 libadwaita
```

### Arch Linux

```bash
sudo pacman -S python python-pip python-virtualenv zenity gtk4 libadwaita
```

</details>

The installer can also offer to install missing GTK/libadwaita packages on
these distributions, with your confirmation. Older desktops that need the
deprecated `swayidle` integration must install `swayidle` separately.

The shell installer supports conventional Linux installations with writable
system locations. First-class NixOS packaging is tracked in
[issue #24](https://github.com/Staphylococcus/LG_Buddy/issues/24).

## Install

1. Download and extract the [release archive](https://github.com/Staphylococcus/LG_Buddy/releases)
   for your platform.
2. Run the installer as your regular user (no sudo):

   ```bash
   chmod +x ./install.sh
   ./install.sh
   ```

   The installer requests elevated access when needed.

3. Follow the setup prompts for your TV's IP address, MAC address, HDMI input,
   and desktop behavior. Keep the TV on and approve its pairing request with
   the remote.
4. Open **LG Buddy** from your app launcher. Automatic power and screen
   behavior runs in the background after setup; the window is there when you
   want to adjust something.

<a id="quick-start"></a>

## Make It Work for You

- [Adjust brightness and sound](docs/user-guide.md#adjust-brightness-and-sound) without using
  the TV remote.
- [Choose when the screen blanks and the TV sleeps](docs/user-guide.md#settings)
  to suit your desk routine.
- [Pair a TV](docs/user-guide.md#pair-your-first-tv) if the app has no saved
  connection, or [change its HDMI input](docs/user-guide.md#tvs) after moving
  your PC's cable.
- [Use terminal commands](docs/user-guide.md#common-commands) for shortcuts,
  scripts, or a desktop without the GUI.

To rerun setup or change installed service wiring, run `./configure.sh` from
the extracted release archive.

## Update

Read what changed in the [release notes on GitHub](https://github.com/Staphylococcus/LG_Buddy/releases).

To install the next release while keeping your settings and pairing, run as
your regular user:

```bash
lg-buddy updates install
```

Review the offered version and confirm to proceed. For preview releases,
automatic check preferences, or older installations, see
[Update LG Buddy](docs/user-guide.md#updates).

<a id="documentation"></a>

## Contribute

To build LG Buddy or work on a change, start with the
[development guide](docs/development.md) and [contribution guidelines](CONTRIBUTING.md).
The [architecture overview](docs/architecture-overview.md) explains how the
pieces fit together; the [release process](docs/release-process.md) covers
publishing a version.

## Credits

- [chros73](https://github.com/chros73) for `bscpylgtv`
- [JPersson77](https://github.com/JPersson77) for
  [LGTV Companion](https://github.com/JPersson77/LGTVCompanion), the original inspiration
- [Faceless3882](https://github.com/Faceless3882) for the original shell script
  implementation
