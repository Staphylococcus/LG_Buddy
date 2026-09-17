#!/bin/bash
# Optional integration setup. Never called from the inhibition query path.
# A failed provisioning attempt leaves the KWin source absent.
set -uo pipefail

# Establish the privileged PATH before even resolving this script's directory.
if [ "$EUID" -eq 0 ]; then
    PATH=/usr/sbin:/usr/bin:/sbin:/bin
    export PATH
fi

payload_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
runtime=/usr/bin/lg-buddy
foreground=0
allow_dependencies=0
noninteractive=0
terminal=0

plugin_root_supported() {
    case "$1" in
        /usr/lib64/qt6/plugins|/usr/lib/qt6/plugins|/usr/lib/x86_64-linux-gnu/qt6/plugins|/usr/lib/aarch64-linux-gnu/qt6/plugins) return 0 ;;
        *) return 1 ;;
    esac
}

valid_id() {
    [[ "$1" =~ ^lg_buddy_inhibition_[0-9]+_[0-9a-f]{64}$ ]]
}

package_owned() {
    { command -v rpm >/dev/null 2>&1 && rpm -qf -- "$1" >/dev/null 2>&1; } \
        || { command -v dpkg-query >/dev/null 2>&1 && dpkg-query -S "$1" >/dev/null 2>&1; } \
        || { command -v pacman >/dev/null 2>&1 && pacman -Qo -- "$1" >/dev/null 2>&1; }
}

system_action() {
    [ "$(id -u)" -eq 0 ] || return 1
    PATH=/usr/sbin:/usr/bin:/sbin:/bin
    export PATH
    local action="$1"
    shift
    case "$action" in
        --system-install)
            [ "$#" -eq 4 ] || return 1
            local uid="$1" root="$2" id="$3" source="$4"
            [[ "$uid" =~ ^[0-9]+$ ]] && valid_id "$id" || return 1
            [[ "$id" = "lg_buddy_inhibition_${uid}_"* ]] || return 1
            plugin_root_supported "$root" || return 1
            [ -d "$root/kwin/plugins" ] && [ ! -L "$root/kwin/plugins" ] || return 1
            # The source is data, compiled as the regular user. Never run a build as root.
            [ -f "$source" ] && [ ! -L "$source" ] || return 1
            local digest
            digest="$(sha256sum -- "$source")" || return 1
            [ "${digest%% *}" = "${id##*_}" ] || return 1
            local destination="$root/kwin/plugins/$id.so"
            package_owned "$destination" && return 1
            [ ! -e "$destination" ] || {
                [ -f "$destination" ] && [ ! -L "$destination" ] || return 1
                [ "$(sha256sum -- "$destination" | cut -d ' ' -f1)" = "${id##*_}" ] || return 1
                return 0
            }
            install -m 644 -o root -g root -- "$source" "$destination" || return 1
            if [ "$(sha256sum -- "$destination" | cut -d ' ' -f1)" != "${id##*_}" ]; then
                rm -f -- "$destination"
                return 1
            fi
            if command -v restorecon >/dev/null 2>&1; then restorecon "$destination" || return 1; fi
            ;;
        --system-remove)
            [ "$#" -eq 3 ] || return 1
            local uid="$1" root="$2" id="$3"
            [[ "$uid" =~ ^[0-9]+$ ]] && valid_id "$id" || return 1
            [[ "$id" = "lg_buddy_inhibition_${uid}_"* ]] || return 1
            plugin_root_supported "$root" || return 1
            [ -d "$root/kwin/plugins" ] && [ ! -L "$root/kwin/plugins" ] || return 1
            package_owned "$root/kwin/plugins/$id.so" && return 1
            rm -f -- "$root/kwin/plugins/$id.so"
            ;;
        --system-dependencies)
            [ "$#" -eq 1 ] && [[ "$1" =~ ^6\.[0-9]+\.[0-9]+$ ]] || return 1
            # Refuse to upgrade KWin incidentally while the old compositor is running.
            [ -x /usr/bin/kwin_wayland ] || return 1
            [ "$(/usr/bin/kwin_wayland --version | awk '{print $NF}')" = "$1" ] || return 1
            if [ -e /run/ostree-booted ] || [ -e /etc/NIXOS ]; then return 1; fi
            if [ -x /usr/bin/dnf ]; then
                local kwin_package
                kwin_package="$(rpm -q --qf '%{VERSION}-%{RELEASE}' kwin)" || return 1
                /usr/bin/dnf --setopt=install_weak_deps=False install -y \
                    "kwin-devel-$kwin_package" cmake gcc-c++ extra-cmake-modules \
                    qt6-qtbase-devel libepoxy-devel libdrm-devel
            elif [ -x /usr/bin/apt-get ]; then
                local kwin_package
                kwin_package="$(dpkg-query -W -f='${Version}' kwin-common)" || return 1
                /usr/bin/apt-get install -y --no-install-recommends \
                    "kwin-dev=$kwin_package" cmake g++ make pkg-config extra-cmake-modules \
                    qt6-base-dev qt6-declarative-dev libepoxy-dev libdrm-dev libvulkan-dev
            elif [ -x /usr/bin/pacman ]; then
                /usr/bin/pacman -S --needed --noconfirm gcc cmake make pkgconf extra-cmake-modules \
                    qt6-base qt6-declarative wayland libepoxy libdrm vulkan-headers
            else
                return 1
            fi
            ;;
        *) return 1 ;;
    esac
}

privileged() {
    # Match the installer privilege route, using trusted absolute executables.
    if [ "$(id -u)" -eq 0 ]; then
        /bin/bash "$payload_dir/setup.sh" "$@"
    elif [ -x /usr/bin/sudo ] && /usr/bin/sudo -n /usr/bin/true 2>/dev/null; then
        /usr/bin/sudo -n /bin/bash "$payload_dir/setup.sh" "$@"
    elif [ "$noninteractive" -eq 1 ]; then
        return 127
    elif [ "$terminal" -eq 1 ]; then
        /usr/bin/sudo /bin/bash "$payload_dir/setup.sh" "$@"
    elif [ -x /usr/bin/pkexec ]; then
        /usr/bin/pkexec --disable-internal-agent "$payload_dir/setup.sh" "$@"
    elif [ -t 0 ] && [ -x /usr/bin/sudo ]; then
        /usr/bin/sudo /bin/bash "$payload_dir/setup.sh" "$@"
    else
        return 1
    fi
}

configure_plugin() {
    local id="$1" enabled="$2"
    local command
    for command in kwriteconfig6 kwriteconfig; do
        if command -v "$command" >/dev/null 2>&1; then
            if [ "$enabled" = true ]; then
                "$command" --file kwinrc --group Plugins --key "${id}Enabled" true
            else
                "$command" --file kwinrc --group Plugins --key "${id}Enabled" --delete
            fi
            return
        fi
    done
    return 1
}

compatible_metadata() {
    local metadata="$1" extra
    IFS=$'\t' read -r candidate_kwin candidate_qt candidate_arch candidate_source candidate_digest extra < "$metadata" || return 1
    [ -z "${extra:-}" ] && [ "$candidate_kwin" = "$kwin_version" ] \
        && [ "$candidate_arch" = "$arch" ] && [ "$candidate_source" = "$source_id" ] || return 1
    [[ "$candidate_qt" =~ ^6\.([0-9]+)\.[0-9]+$ ]] || return 1
    local candidate_minor="${BASH_REMATCH[1]}"
    [[ "$qt_version" =~ ^6\.([0-9]+)\.[0-9]+$ ]] || return 1
    (( 10#$candidate_minor <= 10#${BASH_REMATCH[1]} )) || return 1
    [[ "$candidate_digest" =~ ^[0-9a-f]{64}$ ]] || return 1
    local file="${metadata%/*}/plugin.so"
    [ -f "$file" ] && [ ! -L "$file" ] || return 1
    [ "$(sha256sum -- "$file" | cut -d ' ' -f1)" = "$candidate_digest" ]
}

try_artifacts() {
    local directory="$1" provenance="$2" metadata id reply
    [ -d "$directory" ] || return 1
    while IFS= read -r -d '' metadata; do
        [ -f "$metadata" ] && [ ! -L "$metadata" ] || continue
        compatible_metadata "$metadata" || continue
        id="lg_buddy_inhibition_${uid}_${candidate_digest}"
        local installed="$plugin_root/kwin/plugins/$id.so"
        if [ ! -f "$installed" ] || [ -L "$installed" ] \
            || [ "$(sha256sum -- "$installed" | cut -d ' ' -f1)" != "$candidate_digest" ]; then
            privileged --system-install "$uid" "$plugin_root" "$id" "${metadata%/*}/plugin.so" || {
                local status=$?
                case "$status" in 126|127) return "$status" ;; esac
                continue
            }
        fi
        # Keep receipts even for rejected candidates, so cleanup can be retried
        # if authorization to remove a file is temporarily unavailable.
        printf '%s\t%s\n' "$plugin_root" "$id" > "$state_dir/plugins/$id.tsv"
        # A unique filename per artifact avoids Qt caching a rejected candidate
        # under the same name as a subsequent locally compiled plugin.
        reply="$("$runtime" kwin-bridge load "$id" 2>>"$log_file")" || {
            remove_plugin "$plugin_root" "$id" || {
                local status=$?
                case "$status" in 126|127) return "$status" ;; esac
            }
            continue
        }
        if [ "$reply" != "$kwin_version"$'\t'"$source_id" ]; then
            remove_plugin "$plugin_root" "$id" || {
                local status=$?
                case "$status" in 126|127) return "$status" ;; esac
            }
            continue
        fi
        if ! configure_plugin "$id" true; then
            remove_plugin "$plugin_root" "$id" || {
                local status=$?
                case "$status" in 126|127) return "$status" ;; esac
            }
            continue
        fi
        echo "LG Buddy: KWin source available ($provenance, KWin $kwin_version)."
        return 0
    done < <(find "$directory" -mindepth 2 -maxdepth 2 -type f -name metadata.tsv -print0 | sort -z)
    return 1
}

remove_plugin() {
    local root="$1" id="$2"
    valid_id "$id" && [[ "$id" = "lg_buddy_inhibition_${uid}_"* ]] || return 0
    "$runtime" kwin-bridge unload "$id" >>"$log_file" 2>&1 || true
    configure_plugin "$id" false || true
    if plugin_root_supported "$root" && privileged --system-remove "$uid" "$root" "$id"; then
        rm -f -- "$state_dir/plugins/$id.tsv"
    else
        local status=$?
        case "$status" in 126|127) return "$status" ;; esac
    fi
    return 0
}

remove_previous() {
    local receipt root id
    for receipt in "$state_dir"/plugins/*.tsv; do
        [ -f "$receipt" ] && [ ! -L "$receipt" ] || continue
        IFS=$'\t' read -r root id < "$receipt" || continue
        remove_plugin "$root" "$id" || {
            local status=$?
            case "$status" in 126|127) return "$status" ;; esac
        }
    done
    return 0
}

provision() {
    local status
    remove_previous || return $?
    try_artifacts "$payload_dir/prebuilt" prebuilt && return 0
    status=$?; case "$status" in 126|127) return "$status" ;; esac
    try_artifacts "$cache_dir" cached && return 0
    status=$?; case "$status" in 126|127) return "$status" ;; esac
    if ! /bin/bash "$payload_dir/build.sh" "$payload_dir/source" "$cache_dir" "$kwin_version" >>"$log_file" 2>&1; then
        # 77 is a typed request for a separately explained dependency installation.
        if [ "$foreground" -eq 1 ] && [ "$allow_dependencies" -ne 1 ]; then return 77; fi
        if privileged --system-dependencies "$kwin_version"; then
            /bin/bash "$payload_dir/build.sh" "$payload_dir/source" "$cache_dir" "$kwin_version" >>"$log_file" 2>&1 || true
        else
            status=$?; case "$status" in 126|127) return "$status" ;; esac
        fi
    fi
    try_artifacts "$cache_dir" locally-compiled && return 0
    status=$?; case "$status" in 126|127) return "$status" ;; esac
    echo "LG Buddy: KWin source absent; continuing with available sources. Setup details: $log_file"
    [ "$foreground" -ne 1 ]
}

# Structured status protocol: 0 ready, 2 inapplicable, 3 needs setup,
# 4 unsupported installation, 1 inspection failure. This function never writes.
inspect_session() {
    uid="$(id -u)"
    [ "$uid" -ne 0 ] || return 2
    state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/lg-buddy/kwin"
    cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/lg-buddy/kwin"
    log_file="$state_dir/setup.log"
    local info existing
    info="$("$runtime" kwin-bridge info)" || return 1
    [ -n "$info" ] || return 2
    IFS=$'\t' read -r kwin_version qt_version plugin_root kwin_owner <<< "$info"
    plugin_root_supported "$plugin_root" && [[ "$kwin_version" = 6.* ]] || return 4
    [ ! -e /run/ostree-booted ] && [ ! -e /etc/NIXOS ] || return 4
    source_id="$(cd -- "$payload_dir/source" && sha256sum CMakeLists.txt main.cpp metadata.json | sha256sum)" || return 1
    source_id="${source_id%% *}"
    arch="$(uname -m)"
    existing="$("$runtime" kwin-bridge check 2>/dev/null)" || return 3
    [ "$existing" = "$kwin_version"$'\t'"$source_id" ] || return 3
}

# Login may load an already installed artifact, but never installs or compiles.
load_installed() {
    local directory metadata id installed reply
    for directory in "$payload_dir/prebuilt" "$cache_dir"; do
        [ -d "$directory" ] || continue
        while IFS= read -r -d '' metadata; do
            compatible_metadata "$metadata" || continue
            id="lg_buddy_inhibition_${uid}_${candidate_digest}"
            installed="$plugin_root/kwin/plugins/$id.so"
            [ -f "$installed" ] && [ ! -L "$installed" ] || continue
            [ "$(sha256sum -- "$installed" | cut -d ' ' -f1)" = "$candidate_digest" ] || continue
            reply="$("$runtime" kwin-bridge load "$id")" || continue
            [ "$reply" = "$kwin_version"$'\t'"$source_id" ] && return 0
        done < <(find "$directory" -mindepth 2 -maxdepth 2 -type f -name metadata.tsv -print0 | sort -z)
    done
    return 0
}

main() {
    # Once authorized, operation failures must not resemble pkexec's 126/127.
    case "${1:-}" in --system-*) system_action "$@" || return 1; return 0 ;; esac
    if [ "${1:-}" = --remove ]; then
        uid="$(id -u)"
        state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/lg-buddy/kwin"
        cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/lg-buddy/kwin"
        if [ -d "$state_dir" ]; then
            log_file="$state_dir/setup.log"
            exec 9>"$state_dir/setup.lock"
            flock -n 9 || return 1
            remove_previous || return $?
        fi
        rm -rf -- "$cache_dir"
        return 0
    fi
    local mode="${1:-}" status
    case "$mode" in ""|--status|--foreground) ;; *) return 1 ;; esac
    [ "$mode" != --foreground ] || foreground=1
    for option in "${@:2}"; do
        case "$option" in
            --allow-dependencies) allow_dependencies=1 ;;
            --noninteractive) noninteractive=1 ;;
            --terminal) terminal=1 ;;
            *) return 1 ;;
        esac
    done
    if inspect_session; then status=0; else status=$?; fi
    if [ "$mode" = --status ]; then return "$status"; fi
    case "$status" in
        0) return 0 ;;
        3) ;;
        *) [ "$foreground" -eq 0 ] && return 0; return "$status" ;;
    esac
    if [ "$foreground" -eq 0 ]; then load_installed; return; fi
    mkdir -p -- "$state_dir/plugins" "$cache_dir" || return 1
    chmod 700 "$state_dir" "$cache_dir" || return 1
    exec 9>"$state_dir/setup.lock"
    flock -n 9 || return 1
    # Recheck after taking ownership; another setup may just have completed.
    if inspect_session; then return 0; else status=$?; fi
    [ "$status" -eq 3 ] || return "$status"
    : > "$log_file"
    provision
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
