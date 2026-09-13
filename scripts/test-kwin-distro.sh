#!/bin/bash
# Run only inside a disposable distribution container with runtime packages.
set -euo pipefail
artifact_root="${1:?artifact directory required}"
kwin_version="$(rpm -q --qf '%{VERSION}\n' kwin)"
qt_version="$(rpm -q --qf '%{VERSION}\n' qt6-qtbase)"
qt_minor="$(cut -d . -f 2 <<< "$qt_version")"
[ ! -d /nix/store ]
! command -v c++ >/dev/null 2>&1
count=0
while IFS= read -r -d '' metadata; do
    IFS=$'\t' read -r candidate_kwin candidate_qt arch source_id digest < "$metadata"
    [ "$candidate_kwin" = "$kwin_version" ] || continue
    candidate_minor="$(cut -d . -f 2 <<< "$candidate_qt")"
    (( candidate_minor <= qt_minor )) || continue
    [ -z "$(readelf -d "${metadata%/*}/plugin.so" | sed -n '/RUNPATH\|RPATH/p')" ]
    timeout --kill-after=5 60 dbus-run-session --config-file=scripts/kwin-matrix/session.conf -- bash scripts/test-kwin-plugin.sh "${metadata%/*}"
    count=$((count + 1))
done < <(find "$artifact_root" -name metadata.tsv -type f -print0)
[ "$count" -gt 0 ]
echo "PASS: $count portable plugins loaded on KWin $kwin_version / Qt $qt_version without Nix or a compiler."
