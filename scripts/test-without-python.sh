#!/bin/bash
# Run the installer with only its native host tools on PATH. Python remains
# available to the outer test harness for fixtures and artifact validation.
set -euo pipefail

NATIVE_BIN="$(mktemp -d)"
trap 'rm -rf "$NATIVE_BIN"' EXIT
for command in \
    bash sh awk basename cat chmod chown cmp cp cut date dirname env find \
    getent grep head hostname id install ln mkdir mktemp mv readlink realpath \
    rm rmdir sed sleep sort stat tail tee touch tr uname wc \
    apt dnf pacman pkexec sudo systemctl systemd-tmpfiles; do
    path="$(command -v "$command" || true)"
    [ -z "$path" ] || ln -s "$path" "$NATIVE_BIN/$command"
done

export PATH="$NATIVE_BIN"
for command in python python3 pip pip3 virtualenv bscpylgtvcommand; do
    ! command -v "$command" >/dev/null || exit 1
done
"$@"
