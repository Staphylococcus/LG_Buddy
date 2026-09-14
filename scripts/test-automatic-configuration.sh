#!/bin/bash
# Exercise the real configuration transaction with isolated native/service edges.
set -euo pipefail
repo="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/bin" "$work/home/.config/systemd/user"
touch "$work/home/.config/systemd/user/LG_Buddy_screen.service"
cat > "$work/bin/runtime" <<'SH'
#!/bin/sh
[ "$1" = detect-backend ] || exit 2
[ "${TEST_NATIVE:-yes}" = yes ] || exit 1
echo gnome
SH
cat > "$work/bin/systemctl" <<'SH'
#!/bin/sh
if [ "$2" = restart ] && [ "${TEST_RESTART:-yes}" = no ]; then exit 1; fi
exit 0
SH
for name in ip ping; do
    printf '#!/bin/sh\nexit 0\n' > "$work/bin/$name"
done
chmod +x "$work/bin/"*
config="$work/home/config.env"
run_configure() {
    env HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" \
        LG_BUDDY_CONFIG="$config" LG_BUDDY_RUNTIME_BINARY="$work/bin/runtime" \
        LG_BUDDY_SKIP_SYSTEMD_ACTIONS=0 PATH="$work/bin:$PATH" "$@" \
        bash "$repo/configure.sh" > "$work/output" 2>&1
}
for backend in gnome wayland swayidle; do
    cat > "$config" <<EOF
# retain exact original bytes on failed/cancelled transition
tvs_primary_ip=192.0.2.10
tvs_primary_mac=02:00:00:00:00:01
tvs_primary_input=HDMI_2
tvs_primary_platform=bscpylgtv
screen_backend=$backend
screen_idle_blank=enabled
screen_idle_timeout=731
screen_honor_idle_inhibitors=enabled
screen_restore_policy=aggressive
EOF
    cp "$config" "$work/original"
    # TV address, MAC, input, platform, idle toggle, transition, timeout,
    # restore, app inhibition, and final confirmation.
    if printf '%s\n' '' '' '' '' '' y '' '' '' n | run_configure LG_BUDDY_NONINTERACTIVE=0; then
        echo 'Cancellation unexpectedly succeeded.' >&2; exit 1
    fi
    cmp "$config" "$work/original"
    for failure in TEST_NATIVE=no TEST_RESTART=no; do
        if run_configure LG_BUDDY_NONINTERACTIVE=1 LG_BUDDY_SCREEN_BACKEND=auto "$failure"; then
            echo "Failed transition unexpectedly succeeded: $failure" >&2; exit 1
        fi
        cmp "$config" "$work/original"
    done
    run_configure LG_BUDDY_NONINTERACTIVE=1 LG_BUDDY_SCREEN_BACKEND=auto
    grep -q '^screen_backend=auto$' "$config"
    for setting in screen_idle_timeout=731 screen_honor_idle_inhibitors=enabled screen_restore_policy=aggressive; do
        grep -q "^$setting$" "$config"
    done
    # A later login/reconfiguration does not redo native migration validation.
    cp "$config" "$work/automatic"
    run_configure LG_BUDDY_NONINTERACTIVE=1 TEST_NATIVE=no
    cmp "$config" "$work/automatic"
    cp "$work/original" "$config"
    sed -i 's/screen_idle_blank=enabled/screen_idle_blank=disabled/' "$config"
    run_configure LG_BUDDY_NONINTERACTIVE=1 LG_BUDDY_SCREEN_BACKEND=auto TEST_NATIVE=no
    grep -q '^screen_backend=auto$' "$config"
    grep -q '^screen_idle_blank=disabled$' "$config"
done
rm "$config"
# Fresh setup asks about behavior, with no desktop/backend question.
printf '%s\n' '192.0.2.10' '02:00:00:00:00:01' '2' '2' y 731 2 y y \
    | run_configure LG_BUDDY_NONINTERACTIVE=0
grep -q '^screen_backend=auto$' "$config"
! grep -q 'Choose the screen idle backend\|Use automatic desktop integration?' "$work/output"
echo 'PASS: fresh automatic setup, every legacy transition, cancellation, native/apply failure, disabled monitoring and portable reconfiguration.'
