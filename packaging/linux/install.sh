#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN_SRC="$ROOT_DIR/target/release/kalux"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_DIR/applications"
ICON_DIR="$DATA_DIR/icons/hicolor/scalable/apps"

if [ ! -x "$BIN_SRC" ] || [ "${1:-}" = "--rebuild" ]; then
    echo "Building kalux (release)..."
    cargo build --release -p kalux-app --manifest-path "$ROOT_DIR/Cargo.toml"
fi

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
install -Dm755 "$BIN_SRC" "$BIN_DIR/kalux"
install -Dm644 "$ROOT_DIR/packaging/linux/kalux.desktop" "$APP_DIR/kalux.desktop"
install -Dm644 "$ROOT_DIR/assets/icon.svg" "$ICON_DIR/kalux.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$DATA_DIR/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed:"
echo "  binary:  $BIN_DIR/kalux"
echo "  desktop: $APP_DIR/kalux.desktop"
echo "  icon:    $ICON_DIR/kalux.svg"
echo ""
echo "Optional — make Kalux the default viewer for common image types:"
echo "  xdg-mime default kalux.desktop image/jpeg image/png image/gif image/webp image/bmp image/tiff"
