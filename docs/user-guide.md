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

In normal use, systemd starts the relevant commands automatically. Most users only need `brightness` or `configure.sh`.

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
- `screen_restore_policy`

`screen_idle_timeout` is the inactivity threshold in seconds used by the session monitor.
LG Buddy currently uses that timeout for both the GNOME and `swayidle` backends.

`screen_restore_policy` controls how aggressively LG Buddy reclaims the display on wake and user activity:

- `conservative`: default behavior, only restore when an LG Buddy marker says it previously blanked or powered off the TV
- `aggressive`: attempt restore on session wake/activity and system wake even without a marker

`marker_only` is still accepted as a legacy alias for `conservative`.

Example:

```ini
screen_idle_timeout=300
screen_restore_policy=aggressive
```

Installed services receive the resolved config path through `LG_BUDDY_CONFIG`.

## Uninstall

To remove LG Buddy:

```bash
chmod +x ./uninstall.sh
./uninstall.sh
```

This removes the installed services, desktop entry, Rust runtime binary, Python TV-control environment, and optionally the user config file.
