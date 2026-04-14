#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 [--dist-dir <dir>] [--tag <release-tag>]"
    exit 1
}

DIST_DIR="dist"
TAG="${GITHUB_REF_NAME:-}"
DRY_RUN="${GH_RELEASE_DRY_RUN:-0}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dist-dir)
            DIST_DIR="${2:-}"
            shift 2
            ;;
        --tag)
            TAG="${2:-}"
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$TAG" ] || {
    echo "Release tag must be provided via --tag or GITHUB_REF_NAME."
    exit 1
}

[ -d "$DIST_DIR" ] || {
    echo "Distribution directory not found: $DIST_DIR"
    exit 1
}

mapfile -t ARCHIVES < <(find "$DIST_DIR" -maxdepth 1 -type f -name '*.tar.gz' | sort)
[ "${#ARCHIVES[@]}" -gt 0 ] || {
    echo "No release archives found in $DIST_DIR"
    exit 1
}

CHECKSUM_FILE="$DIST_DIR/sha256sums.txt"
[ -f "$CHECKSUM_FILE" ] || {
    echo "Checksum file not found: $CHECKSUM_FILE"
    exit 1
}

VERSION="${TAG#release-}"
TITLE="LG Buddy ${VERSION}"
NOTES="Prebuilt LG Buddy release bundle for Linux. Extract the archive and run ./install.sh from inside the bundle."

if [ "$DRY_RUN" = "1" ]; then
    echo "Dry run: would publish tag $TAG"
    printf 'Archive: %s\n' "${ARCHIVES[@]}"
    echo "Checksum file: $CHECKSUM_FILE"
    echo "Title: $TITLE"
    exit 0
fi

if gh release view "$TAG" >/dev/null 2>&1; then
    gh release upload "$TAG" "${ARCHIVES[@]}" "$CHECKSUM_FILE" --clobber
else
    gh release create "$TAG" "${ARCHIVES[@]}" "$CHECKSUM_FILE" --title "$TITLE" --notes "$NOTES"
fi
