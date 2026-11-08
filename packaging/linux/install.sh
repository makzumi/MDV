#!/usr/bin/env bash
# Install MDV for the current user (no root):
#   ~/.local/bin/mdv
#   ~/.local/share/applications/mdv.desktop
#   ~/.local/share/icons/hicolor/{256x256,scalable}/apps/mdv.{png,svg}
#
# The desktop entry is what gives MDV its icon on Wayland: the compositor
# matches the window's app id ("mdv") to mdv.desktop and uses its Icon=.
#
# Usage: packaging/linux/install.sh [--uninstall]
set -euo pipefail

cd "$(dirname "$0")/../.."
BIN_DIR="$HOME/.local/bin"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"

refresh() {
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$APP_DIR" || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$ICON_DIR" 2>/dev/null || true
    command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
    rm -f "$BIN_DIR/mdv" "$APP_DIR/mdv.desktop" \
        "$ICON_DIR/256x256/apps/mdv.png" "$ICON_DIR/scalable/apps/mdv.svg"
    refresh
    echo "MDV uninstalled."
    exit 0
fi

cargo build --release --locked
install -Dm755 target/release/mdv "$BIN_DIR/mdv"
install -Dm644 assets/mdv.png "$ICON_DIR/256x256/apps/mdv.png"
install -Dm644 assets/icon.svg "$ICON_DIR/scalable/apps/mdv.svg"
# Absolute Exec path so it works even if ~/.local/bin isn't on the session PATH.
sed "s|^Exec=mdv|Exec=$BIN_DIR/mdv|" assets/mdv.desktop > "$APP_DIR/mdv.desktop.tmp"
install -Dm644 "$APP_DIR/mdv.desktop.tmp" "$APP_DIR/mdv.desktop"
rm -f "$APP_DIR/mdv.desktop.tmp"
refresh

echo "MDV installed: $BIN_DIR/mdv (find it in your app launcher)."
