#!/usr/bin/env python3
"""Expose the GUI fixture's service environment on a systemd-style peer socket."""

import argparse
from pathlib import Path

from gi.repository import Gio, GLib


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--config", required=True)
parser.add_argument("--ready-file", required=True, type=Path)
parser.add_argument("--runtime-dir", required=True, type=Path)
args = parser.parse_args()
socket_path = args.runtime_dir / "systemd" / "private"
socket_path.parent.mkdir(parents=True, exist_ok=True)
server = Gio.DBusServer.new_sync(
    "unix:path=" + Gio.dbus_address_escape_value(str(socket_path)),
    Gio.DBusServerFlags.NONE, Gio.dbus_generate_guid(), None, None,
)

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


connections = set()


def new_connection(server, connection):
    connection.register_object("/org/freedesktop/systemd1", manager.interfaces[0], load_unit, None, None)
    connection.register_object(unit_path, service.interfaces[0], None, property_value, None)
    connections.add(connection)
    connection.connect("closed", lambda connection, *unused: connections.discard(connection))
    return True


server.connect("new-connection", new_connection)
server.start()
args.ready_file.touch()
GLib.MainLoop().run()
