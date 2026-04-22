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

`screen_backend=auto` prefers GNOME when GNOME Shell is available, then falls back to `swayidle` if installed.

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

`screen_restore_policy` controls whether `screen-on` requires the session marker:

- `marker_only`: default behavior, only restore when LG Buddy knows it blanked or powered off the TV
- `aggressive`: attempt restore on session wake/activity even without the marker

Example:

```ini
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
