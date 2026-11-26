#!/usr/bin/env python3
"""Publish and restore verified KWin bridges by exact build input."""
import argparse
import base64
import io
import json
import re
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

import kwin_matrix as matrix

REPOSITORY = "Staphylococcus/LG_Buddy-prebuilts"
FILES = ("build.json", "metadata.tsv", "plugin.so")


def download(url):
    with urllib.request.urlopen(url, timeout=30) as response:
        return response.read()


def archive_bytes(directory):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        for name in FILES:
            entry = zipfile.ZipInfo(name)
            archive.writestr(entry, (directory / name).read_bytes(), zipfile.ZIP_DEFLATED)
    return output.getvalue()


def restore_archive(data, row, directory):
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        if sorted(archive.namelist()) != list(FILES):
            raise ValueError("Unexpected files in KWin prebuilt archive")
        files = {name: archive.read(name) for name in FILES}
    plugin_id = matrix.digest(files["plugin.so"])
    name = f'{row["kwin_version"]}-{row["qt_version"]}-{row["architecture"]}-{plugin_id}'
    with tempfile.TemporaryDirectory(dir=directory.parent) as scratch:
        root = Path(scratch)
        candidate = root / name
        candidate.mkdir()
        for filename, contents in files.items():
            (candidate / filename).write_bytes(contents)
        matrix.verify(root, [row])
        destination = directory / name
        if destination.exists():
            if any((destination / filename).read_bytes() != contents for filename, contents in files.items()):
                raise ValueError(f"Existing KWin prebuilt differs from the verified archive: {destination}")
        else:
            candidate.rename(destination)


def restore(directory, rows):
    try:
        index = json.loads(download(f"https://raw.githubusercontent.com/{REPOSITORY}/main/index.json"))
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        index = {}
    directory.mkdir(parents=True, exist_ok=True)
    missing = []
    for row in rows:
        identity = matrix.input_identity([row])
        entry = index.get(identity)
        if entry is None:
            missing.append(f'{row["kwin_version"]} / Qt {row["qt_minor"]} ({identity})')
            continue
        release = entry["release"]
        if not re.fullmatch(r"kwin-[a-f0-9]{64}", release):
            raise ValueError(f"Invalid prebuilt release tag: {release}")
        data = download(f"https://github.com/{REPOSITORY}/releases/download/{release}/{identity}.zip")
        if matrix.digest(data) != entry["sha256"]:
            raise ValueError(f"KWin archive checksum mismatch: {identity}")
        restore_archive(data, row, directory)
    if missing:
        raise ValueError("Build and publish missing KWin prebuilts locally:\n  " + "\n  ".join(missing))
    return matrix.verify(directory, rows)


def github_json(*arguments, body=None, missing_ok=False):
    result = subprocess.run(
        ["gh", "api", *arguments],
        input=None if body is None else json.dumps(body),
        capture_output=True,
        text=True,
    )
    if result.returncode:
        if missing_ok and "(HTTP 404)" in result.stderr:
            return None
        raise ValueError(result.stderr.strip())
    return json.loads(result.stdout)


def publish_release(tag, paths):
    command = ["gh", "release"]
    release = subprocess.run(
        [*command, "view", tag, "--repo", REPOSITORY, "--json", "isDraft"],
        capture_output=True,
        text=True,
    )
    if release.returncode:
        if "release not found" not in release.stderr:
            raise ValueError(release.stderr.strip())
        subprocess.run(
            [*command, "create", tag, "--repo", REPOSITORY, "--draft", "--target", "main",
             "--title", f"KWin prebuilts {tag[5:17]}",
             "--notes", "Portable KWin bridges for https://github.com/Staphylococcus/LG_Buddy. Each archive includes the pinned source and build inputs in build.json.",
             *map(str, paths)],
            check=True,
        )
    elif json.loads(release.stdout)["isDraft"]:
        subprocess.run(
            [*command, "upload", tag, *map(str, paths), "--repo", REPOSITORY, "--clobber"],
            check=True,
        )
    else:
        return
    subprocess.run(
        [*command, "edit", tag, "--repo", REPOSITORY, "--draft=false"],
        check=True,
    )


def publish(directory, rows):
    settings = github_json(f"repos/{REPOSITORY}/immutable-releases")
    if not settings["enabled"]:
        raise ValueError("Enable GitHub release immutability before publishing KWin prebuilts")
    index_path = f"repos/{REPOSITORY}/contents/index.json"
    previous = github_json(index_path, missing_ok=True)
    index = json.loads(base64.b64decode(previous["content"])) if previous else {}
    expected = {(row["kwin_version"], row["qt_version"], row["architecture"]): row for row in rows}

    with tempfile.TemporaryDirectory() as scratch:
        archives = {}
        for artifact in sorted(directory.iterdir()):
            key, record = matrix.read_artifact(artifact, matrix.source_identity())
            row = expected.get(key)
            identity = matrix.input_identity([row]) if row else None
            if identity is None or record.get("target") != row or record.get("inputs_sha256") != identity:
                raise ValueError(f"KWin artifact has different build inputs: {artifact}")
            if identity in index:
                continue
            if identity in archives:
                raise ValueError(f"Multiple KWin artifacts for {identity}")
            path = Path(scratch) / f"{identity}.zip"
            path.write_bytes(archive_bytes(artifact))
            archives[identity] = path
        if not archives:
            return 0
        digests = {identity: matrix.digest(path.read_bytes()) for identity, path in archives.items()}
        tag = "kwin-" + matrix.digest(json.dumps(digests, sort_keys=True).encode())
        publish_release(tag, archives.values())
        release = github_json(f"repos/{REPOSITORY}/releases/tags/{tag}")
        reported = {asset["name"]: asset["digest"] for asset in release["assets"]}
        hashes = {f"{identity}.zip": f"sha256:{digest}" for identity, digest in digests.items()}
        if not release["immutable"] or reported != hashes:
            raise ValueError(f"KWin release is mutable or contains unexpected assets: {tag}")
        index.update({identity: {"release": tag, "sha256": digest} for identity, digest in digests.items()})

    body = {
        "message": f"Index {len(archives)} verified KWin prebuilts",
        "content": base64.b64encode((json.dumps(index, indent=2, sort_keys=True) + "\n").encode()).decode(),
    }
    if previous:
        body["sha"] = previous["sha"]
    github_json("--method", "PUT", index_path, "--input", "-", body=body)
    return len(archives)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("restore", "publish"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--series")
    parser.add_argument("--qt-minor")
    args = parser.parse_args()
    try:
        directory = args.directory.resolve()
        rows = matrix.targets(matrix.load_manifest(), args.series, args.qt_minor)
        if args.command == "restore":
            print(f"Restored {restore(directory, rows)} verified KWin prebuilts.")
        else:
            print(f"Published {publish(directory, rows)} verified KWin prebuilts.")
    except (OSError, ValueError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
