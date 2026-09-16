#!/usr/bin/env python3
"""Real setup CLI and terminal interrupts, isolated by bubblewrap and loopback TV.

The fixture makes service setup unsupported, so these process tests can verify
pairing, partial completion and resumption without authorization or real services.
Native service repair and whole-flow completion have separate Rust integration tests.
"""
import errno
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def main():
    runtime, fixture = (str(Path(arg).resolve()) for arg in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="lg-buddy-setup-cli-") as temporary:
        root = Path(temporary)
        etc = root / "etc"
        etc.mkdir()
        (etc / "NIXOS").touch()
        for name in ("passwd", "group", "nsswitch.conf"):
            if Path("/etc", name).exists():
                shutil.copyfile(Path("/etc", name), etc / name)
        (root / "control").mkdir()
        config = root / "config.env"
        original = "screen_idle_blank=disabled\nupdates_auto_check=disabled\n"
        config.write_text(original)
        env = os.environ | {"HOME": str(root), "LG_BUDDY_CONFIG": str(config)}
        command = ["bwrap", "--bind", "/", "/", "--tmpfs", "/run", "--dir", f"/run/user/{os.getuid()}",
                   "--ro-bind", str(etc), "/etc",
                   "--dev", "/dev", runtime, "setup"]

        def cli(*args):
            return subprocess.run(command + list(args), env=env, capture_output=True,
                                  text=True, timeout=15)

        def terminal(args, conversation, expected):
            pid, master = pty.fork()
            if pid == 0:
                # The namespace supervisor must survive the foreground signal;
                # the CLI installs its own handler after exec.
                signal.signal(signal.SIGINT, signal.SIG_IGN)
                os.execvpe(command[0], command + args, env)
            output = b""
            status = None
            deadline = time.monotonic() + 15
            try:
                for wanted, reply in conversation:
                    while wanted.encode() not in output:
                        assert time.monotonic() < deadline, output.decode(errors="replace")
                        if select.select([master], [], [], 0.1)[0]:
                            output += os.read(master, 65536)
                    os.write(master, reply)
                while status is None:
                    assert time.monotonic() < deadline, output.decode(errors="replace")
                    if select.select([master], [], [], 0.1)[0]:
                        try:
                            output += os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                    found, value = os.waitpid(pid, os.WNOHANG)
                    if found:
                        status = os.waitstatus_to_exitcode(value)
                assert status == expected, (status, output.decode(errors="replace"))
                return output.decode(errors="replace")
            finally:
                if status is None:
                    os.kill(pid, signal.SIGKILL)
                    os.waitpid(pid, 0)
                os.close(master)

        missing = cli("--non-interactive", "--yes")
        assert missing.returncode == 3, missing.stdout + missing.stderr
        assert config.read_text() == original
        for reply in (b"q\n", b"\x03", b"\x04"):
            terminal([], [("TV IP address", reply)], 130)
            assert config.read_text() == original

        with (root / "fixture.log").open("w+") as log:
            tv = subprocess.Popen([fixture, str(root / "control")], stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 5
                while not (root / "control/state.json").exists():
                    assert time.monotonic() < deadline and tv.poll() is None, "TV fixture failed"
                    time.sleep(0.02)
                paired = cli("--non-interactive", "--yes", "--tv-ip", "127.0.0.1",
                             "--tv-mac", "02:11:22:33:44:55", "--input", "HDMI_2")
                assert paired.returncode == 1, paired.stdout + paired.stderr
                assert "Setup complete." not in paired.stdout
                assert "does not support automatic service setup" in paired.stderr, paired.stdout + paired.stderr
                token = root / "tvs/primary/access-token.json"
                assert token.exists(), paired.stdout + paired.stderr
                saved, credential, modified = config.read_bytes(), token.read_bytes(), token.stat().st_mtime_ns
                assert original.splitlines()[0] in saved.decode()
                resumed = cli("--non-interactive", "--yes")
                assert resumed.returncode == 1, resumed.stdout + resumed.stderr
                assert "Pair a TV" not in resumed.stdout
                assert (config.read_bytes(), token.read_bytes(), token.stat().st_mtime_ns) == (saved, credential, modified)
                deadline = time.monotonic() + 3
                while json.loads((root / "control/state.json").read_text())["pairing_prompt_count"] != 1:
                    assert time.monotonic() < deadline, "TV fixture did not publish pairing state"
                    time.sleep(0.02)
                control_result = subprocess.run([runtime, "brightness", "get"], env=env,
                                                capture_output=True, text=True, timeout=5)
                assert control_result.returncode == 0, control_result.stdout + control_result.stderr
                assert control_result.stdout.strip().isdigit()
                assert token.read_bytes() == credential
            finally:
                tv.terminate()
                tv.wait(timeout=5)
    print("PASS: real CLI missing input, terminal cancellation/EOF/Ctrl+C, pairing, partial setup and resume.")


if __name__ == "__main__":
    main()
