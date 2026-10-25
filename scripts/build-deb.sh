#!/usr/bin/env bash
# Generates a standalone .deb package for Debian/Ubuntu
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
VERSION=$(grep '^version' "$ROOT_DIR/Cargo.toml" | head -n1 | cut -d'"' -f2)
ARCH="amd64"
PKG_DIR="$ROOT_DIR/dist/nebula_${VERSION}_${ARCH}"

echo "==> Building release binary..."
cd "$ROOT_DIR"
. ~/.cargo/env 2>/dev/null || true
cargo build --release

echo "==> Preparing Debian package tree in $PKG_DIR..."
rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR/DEBIAN"
mkdir -p "$PKG_DIR/usr/bin"
mkdir -p "$PKG_DIR/usr/share/applications"
mkdir -p "$PKG_DIR/usr/share/icons/hicolor/256x256/apps"
mkdir -p "$PKG_DIR/usr/share/icons/hicolor/512x512/apps"

# Binary
install -m 755 "$ROOT_DIR/target/release/nebula" "$PKG_DIR/usr/bin/nebula"

# Desktop entry
sed "s|@EXEC@|/usr/bin/nebula|" "$ROOT_DIR/packaging/galaxy-linux.desktop" > "$PKG_DIR/usr/share/applications/nebula.desktop"
chmod 644 "$PKG_DIR/usr/share/applications/nebula.desktop"

# Icons
install -m 644 "$ROOT_DIR/assets/icon-256.png" "$PKG_DIR/usr/share/icons/hicolor/256x256/apps/nebula.png"
install -m 644 "$ROOT_DIR/assets/icon.png" "$PKG_DIR/usr/share/icons/hicolor/512x512/apps/nebula.png"

# Control file
cat <<EOF > "$PKG_DIR/DEBIAN/control"
Package: nebula
Version: $VERSION
Section: games
Priority: optional
Architecture: $ARCH
Maintainer: OpenWorldProjects <noreply@openworldprojects.org>
Depends: libc6, libwebkit2gtk-4.1-0 | libwebkit2gtk-4.0-37, libgtk-3-0, libx11-6
Recommends: zenity, wine | wine64
Description: Unofficial GOG launcher for Linux
 Library, store and game installs for GOG, with Windows games run through
 Proton or Wine. Cloud save sync is not implemented yet.
EOF

echo "==> Packaging .deb..."
dpkg-deb --build "$PKG_DIR" "$ROOT_DIR/dist/nebula_${VERSION}_${ARCH}.deb"
echo "==> Created: $ROOT_DIR/dist/nebula_${VERSION}_${ARCH}.deb"
