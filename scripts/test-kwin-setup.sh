#!/bin/bash
# Exercise the actual provisioner with isolated loader/privilege/build boundaries.
set -euo pipefail
repo="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
source "$repo/data/kwin/setup.sh"
set -e
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
export LG_BUDDY_CONFIG="$fixture/config.env"
printf '%s\n' screen_backend=auto screen_idle_timeout=731 screen_honor_idle_inhibitors=enabled > "$LG_BUDDY_CONFIG"
cp "$LG_BUDDY_CONFIG" "$fixture/original-config.env"
uid=1000
kwin_version=6.7.5
qt_version=6.11.2
arch=x86_64
source_id="$(printf source | sha256sum | cut -d ' ' -f1)"
export fixture source_id kwin_version qt_version arch
runtime="$fixture/runtime"
cat > "$runtime" <<'SH'
#!/bin/bash
set -eu
echo "runtime $*" >> "$fixture/actions"
if [ "$2" = load ]; then
    [ ! -e "$fixture/reject-all" ] || exit 1
    ! grep -q '^rejected$' "$fixture/plugins/kwin/plugins/$3.so" || exit 1
    printf '%s\t%s\n' "$kwin_version" "$source_id"
fi
SH
chmod 755 "$runtime"
plugin_root_supported() { [ "$1" = "$plugin_root" ]; }
configure_plugin() { echo "configure $*" >> "$fixture/actions"; }
privileged() {
    echo "privileged $*" >> "$fixture/actions"
    case "$1" in
        --system-install)
            [ ! -e "$fixture/deny-install" ] || return 1
            cp "$5" "$3/kwin/plugins/$4.so" ;;
        --system-remove)
            [ ! -e "$fixture/deny-remove" ] || return 1
            rm -f "$3/kwin/plugins/$4.so" ;;
        --system-dependencies) return 1 ;;
        *) return 1 ;;
    esac
}
artifact() {
    local directory="$1" contents="$2" version="${3:-$kwin_version}" digest
    mkdir -p "$directory"
    printf '%s\n' "$contents" > "$directory/plugin.so"
    digest="$(sha256sum "$directory/plugin.so" | cut -d ' ' -f1)"
    printf '%s\t%s\t%s\t%s\t%s\n' "$version" "$qt_version" "$arch" "$source_id" "$digest" > "$directory/metadata.tsv"
}
reset_case() {
    cmp "$LG_BUDDY_CONFIG" "$fixture/original-config.env"
    rm -rf "$fixture/payload" "$fixture/state" "$fixture/cache" "$fixture/plugins"
    rm -f "$fixture/actions" "$fixture/deny-install" "$fixture/deny-remove" "$fixture/reject-all" "$fixture/build-fails"
    payload_dir="$fixture/payload"
    cache_dir="$fixture/cache"
    state_dir="$fixture/state"
    plugin_root="$fixture/plugins"
    log_file="$fixture/setup.log"
    mkdir -p "$state_dir/plugins" "$payload_dir" "$cache_dir" "$plugin_root/kwin/plugins"
    cat > "$payload_dir/build.sh" <<'SH'
#!/bin/bash
set -eu
echo build >> "$fixture/actions"
[ ! -e "$fixture/build-fails" ] || exit 1
mkdir -p "$2/local"
echo local > "$2/local/plugin.so"
digest="$(sha256sum "$2/local/plugin.so" | cut -d ' ' -f1)"
printf '%s\t%s\t%s\t%s\t%s\n' "$kwin_version" "$qt_version" "$arch" "$source_id" "$digest" > "$2/local/metadata.tsv"
SH
}

reset_case
artifact "$payload_dir/prebuilt/matching" prebuilt
provision > "$fixture/result"
grep -q '(prebuilt,' "$fixture/result"
! grep -qE '^build|system-dependencies' "$fixture/actions"
test "$(find "$state_dir/plugins" -name '*.tsv' | wc -l)" -eq 1
remove_previous
test -z "$(find "$plugin_root" -name '*.so')"

for mode in absent wrong-version corrupt wrong-source newer-qt rejected; do
    reset_case
    if [ "$mode" != absent ]; then
        artifact "$payload_dir/prebuilt/candidate" "$mode"
        case "$mode" in
            wrong-version) sed -i 's/^6.7.5/6.7.4/' "$payload_dir/prebuilt/candidate/metadata.tsv" ;;
            corrupt) echo corrupt >> "$payload_dir/prebuilt/candidate/plugin.so" ;;
            wrong-source) sed -i "s/$source_id/$(printf old | sha256sum | cut -d ' ' -f1)/" "$payload_dir/prebuilt/candidate/metadata.tsv" ;;
            newer-qt) sed -i 's/6.11.2/6.12.0/' "$payload_dir/prebuilt/candidate/metadata.tsv" ;;
        esac
    fi
    provision > "$fixture/result"
    grep -q '(locally-compiled,' "$fixture/result"
    test "$(find "$plugin_root" -name '*.so' | wc -l)" -eq 1
    test "$(find "$state_dir/plugins" -name '*.tsv' | wc -l)" -eq 1
    : > "$fixture/actions"
    rm -rf "$payload_dir/prebuilt"
    provision > "$fixture/result"
    grep -q '(cached,' "$fixture/result"
    ! grep -qE '^build|system-dependencies' "$fixture/actions"
done

for mode in build-fails deny-install reject-all; do
    reset_case
    touch "$fixture/$mode"
    provision > "$fixture/result"
    grep -q 'source absent; continuing with available sources' "$fixture/result"
    test -z "$(find "$plugin_root" -name '*.so')"
    test -z "$(find "$state_dir/plugins" -name '*.tsv')"
done

reset_case
artifact "$payload_dir/prebuilt/candidate" rejected
touch "$fixture/deny-remove" "$fixture/build-fails"
provision > "$fixture/result"
test "$(find "$state_dir/plugins" -name '*.tsv' | wc -l)" -eq 1
rm "$fixture/deny-remove"
remove_previous
test -z "$(find "$state_dir/plugins" -name '*.tsv')"
test -z "$(find "$plugin_root" -name '*.so')"
cmp "$LG_BUDDY_CONFIG" "$fixture/original-config.env"
echo 'PASS: prebuilt without compilation, rejection/incompatibility fallback, cache, ordinary absence and cleanup retain the portable configuration.'

# CI's disposable Fedora container also exercises the real privileged helper.
if [ "$(id -u)" -eq 0 ] && [ -d /usr/lib64/qt6/plugins/kwin/plugins ]; then
    system_root=/usr/lib64/qt6/plugins
    artifact "$fixture/system" root-install
    digest="$(sha256sum "$fixture/system/plugin.so" | cut -d ' ' -f1)"
    id="lg_buddy_inhibition_424242_$digest"
    helper="$repo/data/kwin/setup.sh"
    ! bash "$helper" --system-install 424242 /tmp "$id" "$fixture/system/plugin.so"
    ! bash "$helper" --system-install 1000 "$system_root" "$id" "$fixture/system/plugin.so"
    ln -s plugin.so "$fixture/system/symlink.so"
    ! bash "$helper" --system-install 424242 "$system_root" "$id" "$fixture/system/symlink.so"
    mkdir "$fixture/shadow-bin"
    for command in dirname id sha256sum install; do
        printf '#!/bin/sh\necho unsafe > "%s"\nexit 1\n' "$fixture/shadow-executed" > "$fixture/shadow-bin/$command"
        chmod 755 "$fixture/shadow-bin/$command"
    done
    PATH="$fixture/shadow-bin:$PATH" /bin/bash "$helper" --system-install 424242 "$system_root" "$id" "$fixture/system/plugin.so"
    test ! -e "$fixture/shadow-executed"
    test "$(stat -c '%u:%a' "$system_root/kwin/plugins/$id.so")" = 0:644
    test "$(sha256sum "$system_root/kwin/plugins/$id.so" | cut -d ' ' -f1)" = "$digest"
    echo corrupted > "$system_root/kwin/plugins/$id.so"
    ! bash "$helper" --system-install 424242 "$system_root" "$id" "$fixture/system/plugin.so"
    bash "$helper" --system-remove 424242 "$system_root" "$id"
    test ! -e "$system_root/kwin/plugins/$id.so"
    package_owned /usr/bin/bash
    echo 'PASS: privileged install identity, paths, symlinks, ownership, corruption refusal and removal.'
fi
