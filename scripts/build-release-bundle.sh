#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 --target <runtime-target> --gui-target <gui-target> --version <version> [--output-dir <dir>] [--require-kwin-matrix]"
    exit 1
}

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="$REPO_ROOT/dist"
TARGET=""
GUI_TARGET=""
VERSION=""
REQUIRE_KWIN_MATRIX=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            TARGET="${2:-}"
            shift 2
            ;;
        --version)
            VERSION="${2:-}"
            shift 2
            ;;
        --gui-target)
            GUI_TARGET="${2:-}"
            shift 2
            ;;
        --output-dir)
            OUTPUT_DIR="${2:-}"
            shift 2
            ;;
        --require-kwin-matrix)
            REQUIRE_KWIN_MATRIX=1
            shift
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$TARGET" ] || usage
[ -n "$GUI_TARGET" ] || usage
[ -n "$VERSION" ] || usage

if [ "$REQUIRE_KWIN_MATRIX" -eq 1 ]; then
    python3 "$SCRIPT_DIR/kwin_matrix.py" verify --directory "$REPO_ROOT/target/kwin-bridges"
fi

BINARY_PATH="$REPO_ROOT/target/$TARGET/release/lg-buddy"
GUI_BINARY_PATH="$REPO_ROOT/target/$GUI_TARGET/release/lg-buddy-gui"
GUI_BUNDLE_PATH="docs/lg-buddy-gui-$GUI_TARGET"
APP_ICON_NAME="io.github.staphylococcus.LGBuddy.svg"
APP_ICON_SOURCE="$REPO_ROOT/data/icons/hicolor/scalable/apps/$APP_ICON_NAME"
APP_ICON_BUNDLE_PATH="docs/$APP_ICON_NAME"
BUNDLE_NAME="lg-buddy-$VERSION-$TARGET"
BUNDLE_DIR="$OUTPUT_DIR/$BUNDLE_NAME"
ARCHIVE_PATH="$OUTPUT_DIR/$BUNDLE_NAME.tar.gz"
PREFLIGHT_MANIFEST="$(mktemp)"
trap 'rm -f "$PREFLIGHT_MANIFEST"' EXIT

python3 "$SCRIPT_DIR/release_bundle_manifest.py" create \
    --output "$PREFLIGHT_MANIFEST" \
    --release-tag "v$VERSION" \
    --target "$TARGET" \
    --binary "$BINARY_PATH" \
    --gui-target "$GUI_TARGET" \
    --gui-binary "$GUI_BINARY_PATH" >/dev/null

[ ! -L "$APP_ICON_SOURCE" ] && [ -f "$APP_ICON_SOURCE" ] && [ -r "$APP_ICON_SOURCE" ] || {
    echo "Release application icon is missing or unsafe: $APP_ICON_SOURCE" >&2
    exit 1
}

rm -rf "$BUNDLE_DIR" "$ARCHIVE_PATH"

install -d "$BUNDLE_DIR"
install -d "$BUNDLE_DIR/bin"
install -d "$BUNDLE_DIR/docs"
install -d "$BUNDLE_DIR/systemd"

install -m 755 "$BINARY_PATH" "$BUNDLE_DIR/lg-buddy"
install -m 755 "$REPO_ROOT/install.sh" "$BUNDLE_DIR/install.sh"
install -m 755 "$REPO_ROOT/configure.sh" "$BUNDLE_DIR/configure.sh"
install -m 755 "$REPO_ROOT/uninstall.sh" "$BUNDLE_DIR/uninstall.sh"
install -m 755 "$REPO_ROOT/bin/LG_Buddy_Common" "$BUNDLE_DIR/bin/LG_Buddy_Common"
# Keep the legacy archive member name until pre-1.5 updaters no longer need to
# validate new bundles. install.sh still installs the application-ID filename.
install -m 644 "$REPO_ROOT/io.github.staphylococcus.LGBuddy.desktop" \
    "$BUNDLE_DIR/LG_Buddy_Brightness.desktop"
install -m 644 "$REPO_ROOT/README.md" "$BUNDLE_DIR/README.md"
install -m 644 "$REPO_ROOT/LICENSE" "$BUNDLE_DIR/LICENSE"
cp -R "$REPO_ROOT/docs/." "$BUNDLE_DIR/docs/"
# Keep optional bridge assets under docs/ for older verified bundle readers.
install -d "$BUNDLE_DIR/docs/kwin"
cp -R "$REPO_ROOT/data/kwin/." "$BUNDLE_DIR/docs/kwin/"
if [ -d "$REPO_ROOT/target/kwin-bridges" ]; then
    install -d "$BUNDLE_DIR/docs/kwin/prebuilt"
    cp -R "$REPO_ROOT/target/kwin-bridges/." "$BUNDLE_DIR/docs/kwin/prebuilt/"
fi
if [ "$REQUIRE_KWIN_MATRIX" -eq 1 ]; then
    python3 "$SCRIPT_DIR/kwin_matrix.py" verify --directory "$BUNDLE_DIR/docs/kwin/prebuilt"
    install -m 644 "$SCRIPT_DIR/kwin-matrix/targets.json" "$BUNDLE_DIR/docs/kwin/matrix.json"
fi
find "$BUNDLE_DIR/docs/kwin" -type d -exec chmod 755 {} +
find "$BUNDLE_DIR/docs/kwin" -type f -exec chmod 644 {} +
chmod 755 "$BUNDLE_DIR/docs/kwin/setup.sh" "$BUNDLE_DIR/docs/kwin/build.sh"
install -m 755 "$GUI_BINARY_PATH" "$BUNDLE_DIR/$GUI_BUNDLE_PATH"
install -m 644 "$APP_ICON_SOURCE" "$BUNDLE_DIR/$APP_ICON_BUNDLE_PATH"
# Documentation uses the source-tree icon path; older updaters require bundled
# assets to remain under docs/. Keep the extracted guide's image links usable.
sed -i "s|src=\"data/icons/hicolor/scalable/apps/$APP_ICON_NAME\"|src=\"$APP_ICON_BUNDLE_PATH\"|g" \
    "$BUNDLE_DIR/README.md"
sed -i "s|src=\"../data/icons/hicolor/scalable/apps/$APP_ICON_NAME\"|src=\"$APP_ICON_NAME\"|g" \
    "$BUNDLE_DIR/docs/user-guide.md"
python3 "$SCRIPT_DIR/release_bundle_manifest.py" create \
    --output "$BUNDLE_DIR/release-manifest.json" \
    --release-tag "v$VERSION" \
    --target "$TARGET" \
    --binary "$BUNDLE_DIR/lg-buddy" \
    --gui-target "$GUI_TARGET" \
    --gui-binary "$BUNDLE_DIR/$GUI_BUNDLE_PATH"
chmod 644 "$BUNDLE_DIR/release-manifest.json"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy.service" "$BUNDLE_DIR/systemd/LG_Buddy.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_lifecycle.service" "$BUNDLE_DIR/systemd/LG_Buddy_lifecycle.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_screen.service" "$BUNDLE_DIR/systemd/LG_Buddy_screen.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_update_check.service" "$BUNDLE_DIR/systemd/LG_Buddy_update_check.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_update_check.timer" "$BUNDLE_DIR/systemd/LG_Buddy_update_check.timer"
install -m 644 "$REPO_ROOT/systemd/lg_buddy.conf" "$BUNDLE_DIR/systemd/lg_buddy.conf"

tar -C "$OUTPUT_DIR" -czf "$ARCHIVE_PATH" "$BUNDLE_NAME"
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --archive "$ARCHIVE_PATH" \
    --expected-release-tag "v$VERSION" \
    --expected-version "$VERSION" \
    --expected-target "$TARGET" \
    --expected-gui-target "$GUI_TARGET"
echo "Created $ARCHIVE_PATH"
