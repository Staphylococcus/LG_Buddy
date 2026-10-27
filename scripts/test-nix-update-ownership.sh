#!/usr/bin/env bash
set -euo pipefail

# Run outside the build sandbox: private build namespaces can expose the store's
# root owner as unmapped uid 65534, which the runtime deliberately cannot trust.
task_package="$(realpath "${1:?usage: test-nix-update-ownership.sh PACKAGE}")"
task_scratch="$(mktemp -d)"
trap 'rm -rf -- "$task_scratch"' EXIT
mkdir -p "$task_scratch/home" "$task_scratch/profile/bin" "$task_scratch/runtime"
chmod 700 "$task_scratch/runtime"
ln -s "$task_package/bin/lg-buddy" "$task_scratch/profile/bin/lg-buddy"

run_isolated() {
    env -i PATH="$PATH" HOME="$task_scratch/home" \
        XDG_CONFIG_HOME="$task_scratch/home/.config" \
        XDG_CACHE_HOME="$task_scratch/home/.cache" \
        XDG_STATE_HOME="$task_scratch/home/.local/state" \
        XDG_RUNTIME_DIR="$task_scratch/runtime" \
        LG_BUDDY_INSTALL_ROOT="$task_scratch/other-install" "$@"
}

check_launchers() {
    for task_launcher in "$task_package/bin/lg-buddy" "$task_scratch/profile/bin/lg-buddy"; do
        for task_command in check background-check; do
            run_isolated "$task_launcher" updates "$task_command" > "$task_scratch/check.log"
        done
        run_isolated "$task_launcher" updates check --notify > "$task_scratch/notify.log"
        if run_isolated "$task_launcher" updates install > "$task_scratch/install.log" 2>&1; then
            echo 'A Nix-owned runtime must not use the application installer' >&2
            exit 1
        fi
    done
}

check_launchers
test ! -e "$task_scratch/home/.config/lg-buddy/config.env"

# Inactive preferences must not become prerequisites for an external owner's
# update commands, even when a retained channel is invalid.
mkdir -p "$task_scratch/home/.config/lg-buddy"
printf 'updates_auto_check=enabled\nupdates_channel=not-a-channel\n' \
    > "$task_scratch/home/.config/lg-buddy/config.env"
cp "$task_scratch/home/.config/lg-buddy/config.env" "$task_scratch/retained-config.env"
check_launchers
cmp "$task_scratch/retained-config.env" "$task_scratch/home/.config/lg-buddy/config.env"
test ! -e "$task_scratch/other-install"
test ! -e "$task_scratch/home/.cache/lg-buddy"
test ! -e "$task_scratch/home/.local/state/lg-buddy"
echo 'Verified Nix update ownership through the store runtime and profile alias.'
