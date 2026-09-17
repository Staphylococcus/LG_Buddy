#!/usr/bin/env python3
"""Expose the GUI fixture's service environment on a standard user bus."""

import argparse
import signal
import subprocess
from pathlib import Path

from gi.repository import Gio, GLib


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--config", required=True)
parser.add_argument("--ready-file", required=True, type=Path)
parser.add_argument("--runtime-dir", required=True, type=Path)
args = parser.parse_args()
socket_path = args.runtime_dir / "bus"
socket_path.parent.mkdir(parents=True, exist_ok=True)
daemon = subprocess.Popen([
    "dbus-daemon", "--session", "--nofork", "--print-address=1",
    "--address=unix:path=" + Gio.dbus_address_escape_value(str(socket_path)),
], stdout=subprocess.PIPE, text=True)
address = daemon.stdout.readline().strip()
connection = Gio.DBusConnection.new_for_address_sync(
    address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None,
)
connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
    GLib.Variant("(su)", ("org.freedesktop.systemd1", 0)), GLib.VariantType.new("(u)"),
    Gio.DBusCallFlags.NONE, -1, None)

manager = Gio.DBusNodeInfo.new_for_xml("""
<node><interface name="org.freedesktop.systemd1.Manager">
<method name="GetUnit"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
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


connection.register_object("/org/freedesktop/systemd1", manager.interfaces[0], load_unit, None, None)
connection.register_object(unit_path, service.interfaces[0], None, property_value, None)
loop = GLib.MainLoop()
signal.signal(signal.SIGTERM, lambda *_: loop.quit())
args.ready_file.touch()
try:
    loop.run()
finally:
    connection.close_sync(None)
    daemon.terminate()
    daemon.wait(timeout=5)
