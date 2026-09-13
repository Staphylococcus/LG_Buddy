#!/usr/bin/env python3
"""Native inhibition acceptance in a disposable Plasma/TV-fixture installation.

Requires a 10-second idle deadline, honoring enabled, python3-dbus and mpv.
The state file and input FIFO belong to the repository's stateful TV/uinput
fixtures. Never point this check at a physical TV installation.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

import dbus


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clip", type=Path, required=True)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--input-fifo", type=Path, required=True)
    parser.add_argument("--plugin", required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    args.evidence.mkdir(parents=True, exist_ok=True)
    bus = dbus.SessionBus()
    plugins = dbus.Interface(bus.get_object("org.kde.KWin", "/Plugins"), "org.kde.KWin.Plugins")
    scripting = dbus.Interface(bus.get_object("org.kde.KWin", "/Scripting"), "org.kde.kwin.Scripting")
    powerdevil = dbus.Interface(bus.get_object(
        "org.kde.Solid.PowerManagement", "/org/kde/Solid/PowerManagement/PolicyAgent"
    ), "org.kde.Solid.PowerManagement.PolicyAgent")
    screensaver = dbus.Interface(bus.get_object(
        "org.freedesktop.ScreenSaver", "/ScreenSaver"
    ), "org.freedesktop.ScreenSaver")
    service = "io.github.staphylococcus.LGBuddy.KWinInhibition"
    players, script_names, cookies = {}, [], []

    def run(*command):
        return subprocess.check_output(command, text=True).strip()

    def record(event, **fields):
        print(json.dumps(dict(time=time.time(), event=event, **fields)), flush=True)

    def state():
        return json.loads(args.state.read_text())

    def native():
        return bool(bus.call_blocking(service, "/io/github/staphylococcus/LGBuddy/KWinInhibition",
                                     service + "1", "IsInhibited", "", (), timeout=3))

    def pd():
        return bool(powerdevil.HasInhibition(dbus.UInt32(4)))

    def wait(condition, timeout=5):
        deadline = time.monotonic() + timeout
        while not condition():
            assert time.monotonic() < deadline, "condition timed out"
            time.sleep(0.05)

    def stays(on, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            assert state()["screen_on"] == on, state()
            time.sleep(0.1)

    def input_event(kind="input"):
        subprocess.run(["sudo", "tee", str(args.input_fifo)], input=kind + "\n",
                       text=True, stdout=subprocess.DEVNULL, check=True, timeout=3)
        wait(lambda: state()["screen_on"])

    def restart_monitor():
        run("systemctl", "--user", "restart", "LG_Buddy_screen.service")
        pid = run("systemctl", "--user", "show", "LG_Buddy_screen.service", "-p", "MainPID", "--value")
        wait(lambda: "source=wayland available=true" in run(
            "journalctl", "--user", f"_PID={pid}", "--no-pager"), 8)

    def start(name):
        log = (args.evidence / f"mpv-{name}.log").open("w")
        process = subprocess.Popen([
            "mpv", "--no-config", "--no-audio", "--vo=wlshm", "--loop-file=inf",
            f"--title=LG-Buddy-KWin-Test-{name}", str(args.clip),
        ], env={**os.environ, "WAYLAND_DEBUG": "1"}, stdout=log, stderr=log)
        players[name] = process, log
        wait(lambda: "create_inhibitor" in (args.evidence / f"mpv-{name}.log").read_text())

    def stop(name):
        process, log = players.pop(name)
        process.terminate()
        process.wait(timeout=5)
        log.close()

    def minimize(name, value):
        script_name = f"lg-buddy-kwin-test-{name}-{value}"
        script = args.evidence / f"{script_name}.js"
        script.write_text("workspace.windowList().filter(w => w.caption === "
                          + json.dumps(f"LG-Buddy-KWin-Test-{name}")
                          + ").forEach(w => w.minimized = " + str(value).lower() + ");\n")
        identifier = bus.call_blocking("org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting",
                                       "loadScript", "ss", (str(script.resolve()), script_name))
        assert identifier >= 0
        script_names.append(script_name)
        bus.call_blocking("org.kde.KWin", f"/Scripting/Script{identifier}",
                          "org.kde.kwin.Script", "run", "", ())

    def inhibit_pd():
        cookie = screensaver.Inhibit("LG Buddy acceptance", "independent PowerDevil route")
        cookies.append(cookie)
        # PowerDevil deliberately delays activation of new requests.
        wait(pd, 12)
        return cookie

    def release_pd(cookie):
        screensaver.UnInhibit(cookie)
        cookies.remove(cookie)
        wait(lambda: not pd())

    def release_then_blank(label):
        released = time.monotonic()
        stays(True, 9.5)
        wait(lambda: not state()["screen_on"], 4)
        stays(False, 1)
        record(label, seconds_to_blank=round(time.monotonic() - released - 1, 2), state=state())

    honoring_changed = False
    try:
        session = run("loginctl", "show-seat", "seat0", "-p", "ActiveSession", "--value")
        assert run("loginctl", "show-session", session, "-p", "LockedHint", "--value") == "no", "unlock the disposable desktop first"
        assert not native() and not pd(), "pre-existing inhibition"
        assert "enabled" in run("/usr/bin/lg-buddy", "settings", "get", "screen.honor_idle_inhibitors")
        assert "10" in run("/usr/bin/lg-buddy", "settings", "get", "screen.idle_timeout")
        record("identity", runtime=run("sha256sum", "/usr/bin/lg-buddy"),
               bridge=run("/usr/bin/lg-buddy", "kwin-bridge", "check"))
        start("A")
        wait(native)
        restart_monitor()
        input_event()
        stays(True, 12)
        assert not pd()
        record("native_playback_before_monitor_start_blocks")
        stop("A")
        wait(lambda: not native())
        release_then_blank("final_release_waits_full_delay_without_restore")

        input_event()
        start("A")
        wait(native)
        stays(True, 12)
        record("native_playback_after_monitor_start_blocks")
        start("B")
        stop("A")
        stays(True, 12)
        assert native() and not pd()
        record("overlapping_native_inhibitor_remains")
        stop("B")
        wait(lambda: not native())
        release_then_blank("last_overlap_release_waits_full_delay")

        input_event()
        start("A")
        wait(native)
        stays(True, 12)
        minimize("A", True)
        wait(lambda: not native())
        release_then_blank("minimized_native_window_releases")
        minimize("A", False)
        wait(native)
        stays(False, 2)
        record("restored_inhibitor_does_not_restore_tv")
        input_event("gamepad")
        stays(True, 12)
        record("gamepad_restores_with_native_inhibition")

        cookie = inhibit_pd()
        assert native() and pd()
        stays(True, 12)
        plugins.UnloadPlugin(args.plugin)
        assert not bus.name_has_owner(service)
        stop("A")
        stays(True, 12)
        record("powerdevil_blocks_with_kwin_absent")
        release_pd(cookie)
        release_then_blank("powerdevil_release_remains_independent")

        start("A")
        input_event()
        wait(lambda: not state()["screen_on"], 14)
        record("absent_kwin_is_ordinary_reduced_coverage")
        assert plugins.LoadPlugin(args.plugin)
        wait(native)
        stays(False, 2)
        input_event()
        stays(True, 12)
        record("kwin_recovery_blocks_without_monitor_restart")

        run("/usr/bin/lg-buddy", "settings", "set", "screen.honor_idle_inhibitors", "disabled")
        honoring_changed = True
        restart_monitor()
        input_event()
        wait(lambda: not state()["screen_on"], 14)
        assert native()
        record("disabled_honoring_bypasses_native_inhibition")
        stop("A")
        wait(lambda: not native())
        stays(False, 2)
        record("release_after_blank_does_not_restore_tv")
        record("PASS")
    finally:
        for name in list(players):
            stop(name)
        for cookie in cookies:
            screensaver.UnInhibit(cookie)
        for name in script_names:
            scripting.unloadScript(name)
        if not bus.name_has_owner(service):
            plugins.LoadPlugin(args.plugin)
        if honoring_changed:
            run("/usr/bin/lg-buddy", "settings", "set", "screen.honor_idle_inhibitors", "enabled")
            restart_monitor()


if __name__ == "__main__":
    main()
