#!/bin/bash
# Real helper file operations; systemctl and installation root are isolated.
set -euo pipefail
repo="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
test_path="$PATH"
source "$repo/data/setup-services.sh"
PATH="$test_path"
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
setup_root="$fixture/root"
payload="$repo/systemd"
config="$fixture/a % \"quoted\" config.env"
printf '%s\n' screen_idle_blank=enabled > "$config"
cp "$config" "$fixture/original.env"
mkdir -p "$fixture/enabled"
systemctl() {
    printf '%s\n' "$*" >> "$fixture/actions"
    case "$1" in
        show)
            if [ -f "$fixture/lifecycle-loaded" ]; then printf 'loaded\n'; else printf 'not-found\n'; fi
            ;;
        is-enabled) [ -f "$fixture/enabled/$3" ] ;;
        enable) touch "$fixture/enabled/$2" ;;
        stop)
            [ ! -f "$fixture/fail-stop" ] || return 1
            rm -f "$fixture/lifecycle-active"
            ;;
        daemon-reload)
            [ ! -f "$fixture/fail-reload" ] || return 1
            cp "$setup_root/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf" "$fixture/lifecycle-loaded"
            ;;
        restart)
            [ ! -f "$fixture/fail-start" ] || return 1
            cp "$fixture/lifecycle-loaded" "$fixture/lifecycle-running"
            touch "$fixture/lifecycle-active"
            ;;
        *) echo 'unexpected systemctl action' >&2; return 1 ;;
    esac
}
systemd-tmpfiles() {
    printf '%s\n' "$*" >> "$fixture/tmpfiles-actions"
    [ ! -f "$fixture/fail-after-reload" ]
}
package_owned() { [ -f "$fixture/package-owned" ]; }
repair_services "$config"
for unit in LG_Buddy.service LG_Buddy_lifecycle.service; do
    cmp "$payload/$unit" "$setup_root/etc/systemd/system/$unit"
    test -f "$fixture/enabled/$unit"
done
test -f "$fixture/lifecycle-active"
cmp "$config" "$fixture/original.env"
cmp "$payload/lg_buddy.conf" "$setup_root/etc/tmpfiles.d/lg_buddy.conf"
test "$(cat "$setup_root/usr/lib/lg-buddy/config-path")" = "$config"
test "$(stat -c %a "$setup_root/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle")" = 755
python3 - "$config" "$setup_root/etc/systemd/system/LG_Buddy.service.d/config.conf" <<'PY'
import pathlib, sys
escaped = sys.argv[1].replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%')
assert pathlib.Path(sys.argv[2]).read_text() == f'[Service]\nEnvironment="LG_BUDDY_CONFIG={escaped}"\n'
PY
# Reapplying leaves the files/inodes intact. Reconciliation of manager state
# remains intentional; the backend skips this helper for a complete setup.
before="$(find "$setup_root" -type f -printf '%p %i\n' | sort)"
: > "$fixture/actions"
repair_services "$config"
test "$before" = "$(find "$setup_root" -type f -printf '%p %i\n' | sort)"
! grep -q '^enable ' "$fixture/actions"
# Partial installation and interrupted manager reload recover on retry.
rm "$setup_root/etc/tmpfiles.d/lg_buddy.conf"
touch "$fixture/fail-reload"
: > "$fixture/actions"
if repair_services "$config"; then echo 'failed reload reported success' >&2; exit 1; fi
! grep -q '^restart ' "$fixture/actions"
rm "$fixture/fail-reload"
repair_services "$config"
cmp "$payload/lg_buddy.conf" "$setup_root/etc/tmpfiles.d/lg_buddy.conf"
# Interruption after reload must leave a stopped service, even though loaded
# properties already have the new binding. Retry starts a new process with it.
next_config="$fixture/next.env"
cp "$config" "$next_config"
touch "$fixture/fail-after-reload"
if repair_services "$next_config"; then echo 'interruption reported success' >&2; exit 1; fi
! cmp -s "$fixture/lifecycle-loaded" "$fixture/lifecycle-running"
test ! -f "$fixture/lifecycle-active"
rm "$fixture/fail-after-reload"
touch "$fixture/fail-start"
if repair_services "$next_config"; then echo 'failed start reported success' >&2; exit 1; fi
test ! -f "$fixture/lifecycle-active"
rm "$fixture/fail-start"
repair_services "$next_config"
test -f "$fixture/lifecycle-active"
cmp "$fixture/lifecycle-loaded" "$fixture/lifecycle-running"
# Failure to stop must leave both disk and loaded configuration untouched.
touch "$fixture/fail-stop"
if repair_services "$config"; then echo 'failed stop reported success' >&2; exit 1; fi
test "$(cat "$setup_root/usr/lib/lg-buddy/config-path")" = "$next_config"
cmp "$setup_root/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf" "$fixture/lifecycle-running"
test -f "$fixture/lifecycle-active"
rm "$fixture/fail-stop"
repair_services "$config"
# Symlinks and package-owned modifications cannot be overwritten.
unit="$setup_root/etc/systemd/system/LG_Buddy.service"
rm "$unit"
ln -s "$config" "$unit"
if repair_services "$config"; then echo 'replaced symlink' >&2; exit 1; fi
cmp "$config" "$fixture/original.env"
rm "$unit"
printf 'managed elsewhere\n' > "$unit"
touch "$fixture/package-owned"
if repair_services "$config" 2>/dev/null; then echo 'replaced package file' >&2; exit 1; fi
grep -qx 'managed elsewhere' "$unit"
rm "$fixture/package-owned"
repair_services "$config"
# Privilege/config validation never dispatches a repair with invalid inputs.
repair_services() { touch "$fixture/unsafe-main"; }
! main relative.env
! main "$config" unexpected
! main "$fixture/missing.env"
test ! -e "$fixture/unsafe-main"
echo 'PASS: service repair payloads, bindings, retries, ownership and unchanged-file preservation.'
