#!/bin/bash
# configure.sh delegates without implementing pairing or changing saved settings.
set -euo pipefail
repo="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
cat > "$work/runtime" <<'SH'
#!/bin/sh
printf '%s\n' "$@" > "$TEST_ARGS"
printf '%s\n' "$LG_BUDDY_CONFIG" > "$TEST_CONFIG"
exit "${TEST_STATUS:-0}"
SH
chmod +x "$work/runtime"
printf '# preserve settings exactly\nscreen_backend=wayland\n' > "$work/config with spaces.env"
cp "$work/config with spaces.env" "$work/original"
export LG_BUDDY_RUNTIME_BINARY="$work/runtime" LG_BUDDY_CONFIG="$work/config with spaces.env"
export TEST_ARGS="$work/args" TEST_CONFIG="$work/config-path"
LG_BUDDY_NONINTERACTIVE=1 LG_BUDDY_TV_IP=192.0.2.1 LG_BUDDY_TV_MAC=02:11:22:33:44:55 LG_BUDDY_INPUT=HDMI_2 \
    bash "$repo/configure.sh" --allow-build-dependencies
printf '%s\n' setup --non-interactive --yes --tv-ip 192.0.2.1 --tv-mac 02:11:22:33:44:55 --input HDMI_2 --allow-build-dependencies > "$work/expected"
cmp "$work/args" "$work/expected"
[ "$(cat "$work/config-path")" = "$LG_BUDDY_CONFIG" ]
for result in 1 3 130; do
    status=0
    TEST_STATUS="$result" LG_BUDDY_NONINTERACTIVE=0 bash "$repo/configure.sh" || status=$?
    [ "$status" = "$result" ]
    cmp "$LG_BUDDY_CONFIG" "$work/original"
done
echo 'PASS: shared setup forwarding, literal arguments, status propagation and configuration preservation.'
