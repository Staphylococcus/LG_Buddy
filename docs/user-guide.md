# User Guide

This guide covers the parts of LG Buddy that users may want after installation: commands, configuration, and desktop-idle behavior.

## Runtime Commands

The installed runtime command is:

```bash
lg-buddy <command>
```

Available commands:

- `startup [auto|boot|wake]`
- `shutdown`
- `sleep-pre`
- `sleep`
- `nm-pre-down`
- `brightness`
- `brightness get`
- `brightness set <0-100>`
- `screen-off`
- `screen-on`
- `monitor`
- `lifecycle`
- `detect-backend`
- `settings`
- `updates`

Examples:

```bash
lg-buddy detect-backend
lg-buddy settings list
lg-buddy monitor
lg-buddy brightness
lg-buddy brightness get
lg-buddy brightness set 65
lg-buddy --version
lg-buddy updates check
lg-buddy updates check --notify
```

In normal use, systemd starts the relevant commands automatically. Most users
only need `brightness`, `settings`, or `configure.sh`.

`brightness` opens the desktop brightness dialog. `brightness get` prints the
current TV OLED brightness, and `brightness set <0-100>` updates it directly.
`--version` prints the installed runtime version, release channel, and commit
metadata when the binary was built as an official release artifact.
`updates check` queries GitHub releases on demand and reports whether a newer
release is available. Stable builds check stable releases by default, while
prerelease builds check the prerelease channel, which includes both prerelease
and stable releases. Use `--channel stable` or `--channel prerelease` to choose
the channel explicitly. The command may cache GitHub response metadata to reduce
repeated API downloads. Add `--notify` to send a desktop notification when the
check finds an available update. Notification delivery is handled by the
running user-session LG Buddy process; if that process is not running,
`--notify` reports a notification failure after printing the update result. When
the desktop notification service supports actions, the notification includes a
`View Release` button that opens the GitHub release page through the system
opener.

`lifecycle`, `nm-pre-down`, `sleep-pre`, and `startup wake` are normally
service-owned system lifecycle commands. They are documented for
troubleshooting, not day-to-day manual use.

## Desktop Idle Monitoring

LG Buddy supports two session backends:

- `gnome`
- `swayidle`

`screen_backend=auto` prefers GNOME when the current session satisfies the full GNOME contract, then falls back to `swayidle` if installed.

The GNOME backend requires:

- GNOME Shell
- `org.gnome.ScreenSaver`
- `org.gnome.Mutter.IdleMonitor`

The monitor runtime keeps one persistent session-bus connection open for GNOME
shell detection, ScreenSaver signals, and Mutter idletime polling.

When the GNOME backend is active, LG Buddy also watches readable Linux gamepad
input devices and treats controller activity as user activity. This is automatic
and has no configuration switch. Devices are discovered at monitor startup,
refreshed when Linux reports input-device add, remove, or change events, and
periodically reconciled so hot-plugged controllers can be picked up without
restarting the service. Standard controllers are read through evdev. The
Logitech G923 also has a raw HID fallback for wheel and pedal activity that is
not exposed as evdev events on some Linux hosts.

Gamepad activity detection requires the user session running
`LG_Buddy_screen.service` to have read access to the relevant `/dev/input/event*`
and, for the G923 fallback, `/dev/hidraw*` nodes. On normal desktop sessions this
is typically granted by logind/udev seat ACLs.

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

For backend semantics and implementation details, see [session-backend-model.md](session-backend-model.md).

## System Sleep And Wake

Default installs enable system sleep/wake TV control through:

```bash
systemctl status LG_Buddy_lifecycle.service
```

The installed lifecycle path has two Linux event sources:

- a NetworkManager `pre-down` dispatcher hook gates network teardown and checks
  logind `PreparingForSleep`
- `LG_Buddy_lifecycle.service` listens to logind `PrepareForSleep(false)` for
  resume restore

When NetworkManager reports `pre-down` and logind says the system is preparing
for sleep, LG Buddy runs pre-sleep TV power-off before the interface is torn
down. Ordinary network disconnects return quickly. After resume, the lifecycle
service runs wake restore policy and clears the sleep-attempt marker.

While system sleep is pending, session idle/activity events do not run screen
blank or restore TV commands. This avoids racing session-level TV control
against the lifecycle sleep path.

LG Buddy does not install the old sleep and wake systemd oneshot handlers or the
old NetworkManager sleep hook. The installer and uninstaller remove those legacy
artifacts from existing installs so there is only one system lifecycle owner.

## Configuration

To inspect structured settings after installation:

```bash
lg-buddy settings list
lg-buddy settings describe screen.restore_policy
lg-buddy settings get screen.idle_timeout
```

To change supported settings:

```bash
lg-buddy settings set tv.input HDMI_2
lg-buddy settings set screen.idle_timeout 600
lg-buddy settings set screen.restore_policy aggressive
lg-buddy settings unset screen.restore_policy
```

`set` and `unset` write `config.env` and then apply the setting when an explicit
runtime apply step is needed. Screen-monitor settings restart
`LG_Buddy_screen.service` when the user service is installed and active or
enabled. TV identity and system sleep/wake policy changes are read by later
runtime actions and do not require a service restart.

To rerun full setup for TV IP, MAC address, HDMI input, or install-time service
wiring:

```bash
./configure.sh
```

The settings CLI, configurator, installer, and manual edits all use the same
`config.env` file. It is resolved from:

- `LG_BUDDY_CONFIG`, if set
- otherwise `${XDG_CONFIG_HOME}/lg-buddy/config.env`
- otherwise `~/.config/lg-buddy/config.env`

Current config keys:

- `tvs_primary_ip`
- `tvs_primary_mac`
- `tvs_primary_input`
- `screen_backend`
- `screen_idle_timeout`
- `screen_restore_policy`
- `system_sleep_wake_policy`

Legacy single-TV keys `tv_ip`, `tv_mac`, and `input` are still read as fallback
values for existing installs. New writes use `tvs_primary_*` storage keys.

If a direct edit leaves a malformed value in `config.env`, `settings list` and
`settings describe` show the raw value as invalid with an `invalid config.env`
source. `settings get <key>` fails with the validation error instead of
pretending the value is missing or defaulted. Repair the entry with
`settings set <key> <value>`, `settings unset <key>` when supported, or a manual
config edit.

Current structured settings:

| Setting key | `config.env` key | Operations |
| --- | --- | --- |
| `tv.ip` | `tvs_primary_ip` | `get`, `describe`, `set` |
| `tv.mac` | `tvs_primary_mac` | `get`, `describe`, `set` |
| `tv.input` | `tvs_primary_input` | `get`, `describe`, `set` |
| `screen.backend` | `screen_backend` | `get`, `describe`, `set`, `unset` |
| `screen.idle_timeout` | `screen_idle_timeout` | `get`, `describe`, `set`, `unset` |
| `screen.restore_policy` | `screen_restore_policy` | `get`, `describe`, `set`, `unset` |
| `system.sleep_wake_policy` | `system_sleep_wake_policy` | `get`, `describe`, `set`, `unset` |

The `tv.*` settings expose the single supported TV in the public API. Their
storage keys are profile-shaped only to leave room for future storage growth;
this version does not expose multiple TVs or TV profile selection. These values
are required, so `unset` is not supported.

`screen_idle_timeout` is the inactivity threshold in seconds used by the session monitor.
LG Buddy currently uses that timeout for both the GNOME and `swayidle` backends.
Values above 86400 seconds are capped at 86400 seconds.

`screen_restore_policy` controls how aggressively LG Buddy reclaims the display on wake and user activity:

- `conservative`: default behavior, only restore when an LG Buddy marker says it previously blanked or powered off the TV
- `aggressive`: attempt restore on session wake/activity and system wake even without a marker

`marker_only` is still accepted as a legacy alias for `conservative`.

`system_sleep_wake_policy` controls automatic system sleep/wake TV handling:

- `enabled`: default behavior, let the installed NetworkManager pre-down gate
  and logind lifecycle service control the TV around system sleep and wake
- `disabled`: leave lifecycle integration installed, but make those commands
  no-op for sleep/wake TV handling

The running lifecycle service rereads config and suppresses actions while this
value is `disabled`. The NetworkManager pre-down hook also reads config on each
invocation, so `lg-buddy settings set system.sleep_wake_policy <value>` changes
runtime policy without reinstalling services.

Example:

```ini
tvs_primary_ip=192.168.1.100
tvs_primary_mac=aa:bb:cc:dd:ee:ff
tvs_primary_input=HDMI_2
screen_idle_timeout=300
screen_restore_policy=aggressive
system_sleep_wake_policy=enabled
```

Installed services receive the resolved config path through `LG_BUDDY_CONFIG`.

## Uninstall

To remove LG Buddy:

```bash
chmod +x ./uninstall.sh
./uninstall.sh
```

This removes the installed services, desktop entry, Rust runtime binary, Python TV-control environment, and optionally the user config file.
