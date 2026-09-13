#!/usr/bin/env python3
"""Build pinned KWin bridges and require complete, verified bundle coverage."""
import argparse
import hashlib
import json
import re
import shlex
import shutil
import struct
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "scripts/kwin-matrix/targets.json"
BRIDGE = ROOT / "data/kwin/source"
INPUTS = (
    "scripts/kwin_matrix.py", "scripts/kwin-matrix/default.nix",
    "scripts/test-kwin-plugin.sh", "scripts/kwin-matrix/session.conf",
    "data/kwin/source/CMakeLists.txt", "data/kwin/source/main.cpp",
    "data/kwin/source/metadata.json",
)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_identity(directory=BRIDGE):
    checksums = "".join(
        f"{digest((directory / name).read_bytes())}  {name}\n"
        for name in ("CMakeLists.txt", "main.cpp", "metadata.json")
    )
    return digest(checksums.encode())


def load_manifest(path=MANIFEST):
    manifest = json.loads(path.read_text())
    if manifest["schema_version"] != 1 or manifest["architecture"] != "x86_64":
        raise ValueError("Unsupported KWin matrix schema or architecture")
    versions = set()
    for minor, env in manifest["environments"].items():
        if not re.fullmatch(r"6\.\d+", minor) or not re.fullmatch(re.escape(minor) + r"\.\d+", env["qt_version"]):
            raise ValueError(f"Invalid Qt environment: {minor}")
        pin = env["qt_nixpkgs"]
        if not re.fullmatch(r"[a-f0-9]{40}", pin["rev"]) or not re.fullmatch(r"sha256-[A-Za-z0-9+/]{43}=", pin["hash"]):
            raise ValueError(f"Unpinned Qt environment: {minor}")
    for series, pin in manifest["kde_environments"].items():
        if not re.fullmatch(r"6\.\d+", series) or not re.fullmatch(r"[a-f0-9]{40}", pin["rev"]) or not re.fullmatch(r"sha256-[A-Za-z0-9+/]{43}=", pin["hash"]):
            raise ValueError(f"Unpinned KDE environment: {series}")
    for target in manifest["kwin"]:
        version = target["version"]
        if not re.fullmatch(r"6\.\d+\.\d+", version) or version in versions:
            raise ValueError(f"Invalid or duplicate KWin target: {version}")
        versions.add(version)
        if version.rsplit(".", 1)[0] not in manifest["kde_environments"]:
            raise ValueError(f"Missing KDE environment: {version}")
        if not re.fullmatch(r"[a-f0-9]{40}", target["rev"]) or not re.fullmatch(r"[0-9abcdfghijklmnpqrsvwxyz]{52}", target["sha256"]):
            raise ValueError(f"Unpinned KWin source: {version}")
        minors = target["qt_minors"]
        if not minors or len(set(minors)) != len(minors) or any(q not in manifest["environments"] for q in minors):
            raise ValueError(f"Invalid Qt targets: {version}")
    if not versions:
        raise ValueError("Empty KWin target matrix")
    return manifest


def targets(manifest, series=None, qt_minor=None, kwin_version=None):
    result = []
    for source in manifest["kwin"]:
        if kwin_version and source["version"] != kwin_version:
            continue
        if series and not source["version"].startswith(series + "."):
            continue
        for minor in source["qt_minors"]:
            if qt_minor and minor != qt_minor:
                continue
            env = manifest["environments"][minor]
            result.append({
                "kwin_version": source["version"], "qt_minor": minor,
                "qt_version": env["qt_version"], "architecture": manifest["architecture"],
                "source": source, "environment": env,
                "kde_environment": manifest["kde_environments"][source["version"].rsplit(".", 1)[0]],
            })
    if not result:
        raise ValueError("No matching KWin targets")
    return result


def input_identity(rows, root=ROOT):
    content = json.dumps(rows, sort_keys=True, separators=(",", ":")).encode()
    for name in INPUTS:
        content += b"\0" + name.encode() + b"\0" + (root / name).read_bytes()
    return digest(content)


def read_artifact(directory, bridge_id):
    required = ("plugin.so", "metadata.tsv", "build.json")
    if directory.is_symlink() or any((directory / n).is_symlink() or not (directory / n).is_file() for n in required):
        raise ValueError(f"Missing or linked artifact file: {directory}")
    fields = (directory / "metadata.tsv").read_text().strip().split("\t")
    if len(fields) != 5:
        raise ValueError(f"Invalid metadata: {directory}")
    kwin, qt, arch, source_id, plugin_id = fields
    binary = (directory / "plugin.so").read_bytes()
    if source_id != bridge_id or digest(binary) != plugin_id:
        raise ValueError(f"Stale source or corrupt plugin: {directory}")
    if len(binary) < 20 or binary[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<HH", binary, 16) != (3, 62):
        raise ValueError(f"Plugin is not an x86_64 ELF shared object: {directory}")
    if directory.name != f"{kwin}-{qt}-{arch}-{plugin_id}":
        raise ValueError(f"Artifact directory does not match metadata: {directory}")
    record = json.loads((directory / "build.json").read_text())
    if record.get("plugin_sha256") != plugin_id or record.get("loader_test") != "passed":
        raise ValueError(f"Missing loader validation: {directory}")
    return (kwin, qt, arch), record


def verify(directory, rows, bridge_id=None, root=ROOT):
    bridge_id = bridge_id or source_identity()
    expected = {(r["kwin_version"], r["qt_version"], r["architecture"]): r for r in rows}
    found = set()
    for artifact in directory.iterdir():
        if not artifact.is_dir():
            raise ValueError(f"Unexpected artifact entry: {artifact}")
        key, record = read_artifact(artifact, bridge_id)
        if key not in expected or key in found:
            raise ValueError(f"Unexpected or duplicate artifact: {key}")
        row = expected[key]
        if record.get("target") != row or record.get("inputs_sha256") != input_identity([row], root):
            raise ValueError(f"Artifact was built from different matrix inputs: {artifact}")
        found.add(key)
    missing = expected.keys() - found
    if missing:
        raise ValueError(f"Missing KWin artifacts: {sorted(missing)}")
    return len(found)


def build(directory, rows, cores, jobs):
    directory.mkdir(parents=True, exist_ok=True)
    for row in rows:
        expected_inputs = input_identity([row])
        nix_args = ["scripts/kwin-matrix/default.nix", "--argstr", "kwinVersion", row["kwin_version"],
                    "--argstr", "qtMinor", row["qt_minor"]]
        print(f"Building KWin {row['kwin_version']} / Qt {row['qt_version']}", flush=True)
        result = subprocess.run(["nix-build", *nix_args, "-A", "plugin", "--no-out-link",
                                 "--cores", str(cores), "--max-jobs", str(jobs)],
                                cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
        output = Path(result.stdout.strip())
        artifacts = list(output.iterdir())
        if len(artifacts) != 1 or not artifacts[0].is_dir():
            raise ValueError(f"Invalid Nix plugin output: {output}")
        artifact = artifacts[0]
        command = shlex.join(["timeout", "--kill-after=5", "60", "dbus-run-session", "--config-file=scripts/kwin-matrix/session.conf", "--", "bash", "scripts/test-kwin-plugin.sh", str(output)])
        subprocess.run(["nix-shell", *nix_args, "-A", "testEnvironment", "--pure", "--run", command,
                        "--cores", str(cores), "--max-jobs", str(jobs)], cwd=ROOT, check=True)
        current_row = targets(load_manifest(), qt_minor=row["qt_minor"], kwin_version=row["kwin_version"])[0]
        if current_row != row or input_identity([row]) != expected_inputs:
            raise ValueError("KWin build inputs changed during the build; rerun with stable inputs")
        destination = directory / artifact.name
        if destination.exists():
            # Re-running a partial matrix rechecks the loader before refreshing its record.
            read_artifact(destination, source_identity())
        else:
            shutil.copytree(artifact, destination)
            destination.chmod(0o755)
        metadata = (destination / "metadata.tsv").read_text().strip().split("\t")
        record = {"target": row, "inputs_sha256": expected_inputs,
                  "plugin_sha256": metadata[4], "loader_test": "passed"}
        (destination / "build.json").write_text(json.dumps(record, indent=2) + "\n")
        read_artifact(destination, source_identity())
    verify(directory, rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("matrix", "build", "verify"))
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--series")
    parser.add_argument("--qt-minor")
    parser.add_argument("--kwin-version")
    parser.add_argument("--cores", type=int, default=4)
    parser.add_argument("--jobs", type=int, default=1)
    args = parser.parse_args()
    manifest = load_manifest()
    rows = targets(manifest, args.series, args.qt_minor, args.kwin_version)
    if args.command == "matrix":
        groups = {}
        for row in rows:
            key = (row["kwin_version"].rsplit(".", 1)[0], row["qt_minor"])
            groups.setdefault(key, []).append(row)
        print(json.dumps({"include": [
            {"series": s, "qt_minor": q, "inputs_sha256": input_identity(group)}
            for (s, q), group in groups.items()
        ]}))
        return
    if args.directory is None:
        parser.error("--directory is required")
    if args.command == "build":
        if args.cores < 1 or args.jobs < 1:
            parser.error("--cores and --jobs must be positive")
        build(args.directory.resolve(), rows, args.cores, args.jobs)
    count = verify(args.directory, rows)
    print(f"Verified {count} KWin plugins with matching source, toolchain and loader records.")


if __name__ == "__main__":
    main()
