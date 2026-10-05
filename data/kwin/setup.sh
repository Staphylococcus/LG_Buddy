#!/bin/bash
# Compatibility launcher for installed bundles and the authorization owner.
# KWin provisioning, validation, and locking live in the Rust application.
case "${BASH_SOURCE[0]}" in
    */*) payload_dir="${BASH_SOURCE[0]%/*}" ;;
    *) payload_dir=. ;;
esac
payload_dir="$(CDPATH= cd -- "$payload_dir" && pwd)"
runtime=/usr/bin/lg-buddy
main() {
    if declare -F _lg_buddy_kwin >/dev/null; then
        _lg_buddy_kwin "$runtime" "$payload_dir" "$@"
    else
        "$runtime" kwin-setup --payload-dir "$payload_dir" "$@"
    fi
}
if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
