#!/bin/bash
# Build only the bridge, using the host's matching KWin/Qt development files.
set -euo pipefail

source_dir="${1:?source directory required}"
output_dir="${2:?output directory required}"
expected_kwin="${3:-}"
source_dir="$(cd -- "$source_dir" && pwd)"
mkdir -p -- "$output_dir"
output_dir="$(cd -- "$output_dir" && pwd)"
build_dir="$(mktemp -d "$output_dir/.build.XXXXXX")"
trap 'rm -rf -- "$build_dir"' EXIT
source_id="$(cd -- "$source_dir" && sha256sum CMakeLists.txt main.cpp metadata.json | sha256sum)"
source_id="${source_id%% *}"
cmake -S "$source_dir" -B "$build_dir" -DCMAKE_BUILD_TYPE=Release \
    -DLG_BUDDY_KWIN_BUILD_ID="$source_id"
IFS=$'\t' read -r kwin qt arch actual_source < "$build_dir/build-info.tsv"
if [ -n "$expected_kwin" ] && [ "$kwin" != "$expected_kwin" ]; then
    echo "Development files target KWin $kwin; the running compositor is $expected_kwin." >&2
    exit 1
fi
[[ "$kwin" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && "$qt" =~ ^6\.[0-9]+\.[0-9]+$ ]]
[[ "$arch" =~ ^[a-zA-Z0-9_]+$ && "$actual_source" = "$source_id" ]]
cmake --build "$build_dir" --parallel 2
strip --strip-unneeded "$build_dir/lg_buddy_inhibition.so"
digest="$(sha256sum "$build_dir/lg_buddy_inhibition.so")"
digest="${digest%% *}"
artifact_dir="$output_dir/$kwin-$qt-$arch-$digest"
mkdir -p -- "$artifact_dir"
install -m 644 "$build_dir/lg_buddy_inhibition.so" "$artifact_dir/plugin.so"
printf '%s\t%s\t%s\t%s\t%s\n' "$kwin" "$qt" "$arch" "$source_id" "$digest" > "$artifact_dir/metadata.tsv"
echo "Built KWin bridge: $artifact_dir"
