#!/usr/bin/env python3

"""Protocol tests for tools/mock_github_gui.py."""

from __future__ import annotations

import io
import json
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
SERVER = ROOT / "tools" / "mock_github_gui.py"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


def write_candidate(path: Path) -> bytes:
    manifest = {
        "schema_version": 1,
        "critical": ["release_tag", "version", "channel", "target", "commit"],
        "release_tag": "v1.7.0-beta.1",
        "version": "1.7.0-beta.1",
        "channel": "prerelease",
        "target": "x86_64-unknown-linux-musl",
        "commit": "0123456789abcdef0123456789abcdef01234567",
    }
    content = (json.dumps(manifest) + "\n").encode("utf-8")
    root = "lg-buddy-1.7.0-beta.1-x86_64-unknown-linux-musl"
    with tarfile.open(path, mode="w:gz") as bundle:
        info = tarfile.TarInfo(f"{root}/release-manifest.json")
        info.mode = 0o644
        info.size = len(content)
        bundle.addfile(info, io.BytesIO(content))
    return path.read_bytes()


class MockGitHubGuiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="lg-buddy-mock-github-")
        root = Path(self.directory.name)
        self.archive = root / "candidate.tar.gz"
        self.state = root / "state.json"
        self.ready = root / "ready.json"
        self.requests = root / "requests.jsonl"
        self.archive_bytes = write_candidate(self.archive)
        self.state.write_text('{"mode":"available"}\n', encoding="utf-8")
        self.process = subprocess.Popen(
            [
                sys.executable,
                str(SERVER),
                "--archive",
                str(self.archive),
                "--state",
                str(self.state),
                "--ready",
                str(self.ready),
                "--requests",
                str(self.requests),
            ],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in range(100):
            if self.ready.exists():
                break
            if self.process.poll() is not None:
                self.fail(self.process.stderr.read())
            time.sleep(0.05)
        self.assertTrue(self.ready.exists())
        self.address = json.loads(self.ready.read_text(encoding="utf-8"))["address"]
        self.no_redirect = urllib.request.build_opener(NoRedirect)

    def tearDown(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=5)
        if self.process.stdout is not None:
            self.process.stdout.close()
        if self.process.stderr is not None:
            self.process.stderr.close()
        self.directory.cleanup()

    def url(self, path: str) -> str:
        return f"http://{self.address}{path}"

    def get(self, path: str):  # type: ignore[no-untyped-def]
        return urllib.request.urlopen(self.url(path), timeout=3)

    def get_no_redirect(self, path: str):  # type: ignore[no-untyped-def]
        try:
            return self.no_redirect.open(self.url(path), timeout=3)
        except urllib.error.HTTPError as error:
            if 300 <= error.code < 400:
                return error
            raise

    def test_prerelease_metadata_tag_peeling_and_asset_bytes(self) -> None:
        with self.get(
            "/api.github.com/repos/Staphylococcus/LG_Buddy/releases?per_page=1"
        ) as response:
            release = json.load(response)[0]
        self.assertEqual(release["tag_name"], "v1.7.0-beta.1")
        self.assertTrue(release["prerelease"])
        archive_asset = next(
            asset
            for asset in release["assets"]
            if asset["name"].endswith(".tar.gz")
        )

        with self.get(
            "/api.github.com/repos/Staphylococcus/LG_Buddy/git/ref/tags/v1.7.0-beta.1"
        ) as response:
            reference = json.load(response)
        self.assertEqual(reference["object"]["type"], "tag")
        with self.get(
            "/api.github.com/repos/Staphylococcus/LG_Buddy/git/tags/"
            + reference["object"]["sha"]
        ) as response:
            tag_object = json.load(response)
        self.assertEqual(tag_object["object"]["type"], "commit")
        self.assertEqual(
            tag_object["object"]["sha"],
            "0123456789abcdef0123456789abcdef01234567",
        )

        with self.get_no_redirect(
            "/api.github.com/repos/Staphylococcus/LG_Buddy/releases/assets/"
            + str(archive_asset["id"])
        ) as response:
            self.assertEqual(response.status, 302)
            location = response.headers["Location"]
        # The Rust transport fixture rewrites this validated external host to
        # the following local path before making the redirected request.
        local_path = "/release-assets.githubusercontent.com" + urlsplit(location).path
        with self.get(local_path) as response:
            self.assertEqual(response.status, 200)
            self.assertEqual(response.read(), self.archive_bytes)

    def test_state_transitions_include_error_and_corrupt_asset(self) -> None:
        stable = "/api.github.com/repos/Staphylococcus/LG_Buddy/releases/latest"
        prerelease = "/api.github.com/repos/Staphylococcus/LG_Buddy/releases?per_page=1"
        with self.get(stable) as response:
            self.assertEqual(json.load(response)["tag_name"], "v1.7.0")
        with self.get(prerelease) as response:
            self.assertEqual(json.load(response)[0]["tag_name"], "v1.7.0-beta.1")

        self.state.write_text('{"mode":"up-to-date"}\n', encoding="utf-8")
        with self.get(stable) as response:
            self.assertEqual(json.load(response)["tag_name"], "v0.0.0")

        self.state.write_text('{"mode":"error","status":503}\n', encoding="utf-8")
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.get(stable)
        self.assertEqual(error.exception.code, 500)

        self.state.write_text('{"mode":"corrupt-archive"}\n', encoding="utf-8")
        with self.get_no_redirect(
            "/api.github.com/repos/Staphylococcus/LG_Buddy/releases/assets/1200"
        ) as response:
            location = response.headers["Location"]
        redirect_path = urlsplit(location).path
        local_path = "/release-assets.githubusercontent.com" + redirect_path
        with self.get(local_path) as response:
            self.assertNotEqual(response.read(), self.archive_bytes)

        records = [
            json.loads(line)
            for line in self.requests.read_text(encoding="utf-8").splitlines()
        ]
        self.assertTrue(any(record["path"].endswith("/releases?per_page=1") for record in records))
        self.assertTrue(any("/releases/assets/1200" in record["path"] for record in records))


if __name__ == "__main__":
    unittest.main()
