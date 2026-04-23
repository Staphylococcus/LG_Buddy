#!/usr/bin/env python3

"""Stateful contract mock for gdbus.

This mock is intentionally narrow. It primarily covers the remaining subprocess
surfaces LG Buddy still uses:

- `gdbus call ... NameHasOwner org.gnome.Shell`
- `gdbus wait ... org.gnome.Shell`

It also retains a few legacy GNOME helper surfaces for focused contract tests:

- `gdbus call ... org.gnome.ScreenSaver.GetActive`
- `gdbus call ... org.gnome.Mutter.IdleMonitor.GetIdletime`
- `gdbus monitor ... org.gnome.ScreenSaver`

The live GNOME monitor runtime no longer uses `gdbus monitor` or repeated
`GetIdletime` subprocess calls. Those paths now run through the in-process
session-bus client and the private test-bus harness.

The goal here is to make the remaining subprocess contract explicit without
pretending to be a general D-Bus implementation.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path


DEFAULT_STATE = {
    "shell_available": True,
    "screen_saver_available": True,
    "idle_monitor_available": True,
    "idle_monitor_idletime": 1500,
    "idle_monitor_idletime_plan": [],
    "monitor_lines": [],
    "monitor_sleep_secs": 0.0,
    "invocations": [],
}

GNOME_SCREEN_SAVER_NAME = "org.gnome.ScreenSaver"
GNOME_SCREEN_SAVER_PATH = "/org/gnome/ScreenSaver"
GNOME_IDLE_MONITOR_NAME = "org.gnome.Mutter.IdleMonitor"
GNOME_IDLE_MONITOR_PATH = "/org/gnome/Mutter/IdleMonitor/Core"


def parse_global_args(argv: list[str]) -> tuple[Path, list[str]]:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--state", required=True)
    parsed, remaining = parser.parse_known_args(argv)
    return Path(parsed.state), remaining


def load_state(path: Path) -> dict[str, object]:
    if not path.exists():
        return DEFAULT_STATE.copy()

    with path.open("r", encoding="utf-8") as handle:
        data = json.load(handle)

    state = DEFAULT_STATE.copy()
    state.update(data)
    state.setdefault("idle_monitor_idletime_plan", [])
    state.setdefault("monitor_lines", [])
    state.setdefault("invocations", [])
    return state


def save_state(path: Path, state: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp_path = path.with_suffix(f"{path.suffix}.tmp.{os.getpid()}")
    with temp_path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, sort_keys=True)
    temp_path.replace(path)


def record_invocation(state: dict[str, object], argv: list[str]) -> None:
    invocations = state.setdefault("invocations", [])
    if not isinstance(invocations, list):
        raise TypeError("state invocations must be a list")

    invocations.append({"argv": argv})


def option_value(argv: list[str], flag: str) -> str | None:
    if flag not in argv:
        return None

    index = argv.index(flag)
    if index + 1 >= len(argv):
        return None

    return argv[index + 1]


def print_dbus_error(message: str) -> int:
    print(message, file=sys.stderr)
    return 1


def handle_call(state: dict[str, object], argv: list[str]) -> int:
    dest = option_value(argv, "--dest")
    object_path = option_value(argv, "--object-path")
    method = option_value(argv, "--method")

    if method == "org.freedesktop.DBus.NameHasOwner":
        name = argv[-1]
        availability = {
            "org.gnome.Shell": bool(state.get("shell_available", True)),
            GNOME_SCREEN_SAVER_NAME: bool(state.get("screen_saver_available", True)),
            GNOME_IDLE_MONITOR_NAME: bool(state.get("idle_monitor_available", True)),
        }.get(name, False)
        if availability:
            sys.stdout.write("(true,)\n")
        else:
            sys.stdout.write("(false,)\n")
        sys.stdout.flush()
        return 0

    if (
        dest == GNOME_SCREEN_SAVER_NAME
        and object_path == GNOME_SCREEN_SAVER_PATH
        and method == "org.gnome.ScreenSaver.GetActive"
    ):
        if not bool(state.get("screen_saver_available", True)):
            return print_dbus_error(
                "Error: GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: org.gnome.ScreenSaver is unavailable"
            )
        sys.stdout.write("(false,)\n")
        sys.stdout.flush()
        return 0

    if (
        dest == GNOME_IDLE_MONITOR_NAME
        and object_path == GNOME_IDLE_MONITOR_PATH
        and method == "org.gnome.Mutter.IdleMonitor.GetIdletime"
    ):
        if not bool(state.get("idle_monitor_available", True)):
            return print_dbus_error(
                "Error: GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: org.gnome.Mutter.IdleMonitor is unavailable"
            )
        idletime_plan = state.get("idle_monitor_idletime_plan", [])
        if not isinstance(idletime_plan, list):
            raise TypeError("state idle_monitor_idletime_plan must be a list")

        if idletime_plan:
            idletime = int(idletime_plan.pop(0))
        else:
            idletime = int(state.get("idle_monitor_idletime", 1500))
        sys.stdout.write(f"(uint64 {idletime},)\n")
        sys.stdout.flush()
        return 0

    return print_dbus_error(f"unsupported mock gdbus call: {' '.join(argv)}")


def handle_wait(state: dict[str, object], argv: list[str]) -> int:
    if not argv or argv[-1] != "org.gnome.Shell":
        return print_dbus_error(f"unsupported mock gdbus wait: {' '.join(argv)}")

    return 0 if bool(state.get("shell_available", True)) else 1


def handle_monitor(state: dict[str, object], argv: list[str]) -> int:
    dest = option_value(argv, "--dest")
    object_path = option_value(argv, "--object-path")

    if dest != GNOME_SCREEN_SAVER_NAME or object_path != GNOME_SCREEN_SAVER_PATH:
        return print_dbus_error(f"unsupported mock gdbus monitor: {' '.join(argv)}")

    if not bool(state.get("screen_saver_available", True)):
        return print_dbus_error(
            "Error: GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: org.gnome.ScreenSaver is unavailable"
        )

    monitor_lines = state.get("monitor_lines", [])
    if not isinstance(monitor_lines, list):
        raise TypeError("state monitor_lines must be a list")

    for line in monitor_lines:
        sys.stdout.write(f"{line}\n")
        sys.stdout.flush()

    monitor_sleep_secs = float(state.get("monitor_sleep_secs", 0.0))
    if monitor_sleep_secs > 0:
        time.sleep(monitor_sleep_secs)

    return 0


def main(argv: list[str]) -> int:
    state_path, args = parse_global_args(argv)
    state = load_state(state_path)
    record_invocation(state, args)

    if not args:
        save_state(state_path, state)
        return print_dbus_error("missing gdbus subcommand")

    command = args[0]
    if command == "call":
        exit_code = handle_call(state, args)
    elif command == "wait":
        exit_code = handle_wait(state, args)
    elif command == "monitor":
        exit_code = handle_monitor(state, args)
    else:
        exit_code = print_dbus_error(f"unsupported mock gdbus command: {command}")

    save_state(state_path, state)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
