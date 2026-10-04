#!/usr/bin/env python3
"""Private-bus daemon peer for installed GUI transport tests, not a GUI bypass.

Reads return only the last publication. Explicit requests emulate a daemon
assessment of the fixture's pairing and service state, outside the read path.
Native service/integration inspections are covered by the Rust runtime tests.
"""
import argparse
import json
import os
from pathlib import Path
import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

INTERFACE = "io.github.Staphylococcus.LGBuddy.Session1"


class Session(dbus.service.Object):
    def __init__(self, bus, config, services, read_error_marker=None):
        self.config = config.resolve()
        self.services = services
        self.read_error_marker = read_error_marker
        self.read_failures = 0
        self.revision = 0
        self.snapshot = {}
        self.instance = str(os.getpid())
        self.assess()
        super().__init__(bus, "/io/github/Staphylococcus/LGBuddy/Session")

    def assess(self):
        values = dict(line.split("=", 1) for line in self.config.read_text().splitlines() if "=" in line)
        token = self.config.parent / "tvs/primary/access-token.json"
        paired = values.get("tvs_primary_platform") == "lg_webos" and token.is_file()
        services_ready = self.services is None or self.services.exists()
        requirements = []
        if not paired:
            requirements.append({"step": "pairing", "reason": "Complete TV details and pairing.", "actionable": True})
        if not services_ready:
            requirements.append({"step": "services", "reason": "Repair background services.", "actionable": True})
        self.revision += 1
        self.snapshot = {"instance": self.instance, "revision": self.revision,
                         "config": str(self.config), "status": "Incomplete" if requirements else "Complete",
                         "requirements": requirements}
        return False

    @dbus.service.method(INTERFACE, in_signature="", out_signature="s")
    def GetSetupAssessment(self):
        if self.read_error_marker is not None and self.read_error_marker.exists():
            self.read_failures += 1
            self.read_error_marker.with_suffix(".observed").write_text(str(self.read_failures))
            raise dbus.exceptions.DBusException(
                "Injected cached-read failure", name="org.freedesktop.DBus.Error.Failed")
        return json.dumps(self.snapshot)

    @dbus.service.method(INTERFACE, in_signature="", out_signature="st")
    def RequestSetupAssessment(self):
        GLib.idle_add(self.assess)
        return self.instance, self.revision + 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--services-ready", type=Path)
    parser.add_argument("--read-error-marker", type=Path)
    parser.add_argument("--ready-file", type=Path, required=True)
    args = parser.parse_args()
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName("io.github.Staphylococcus.LGBuddy", bus, do_not_queue=True)
    session = Session(bus, args.config, args.services_ready, args.read_error_marker)
    args.ready_file.touch()
    GLib.MainLoop().run()
    return name, session


if __name__ == "__main__":
    main()
