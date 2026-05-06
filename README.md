# LG Buddy

Inspired by [LGTV Companion for Windows](https://github.com/JPersson77/LGTVCompanion), LG Buddy makes an LG WebOS TV behave more like a monitor for a Linux PC.

It can:

- turn the TV on at boot and wake
- turn the TV off at shutdown and before system sleep
- blank and restore the panel on desktop idle and activity, including gamepad activity on GNOME
- adjust OLED pixel brightness with a small desktop dialog

LG Buddy supports GNOME and `swayidle`-based sessions. Official release bundles include a prebuilt `lg-buddy` binary, so normal installation does not require a Rust toolchain.

If you build `lg-buddy` from source instead of using a release bundle, `cargo`
now also needs a working C toolchain because the vendored `libdbus` runtime is
compiled as part of the build.

## Before You Install

Install prerequisites:

- `python3-venv`
- `python3-pip`
- `zenity`

Backend-specific:

- `swayidle` for the `swayidle` backend

The GNOME backend requires a compatible GNOME session with:

- GNOME Shell
- `org.gnome.ScreenSaver`
- `org.gnome.Mutter.IdleMonitor`

At runtime, GNOME support now uses a persistent in-process session-bus client
for shell detection, ScreenSaver signals, and Mutter idletime polling.
The GNOME monitor also observes readable Linux gamepad input devices so
controller activity can keep the TV output awake even when GNOME does not count
that input as desktop activity. It refreshes the watched device set when Linux
reports input-device add, remove, or change events, with a periodic
reconciliation scan as a fallback.

Typical package installs:

**Debian/Ubuntu/Pop!_OS**
```bash
sudo apt install python3-venv python3-pip zenity
```

**Fedora**
```bash
sudo dnf install python3 python3-pip python3-virtualenv zenity
```

**Arch**
```bash
sudo pacman -S python python-pip python-virtualenv zenity
```

For source builds, also install a C toolchain:

- Debian/Ubuntu/Pop!_OS: `build-essential`
- Fedora: `gcc`
- Arch: `base-devel`

## Install

1. Download the release archive for your platform.
2. Extract it.
3. Run:

```bash
chmod +x ./install.sh
./install.sh
```

The installer will prompt for your TV IP, MAC address, HDMI input, idle-monitor backend, idle timeout, and screen restore policy, then install the required services. System sleep/wake handling uses the default lifecycle service plus NetworkManager pre-down gate unless you opt out in `config.env`.

On first use, you may need to accept a pairing prompt on the TV:

<https://github.com/chros73/bscpylgtv/blob/master/docs/guides/first_use.md>

## Day to Day

LG Buddy is mostly automatic after installation.

- To inspect settings, run `lg-buddy settings list`
- To change supported settings, use `lg-buddy settings set <key> <value>`
- To rerun full setup for TV IP, MAC address, or HDMI input, run `./configure.sh`
- To check the screen monitor, run `systemctl --user status LG_Buddy_screen.service`
- To remove LG Buddy, run `./uninstall.sh`

The settings CLI is a structured layer over `config.env`. These examples write
the same file that manual editing and `configure.sh` use:

```bash
lg-buddy settings describe tv.input
lg-buddy settings set tv.input HDMI_2
lg-buddy settings describe screen.restore_policy
lg-buddy settings set screen.idle_timeout 600
lg-buddy settings set screen.restore_policy aggressive
lg-buddy settings set system.sleep_wake_policy disabled
lg-buddy settings unset screen.restore_policy
```

Settings can also be edited directly in `config.env`:

```ini
tvs_primary_ip=192.168.1.100
tvs_primary_mac=aa:bb:cc:dd:ee:ff
tvs_primary_input=HDMI_2
screen_idle_timeout=300
screen_restore_policy=conservative
system_sleep_wake_policy=enabled
```

`tv_ip`, `tv_mac`, and `input` are still accepted as legacy single-TV keys, but
new writes use the `tvs_primary_*` shape so the storage can grow later without
changing the current single-TV settings interface.

`screen_restore_policy=conservative` is the default. LG Buddy only restores when a matching LG Buddy marker says it previously blanked or powered off the TV.

Set `screen_restore_policy=aggressive` to let session wake/activity and system wake restore the TV even when no LG Buddy marker exists. This is intentionally more aggressive and can turn the TV on in cases where another device or a manual action powered it off.

`marker_only` is still accepted as a legacy alias for `conservative`.

`system_sleep_wake_policy=enabled` is the default. Set
`system_sleep_wake_policy=disabled` if you do not want LG Buddy to control the
TV around system sleep and wake. The lifecycle service and NetworkManager
pre-down hook stay installed and no-op while the policy is disabled.

## More Help

- [User guide](docs/user-guide.md)
- [Development](docs/development.md)
- [Defaults and configuration](docs/defaults-and-configuration.md)
- [Runtime event handler map](docs/runtime-event-handler-map.md)
- [Gamepad subsystem](docs/gamepad-subsystem.md)
- [Contributing](CONTRIBUTING.md)
- [Release process](docs/release-process.md)

## Credits

- <https://github.com/chros73> for `bscpylgtv`
- <https://github.com/JPersson77> for the original inspiration
- <https://github.com/Faceless3882> for the original shell script implementation
