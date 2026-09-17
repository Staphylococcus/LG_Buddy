#!/bin/bash

# Exercise the final public surface and retained legacy commands against an
# actual runtime binary. Used for both fresh bundles and cross-version upgrades.
set -euo pipefail

BINARY="$(realpath "${1:?Usage: $0 <lg-buddy-binary>}")"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
export LG_BUDDY_CONFIG="$WORK_DIR/config.env"
export LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1
unset LG_BUDDY_SCREEN_BACKEND
printf '%s\n' '# retained behavior' 'screen_idle_timeout=731' > "$LG_BUDDY_CONFIG"
cp "$LG_BUDDY_CONFIG" "$WORK_DIR/original"

for backend in auto gnome wayland swayidle; do
    "$BINARY" settings set screen.backend "$backend" > "$WORK_DIR/set"
    [ "$("$BINARY" settings get screen.backend)" = "$backend" ]
    cp "$LG_BUDDY_CONFIG" "$WORK_DIR/saved"
    for command in list describe; do
        "$BINARY" settings "$command" > "$WORK_DIR/discovery"
        if grep -Fq 'screen.backend' "$WORK_DIR/discovery"; then
            echo "Public settings still advertise the legacy backend selector." >&2
            exit 1
        fi
        grep -Fq 'screen.idle_timeout' "$WORK_DIR/discovery"
    done
    "$BINARY" settings describe screen.backend > "$WORK_DIR/legacy"
    grep -Fq 'compatibility: legacy CLI only' "$WORK_DIR/legacy"
    grep -Fq 'an apply failure leaves the saved value in place' "$WORK_DIR/legacy"
    cmp "$WORK_DIR/saved" "$LG_BUDDY_CONFIG"
done

# Retain the old write contract, including failure and retry behavior, without
# addressing the host's service manager.
cat > "$WORK_DIR/systemctl" <<'EOF'
#!/bin/sh
case "$2" in
    cat|is-active|is-enabled) exit 0 ;;
    restart) [ "${LG_BUDDY_TEST_APPLY_OK:-0}" = 1 ]; exit $? ;;
esac
exit 24
EOF
chmod +x "$WORK_DIR/systemctl"
export LG_BUDDY_SYSTEMCTL="$WORK_DIR/systemctl"
if LG_BUDDY_SKIP_SYSTEMD_ACTIONS=0 "$BINARY" settings set screen.backend auto > "$WORK_DIR/failed" 2>&1; then
    echo 'Legacy write unexpectedly succeeded after a service apply failure.' >&2
    exit 1
fi
grep -Fq 'was saved' "$WORK_DIR/failed"
[ "$("$BINARY" settings get screen.backend)" = auto ]
[ "$("$BINARY" settings get screen.idle_timeout)" = 731 ]
cp "$LG_BUDDY_CONFIG" "$WORK_DIR/before-retry"
LG_BUDDY_SKIP_SYSTEMD_ACTIONS=0 LG_BUDDY_TEST_APPLY_OK=1 "$BINARY" settings set screen.backend auto > "$WORK_DIR/retry"
grep -Fq 'already set to auto' "$WORK_DIR/retry"
grep -Fq 'apply: restarted LG_Buddy_screen.service' "$WORK_DIR/retry"
cmp "$WORK_DIR/before-retry" "$LG_BUDDY_CONFIG"

"$BINARY" settings unset screen.backend > "$WORK_DIR/unset"
[ "$("$BINARY" settings get screen.backend)" = auto ]
cmp "$WORK_DIR/original" "$LG_BUDDY_CONFIG"
echo 'Settings compatibility passed: behavior discovery, explicit legacy commands and retained configuration.'
