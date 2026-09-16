#!/bin/bash
# Privileged, fixed-scope native service repair. No caller-supplied commands.
set -euo pipefail
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
umask 022
setup_root=""
payload=/usr/lib/lg-buddy/setup/systemd

package_owned() {
    { command -v rpm >/dev/null 2>&1 && rpm -qf -- "$1" >/dev/null 2>&1; } \
        || { command -v dpkg-query >/dev/null 2>&1 && dpkg-query -S "$1" >/dev/null 2>&1; } \
        || { command -v pacman >/dev/null 2>&1 && pacman -Qo -- "$1" >/dev/null 2>&1; }
}

write_if_changed() {
    local destination="$1" mode="$2" temporary
    [ ! -L "$destination" ] || return 1
    mkdir -p -- "$(dirname -- "$destination")" || return 1
    temporary="$(mktemp "${destination}.XXXXXX")" || return 1
    if ! cat > "$temporary" || ! chmod "$mode" "$temporary"; then
        rm -f -- "$temporary"
        return 1
    fi
    if [ -f "$destination" ] && cmp -s "$temporary" "$destination" \
        && [ "$(stat -c %a "$destination")" = "$mode" ] \
        && [ "$(stat -c %u "$destination")" = "$EUID" ]; then
        rm -f "$temporary"
    else
        if package_owned "$destination"; then
            rm -f -- "$temporary"
            echo "Refusing to replace a package-owned file: $destination" >&2
            return 1
        fi
        mv -f -- "$temporary" "$destination" || { rm -f -- "$temporary"; return 1; }
    fi
}

cleanup_legacy_sleep_wake_handlers() {
    local path unit load_state
    local paths=(
        "$setup_root/etc/systemd/system/LG_Buddy_wake.service"
        "$setup_root/etc/systemd/system/LG_Buddy_sleep.service"
        "$setup_root/etc/systemd/system/LG_Buddy_wake.service.d/config.conf"
        "$setup_root/etc/systemd/system/LG_Buddy_sleep.service.d/config.conf"
        "$setup_root/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep"
        "$setup_root/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook"
    )
    # Check ownership before disabling or removing any legacy handler.
    for path in "${paths[@]}"; do
        if { [ -e "$path" ] || [ -L "$path" ]; } && package_owned "$path"; then
            echo "Refusing to remove a package-owned file: $path" >&2
            return 1
        fi
    done
    for unit in LG_Buddy_wake.service LG_Buddy_sleep.service; do
        load_state="$(systemctl show --property=LoadState --value "$unit")" || return 1
        if [ "$load_state" != not-found ]; then
            systemctl stop "$unit" || return 1
            systemctl disable "$unit" || return 1
        fi
    done
    # Stop and disable first: interruption must never leave an active handler
    # whose files have already been removed. Keep unrelated custom drop-ins.
    rm -f -- "${paths[@]}" || return 1
    for unit in LG_Buddy_wake.service LG_Buddy_sleep.service; do
        rmdir "$setup_root/etc/systemd/system/$unit.d" 2>/dev/null || true
    done
}

repair_services() {
    local config="$1" escaped unit load_state
    # Loaded Environment properties do not describe an already-running process.
    # Stop before writing/reloading so interruption leaves an inactive service
    # that the next read-only setup check can identify and repair.
    load_state="$(systemctl show --property=LoadState --value LG_Buddy_lifecycle.service)" || return 1
    if [ "$load_state" != not-found ]; then
        systemctl stop LG_Buddy_lifecycle.service || return 1
    fi
    cleanup_legacy_sleep_wake_handlers || return 1
    # Values are literal systemd Environment content, never shell source.
    escaped="${config//\\/\\\\}"
    escaped="${escaped//\"/\\\"}"
    escaped="${escaped//%/%%}"
    for unit in LG_Buddy.service LG_Buddy_lifecycle.service; do
        write_if_changed "$setup_root/etc/systemd/system/$unit" 644 < "$payload/$unit" || return 1
        write_if_changed "$setup_root/etc/systemd/system/$unit.d/config.conf" 644 < <(printf '[Service]\nEnvironment="LG_BUDDY_CONFIG=%s"\n' "$escaped") || return 1
    done
    write_if_changed "$setup_root/etc/tmpfiles.d/lg_buddy.conf" 644 < "$payload/lg_buddy.conf" || return 1
    write_if_changed "$setup_root/usr/lib/lg-buddy/config-path" 644 < <(printf '%s\n' "$config") || return 1
    write_if_changed "$setup_root/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle" 755 <<'HOOK' || return 1
#!/bin/sh
set -eu
[ "${2:-}" = pre-down ] || exit 0
exec /usr/bin/lg-buddy nm-pre-down
HOOK
    # Always reload after repair; this also recovers an interrupted prior write.
    systemctl daemon-reload || return 1
    systemd-tmpfiles --create "$setup_root/etc/tmpfiles.d/lg_buddy.conf" || return 1
    for unit in LG_Buddy.service LG_Buddy_lifecycle.service; do
        systemctl is-enabled --quiet "$unit" || systemctl enable "$unit" || return 1
    done
    # Called only for a verified incomplete setup. Start even when the files
    # already match: a previous attempt may have stopped between reload/start.
    systemctl restart LG_Buddy_lifecycle.service
}

main() {
    [ "$EUID" -eq 0 ] && [ "$#" -eq 1 ] || return 1
    local config="$1" owner
    case "$config" in /*) ;; *) return 1 ;; esac
    [[ "$config" != *$'\n'* && "$config" != *$'\r'* ]] || return 1
    [ -f "$config" ] && [ ! -L "$config" ] || return 1
    [ "$(readlink -f -- "$config")" = "$config" ] || return 1
    owner="${PKEXEC_UID:-${SUDO_UID:-}}"
    [[ "$owner" =~ ^[0-9]+$ ]] && [ "$owner" -ne 0 ] || return 1
    [ "$(stat -c %u -- "$config")" = "$owner" ] || return 1
    [ ! -e /etc/NIXOS ] && [ ! -e /run/ostree-booted ] || return 1
    repair_services "$config"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
