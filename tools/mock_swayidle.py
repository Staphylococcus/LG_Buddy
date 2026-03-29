#!/usr/bin/env python3

"""Stateful contract mock for swayidle.

This mock is intentionally narrower than a real swayidle implementation.

It exists to make LG Buddy's current expectations explicit:
- `swayidle -h` exposes the event surface we rely on
- the runtime may eventually invoke swayidle with a sequence of event hooks
- tests can inspect which hooks were configured without needing a Wayland session

The mock records invocations to a JSON state file and exits immediately.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


DEFAULT_STATE = {
    "help_mode": "systemd",
    "emissions": [],
    "invocations": [],
}


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
    state.setdefault("emissions", [])
    state.setdefault("invocations", [])
    return state


def save_state(path: Path, state: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, sort_keys=True)


def record_invocation(state: dict[str, object], invocation: dict[str, object]) -> None:
    invocations = state.setdefault("invocations", [])
    if not isinstance(invocations, list):
        raise TypeError("state invocations must be a list")
    invocations.append(invocation)


def render_help(help_mode: str) -> str:
    lines = [
        "swayidle [options] [events...]",
        "timeout <timeout> <timeout command> [resume <resume command>]",
    ]

    if help_mode == "systemd":
        lines.extend(
            [
                "before-sleep <command>",
                "after-resume <command>",
                "lock <command>",
                "unlock <command>",
                "idlehint <timeout>",
            ]
        )

    return "\n".join(lines) + "\n"


def parse_invocation(argv: list[str]) -> dict[str, object]:
    index = 0
    wait = False
    debug = False
    config_path: str | None = None
    seat: str | None = None

    while index < len(argv):
        token = argv[index]
        if token == "-w":
            wait = True
            index += 1
        elif token == "-d":
            debug = True
            index += 1
        elif token == "-C":
            config_path = argv[index + 1]
            index += 2
        elif token == "-S":
            seat = argv[index + 1]
            index += 2
        else:
            break

    events: list[dict[str, object]] = []
    while index < len(argv):
        token = argv[index]
        if token == "timeout":
            timeout = int(argv[index + 1])
            command = argv[index + 2]
            index += 3
            event: dict[str, object] = {
                "kind": "timeout",
                "timeout": timeout,
                "command": command,
            }
            if index < len(argv) and argv[index] == "resume":
                event["resume"] = argv[index + 1]
                index += 2
            events.append(event)
        elif token in {"before-sleep", "after-resume", "lock", "unlock"}:
            events.append(
                {
                    "kind": token,
                    "command": argv[index + 1],
                }
            )
            index += 2
        elif token == "idlehint":
            events.append(
                {
                    "kind": "idlehint",
                    "timeout": int(argv[index + 1]),
                }
            )
            index += 2
        else:
            raise ValueError(f"unsupported mock swayidle token: {token}")

    return {
        "argv": argv,
        "wait": wait,
        "debug": debug,
        "config_path": config_path,
        "seat": seat,
        "events": events,
    }


def emit_command(command: str) -> None:
    subprocess.run(["/bin/sh", "-c", command], check=False)


def emit_planned_events(state: dict[str, object], invocation: dict[str, object]) -> None:
    emissions = state.get("emissions", [])
    if not isinstance(emissions, list):
        raise TypeError("state emissions must be a list")

    timeout_events = [event for event in invocation["events"] if event["kind"] == "timeout"]

    for emission in emissions:
        if emission == "timeout":
            for event in timeout_events:
                emit_command(str(event["command"]))
        elif emission == "resume":
            for event in timeout_events:
                resume = event.get("resume")
                if resume:
                    emit_command(str(resume))
        elif emission in {"before-sleep", "after-resume", "lock", "unlock"}:
            for event in invocation["events"]:
                if event["kind"] == emission:
                    emit_command(str(event["command"]))

    state["emissions"] = []


def main(argv: list[str]) -> int:
    state_path, args = parse_global_args(argv)
    state = load_state(state_path)

    if args and args[0] in {"-h", "--help"}:
        record_invocation(
            state,
            {
                "argv": args,
                "wait": False,
                "debug": False,
                "config_path": None,
                "seat": None,
                "events": [],
            },
        )
        sys.stdout.write(render_help(str(state.get("help_mode", "systemd"))))
        save_state(state_path, state)
        return 0

    try:
        invocation = parse_invocation(args)
    except (IndexError, ValueError) as err:
        print(str(err), file=sys.stderr)
        save_state(state_path, state)
        return 2

    record_invocation(state, invocation)
    emit_planned_events(state, invocation)
    save_state(state_path, state)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
