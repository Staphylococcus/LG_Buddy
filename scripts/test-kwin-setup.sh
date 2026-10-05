#!/bin/bash
# Smoke the real native entry point with an empty executable search path.
# Artifact selection and privileged-owner contracts are covered by Rust tests.
set -euo pipefail
repo="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
runtime="${LG_BUDDY_RUNTIME_BINARY:-$repo/target/debug/lg-buddy}"
[ -x "$runtime" ] || { echo "Build lg-buddy before running the native KWin smoke." >&2; exit 1; }
runtime="$(readlink -f "$runtime")"
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
mkdir "$fixture/empty-path"
status=0
dbus-run-session -- env HOME="$fixture" XDG_STATE_HOME="$fixture/state" XDG_CACHE_HOME="$fixture/cache" \
    PATH="$fixture/empty-path" "$runtime" kwin-setup --payload-dir "$repo/data/kwin" --status || status=$?
test "$status" = 2
test ! -e "$fixture/state"
test ! -e "$fixture/cache"
mkdir -p "$fixture/state/lg-buddy/kwin"
env HOME="$fixture" XDG_STATE_HOME="$fixture/state" XDG_CACHE_HOME="$fixture/cache" \
    PATH="$fixture/empty-path" "$runtime" kwin-setup --remove
test -f "$fixture/state/lg-buddy/kwin/setup.lock"
echo 'PASS: native inspection is read-only and removal acquires its kernel lock without flock or shell utilities.'

# Exercise the compatibility entry point used by existing installed layouts.
mkdir "$fixture/payload"
sed "s|runtime=/usr/bin/lg-buddy|runtime=$runtime|" "$repo/data/kwin/setup.sh" > "$fixture/payload/setup.sh"
env HOME="$fixture" XDG_STATE_HOME="$fixture/state" XDG_CACHE_HOME="$fixture/cache" \
    bash "$fixture/payload/setup.sh" --remove
echo 'PASS: the installed compatibility launcher delegates to the native provisioner.'

# A disposable Fedora host can also validate the real privileged file boundary.
if [ "$(id -u)" -eq 0 ] && [ -d /usr/lib64/qt6/plugins/kwin/plugins ]; then
    root=/usr/lib64/qt6/plugins
    printf root-install > "$fixture/plugin.so"
    digest="$(sha256sum "$fixture/plugin.so" | cut -d ' ' -f1)"
    id="lg_buddy_inhibition_424242_$digest"
    ! "$runtime" kwin-setup --system-install 424242 /tmp "$id" "$fixture/plugin.so"
    ! "$runtime" kwin-setup --system-install 1000 "$root" "$id" "$fixture/plugin.so"
    ln -s plugin.so "$fixture/symlink.so"
    ! "$runtime" kwin-setup --system-install 424242 "$root" "$id" "$fixture/symlink.so"
    "$runtime" kwin-setup --system-install 424242 "$root" "$id" "$fixture/plugin.so"
    test "$(stat -c '%u:%a' "$root/kwin/plugins/$id.so")" = 0:644
    echo corrupted > "$root/kwin/plugins/$id.so"
    ! "$runtime" kwin-setup --system-install 424242 "$root" "$id" "$fixture/plugin.so"
    "$runtime" kwin-setup --system-remove 424242 "$root" "$id"
    test ! -e "$root/kwin/plugins/$id.so"
    echo 'PASS: native privileged install validates identity, paths, symlinks, ownership and corruption.'
fi
