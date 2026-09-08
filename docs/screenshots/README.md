# Documentation screenshots

Captured 2026-09-08 from source commit `e5766c5`, using the native
GTK/libadwaita interface in dark mode from the `1.6.0-beta.1` development
build. The app was launched through the no-argument `target/debug/lg-buddy`
Overview entrypoint.

- `overview.png`: connected controls with fixture state from
  `tools/mock_bscpylgtvcommand.py` (brightness 65, volume 20, unmuted).
- `tvs-configured.png`: the configured-TV details view. It uses the fixture
  model `OLED42C2`, documentation-only TEST-NET address `192.0.2.10`, locally
  administered MAC `02:00:00:00:00:10`, HDMI 3, and the compatibility platform.
- `settings.png`: the current Settings view with the isolated configuration's
  GNOME integration and default values (enabled policies, 300-second timeout,
  Conservative restore policy, and Stable update channel).
- `tvs-empty.png`: TVs with an empty configuration.
- `pairing.png`: the first-TV dialog before entering connection details.

Capture used `dbus-run-session`, Xvfb, Openbox, xcompmgr, and the repository's
AT-SPI accessibility contract script. The TV responses came from the existing
mock command and the configuration was temporary and isolated; no live TV,
personal address, credential, user service, or host configuration was used.
