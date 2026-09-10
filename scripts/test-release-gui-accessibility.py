#!/usr/bin/env python3

from __future__ import annotations

import argparse
import math
from pathlib import Path
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
    parser.add_argument(
        "--expected-updater-state",
        choices=(
            "idle",
            "checking",
            "available",
            "confirmation",
            "downloading",
        ),
        help="also verify the compact update row or its installation dialog",
    )
    parser.add_argument("--expected-settings-timeout")
    parser.add_argument("--edit-settings-timeout", help="type a timeout draft through native keyboard input")
    parser.add_argument("--expected-tvs-state", choices=("empty", "configured", "pairing", "pairing-invalid", "unpair"))
    parser.add_argument("--expected-tv-address")
    parser.add_argument("--expected-tv-name", default="Primary TV")
    parser.add_argument("--edit-pairing-address", help="type a TV address into the pairing form")
    parser.add_argument("--edit-pairing-mac", help="type a MAC address into the pairing form")
    parser.add_argument(
        "--expected-diagnostics-state",
        choices=("collecting", "report", "error"),
        help="also verify the visible Diagnostics dialog state",
    )
    parser.add_argument(
        "--expected-text",
        dest="expected_text",
        help="also require this text in a visible accessible name or text value",
    )
    parser.add_argument(
        "--expected-absent-text",
        help="also wait until this text is absent from visible accessible names and text values",
    )
    parser.add_argument(
        "--expected-toggle",
        action="append",
        default=[],
        metavar="NAME=on|off",
        help="also require a named toggle to have the requested checked state (repeatable)",
    )
    parser.add_argument(
        "--read-diagnostics",
        metavar="PATH",
        help="write the visible Diagnostic report text to PATH",
    )
    parser.add_argument(
        "--copy-diagnostics",
        metavar="PATH",
        help="activate Copy and write the resulting clipboard text to PATH",
    )
    parser.add_argument(
        "--save-diagnostics",
        metavar="PATH",
        help="activate Save and complete the native chooser with PATH",
    )
    parser.add_argument("--focus-control", help="focus a control using native Tab navigation")
    parser.add_argument("--activate-control", help="activate an accessible button")
    parser.add_argument("--window-id", help="X window used for keyboard navigation")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be greater than zero")
    if args.expected_state != "ready" and args.expected_slider_value is not None:
        parser.error("--expected-slider-value requires --expected-state ready")
    if (args.edit_pairing_address is not None or args.edit_pairing_mac is not None) and not args.window_id:
        parser.error("pairing edits need --window-id")
    for expectation in args.expected_toggle:
        toggle_name, separator, toggle_state = expectation.partition("=")
        if not separator or not toggle_name.strip() or toggle_state.lower() not in ("on", "off", "true", "false"):
            parser.error(f"invalid --expected-toggle {expectation!r}; use NAME=on|off")
    if sum(value is not None for value in (
        args.read_diagnostics,
        args.copy_diagnostics,
        args.save_diagnostics,
    )) > 1:
        parser.error("choose only one diagnostics export action")
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
        pair = next(
            (
                item for item in visible
                if name(item) == "Pair"
                and role(item) == pyatspi.ROLE_PUSH_BUTTON
            ),
            None,
        )
        cancel = next(
            (
                item for item in visible
                if name(item) == "Cancel"
                and role(item) == pyatspi.ROLE_PUSH_BUTTON
            ),
            None,
        )
        if pair is None or cancel is None or not is_sensitive(cancel):
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
        if any(role(item) == pyatspi.ROLE_PAGE_TAB for item in visible):
            return None
        return (accessibles, None) if {"No TV configured", "Pair a TV", "Main Menu"} <= names else None
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


def is_showing(accessible: object) -> bool:
    try:
        return accessible.getState().contains(pyatspi.STATE_SHOWING)
    except Exception:
        return False


def is_sensitive(accessible: object) -> bool:
    try:
        return accessible.getState().contains(pyatspi.STATE_SENSITIVE)
    except Exception:
        return False


def is_focused(accessible: object) -> bool:
    try:
        return accessible.getState().contains(pyatspi.STATE_FOCUSED)
    except Exception:
        return False


def is_text_control(accessible: object) -> bool:
    text_roles = {pyatspi.ROLE_TEXT}
    entry_role = getattr(pyatspi, "ROLE_ENTRY", None)
    if entry_role is not None:
        text_roles.add(entry_role)
    return role(accessible) in text_roles


def is_checked(accessible: object) -> bool:
    try:
        return accessible.getState().contains(pyatspi.STATE_CHECKED)
    except Exception:
        return False


def is_toggle_control(accessible: object) -> bool:
    return role(accessible) in {
        pyatspi.ROLE_CHECK_BOX,
        pyatspi.ROLE_TOGGLE_BUTTON,
    }


def wait_for_accessible(predicate, timeout: float, description: str):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for item in accessible_tree():
            try:
                if predicate(item):
                    return item
            except Exception:
                continue
        time.sleep(0.05)
    raise SystemExit(f"could not observe {description} over AT-SPI")


def visible_dialog():
    return next(
        (
            item
            for item in accessible_tree()
            if role(item) == pyatspi.ROLE_DIALOG and is_showing(item)
        ),
        None,
    )


def showing_named(name_value: str, roles=None, root: object | None = None):
    allowed_roles = set(roles) if roles is not None else None
    return next(
        (
            item
            for item in accessible_tree(root)
            if name(item) == name_value
            and is_showing(item)
            and (allowed_roles is None or role(item) in allowed_roles)
        ),
        None,
    )


def activate_control(control_name: str, timeout: float) -> None:
    action_roles = {
        pyatspi.ROLE_PUSH_BUTTON,
        pyatspi.ROLE_TOGGLE_BUTTON,
        pyatspi.ROLE_MENU_ITEM,
        pyatspi.ROLE_CHECK_BOX,
    }
    for optional_role_name in ("ROLE_MENU_BUTTON",):
        optional_role = getattr(pyatspi, optional_role_name, None)
        if optional_role is not None:
            action_roles.add(optional_role)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        dialog = visible_dialog()
        candidates = accessible_tree(dialog) if dialog is not None else accessible_tree()
        for item in candidates:
            try:
                if (
                    name(item) == control_name
                    and role(item) in action_roles
                    and is_showing(item)
                    and is_sensitive(item)
                    and item.queryAction().doAction(0)
                ):
                    return
            except Exception:
                continue
        time.sleep(0.05)
    for item in accessible_tree():
        if name(item):
            print(f"  {role_name(item)}: {name(item)!r} showing={is_showing(item)} sensitive={is_sensitive(item)}", file=sys.stderr)
    raise SystemExit(f"could not activate {control_name}")


def focus_control(control_name: str, window_id: str, timeout: float, roles=None) -> None:
    subprocess.run(["xdotool", "windowfocus", "--sync", window_id], check=True)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if any(
            name(item) == control_name
            and (roles is None or role(item) in roles)
            and is_showing(item)
            and is_focused(item)
            for item in accessible_tree()
        ):
            return
        subprocess.run(["xdotool", "key", "--window", window_id, "Tab"], check=True)
        time.sleep(0.05)
    raise SystemExit(f"could not focus {control_name} through Tab navigation")


def edit_text_control(control_name: str, value: str, window_id: str, timeout: float) -> None:
    text_roles = {pyatspi.ROLE_TEXT}
    entry_role = getattr(pyatspi, "ROLE_ENTRY", None)
    if entry_role is not None:
        text_roles.add(entry_role)
    focus_control(control_name, window_id, timeout, text_roles)
    subprocess.run(["xdotool", "key", "--window", window_id, "ctrl+a"], check=True)
    subprocess.run(
        ["xdotool", "type", "--window", window_id, "--clearmodifiers", "--", value],
        check=True,
    )
    wait_for_accessible(
        lambda item: (
            name(item) == control_name
            and is_text_control(item)
            and is_showing(item)
            and text_value(item) == value
        ),
        timeout,
        f"{control_name} containing the typed value",
    )


def settings_contract(args: argparse.Namespace):
    accessibles = accessible_tree()
    names = {name(item) for item in accessibles}
    if not {"Screen", "Sleep & Wake", "Updates", "Idle blanking",
            "TV sleep & wake", "Automatic update checks",
            "Update channel"} <= names:
        return None
    updater_titles = {"Installed version", "Update available", "Restart required"}
    if not any(value in names for value in updater_titles):
        return None
    if args.expected_updater_state and not updater_contract(args.expected_updater_state, accessibles):
        return None
    for toggle_name, expected_checked in expected_toggles(args):
        toggle = next(
            (
                item for item in accessibles
                if name(item) == toggle_name
                and is_toggle_control(item)
                and is_showing(item)
            ),
            None,
        )
        if toggle is None or is_checked(toggle) != expected_checked:
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


def updater_contract(expected_state: str, accessibles: list[object]) -> bool:
    visible = [item for item in accessibles if is_showing(item)]
    names = {name(item) for item in visible}
    if expected_state == "idle":
        return (
            "Installed version" in names
            and any(
                name(item) == "Check for updates"
                and role(item) == pyatspi.ROLE_PUSH_BUTTON
                and is_sensitive(item)
                for item in visible
            )
        )
    if expected_state == "checking":
        return (
            "Installed version" in names
            and any(
                name(item) == "Checking…"
                and role(item) == pyatspi.ROLE_PUSH_BUTTON
                and not is_sensitive(item)
                for item in visible
            )
        )
    if expected_state == "available":
        return (
            "Update available" in names
            and any(
                name(item) == "Install update…"
                and role(item) == pyatspi.ROLE_PUSH_BUTTON
                and is_sensitive(item)
                for item in visible
            )
            and not any(role(item) == pyatspi.ROLE_DIALOG and is_showing(item) for item in visible)
        )
    dialog = next(
        (item for item in visible if role(item) == pyatspi.ROLE_DIALOG),
        None,
    )
    if dialog is None:
        return False
    dialog_items = accessible_tree(dialog)
    dialog_names = {name(item) for item in dialog_items}
    # GTK status labels expose their content through the Text interface.
    dialog_text = dialog_names | {text_value(item) for item in dialog_items}
    if expected_state == "confirmation":
        cancel = showing_named("Cancel", (pyatspi.ROLE_PUSH_BUTTON,), dialog)
        install = showing_named("Install and restart", (pyatspi.ROLE_PUSH_BUTTON,), dialog)
        return (
            any(
                value.startswith("Install LG Buddy ") and value.endswith("?")
                for value in dialog_text
            )
            and "Install and restart" in dialog_names
            and cancel is not None
            and install is not None
            and is_sensitive(cancel)
            and is_sensitive(install)
        )
    if expected_state != "downloading":
        return False
    title = "Downloading and verifying update…"
    cancel = showing_named("Cancel", (pyatspi.ROLE_PUSH_BUTTON,), dialog)
    return (
        title in dialog_text
        and showing_named(title, (pyatspi.ROLE_PROGRESS_BAR,), dialog) is not None
        and cancel is not None
        and is_sensitive(cancel)
    )


def diagnostics_contract(expected_state: str, expected_text: str | None):
    accessibles = accessible_tree()
    dialog = next(
        (
            item
            for item in accessibles
            if role(item) == pyatspi.ROLE_DIALOG and is_showing(item)
            and any(name(child) == "Diagnostic report" for child in accessible_tree(item))
        ),
        None,
    )
    if dialog is None:
        return None
    dialog_items = accessible_tree(dialog)
    names = {name(item) for item in dialog_items}
    if not {"Diagnostic report", "Close", "Refresh", "Copy", "Save…"} <= names:
        return None
    buttons = {
        button_name: showing_named(button_name, (pyatspi.ROLE_PUSH_BUTTON,), dialog)
        for button_name in ("Refresh", "Copy", "Save…")
    }
    if any(button is None for button in buttons.values()):
        return None
    if expected_state == "collecting":
        if "Collecting diagnostics…" not in names:
            return None
        if any(is_sensitive(button) for button in buttons.values()):
            return None
    elif expected_state == "report":
        if not any(
            is_text_control(item)
            and name(item) == "Diagnostic report"
            and text_value(item).strip()
            for item in dialog_items
        ):
            return None
        if any(not is_sensitive(button) for button in buttons.values()):
            return None
    elif expected_state == "error":
        if not any(role(item) == pyatspi.ROLE_ALERT and is_showing(item) for item in dialog_items):
            return None
        if not is_sensitive(buttons["Refresh"]):
            return None
    if expected_text is not None and not any(
        expected_text in name(item) or expected_text in text_value(item)
        for item in dialog_items
    ):
        return None
    return accessibles, None


def visible_dialogs(accessibles: list[object] | None = None) -> list[object]:
    return [
        item
        for item in (accessible_tree() if accessibles is None else accessibles)
        if role(item) == pyatspi.ROLE_DIALOG and is_showing(item)
    ]


def diagnostics_dialog(accessibles: list[object] | None = None):
    candidates = accessible_tree() if accessibles is None else accessibles
    return next(
        (
            item for item in candidates
            if role(item) == pyatspi.ROLE_DIALOG
            and is_showing(item)
            and any(name(child) == "Diagnostic report" for child in accessible_tree(item))
        ),
        None,
    )


def diagnostics_report_text(accessibles: list[object] | None = None) -> str | None:
    dialog = diagnostics_dialog(accessibles)
    if dialog is None:
        return None
    for item in accessible_tree(dialog):
        if name(item) == "Diagnostic report" and is_text_control(item):
            report = text_value(item)
            if report:
                return report
    return None


def contains_visible_text(accessibles: list[object], expected_text: str) -> bool:
    return any(
        is_showing(item)
        and (expected_text in name(item) or expected_text in text_value(item))
        for item in accessibles
    )


def text_contract(expected_text: str | None):
    accessibles = accessible_tree()
    if not any(name(item) == WINDOW_TITLE for item in accessibles):
        return None
    if expected_text is not None and not contains_visible_text(accessibles, expected_text):
        return None
    return accessibles, None


def expected_toggles(args: argparse.Namespace) -> list[tuple[str, bool]]:
    toggles = []
    for expectation in args.expected_toggle:
        toggle_name, _, toggle_state = expectation.partition("=")
        toggles.append((toggle_name.strip(), toggle_state.lower() in ("on", "true")))
    return toggles


def read_clipboard() -> str | None:
    # GTK owns the clipboard in the installed application. Keep this
    # asynchronous so the clipboard owner can service the request normally.
    gtk_reader = r'''
import sys
try:
    import gi
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gdk, GLib, Gtk
    Gtk.init()
    display = Gdk.Display.get_default()
    if display is None:
        raise RuntimeError("no default display")
    result = []
    loop = GLib.MainLoop()
    def done(clipboard, operation, _user_data=None):
        try:
            result.append(clipboard.read_text_finish(operation) or "")
        except Exception:
            result.append("")
        loop.quit()
    display.get_clipboard().read_text_async(None, done)
    GLib.timeout_add(1000, lambda: (loop.quit(), False)[1])
    loop.run()
    sys.stdout.write(result[0] if result else "")
except Exception:
    raise SystemExit(1)
'''
    try:
        result = subprocess.run(
            [sys.executable, "-c", gtk_reader],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return result.stdout


def visible_window_ids() -> set[str]:
    try:
        result = subprocess.run(
            ["xdotool", "search", "--onlyvisible", "--name", ".*"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return set()
    return set(result.stdout.split())


def save_path_after_chooser(
    path: str,
    timeout: float,
    initial_dialog_count: int,
    initial_window_ids: set[str],
    expected_text: str,
) -> None:
    destination = Path(path)
    initial_stat = destination.stat() if destination.exists() else None
    wait_for_accessible(
        lambda _item: len(visible_dialogs()) > initial_dialog_count
        or bool(visible_window_ids() - initial_window_ids),
        timeout,
        "the native diagnostics save chooser",
    )
    subprocess.run(["xdotool", "key", "ctrl+a"], check=True)
    subprocess.run(["xdotool", "type", "--clearmodifiers", "--", path], check=True)
    subprocess.run(["xdotool", "key", "Return"], check=True)

    def saved() -> bool:
        try:
            current = destination.stat()
        except FileNotFoundError:
            return False
        if current.st_size == 0:
            return False
        if initial_stat is not None and current.st_mtime_ns == initial_stat.st_mtime_ns:
            return False
        try:
            return destination.read_text(encoding="utf-8") == expected_text
        except (OSError, UnicodeDecodeError):
            return False

    wait_for_accessible(lambda _item: saved(), timeout, f"diagnostics saved at {path}")


def main() -> int:
    args = parse_args()
    if args.select_page:
        deadline = time.monotonic() + args.timeout
        activated = False
        while time.monotonic() < deadline:
            for item in accessible_tree():
                try:
                    if (
                        normalized_name(item) == args.select_page
                        and role(item) == pyatspi.ROLE_PAGE_TAB
                        and is_showing(item)
                    ):
                        if item.getState().contains(pyatspi.STATE_SELECTED):
                            return 0
                        if not activated:
                            activated = item.queryAction().doAction(0)
                except Exception:
                    continue
            time.sleep(0.05)
        raise SystemExit(f"could not select the {args.select_page} tab")
    if args.edit_settings_timeout is not None:
        if not args.window_id:
            raise SystemExit("--edit-settings-timeout needs --window-id")
        edit_text_control("Idle timeout", args.edit_settings_timeout, args.window_id, args.timeout)
        return 0
    if args.edit_pairing_address is not None:
        edit_text_control("TV address", args.edit_pairing_address, args.window_id, args.timeout)
    if args.edit_pairing_mac is not None:
        edit_text_control("MAC address", args.edit_pairing_mac, args.window_id, args.timeout)
    if args.edit_pairing_address is not None or args.edit_pairing_mac is not None:
        return 0
    if args.focus_control:
        if not args.window_id:
            raise SystemExit("--focus-control needs --window-id")
        focus_control(args.focus_control, args.window_id, args.timeout)
        return 0
    if args.activate_control:
        activate_control(args.activate_control, args.timeout)
        return 0
    deadline = time.monotonic() + args.timeout
    contract = None
    while time.monotonic() < deadline:
        if (
            args.expected_settings_state
            or args.expected_settings_timeout
            or args.expected_updater_state
            or args.expected_toggle
        ):
            contract = settings_contract(args)
        elif args.expected_tvs_state:
            contract = tvs_contract(args.expected_tvs_state, args.expected_tv_address, args.expected_tv_name)
        elif (
            args.expected_diagnostics_state
            or args.read_diagnostics
            or args.copy_diagnostics
            or args.save_diagnostics
        ):
            contract = diagnostics_contract(args.expected_diagnostics_state or "report", args.expected_text)
        elif args.expected_text or args.expected_absent_text:
            contract = text_contract(args.expected_text)
        else:
            contract = observed_contract(args.expected_state, args.expected_slider_value)
        expected_text_ok = (
            args.expected_text is None
            or contains_visible_text(contract[0], args.expected_text)
        ) and (
            args.expected_absent_text is None
            or not contains_visible_text(contract[0], args.expected_absent_text)
        ) if contract is not None else False
        if contract is not None and expected_text_ok and (
            args.expected_settings_state
            or args.expected_settings_timeout
            or args.expected_updater_state
            or args.expected_toggle
            or args.expected_tvs_state
            or args.expected_diagnostics_state
            or args.read_diagnostics
            or args.copy_diagnostics
            or args.save_diagnostics
            or audio_contract(contract[0], args)
        ):
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
        expected = f" {args.expected_settings_state or args.expected_tvs_state or args.expected_diagnostics_state or args.expected_updater_state or args.expected_state} state"
        if args.expected_absent_text is not None:
            expected += f" without {args.expected_absent_text!r}"
        if args.expected_slider_value is not None:
            expected += f" at slider value {args.expected_slider_value:g}"
        raise SystemExit(
            f"installed GUI did not expose its expected focusable{expected} over AT-SPI"
        )

    accessibles, slider_value = contract

    if args.expected_text is not None and not contains_visible_text(accessibles, args.expected_text):
        raise SystemExit(f"visible accessibility tree did not contain expected text {args.expected_text!r}")
    if args.read_diagnostics or args.copy_diagnostics or args.save_diagnostics:
        report = diagnostics_report_text(accessibles)
        if not report:
            raise SystemExit("Diagnostics did not expose a non-empty report")
        if args.read_diagnostics:
            Path(args.read_diagnostics).write_text(report, encoding="utf-8")
        elif args.copy_diagnostics:
            activate_control("Copy", args.timeout)
            deadline = time.monotonic() + args.timeout
            clipboard = None
            while time.monotonic() < deadline:
                clipboard = read_clipboard()
                if clipboard == report:
                    break
                time.sleep(0.05)
            if clipboard != report:
                raise SystemExit("Copy did not place the diagnostic report on the clipboard")
            Path(args.copy_diagnostics).write_text(clipboard, encoding="utf-8")
        else:
            initial_dialog_count = len(visible_dialogs(accessibles))
            initial_window_ids = visible_window_ids()
            activate_control("Save…", args.timeout)
            save_path_after_chooser(
                args.save_diagnostics,
                args.timeout,
                initial_dialog_count,
                initial_window_ids,
                report,
            )
            # Xvfb has no window manager to return focus from the native chooser.
            subprocess.run(["xdotool", "windowfocus", "--sync", args.window_id], check=True)

    print("AT-SPI accessibility contract verified:")
    print(f"  presentation state: {args.expected_settings_state or args.expected_tvs_state or args.expected_diagnostics_state or args.expected_updater_state or args.expected_state}")
    if slider_value is not None:
        print(f"  slider value: {slider_value:g}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
