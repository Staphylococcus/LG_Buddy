#!/usr/bin/env python3
"""Exercise the process fixture with the real CLI and independent UDP packets.

Only the fixture's loopback TV and temporary configuration are used. Run after
building lg-buddy and its gui_journey_tv example, with TCP port 3001 free.
"""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


def main():
    runtime, fixture = (str(Path(arg).resolve()) for arg in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="lg-buddy-tv-fixture-") as temporary:
        root = Path(temporary)
        control = root / "control"
        control.mkdir()
        config = root / "config.env"
        config.write_text(
            "tv_ip=127.0.0.1\ntv_mac=02:00:00:00:02:17\ninput=HDMI_3\n"
            "tvs_primary_platform=lg_webos\nscreen_idle_blank=disabled\n"
            "system_sleep_wake_policy=disabled\nupdates_auto_check=disabled\n"
        )
        token = root / "tvs/primary/access-token.json"
        token.parent.mkdir(parents=True, mode=0o700)
        token.write_text('{"access_token":"webos-test-access-token"}\n')
        token.chmod(0o600)
        original_token = token.read_bytes()
        env = os.environ | {
            "LG_BUDDY_CONFIG": str(config),
            "LG_BUDDY_SESSION_RUNTIME_DIR": str(root / "session"),
            "LG_BUDDY_SYSTEM_RUNTIME_DIR": str(root / "system"),
            "LG_BUDDY_NONINTERACTIVE": "1",
        }
        with (root / "fixture.log").open("w+") as log:
            process = subprocess.Popen(
                [fixture, str(control), "127.0.0.1:0", "02:00:00:00:02:17", "60000"],
                stdout=log, stderr=log,
            )
            try:
                def state():
                    assert process.poll() is None, "fixture exited unexpectedly"
                    path = control / "state.json"
                    return json.loads(path.read_text()) if path.exists() else {}

                def wait_for(predicate):
                    deadline = time.monotonic() + 5
                    while True:
                        current = state()
                        if predicate(current):
                            return current
                        assert time.monotonic() < deadline, current
                        time.sleep(0.025)

                def command(value):
                    pending = control / "command.tmp"
                    pending.write_text(value + "\n")
                    pending.replace(control / "command")

                def cli(*args, success=True):
                    result = subprocess.run(
                        [runtime, *args], env=env, capture_output=True, text=True, timeout=15,
                    )
                    assert (result.returncode == 0) == success, result.stdout + result.stderr
                    return result.stdout.strip()

                initial = wait_for(lambda s: s.get("tv_ready"))
                cli("volume", "37")
                cli("screen", "off")
                wait_for(lambda s: s.get("power_on") and not s.get("screen_on"))
                cli("screen", "on")
                wait_for(lambda s: s.get("screen_on"))
                cli("power", "off")
                before_wake = wait_for(lambda s: s.get("power_off_count") == 1)
                assert not before_wake["power_on"] and not before_wake["tv_ready"]

                address, port = initial["wol_address"].rsplit(":", 1)
                packet = b"\xff" * 6 + bytes.fromhex("020000000217") * 16
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
                    destination = (address, int(port))
                    for invalid in (packet[:-1], packet + b"x", b"\x00" + packet[1:],
                                    b"\xff" * 6 + bytes.fromhex("020000000218") * 16):
                        udp.sendto(invalid, destination)
                    time.sleep(0.2)
                    assert state()["wake_count"] == 0, "invalid packet woke fixture"
                    udp.sendto(packet, destination)
                    waking = wait_for(lambda s: s.get("wake_count") == 1)
                    assert waking["power_on"] and not waking["tv_ready"]
                    assert waking["volume"] == 37
                    for _ in range(3):
                        udp.sendto(packet, destination)
                    cli("volume", success=False)
                    command("ready")
                    wait_for(lambda s: s.get("tv_ready"))

                assert cli("volume") == "37"
                assert token.read_bytes() == original_token
                cli("power", "off")
                wait_for(lambda s: s.get("power_off_count") == 2)
                command("wake")
                recovered = wait_for(lambda s: s.get("wake_count") == 2)
                assert recovered["tv_ready"] and recovered["input"] == "HDMI_3"
                assert recovered["connection_count"] > before_wake["connection_count"]
                assert recovered["pairing_prompt_count"] == 0
                # Stopping must also release a client that never starts TLS.
                with socket.create_connection(("127.0.0.1", 3001), timeout=2):
                    command("stop")
                    assert process.wait(timeout=3) == 0
                assert json.loads((control / "state.json").read_text())["status"] == "stopped"
                print("TV fixture: CLI power cycle, UDP validation, delayed readiness and stop passed")
            except BaseException:
                log.seek(0)
                print(log.read(), file=sys.stderr)
                raise
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    main()
