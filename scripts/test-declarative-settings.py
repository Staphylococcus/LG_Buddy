#!/usr/bin/env python3
"""Exercise effective reads, rejected writes and declaration replacement via CLI."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runtime", type=Path)
    parser.add_argument("--nix", action="store_true", help="use real immutable Nix store generations")
    args = parser.parse_args()
    runtime = str(args.runtime.resolve())
    with tempfile.TemporaryDirectory(prefix="lg-buddy-declarative-cli-") as temporary:
        root = Path(temporary)
        config = root / "config.env"
        config.write_text("tvs_primary_ip=192.0.2.10\ntvs_primary_mac=02:00:00:00:00:10\n"
                          "tvs_primary_input=HDMI_1\ntvs_primary_platform=lg_webos\n"
                          "screen_idle_timeout=600\nupdates_channel=stable\n")
        token = root / "tvs/primary/access-token.json"
        token.parent.mkdir(parents=True)
        token.write_text('{"access_token":"private-test-token"}\n')
        token.chmod(0o600)
        credential = (token.read_bytes(), token.stat().st_mtime_ns)
        binding = config.with_name(config.name + ".declarative.json")
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith("LG_BUDDY_")}
        environment.update(HOME=str(root), XDG_CONFIG_HOME=str(root / ".config"),
                           XDG_CACHE_HOME=str(root / ".cache"), XDG_STATE_HOME=str(root / ".state"),
                           LG_BUDDY_CONFIG=str(config), LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1")

        def run(*command, success=True):
            result = subprocess.run([runtime, *command], env=environment, capture_output=True,
                                    text=True, timeout=15)
            assert (result.returncode == 0) == success, (command, result.stdout, result.stderr)
            assert (token.read_bytes(), token.stat().st_mtime_ns) == credential
            return result.stdout.strip()

        def publish(settings):
            document = {"version": 1, "source": "nix" if args.nix else "generic", "settings": settings}
            if args.nix:
                # Only this test fixture evaluates Nix; the application reads data.
                expression = 'builtins.toFile "lg-buddy-declarative-test.json" ' + json.dumps(json.dumps(document))
                target = Path(subprocess.check_output(["nix", "eval", "--raw", "--expr", expression], text=True))
                assert target.is_relative_to("/nix/store")
                assert target.stat().st_uid == 0
                assert target.stat().st_mode & 0o222 == 0
            else:
                target = root / f"generation-{len(list(root.glob('generation-*')))}.json"
                target.write_text(json.dumps(document))
                target.chmod(0o400)
            pending = root / "declaration.pending"
            pending.symlink_to(target)
            pending.replace(binding)

        before = config.read_bytes()
        publish({"screen.idle_timeout": 300})
        assert int(run("settings", "get", "screen.idle_timeout")) == 300
        for operation in [("set", "screen.idle_timeout", "600"), ("unset", "screen.idle_timeout")]:
            run("settings", *operation, success=False)
            assert config.read_bytes() == before
        run("settings", "set", "updates.channel", "prerelease")
        saved = config.read_bytes()
        entries = dict(line.split("=", 1) for line in saved.decode().splitlines() if "=" in line)
        assert int(entries["screen_idle_timeout"]) == 600
        assert entries["updates_channel"] == "prerelease"
        publish({"screen.idle_timeout": 450})
        assert int(run("settings", "get", "screen.idle_timeout")) == 450
        assert config.read_bytes() == saved

        binding.unlink()
        binding.symlink_to(root / "missing-generation.json")
        run("settings", "get", "screen.idle_timeout", success=False)
        run("settings", "set", "updates.channel", "stable", success=False)
        assert config.read_bytes() == saved
        run("--version")
        run("settings", "--help")
        assert config.read_bytes() == saved
        publish({})
        assert int(run("settings", "get", "screen.idle_timeout")) == 600
        run("settings", "set", "screen.idle_timeout", "900")
        assert int(run("settings", "get", "screen.idle_timeout")) == 900
        binding.unlink()
        assert int(run("settings", "get", "screen.idle_timeout")) == 900
    print("PASS: declarative CLI reads, mutation refusal, retained state and generation replacement.")


if __name__ == "__main__":
    main()
