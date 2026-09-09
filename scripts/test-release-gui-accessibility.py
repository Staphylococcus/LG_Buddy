#!/usr/bin/env python3

from __future__ import annotations

import argparse
import math
import subprocess
import sys
import time

import pyatspi


WINDOW_TITLE = "LG Buddy"
CONTROL_NAME = "OLED Pixel Brightness"
VOLUME_NAME = "TV Volume"
DEFAULT_TIMEOUT_SECONDS = 10
# Settings uses direct native rows; keep traversal bounded while the UI updates.
MAX_ACCESSIBLES = 1024


def accessible_tree(root: object | None = None) -> list[object]:
    pending = [root if root is not None else pyatspi.Registry.getDesktop(0)]
    observed: list[object] = []
    while pending and len(observed) < MAX_ACCESSIBLES:
        accessible = pending.pop()
        observed.append(accessible)
        try:
            pending.extend(reversed(list(accessible)))
        except Exception:  # Defunct remote accessibles are expected during traversal.
            continue
    return observed


def name(accessible: object) -> str:
    try:
        return str(accessible.name or "")
    except Exception:  # The remote accessible may disappear between queries.
        return ""


def role(accessible: object) -> object | None:
    try:
        return accessible.getRole()
    except Exception:  # The remote accessible may disappear between queries.
        return None


def role_name(accessible: object) -> str:
    try:
        return str(accessible.getRoleName())
    except Exception:  # The remote accessible may disappear between queries.
        return "unknown"


def normalized_name(accessible: object) -> str:
    return name(accessible).replace("_", "")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Wait for and verify the installed GTK GUI's AT-SPI contract."
    )
    parser.add_argument(
        "--expected-state",
        choices=("ready", "read-failed"),
        default="ready",
        help="presentation state to observe (default: ready)",
    )
    parser.add_argument(
        "--expected-slider-value",
        type=float,
        help="wait until the ready slider exposes this value",
    )
    parser.add_argument(
        "--expected-volume",
        type=float,
        help="also wait for an enabled volume control exposing this value",
    )
    parser.add_argument(
        "--expected-muted",
        choices=("true", "false"),
        help="also wait for an enabled mute control in this state",
    )
    parser.add_argument(
        "--require-audio-retry",
        action="store_true",
        help="also wait for an enabled audio recovery action",
    )
    parser.add_argument(
        "--require-brightness-focus",
        action="store_true",
        help="verify that the brightness deep link initially focuses its slider",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT_SECONDS,
        help=f"maximum observation time in seconds (default: {DEFAULT_TIMEOUT_SECONDS})",
    )
    parser.add_argument("--select-page", choices=("Overview", "TVs", "Settings"))
    parser.add_argument("--expected-settings-state", choices=("ready", "invalid"))
    parser.add_argument("--expected-settings-timeout")
    parser.add_argument("--edit-settings-timeout", help="type a timeout draft through native keyboard input")
    parser.add_argument("--expected-tvs-state", choices=("empty", "configured", "pairing", "pairing-invalid", "unpair"))
    parser.add_argument("--expected-tv-address")
    parser.add_argument("--expected-tv-name", default="Primary TV")
    parser.add_argument("--focus-control", help="focus a control using native Tab navigation")
    parser.add_argument("--activate-control", help="activate an accessible button")
    parser.add_argument("--window-id", help="X window used for keyboard navigation")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be greater than zero")
    if args.expected_state != "ready" and args.expected_slider_value is not None:
        parser.error("--expected-slider-value requires --expected-state ready")
    return args


def observed_contract(expected_state: str, expected_slider_value: float | None):
    accessibles = accessible_tree()
    if not any(name(item) == WINDOW_TITLE for item in accessibles):
        return None
    if any(normalized_name(item) in ("Apply", "Apply Volume", "Cancel")
           and item.getState().contains(pyatspi.STATE_SHOWING) for item in accessibles):
        raise SystemExit("Overview still exposes a removed action")
    slider = next((item for item in accessibles
        if role(item) == pyatspi.ROLE_SLIDER and name(item) == CONTROL_NAME), None)
    if expected_state == "read-failed":
        retry = next((item for item in accessibles
            if name(item) == "Retry OLED Pixel Brightness"), None)
        alert = any(role(item) == pyatspi.ROLE_ALERT and name(item) for item in accessibles)
        return (accessibles, None) if slider is None and retry is not None and alert else None
    if slider is None:
        return None
    try:
        if not all(slider.getState().contains(state) for state in (
            pyatspi.STATE_FOCUSABLE, pyatspi.STATE_SENSITIVE,
        )):
            return None
        slider_value = float(slider.queryValue().currentValue)
    except Exception:
        return None
    if not 0 <= slider_value <= 100:
        return None
    if expected_slider_value is not None and not math.isclose(slider_value, expected_slider_value, abs_tol=0.01):
        return None
    return accessibles, slider_value


def audio_contract(accessibles: list[object], args: argparse.Namespace) -> bool:
    try:
        if args.require_audio_retry and not any(
            role(item) == pyatspi.ROLE_PUSH_BUTTON
            and normalized_name(item) == "Retry Audio"
            and item.getState().contains(pyatspi.STATE_SENSITIVE)
            for item in accessibles
        ):
            return False
        if args.expected_volume is not None:
            volume = next((item for item in accessibles
                if role(item) == pyatspi.ROLE_SLIDER and name(item) == VOLUME_NAME), None)
            if volume is None or not volume.getState().contains(pyatspi.STATE_SENSITIVE):
                return False
            if not math.isclose(float(volume.queryValue().currentValue), args.expected_volume,
                                abs_tol=0.01):
                return False
        if args.expected_muted is not None:
            mute = next((item for item in accessibles
                if normalized_name(item) in ("Mute TV", "Unmute TV")
                and item.getState().contains(pyatspi.STATE_FOCUSABLE)), None)
            if mute is None or not mute.getState().contains(pyatspi.STATE_SENSITIVE):
                return False
            if (name(mute) == "Unmute TV") != (args.expected_muted == "true"):
                return False
        if args.require_brightness_focus:
            return any(role(item) == pyatspi.ROLE_SLIDER and name(item) == CONTROL_NAME
                and item.getState().contains(pyatspi.STATE_FOCUSED) for item in accessibles)
    except Exception:  # Rendering may update the remote accessibles during observation.
        return False
    return True


def tvs_contract(expected_state: str, address: str | None, tv_name: str):
    accessibles = accessible_tree()
    visible = []
    for item in accessibles:
        try:
            if item.getState().contains(pyatspi.STATE_SHOWING):
                visible.append(item)
        except Exception:
            continue
    names = {normalized_name(item) for item in visible}
    dialogs = [item for item in visible if role(item) == pyatspi.ROLE_DIALOG]
    if expected_state == "unpair":
        return (accessibles, None) if {"Unpair TV?", "Cancel", "Unpair"} <= names else None
    if expected_state in ("pairing", "pairing-invalid"):
        if len(dialogs) != 1 or not {"TV address", "MAC address", "HDMI input", "Cancel", "Pair"} <= names:
            return None
        dialog_names = {normalized_name(item) for item in accessible_tree(dialogs[0])}
        if "Close" in dialog_names:
            raise SystemExit("Pairing exposed a close button alongside Cancel")
        if expected_state == "pairing-invalid":
            if not any(role(item) == pyatspi.ROLE_ALERT and "address" in name(item).lower()
                       for item in visible):
                return None
        elif not any(role(item) == pyatspi.ROLE_TEXT
                     and item.getState().contains(pyatspi.STATE_FOCUSED) for item in visible):
            return None
        return accessibles, None
    if dialogs:
        return None
    if "Add TV" in names:
        raise SystemExit("TVs exposed an unsupported second-TV action")
    if expected_state == "empty":
        return (accessibles, None) if {"No TV configured", "Pair a TV"} <= names else None
    if any(value in names for value in ("Pair a TV", "Pair a TV…")):
        raise SystemExit("A configured TV exposed the first-TV pairing action")
    if tv_name not in names or (address and address not in names):
        return None
    return accessibles, None


def text_value(accessible: object) -> str:
    try:
        text = accessible.queryText()
        return str(text.getText(0, text.characterCount))
    except Exception:
        return ""


def settings_contract(args: argparse.Namespace):
    accessibles = accessible_tree()
    names = {name(item) for item in accessibles}
    if not {"Screen", "Sleep & Wake", "Updates", "Desktop integration", "Idle blanking",
            "Idle timeout", "Restore policy", "TV sleep & wake", "Automatic update checks",
            "Update channel", "Check for updates", "Installed version"} <= names:
        return None
    if args.expected_settings_timeout:
        timeout_entry = next(
            (
                item
                for item in accessibles
                if name(item) == "Idle timeout" and role(item) == pyatspi.ROLE_TEXT
            ),
            None,
        )
        if timeout_entry is None or text_value(timeout_entry) != args.expected_settings_timeout:
            return None
    visible_names = set()
    for item in accessibles:
        try:
            if item.getState().contains(pyatspi.STATE_SHOWING):
                visible_names.add(name(item))
        except Exception:
            continue
    invalid_values = any(
        value.startswith("Invalid value") or value.startswith("Invalid configured value")
        for value in visible_names
    )
    if args.expected_settings_state == "invalid" and not invalid_values:
        return None
    if args.expected_settings_state == "ready" and invalid_values:
        return None
    return accessibles, None


def main() -> int:
    args = parse_args()
    if args.select_page:
        deadline = time.monotonic() + args.timeout
        # Native button activation may finish after the AT-SPI call returns.
        activated = False
        while time.monotonic() < deadline:
            for item in accessible_tree():
                try:
                    if (normalized_name(item) == args.select_page
                            and role(item) == pyatspi.ROLE_PAGE_TAB
                            and item.getState().contains(pyatspi.STATE_SHOWING)):
                        if item.getState().contains(pyatspi.STATE_SELECTED):
                            return 0
                        if not activated:
                            activated = item.queryAction().doAction(0)
                except Exception:
                    continue
            time.sleep(0.1)
        raise SystemExit(f"could not select the {args.select_page} tab")
    if args.edit_settings_timeout is not None:
        if not args.window_id:
            raise SystemExit("--edit-settings-timeout needs --window-id")
        accessibles = accessible_tree()
        entry = next((item for item in accessibles if name(item) == "Idle timeout"
                      and role(item) == pyatspi.ROLE_TEXT
                      and item.getState().contains(pyatspi.STATE_SHOWING)), None)
        if entry is None:
            deadline = time.monotonic() + args.timeout
            while entry is None and time.monotonic() < deadline:
                subprocess.run(["xdotool", "key", "--window", args.window_id, "Tab"], check=True)
                entry = next((item for item in accessible_tree() if name(item) == "Idle timeout"
                              and role(item) == pyatspi.ROLE_TEXT
                              and item.getState().contains(pyatspi.STATE_SHOWING)), None)
                time.sleep(0.05)
        if entry is None:
            raise SystemExit("could not find the Idle timeout editor")
        for _ in range(40):
            if any(name(item) == "Idle timeout" and role(item) == pyatspi.ROLE_TEXT
                   and item.getState().contains(pyatspi.STATE_FOCUSED)
                   for item in accessible_tree()):
                break
            subprocess.run(["xdotool", "key", "--window", args.window_id, "Tab"], check=True)
            time.sleep(0.05)
        else:
            raise SystemExit("could not focus the Idle timeout editor through Tab navigation")
        subprocess.run(["xdotool", "key", "--window", args.window_id, "ctrl+a"], check=True)
        subprocess.run(["xdotool", "type", "--window", args.window_id, "--clearmodifiers", "--", args.edit_settings_timeout], check=True)
        return 0
    if args.focus_control:
        if not args.window_id:
            raise SystemExit("--focus-control needs --window-id")
        for _ in range(20):
            if any(name(item) == args.focus_control and item.getState().contains(pyatspi.STATE_FOCUSED)
                   for item in accessible_tree()):
                return 0
            subprocess.run(["xdotool", "key", "--window", args.window_id, "Tab"], check=True)
            time.sleep(0.1)
        raise SystemExit(f"could not focus {args.focus_control} through Tab navigation")
    if args.activate_control:
        accessibles = accessible_tree()
        dialog = next((item for item in accessibles if role(item) == pyatspi.ROLE_DIALOG
                       and item.getState().contains(pyatspi.STATE_SHOWING)), None)
        for item in accessible_tree(dialog) if dialog is not None else accessibles:
            if (name(item) == args.activate_control
                    and role(item) in (pyatspi.ROLE_PUSH_BUTTON, pyatspi.ROLE_TOGGLE_BUTTON)
                    and item.getState().contains(pyatspi.STATE_SHOWING)
                    and item.getState().contains(pyatspi.STATE_SENSITIVE)):
                if item.queryAction().doAction(0):
                    return 0
        raise SystemExit(f"could not activate {args.activate_control}")
    deadline = time.monotonic() + args.timeout
    contract = None
    while time.monotonic() < deadline:
        if args.expected_settings_state:
            contract = settings_contract(args)
        elif args.expected_tvs_state:
            contract = tvs_contract(args.expected_tvs_state, args.expected_tv_address, args.expected_tv_name)
        else:
            contract = observed_contract(args.expected_state, args.expected_slider_value)
        if contract is not None and (args.expected_settings_state or args.expected_tvs_state or audio_contract(contract[0], args)):
            break
        contract = None
        time.sleep(0.1)

    if contract is None:
        for item in accessible_tree():
            if not name(item):
                continue
            try:
                states = item.getState()
                flags = [label for state, label in (
                    (pyatspi.STATE_FOCUSABLE, "focusable"),
                    (pyatspi.STATE_FOCUSED, "focused"),
                    (pyatspi.STATE_SENSITIVE, "sensitive"),
                    (pyatspi.STATE_CHECKED, "checked"),
                ) if states.contains(state)]
                print(f"  {role_name(item)}: {name(item)} [{', '.join(flags)}]", file=sys.stderr)
                if role(item) == pyatspi.ROLE_SLIDER:
                    print(f"    value: {item.queryValue().currentValue}", file=sys.stderr)
            except Exception:
                continue
        expected = f" {args.expected_settings_state or args.expected_tvs_state or args.expected_state} state"
        if args.expected_slider_value is not None:
            expected += f" at slider value {args.expected_slider_value:g}"
        raise SystemExit(
            f"installed GUI did not expose its expected focusable{expected} over AT-SPI"
        )

    accessibles, slider_value = contract

    observed = sorted(
        {(role_name(item), name(item)) for item in accessibles if name(item)}
    )
    print("AT-SPI accessibility contract verified:")
    print(f"  presentation state: {args.expected_settings_state or args.expected_tvs_state or args.expected_state}")
    if slider_value is not None:
        print(f"  slider value: {slider_value:g}")
    for observed_role, accessible_name in observed:
        print(f"  {observed_role}: {accessible_name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
