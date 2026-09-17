#!/bin/bash
# Compatibility entry point: all setup decisions belong to the shared flow.
set -euo pipefail
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RUNTIME_BINARY="${LG_BUDDY_RUNTIME_BINARY:-$SCRIPT_DIR/lg-buddy}"
if [ ! -x "$RUNTIME_BINARY" ] && [ -z "${LG_BUDDY_RUNTIME_BINARY:-}" ]; then
    RUNTIME_BINARY="$(command -v lg-buddy || true)"
fi
if [ -z "$RUNTIME_BINARY" ] || [ ! -x "$RUNTIME_BINARY" ]; then
    echo 'Install LG Buddy first with ./install.sh --headless, then rerun setup.' >&2
    exit 1
fi
args=()
if [ "${LG_BUDDY_NONINTERACTIVE:-0}" = 1 ]; then
    args+=(--non-interactive --yes)
fi
[ -z "${LG_BUDDY_TV_IP:-}" ] || args+=(--tv-ip "$LG_BUDDY_TV_IP")
[ -z "${LG_BUDDY_TV_MAC:-}" ] || args+=(--tv-mac "$LG_BUDDY_TV_MAC")
[ -z "${LG_BUDDY_INPUT:-}" ] || args+=(--input "$LG_BUDDY_INPUT")
exec "$RUNTIME_BINARY" setup "${args[@]}" "$@"
