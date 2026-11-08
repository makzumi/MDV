#!/usr/bin/env bash
# Build dist/MDV-<version>-x86_64.AppImage from a release binary.
#
# The binary only links glibc; OpenGL/Wayland/X11 are loaded at runtime from
# the host, so no libraries need bundling. Build on the *oldest* distro you
# want to support (CI uses Ubuntu 22.04 → runs on glibc 2.35+).
set -euo pipefail

cd "$(dirname "$0")/../.."
VERSION="${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
ARCH="x86_64"
BIN="target/release/mdv"
DIST="dist"
APPDIR="$DIST/MDV.AppDir"
TOOL="$DIST/appimagetool-$ARCH.AppImage"

[[ -x "$BIN" ]] || cargo build --release --locked

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
install -m755 "$BIN" "$APPDIR/usr/bin/mdv"
install -m644 assets/mdv.desktop "$APPDIR/usr/share/applications/mdv.desktop"
install -m644 assets/mdv.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/mdv.png"
# appimagetool wants the desktop file + icon at the AppDir root too.
cp assets/mdv.desktop assets/mdv.png "$APPDIR/"
ln -sf mdv.png "$APPDIR/.DirIcon"
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/mdv" "$@"
EOF
chmod +x "$APPDIR/AppRun"

if [[ ! -x "$TOOL" ]]; then
    curl -fsSL -o "$TOOL" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
    chmod +x "$TOOL"
fi

# Extract-and-run avoids needing FUSE (e.g. in CI containers).
OUT="$DIST/MDV-$VERSION-$ARCH.AppImage"
ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT"
echo "Built $OUT"
