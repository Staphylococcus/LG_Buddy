#!/usr/bin/env python3
"""Expose the installed-GUI fixture's screen-service environment on its private bus."""

import argparse
from pathlib import Path

from gi.repository import Gio, GLib


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--config", required=True)
parser.add_argument("--ready-file", required=True, type=Path)
args = parser.parse_args()
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
result = bus.call_sync(
    "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
    "RequestName", GLib.Variant("(su)", ("org.freedesktop.systemd1", 4)),
    GLib.VariantType.new("(u)"), Gio.DBusCallFlags.NONE, 2000, None,
)
if result.unpack() != (1,):
    raise SystemExit("Refusing to replace an existing systemd manager")

manager = Gio.DBusNodeInfo.new_for_xml("""
<node><interface name="org.freedesktop.systemd1.Manager">
<method name="LoadUnit"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
</interface></node>
""")
service = Gio.DBusNodeInfo.new_for_xml("""
<node><interface name="org.freedesktop.systemd1.Service">
<property name="Environment" type="as" access="read"/>
<property name="EnvironmentFiles" type="a(sb)" access="read"/>
<property name="UnsetEnvironment" type="as" access="read"/>
</interface></node>
""")
unit_path = "/org/freedesktop/systemd1/unit/screen"


def load_unit(connection, sender, path, interface, method, parameters, invocation):
    if parameters.unpack() != ("LG_Buddy_screen.service",):
        invocation.return_dbus_error("org.freedesktop.systemd1.NoSuchUnit", "Unknown fixture unit")
    else:
        invocation.return_value(GLib.Variant("(o)", (unit_path,)))


def property_value(connection, sender, path, interface, name):
    return {
        "Environment": GLib.Variant("as", [f"LG_BUDDY_CONFIG={args.config}"]),
        "EnvironmentFiles": GLib.Variant("a(sb)", []),
        "UnsetEnvironment": GLib.Variant("as", []),
    }[name]


bus.register_object("/org/freedesktop/systemd1", manager.interfaces[0], load_unit, None, None)
bus.register_object(unit_path, service.interfaces[0], None, property_value, None)
args.ready_file.touch()
GLib.MainLoop().run()
