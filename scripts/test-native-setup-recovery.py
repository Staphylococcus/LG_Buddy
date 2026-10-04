#!/usr/bin/env python3
"""Installed setup checks on a disposable VM's real desktop and session bus."""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import os
from pathlib import Path
import subprocess
import time

import dbus
import pyatspi


SERVICE = "LG_Buddy_screen.service"
BUS_NAME = "io.github.Staphylococcus.LGBuddy"
GUI_NAME = "io.github.staphylococcus.LGBuddy"
SESSION_PATH = "/io/github/Staphylococcus/LGBuddy/Session"
SESSION_INTERFACE = BUS_NAME + ".Session1"
ui = importlib.import_module("test-release-gui-accessibility")


def run(*arguments: str) -> str:
    return subprocess.check_output(arguments, text=True, timeout=30).strip()


def wait(description, predicate, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.1)
    raise AssertionError(f"timed out waiting for {description}")


def controls():
    apps = [app for app in pyatspi.Registry.getDesktop(0)
            if ui.name(app) in {"lg-buddy-gui", ".lg-buddy-gui-wrapped"}]
    return [item for app in apps for item in ui.accessible_tree(app) if ui.is_showing(item)]


def names():
    return {ui.name(item) for item in controls()}


def activate(label):
    def attempt():
        for item in controls():
            if ui.name(item) == label and ui.role(item) == pyatspi.ROLE_PUSH_BUTTON and ui.is_sensitive(item):
                if item.queryAction().doAction(0):
                    return True
        return False
    wait(label, attempt)


def snapshot(bus):
    proxy = bus.get_object(BUS_NAME, SESSION_PATH, introspect=False)
    return json.loads(dbus.Interface(proxy, SESSION_INTERFACE).GetSetupAssessment(timeout=2))


def complete(bus):
    result = snapshot(bus)
    assert result["status"] == "Complete", result
    return result


def gui_process(bus):
    peer = bus.get_object("org.freedesktop.DBus", "/org/freedesktop/DBus")
    return int(dbus.Interface(peer, "org.freedesktop.DBus").GetConnectionUnixProcessID(GUI_NAME))


def functional():
    return {"Overview", "TVs", "Settings"} <= names()


def activate_gnome_notification(bus):
    remote_name = "org.gnome.Mutter.RemoteDesktop"
    remote = dbus.Interface(bus.get_object(remote_name, "/org/gnome/Mutter/RemoteDesktop"), remote_name)
    path = remote.CreateSession()
    session = dbus.Interface(bus.get_object(remote_name, path), remote_name + ".Session")
    properties = dbus.Interface(bus.get_object(remote_name, path), "org.freedesktop.DBus.Properties")
    identifier = properties.Get(remote_name + ".Session", "SessionId")
    cast_name = "org.gnome.Mutter.ScreenCast"
    cast = dbus.Interface(bus.get_object(cast_name, "/org/gnome/Mutter/ScreenCast"), cast_name)
    cast_path = cast.CreateSession({"remote-desktop-session-id": dbus.String(identifier)})
    cast_session = dbus.Interface(bus.get_object(cast_name, cast_path), cast_name + ".Session")
    stream = cast_session.RecordMonitor("", dbus.Dictionary({}, signature="sv"))
    session.Start()
    try:
        # Native compositor input, not a forged ActionInvoked signal.
        session.NotifyKeyboardKeysym(0xffeb, True)
        session.NotifyKeyboardKeysym(ord("v"), True)
        session.NotifyKeyboardKeysym(ord("v"), False)
        session.NotifyKeyboardKeysym(0xffeb, False)
        def notice():
            for app in pyatspi.Registry.getDesktop(0):
                if ui.name(app) == "gnome-shell":
                    ui.MAX_ACCESSIBLES = 32768
                    for item in ui.accessible_tree(app):
                        if ui.name(item) == "LG Buddy needs attention" and ui.is_showing(item):
                            rectangle = item.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
                            if rectangle.x >= 0 and rectangle.y >= 0 and rectangle.width > 0 and rectangle.height > 0:
                                return rectangle
            return None
        rectangle = wait("real GNOME setup notification", notice)
        session.NotifyPointerMotionAbsolute(stream, float(rectangle.x + rectangle.width / 2), float(rectangle.y + rectangle.height / 2))
        session.NotifyPointerButton(272, True)
        session.NotifyPointerButton(272, False)
        wait("ordinary GUI launch", lambda: bus.name_has_owner(GUI_NAME))
    finally:
        session.Stop()


def digest(path):
    with open(path, "rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", choices=("cached-open", "retained-admission", "daemon-delay", "notification-open", "repair", "inspect"))
    parser.add_argument("--disposable-vm", required=True, help="exact hostname of the disposable VM")
    parser.add_argument("--source", required=True, help="candidate commit plus any explicit patch identity")
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() == 0:
        parser.error("run as the logged-in desktop user")
    if run("hostname") != args.disposable_vm:
        parser.error("disposable VM hostname mismatch")
    run("systemd-detect-virt", "--vm")
    if any(key.startswith("LG_BUDDY_") for key in os.environ):
        parser.error("native checks reject runtime overrides")
    bus = dbus.SessionBus()
    if bus.name_has_owner(GUI_NAME):
        parser.error("close any existing GUI before running a scenario")
    args.evidence.mkdir(parents=True, exist_ok=False)
    record = {
        "outcome": "failed",
        "scenario": args.scenario,
        "source": args.source,
        "os": Path("/etc/os-release").read_text(),
        "boot": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
        "substituted_boundary": "loopback webOS TV only; no assessment, authorization, service or compositor peer",
        "installed_sha256": {name: digest(name)
                             for name in ("/usr/bin/lg-buddy", "/usr/bin/lg-buddy-gui")},
        "service": run("systemctl", "--user", "show", SERVICE, "-p", "MainPID", "-p", "ActiveState", "-p", "ExecStart"),
    }
    gui_pid = None
    owns_gui = False
    stopped = False
    try:
        record["before"] = snapshot(bus)
        if args.scenario == "inspect":
            record["outcome"] = "observed"
            return
        if args.scenario == "notification-open":
            assert record["before"]["status"] == "Incomplete"
            activate_gnome_notification(bus)
            owns_gui = True
            gui_pid = gui_process(bus)
            wait("single CTA gate", lambda: "Complete setup" in names())
            assert not functional()
            assert "Cancel" not in names()
            assert not any(ui.role(item) == pyatspi.ROLE_DIALOG for item in controls())
            record["splash_controls"] = sorted(names())
            record["outcome"] = "passed"
            return
        expected = "Incomplete" if args.scenario == "repair" else "Complete"
        assert record["before"]["status"] == expected, record["before"]
        if args.scenario == "daemon-delay":
            run("systemctl", "--user", "kill", "--kill-who=main", "--signal=STOP", SERVICE)
            stopped = True
        run("systemd-run", "--user", "--collect", f"--unit=lg-buddy-native-test-{os.getpid()}", "/usr/bin/lg-buddy-gui")
        owns_gui = True
        wait("GUI session owner", lambda: bus.name_has_owner(GUI_NAME))
        gui_pid = gui_process(bus)
        if args.scenario == "repair":
            wait("single CTA gate", lambda: "Complete setup" in names())
            assert not functional()
            assert "Cancel" not in names()
            activate("Complete setup")
            record["pages"] = []
            def repaired():
                page = sorted(names())
                if not record["pages"] or record["pages"][-1] != page:
                    record["pages"].append(page)
                    print(json.dumps(page), flush=True)
                return functional()
            # Consent, pairing and native authentication remain user decisions.
            wait("native repair and verified admission", repaired, 3600)
        if args.scenario == "daemon-delay":
            wait("initial verification gate", lambda: "Complete setup" in names())
            assert not functional()
            activate("Complete setup")
            wait("existing setup modal", lambda: "Verifying setup" in names() and "Cancel" in names())
            wait("bounded verification recovery", lambda: "Retry verification" in names(), 60)
            assert not functional()
            record["paused_controls"] = sorted(names())
            run("systemctl", "--user", "kill", "--kill-who=main", "--signal=CONT", SERVICE)
            stopped = False
            # Cached polling may observe the resumed daemon before the click.
            def recover():
                if functional():
                    return True
                for item in controls():
                    if ui.name(item) == "Retry verification" and ui.role(item) == pyatspi.ROLE_PUSH_BUTTON and ui.is_sensitive(item):
                        return item.queryAction().doAction(0)
                return False
            wait("verification retry or fresh publication", recover)
        wait("functional installed GUI", functional)
        assert "Complete setup" not in names()
        record["admitted"] = complete(bus)
        if args.scenario == "repair":
            before, after = record["before"], record["admitted"]
            assert before["instance"] != after["instance"] or after["revision"] > before["revision"], "repair did not publish a fresh assessment"
        if args.scenario == "cached-open":
            assert record["before"] == record["admitted"], "opening the GUI triggered a new assessment"
        if args.scenario == "retained-admission":
            run("systemctl", "--user", "kill", "--kill-who=main", "--signal=STOP", SERVICE)
            stopped = True
            # Longer than the production cached-read deadline; no replacement peer.
            time.sleep(5)
            assert functional(), "a transient cached read revoked verified admission"
            assert "Complete setup" not in names()
            record["retained_controls"] = sorted(names())
        record["outcome"] = "passed"
    except BaseException as error:
        record["error"] = repr(error)
        raise
    finally:
        try:
            if stopped:
                run("systemctl", "--user", "kill", "--kill-who=main", "--signal=CONT", SERVICE)
            if owns_gui:
                subprocess.run(["gapplication", "action", GUI_NAME, "quit"], check=False, timeout=10)
            wait("GUI cleanup", lambda: not bus.name_has_owner(GUI_NAME))
            if gui_pid is not None:
                wait("GUI process exit", lambda: not Path(f"/proc/{gui_pid}").exists())
            try:
                record["after"] = snapshot(bus)
            except dbus.DBusException as error:
                record["after_error"] = str(error)
            record["workers"] = run("ps", "-u", str(os.getuid()), "-o", "pid,ppid,args")
        except BaseException as error:
            record["outcome"] = "failed"
            record["cleanup_error"] = repr(error)
            raise
        finally:
            (args.evidence / "result.json").write_text(json.dumps(record, indent=2) + "\n")


if __name__ == "__main__":
    main()
