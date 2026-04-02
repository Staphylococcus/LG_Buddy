# LG Buddy

Inspired by [LGTV Companion for Windows](https://github.com/JPersson77/LGTVCompanion), LG Buddy controls an LG WebOS TV from Linux so the TV follows the PC lifecycle more like a dedicated monitor.

The current runtime is implemented in Rust. Shell is still used for setup, installation, and removal.

LG Buddy currently uses [`bscpylgtv`](https://github.com/chros73/bscpylgtv) as its TV control backend.

## Features

- Turn the TV on at boot and wake.
- Turn the TV off at shutdown and before system sleep.
- Blank and restore the panel on desktop idle and activity.
- Support GNOME and `swayidle`-based idle monitoring.
- Adjust OLED pixel brightness with a `zenity` slider.
- Configure TV and backend settings with `configure.sh`.

## Requirements

Required:

- a Rust toolchain with `cargo`
- `python3-venv`
- `python3-pip`
- `zenity`

Backend-specific:

- `gdbus` for the GNOME monitor backend
- `swayidle` for the `swayidle` monitor backend

Notes:

- Wake-on-LAN is sent natively by the Rust runtime. A separate `wakeonlan` or `wol` package is no longer required.
- The installer builds `lg-buddy` from the local source tree. It does not download a prebuilt binary.
- The TV control path still depends on the Python `bscpylgtvcommand` CLI, which the installer places in `/usr/bin/LG_Buddy_PIP`.

Typical package installs:

**Debian/Ubuntu/Pop!_OS**
```bash
sudo apt install python3-venv python3-pip zenity libglib2.0-bin
```

**Fedora**
```bash
sudo dnf install python3 python3-pip python3-virtualenv zenity glib2
```

**Arch**
```bash
sudo pacman -S python python-pip python-virtualenv zenity glib2
```

If you want the `swayidle` backend, install `swayidle` separately for your distro.

## Installation

1. Clone the repository.
2. Make sure `cargo` is installed and available in `PATH`.
3. Run the installer:

```bash
chmod +x ./install.sh
./install.sh
```

The installer will:

- run `configure.sh`
- build `lg-buddy` with `cargo build --release -p lg-buddy`
- create `/usr/bin/LG_Buddy_PIP` and install `bscpylgtv`
- install `/usr/bin/lg-buddy`
- install systemd units for boot, wake, sleep, and the user-session monitor
- install the brightness desktop entry
- install a NetworkManager pre-down hook that runs `lg-buddy sleep`

On first use, you may need to accept a pairing prompt on the TV:

<https://github.com/chros73/bscpylgtv/blob/master/docs/guides/first_use.md>

## Runtime Commands

The installed runtime command is:

```bash
lg-buddy <command>
```

Current commands:

- `startup [auto|boot|wake]`
- `shutdown`
- `sleep-pre`
- `sleep`
- `brightness`
- `screen-off`
- `screen-on`
- `monitor`
- `detect-backend`

Examples:

```bash
lg-buddy detect-backend
lg-buddy monitor
lg-buddy brightness
```

## Desktop Idle Monitoring

LG Buddy supports two session backends:

- `gnome`
- `swayidle`

`screen_backend=auto` prefers GNOME when GNOME Shell is available, then falls back to `swayidle` if installed.

### GNOME

The GNOME backend uses:

- `org.gnome.ScreenSaver`
- `org.gnome.Mutter.IdleMonitor`
- `gdbus wait`, `gdbus call`, and `gdbus monitor`

LG Buddy follows GNOME’s own idle timing and uses Mutter idle-monitor activity to restore the screen early when possible.

### swayidle

The `swayidle` backend delegates idle timing to `swayidle` and currently wires:

- `timeout` -> idle blank/power-off policy
- `resume` -> restore policy

When `screen_backend=swayidle`, `configure.sh` prompts for `screen_idle_timeout`.

### Useful commands

Check the user-session monitor:

```bash
systemctl --user status LG_Buddy_screen.service
```

Temporarily force a backend:

```bash
systemctl --user edit LG_Buddy_screen.service
```

Then add:

```ini
[Service]
Environment=LG_BUDDY_SCREEN_BACKEND=gnome
```

Supported values are `auto`, `gnome`, and `swayidle`.

## Configuration

To change settings after installation:

```bash
./configure.sh
```

The configurator writes `config.env` to:

- `LG_BUDDY_CONFIG`, if set
- otherwise `${XDG_CONFIG_HOME}/lg-buddy/config.env`
- otherwise `~/.config/lg-buddy/config.env`

Current config keys:

- `tv_ip`
- `tv_mac`
- `input`
- `screen_backend`
- `screen_idle_timeout`

Installed services receive the resolved config path through `LG_BUDDY_CONFIG`.

## Uninstall

To remove LG Buddy:

```bash
chmod +x ./uninstall.sh
./uninstall.sh
```

This removes the installed services, desktop entry, Rust runtime binary, Python TV-control environment, and optionally the user config file.

## Repository Layout

| Path | Purpose |
| --- | --- |
| `crates/lg-buddy/src/lib.rs` | CLI parsing and command dispatch |
| `crates/lg-buddy/src/commands.rs` | Runtime lifecycle and policy commands |
| `crates/lg-buddy/src/session/runner.rs` | Session monitor loop |
| `crates/lg-buddy/src/gnome.rs` | GNOME backend integration |
| `crates/lg-buddy/src/swayidle.rs` | `swayidle` backend integration |
| `crates/lg-buddy/src/tv.rs` | TV transport boundary and facade |
| `crates/lg-buddy/src/wol.rs` | Native Wake-on-LAN support |
| `configure.sh` | Interactive configuration tool |
| `install.sh` | Installer |
| `uninstall.sh` | Uninstaller |
| `bin/LG_Buddy_Common` | Shared shell config helper used by setup scripts |
| `systemd/` | Installed unit files and tmpfiles config |
| `docs/architecture-overview.md` | Current runtime architecture |
| `docs/session-backend-model.md` | Session backend semantics and capability model |
| `docs/testing-strategy.md` | Test strategy and scope |

## Credits

- <https://github.com/chros73> for `bscpylgtv`
- <https://github.com/JPersson77> for the original inspiration
