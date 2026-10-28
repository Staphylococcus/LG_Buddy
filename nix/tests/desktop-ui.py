"""Observe the actual installed GTK application on the native VM desktop."""
import importlib
import json
import sys
import time
from pathlib import Path

import dbus
import pyatspi

recovery = importlib.import_module("test-native-setup-recovery")
ui = recovery.ui
scenario = sys.argv[1]


def sensitivity_path(item):
    path = []
    while item is not None and ui.role(item) != pyatspi.ROLE_APPLICATION:
        path.append({"name": ui.name(item), "role": ui.role_name(item),
                     "sensitive": ui.is_sensitive(item)})
        item = item.parent
    return path


if scenario == "inhibit":
    bus = dbus.SessionBus()
    if bus.name_has_owner("org.gnome.SessionManager"):
        api = dbus.Interface(bus.get_object("org.gnome.SessionManager", "/org/gnome/SessionManager"),
                             "org.gnome.SessionManager")
        cookie = api.Inhibit("LG Buddy qualification", dbus.UInt32(0), "native idle inhibition", dbus.UInt32(8))
        assert api.IsInhibited(dbus.UInt32(8))
    else:
        api = dbus.Interface(bus.get_object("org.freedesktop.ScreenSaver", "/ScreenSaver"),
                             "org.freedesktop.ScreenSaver")
        cookie = api.Inhibit("LG Buddy qualification", "native idle inhibition")
        pd = dbus.Interface(bus.get_object("org.kde.Solid.PowerManagement",
                            "/org/kde/Solid/PowerManagement/PolicyAgent"),
                            "org.kde.Solid.PowerManagement.PolicyAgent")
        recovery.wait("effective PowerDevil inhibition", lambda: pd.HasInhibition(dbus.UInt32(4)), 15)
    Path("/tmp/qualification-inhibitor-ready").touch()
    while True:
        time.sleep(1)
elif scenario == "assessment":
    result = recovery.snapshot(dbus.SessionBus())
    print(json.dumps(result))
    assert result["status"] == sys.argv[2], result
elif scenario == "gate":
    recovery.wait("setup gate", lambda: "Complete setup" in recovery.names())
    assert not recovery.functional()
elif scenario == "settings":
    recovery.wait("functional GUI", recovery.functional)
    def select_settings():
        for item in recovery.controls():
            if ui.name(item) == "Settings":
                try:
                    if item.queryAction().doAction(0):
                        return True
                except NotImplementedError:
                    pass
        return False

    recovery.wait("Settings navigation action", select_settings)
    expected = sys.argv[2]
    editable = "editable" in sys.argv[3:]

    def settings_ready():
        controls = recovery.controls()
        entries = [item for item in controls if ui.is_text_control(item)
                   and ui.name(item) == "Idle timeout"]
        if not entries or ui.text_value(entries[0]) != expected:
            return False
        # AT-SPI exposes the child's own state even when its preferences row
        # disables the subtree. Observe the effective native control hierarchy.
        path = sensitivity_path(entries[0])
        assert all(node["sensitive"] for node in path) == editable, path
        names = {ui.name(item) for item in controls}
        assert "Managed externally" in names
        notice = "Some settings are disabled because they are managed declaratively in your nix configuration"
        assert (notice in names) != editable
        toggles = [item for item in controls if ui.is_toggle_control(item)
                   and ui.name(item) == "Idle blanking"]
        assert toggles and ui.is_sensitive(toggles[0])
        return [{"name": ui.name(item), "role": ui.role_name(item),
                 "sensitive": ui.is_sensitive(item), "text": ui.text_value(item)}
                for item in controls if ui.name(item)]

    print(json.dumps(recovery.wait("settings values and sensitivity", settings_ready)))
else:
    raise AssertionError(scenario)
