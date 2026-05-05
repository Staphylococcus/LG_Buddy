#!/usr/bin/env python3

"""Stateful mock for nm-online."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


DEFAULT_STATE = {
    "status": 0,
    "invocations": [],
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Mock nm-online")
    parser.add_argument(
        "--state",
        required=True,
        help="Path to the JSON file that stores mock nm-online state",
    )
    args, nm_args = parser.parse_known_args()
    args.nm_args = nm_args
    return args


def load_state(path: Path) -> dict[str, object]:
    if not path.exists():
        return DEFAULT_STATE.copy()

    with path.open("r", encoding="utf-8") as handle:
        data = json.load(handle)

    state = DEFAULT_STATE.copy()
    state.update(data)
    state.setdefault("invocations", [])
    return state


def save_state(path: Path, state: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, sort_keys=True)


def main() -> int:
    args = parse_args()
    state_path = Path(args.state)
    state = load_state(state_path)

    invocations = state.setdefault("invocations", [])
    if not isinstance(invocations, list):
        raise TypeError("state invocations must be a list")

    invocations.append({"argv": list(args.nm_args)})
    save_state(state_path, state)
    return int(state.get("status", 0))


if __name__ == "__main__":
    sys.exit(main())
