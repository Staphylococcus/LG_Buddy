#!/bin/bash

set -euo pipefail
umask 0022

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPOSITORY_ROOT="$(dirname "$SCRIPT_DIR")"
INSTALL_SCRIPT="$REPOSITORY_ROOT/install.sh"

[ "$(id -u)" -ne 0 ] || {
    echo "Graphical installer smoke must run as a regular user."
    exit 1
}

if ! command -v unshare >/dev/null 2>&1 || ! unshare -Ur true >/dev/null 2>&1; then
    echo "Graphical installer smoke requires unprivileged user namespaces."
    exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

BUNDLE="$WORK_DIR/bundle"
STUB_DIR="$WORK_DIR/stubs"
mkdir -p "$BUNDLE/docs" "$BUNDLE/systemd" "$BUNDLE/bin" "$STUB_DIR"
cp "$INSTALL_SCRIPT" "$BUNDLE/install.sh"
cp "$REPOSITORY_ROOT/bin/LG_Buddy_Common" "$BUNDLE/bin/LG_Buddy_Common"
chmod 755 "$BUNDLE/install.sh"

cat >"$BUNDLE/lg-buddy" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}" in
    --version)
        printf '%s\n' '1.7.0'
        ;;
    upgrade-preflight)
        exit 0
        ;;
    settings)
        case "${3:-}" in
            tv.platform) printf '%s\n' lg_webos ;;
            screen.idle_blank) printf '%s\n' enabled ;;
            screen.backend) printf '%s\n' auto ;;
            system.sleep_wake_policy) printf '%s\n' enabled ;;
            updates.auto_check) printf '%s\n' enabled ;;
            updates.channel) printf '%s\n' stable ;;
            *) exit 1 ;;
        esac
        ;;
    *) exit 1 ;;
esac
EOF
cat >"$BUNDLE/docs/lg-buddy-gui-x86_64-unknown-linux-gnu" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = --version ] || exit 1
printf '%s\n' '1.7.0'
EOF
cat >"$BUNDLE/LG_Buddy_Brightness.desktop" <<'EOF'
[Desktop Entry]
Name=LG Buddy
Exec=/usr/bin/lg-buddy
Icon=io.github.staphylococcus.LGBuddy
Terminal=false
Type=Application
EOF
cat >"$BUNDLE/docs/io.github.staphylococcus.LGBuddy.svg" <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>
EOF
for source in \
    LG_Buddy.service LG_Buddy_lifecycle.service lg_buddy.conf \
    LG_Buddy_screen.service LG_Buddy_update_check.service LG_Buddy_update_check.timer; do
    printf '%s\n' "$source" >"$BUNDLE/systemd/$source"
done
chmod 755 "$BUNDLE/lg-buddy" "$BUNDLE/docs/lg-buddy-gui-x86_64-unknown-linux-gnu"
chmod 644 "$BUNDLE/LG_Buddy_Brightness.desktop" "$BUNDLE/docs/io.github.staphylococcus.LGBuddy.svg"

cat >"$STUB_DIR/gui-runtime-probe" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$STUB_DIR/gui-runtime-probe"

cat >"$STUB_DIR/pkexec" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = --disable-internal-agent ] || exit 2
shift
case "${LG_BUDDY_GRAPHICAL_AUTH_MODE:?}" in
    cancelled)
        exit 126
        ;;
    unavailable)
        exit 127
        ;;
    accepted)
        exec unshare -Ur "$@"
        ;;
    accepted126)
        # BASH_ENV injects the failure after the real install utility copies
        # the first file without masking the helper's utility search paths.
        exec unshare -Ur env BASH_ENV="${LG_BUDDY_GRAPHICAL_AUTH_BASH_ENV:?}" "$@"
        ;;
    *)
        exit 2
        ;;
esac
EOF
chmod 755 "$STUB_DIR/pkexec"

REAL_INSTALL="$(command -v install)"
[ -x "$REAL_INSTALL" ] || {
    echo "Could not locate the host install utility for the isolated command stub."
    exit 1
}
cat >"$STUB_DIR/bash-env" <<'EOF'
install() {
    "${LG_BUDDY_GRAPHICAL_AUTH_REAL_INSTALL:?}" "$@"
    return 126
}
EOF

prepare_fixture() {
    local root="$1"
    local home="$2"
    mkdir -p \
        "$root/usr/bin" "$root/usr/lib/lg-buddy" \
        "$root/etc" "$root/usr/share" \
        "$home/.config/lg-buddy" "$home/Desktop"
    printf '%s\n' "$home/.config/lg-buddy/config.env" >"$root/usr/lib/lg-buddy/config-path"
    cat >"$home/.config/lg-buddy/config.env" <<'EOF'
tvs_primary_platform=lg_webos
screen_idle_blank=enabled
screen_backend=auto
system_sleep_wake_policy=enabled
updates_auto_check=enabled
updates_channel=stable
EOF
}

run_upgrade() {
    local scenario="$1"
    local mode="$2"
    local root="$WORK_DIR/$scenario/root"
    local home="$WORK_DIR/$scenario/home"
    local output="$WORK_DIR/$scenario/output"
    mkdir -p "$WORK_DIR/$scenario"
    prepare_fixture "$root" "$home"
    if [ "$scenario" = partial ]; then
        rm -rf "$root/usr/share"
        printf '%s\n' obstructed >"$root/usr/share"
    fi
    set +e
    PATH="$STUB_DIR:$PATH" \
    HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" \
    LG_BUDDY_INSTALL_ROOT="$root" \
    LG_BUDDY_SUDO_CMD=pkexec \
    LG_BUDDY_NONINTERACTIVE=1 \
    LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1 \
    LG_BUDDY_GUI_RUNTIME_PROBE="$STUB_DIR/gui-runtime-probe" \
    LG_BUDDY_GRAPHICAL_AUTH_BASH_ENV="$STUB_DIR/bash-env" \
    LG_BUDDY_GRAPHICAL_AUTH_REAL_INSTALL="$REAL_INSTALL" \
    LG_BUDDY_GRAPHICAL_AUTH_MODE="$mode" \
        bash "$BUNDLE/install.sh" --upgrade </dev/null >"$output" 2>&1
    RUN_STATUS=$?
    set -e
}

run_upgrade cancelled cancelled
[ "$RUN_STATUS" -eq 126 ] || { echo "Cancelled auth returned $RUN_STATUS."; exit 1; }
grep -F -q 'Graphical authorization was cancelled' "$WORK_DIR/cancelled/output"
! grep -F -q 'LG_BUDDY_INSTALL_STATUS=' "$WORK_DIR/cancelled/output"
[ ! -e "$WORK_DIR/cancelled/root/usr/bin/lg-buddy" ] || {
    echo "Cancelled auth mutated the installation root."
    exit 1
}

run_upgrade unavailable unavailable
[ "$RUN_STATUS" -eq 127 ] || { echo "Unavailable auth returned $RUN_STATUS."; exit 1; }
grep -F -q 'no authentication agent was available' "$WORK_DIR/unavailable/output"
! grep -F -q 'LG_BUDDY_INSTALL_STATUS=' "$WORK_DIR/unavailable/output"

run_upgrade partial accepted
[ "$RUN_STATUS" -ne 0 ] || { echo "Partial helper unexpectedly succeeded."; exit 1; }
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=authorized' "$WORK_DIR/partial/output"
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=mutation_started' "$WORK_DIR/partial/output"
! grep -F -q 'LG_BUDDY_INSTALL_STATUS=root_complete' "$WORK_DIR/partial/output"
grep -F -q 'The installation may be partial' "$WORK_DIR/partial/output"
[ -x "$WORK_DIR/partial/root/usr/bin/lg-buddy" ] || {
    echo "Partial helper did not leave the expected first mutation."
    exit 1
}

run_upgrade partial126 accepted126
[ "$RUN_STATUS" -eq 126 ] || { echo "Post-mutation status returned $RUN_STATUS instead of 126."; exit 1; }
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=authorized' "$WORK_DIR/partial126/output"
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=mutation_started' "$WORK_DIR/partial126/output"
! grep -F -q 'LG_BUDDY_INSTALL_STATUS=root_complete' "$WORK_DIR/partial126/output"
! grep -F -q 'Graphical authorization was cancelled' "$WORK_DIR/partial126/output"
grep -F -q 'The installation may be partial' "$WORK_DIR/partial126/output"
[ ! -e "$WORK_DIR/partial126/root/usr/bin/lg-buddy-gui" ] || {
    echo "Post-mutation command unexpectedly completed the GUI copy."
    exit 1
}
[ -x "$WORK_DIR/partial126/root/usr/bin/lg-buddy" ] || {
    echo "Post-mutation status did not retain the root-phase mutation."
    exit 1
}

run_upgrade success accepted
[ "$RUN_STATUS" -eq 0 ] || { echo "Accepted auth returned $RUN_STATUS."; exit 1; }
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=authorized' "$WORK_DIR/success/output"
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=mutation_started' "$WORK_DIR/success/output"
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=root_complete' "$WORK_DIR/success/output"
grep -F -x -q 'LG_BUDDY_INSTALL_STATUS=complete' "$WORK_DIR/success/output"
grep -F -q 'Upgrade complete!' "$WORK_DIR/success/output"
[ -x "$WORK_DIR/success/root/usr/bin/lg-buddy" ]
[ -x "$WORK_DIR/success/root/usr/bin/lg-buddy-gui" ]

EXPECTED_CONFIG="$WORK_DIR/expected-config"
cat >"$EXPECTED_CONFIG" <<'EOF'
tvs_primary_platform=lg_webos
screen_idle_blank=enabled
screen_backend=auto
system_sleep_wake_policy=enabled
updates_auto_check=enabled
updates_channel=stable
EOF
cmp -s "$WORK_DIR/success/home/.config/lg-buddy/config.env" "$EXPECTED_CONFIG"

echo "Graphical installer authorization smoke passed: cancelled=126 unavailable=127 partial=failed post-mutation-126=126 success=0."
