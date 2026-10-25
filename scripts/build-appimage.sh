#!/usr/bin/env bash
# Generates an AppImage package for Linux distributions
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
VERSION=$(grep '^version' "$ROOT_DIR/Cargo.toml" | head -n1 | cut -d'"' -f2)
APP_DIR="$ROOT_DIR/dist/Nebula.AppDir"

echo "==> Building release binary..."
cd "$ROOT_DIR"
. ~/.cargo/env 2>/dev/null || true
cargo build --release

echo "==> Setting up AppDir..."
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/usr/bin"
mkdir -p "$APP_DIR/usr/share/icons/hicolor/256x256/apps"
mkdir -p "$APP_DIR/usr/share/applications"

install -m 755 "$ROOT_DIR/target/release/nebula" "$APP_DIR/usr/bin/nebula"
install -m 644 "$ROOT_DIR/assets/icon-256.png" "$APP_DIR/usr/share/icons/hicolor/256x256/apps/nebula.png"
install -m 644 "$ROOT_DIR/assets/icon-256.png" "$APP_DIR/nebula.png"

sed "s|@EXEC@|nebula|" "$ROOT_DIR/packaging/galaxy-linux.desktop" > "$APP_DIR/nebula.desktop"
cp "$APP_DIR/nebula.desktop" "$APP_DIR/usr/share/applications/nebula.desktop"

# AppRun script
cat <<'EOF' > "$APP_DIR/AppRun"
#!/bin/sh
SELF=$(readlink -f "$0")
HERE=${SELF%/*}
export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/lib:${LD_LIBRARY_PATH:-}"
export XDG_DATA_DIRS="${HERE}/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "${HERE}/usr/bin/nebula" "$@"
EOF
chmod +x "$APP_DIR/AppRun"

echo "==> Packaging with appimagetool if available..."
mkdir -p "$ROOT_DIR/dist"
if command -v appimagetool >/dev/null 2>&1; then
    ARCH=x86_64 appimagetool "$APP_DIR" "$ROOT_DIR/dist/Nebula-${VERSION}-x86_64.AppImage"
    echo "==> Created AppImage: $ROOT_DIR/dist/Nebula-${VERSION}-x86_64.AppImage"
else
    echo "AppDir prepared at: $APP_DIR"
    echo "Tip: Run 'appimagetool $APP_DIR $ROOT_DIR/dist/Nebula-${VERSION}-x86_64.AppImage' to build the single binary."
fi
