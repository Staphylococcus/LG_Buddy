#!/usr/bin/env python3
"""Deterministic local GitHub API/assets fixture for installed GUI tests."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import sys
import tarfile
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from urllib.parse import parse_qs, unquote, urlsplit


OWNER = "Staphylococcus"
REPO = "LG_Buddy"
API = f"/api.github.com/repos/{OWNER}/{REPO}"
ASSET_PREFIX = "/release-assets.githubusercontent.com/github-production-release-asset/"
REDIRECT_PATH = "/github-production-release-asset/"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def archive_facts(path: Path) -> dict[str, object]:
    data = path.read_bytes()
    with tarfile.open(path, "r:gz") as bundle:
        matches = [
            m for m in bundle.getmembers()
            if (
                m.isfile()
                and len(PurePosixPath(m.name).parts) == 2
                and PurePosixPath(m.name).parts[1] == "release-manifest.json"
            )
        ]
        if len(matches) != 1:
            raise ValueError("archive must contain one top-level release-manifest.json")
        stream = bundle.extractfile(matches[0])
        if stream is None:
            raise ValueError("release-manifest.json is unreadable")
        manifest = json.load(stream)
    version = manifest["version"]
    target = manifest["target"]
    return {
        "bytes": data,
        "version": version,
        "tag": manifest["release_tag"],
        "channel": manifest["channel"],
        "target": target,
        "commit": manifest["commit"],
        "sha": sha256(data),
    }


def make_release(
    facts: dict[str, object], version: str, channel: str, asset_id: int
) -> dict[str, object]:
    name = f"lg-buddy-{version}-{facts['target']}.tar.gz"
    checksums = f"{facts['sha']}  {name}\n".encode("ascii")
    return {
        "version": version,
        "tag": facts["tag"] if version == facts["version"] else f"v{version}",
        "channel": channel,
        "commit": facts["commit"],
        "name": name,
        "archive_id": asset_id,
        "checksum_id": asset_id + 1,
        "archive": facts["bytes"],
        "checksums": checksums,
    }


def release_json(item: dict[str, object]) -> dict[str, object]:
    base = f"https://api.github.com/repos/{OWNER}/{REPO}"
    tag, archive, checksums = item["tag"], item["archive"], item["checksums"]
    return {
        "tag_name": tag,
        "html_url": f"https://github.com/{OWNER}/{REPO}/releases/tag/{tag}",
        "draft": False,
        "prerelease": item["channel"] == "prerelease",
        "assets": [
            {
                "id": item["archive_id"],
                "name": item["name"],
                "state": "uploaded",
                "size": len(archive),
                "digest": f"sha256:{sha256(archive)}",
                "url": f"{base}/releases/assets/{item['archive_id']}",
                "browser_download_url": (
                    f"https://github.com/{OWNER}/{REPO}/releases/download/"
                    f"{tag}/{item['name']}"
                ),
            },
            {
                "id": item["checksum_id"],
                "name": "sha256sums.txt",
                "state": "uploaded",
                "size": len(checksums),
                "digest": f"sha256:{sha256(checksums)}",
                "url": f"{base}/releases/assets/{item['checksum_id']}",
                "browser_download_url": (
                    f"https://github.com/{OWNER}/{REPO}/releases/download/"
                    f"{tag}/sha256sums.txt"
                ),
            },
        ],
    }


def body(value: object) -> bytes:
    return json.dumps(value, separators=(",", ":")).encode() + b"\n"


class Fixture:
    def __init__(
        self, archive: Path, state: Path, request_log: Path | None, current: str
    ):
        facts = archive_facts(archive)
        self.state, self.request_log, self.lock = state, request_log, threading.Lock()
        stable = str(facts["version"]).split("-", 1)[0]
        self.current = make_release(
            facts, current, "prerelease" if "-" in current else "stable", 1000
        )
        self.stable = make_release(facts, stable, "stable", 1100)
        self.candidate = make_release(
            facts, str(facts["version"]), str(facts["channel"]), 1200
        )
        self.releases = (self.current, self.stable, self.candidate)

    def selected(self, channel: str, mode: str) -> dict[str, object]:
        return (
            self.current
            if mode == "up-to-date"
            else (self.stable if channel == "stable" else self.candidate)
        )

    def log(self, handler: BaseHTTPRequestHandler) -> None:
        if self.request_log is None:
            return
        parsed = urlsplit(handler.path)
        path = parsed.path + (f"?{parsed.query}" if parsed.query else "")
        record = {"path": path}
        self.request_log.parent.mkdir(parents=True, exist_ok=True)
        with self.lock, self.request_log.open("a", encoding="utf-8") as output:
            output.write(
                json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n"
            )


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "LG-Buddy-GitHub-Fixture/1"

    def log_message(self, _format: str, *_args: object) -> None:
        return

    @property
    def fixture(self) -> Fixture:
        return self.server.fixture  # type: ignore[attr-defined]

    def do_GET(self) -> None:  # noqa: N802
        self.fixture.log(self)
        try:
            mode, options = self.load_state()
            if mode == "error":
                return self.send(500, b'{"message":"mock GitHub error"}\n')
            time.sleep(max(0, min(float(options.get("delay_seconds", 0)), 120)))
            status, headers, data = self.route(mode)
            self.send(status, data, headers)
        except (ValueError, OSError) as error:
            self.send(500, body({"message": str(error)}))

    def load_state(self) -> tuple[str, dict[str, object]]:
        raw = self.fixture.state.read_text(encoding="utf-8").strip()
        value = json.loads(raw)
        if not isinstance(value, dict):
            raise ValueError("state must be a JSON object")
        return str(value.get("mode", "available")), value

    def route(self, mode: str) -> tuple[int, dict[str, str], bytes]:
        parsed = urlsplit(self.path)
        path = unquote(parsed.path)
        query = parse_qs(parsed.query, keep_blank_values=True)
        if path == f"{API}/releases/latest":
            item = self.fixture.selected("stable", mode)
            return 200, {"Content-Type": "application/json"}, body(release_json(item))
        if path == f"{API}/releases" and query.get("per_page") == ["1"]:
            item = self.fixture.selected("prerelease", mode)
            return 200, {"Content-Type": "application/json"}, body([release_json(item)])
        if path.startswith(f"{API}/releases/tags/"):
            tag = path.rsplit("/", 1)[-1]
            item = next((x for x in self.fixture.releases if x["tag"] == tag), None)
            if item:
                return 200, {"Content-Type": "application/json"}, body(
                    release_json(item)
                )
            return 404, {}, body({"message": "unknown release tag"})
        if path.startswith(f"{API}/git/ref/tags/"):
            tag = path.rsplit("/", 1)[-1]
            if not any(x["tag"] == tag for x in self.fixture.releases):
                return 404, {}, body({"message": "unknown tag"})
            tag_sha = sha256(f"annotated-tag:{tag}".encode())[:40]
            return 200, {"Content-Type": "application/json"}, body(
                {"object": {"type": "tag", "sha": tag_sha}}
            )
        if path.startswith(f"{API}/git/tags/"):
            tag_sha = path.rsplit("/", 1)[-1]
            item = next(
                (
                    x
                    for x in self.fixture.releases
                    if sha256(f"annotated-tag:{x['tag']}".encode())[:40] == tag_sha
                ),
                None,
            )
            if item:
                return 200, {"Content-Type": "application/json"}, body(
                    {"object": {"type": "commit", "sha": item["commit"]}}
                )
            return 404, {}, body({"message": "unknown tag object"})
        if path.startswith(f"{API}/releases/assets/"):
            asset = path.rsplit("/", 1)[-1]
            if not asset.isdigit() or not any(
                int(asset) in {x["archive_id"], x["checksum_id"]}
                for x in self.fixture.releases
            ):
                return 404, {}, body({"message": "unknown asset"})
            return (
                302,
                {
                    "Location": (
                        f"https://release-assets.githubusercontent.com"
                        f"{REDIRECT_PATH}{asset}/file"
                    )
                },
                b"",
            )
        if path.startswith(ASSET_PREFIX):
            asset = path.rstrip("/").split("/")[-2]
            if not asset.isdigit():
                return 404, {}, body({"message": "unknown asset"})
            item = next(
                (
                    x
                    for x in self.fixture.releases
                    if int(asset) in {x["archive_id"], x["checksum_id"]}
                ),
                None,
            )
            if item is None:
                return 404, {}, body({"message": "unknown asset"})
            data = (
                item["archive"]
                if int(asset) == item["archive_id"]
                else item["checksums"]
            )
            if mode == "corrupt-archive" and int(asset) == item["archive_id"]:
                data = bytes([data[0] ^ 1]) + data[1:]
            return 200, {"Content-Type": "application/octet-stream"}, data
        return 404, {}, body({"message": "unknown fixture path"})

    def send(
        self, status: int, data: bytes, headers: dict[str, str] | None = None
    ) -> None:
        self.send_response(status)
        self.send_header("Content-Length", str(len(data)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--ready", type=Path, required=True)
    parser.add_argument("--requests", type=Path)
    parser.add_argument("--current-version", default="0.0.0")
    args = parser.parse_args()
    try:
        fixture = Fixture(args.archive, args.state, args.requests, args.current_version)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.fixture = fixture  # type: ignore[attr-defined]
        host, port = server.server_address
        atomic_write(
            args.ready,
            body({"address": f"{host}:{port}", "host": host, "port": port}),
        )
    except (ValueError, OSError) as error:
        print(f"mock GitHub fixture: {error}", file=sys.stderr)
        return 2

    def stop(_signal: int, _frame: object) -> None:
        threading.Thread(target=server.shutdown, daemon=True).start()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        server.serve_forever(poll_interval=0.05)
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
