#!/usr/bin/env bash
# Validate the optional GNOME Shell extension: metadata shape and JS syntax.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXTENSIONS_DIR="$REPO_ROOT/data/gnome-shell"

count=0
for dir in "$EXTENSIONS_DIR"/*/; do
    dir="${dir%/}"
    python3 - "$dir" <<'PY'
import json
import pathlib
import sys

directory = pathlib.Path(sys.argv[1])
metadata = json.loads((directory / "metadata.json").read_text())
assert metadata["uuid"] == directory.name, (
    f"uuid {metadata['uuid']!r} must match directory {directory.name!r}"
)
for key in ("name", "description"):
    assert metadata.get(key), f"{directory.name}: missing {key}"
versions = metadata["shell-version"]
assert versions and all(v.isdigit() and int(v) >= 45 for v in versions), (
    f"{directory.name}: shell-version must list ESM-era major versions, got {versions}"
)
PY
    node --check "$dir/extension.js"
    count=$((count + 1))
done

[ "$count" -gt 0 ] || { echo "No GNOME Shell extensions found in $EXTENSIONS_DIR" >&2; exit 1; }
echo "Verified $count GNOME Shell extension(s)."
