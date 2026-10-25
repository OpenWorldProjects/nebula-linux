#!/usr/bin/env bash
# Installs the launcher entry + icon for the current user so GNOME/KDE show the
# right name and icon (window, dock, tray). Usage: scripts/install-local.sh [path-to-binary]
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${1:-$root/target/release/nebula}"
[ -x "$bin" ] || bin="$root/target/debug/nebula"
bin="$(realpath -s "$bin")"

data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
bin_dir="$HOME/.local/bin"
[ -x "$bin" ] || { echo "No executable found: $bin" >&2; exit 1; }
mkdir -p "$bin_dir" "$data_dir/applications"
install -m755 "$bin" "$bin_dir/nebula"
for size in 256 512; do
    icon="$root/assets/icon-256.png"
    [ "$size" != 512 ] || icon="$root/assets/icon.png"
    install -Dm644 "$icon" "$data_dir/icons/hicolor/${size}x${size}/apps/nebula.png"
done
# Use an absolute icon path locally; packaged builds use the hicolor theme.
python3 - "$root/packaging/galaxy-linux.desktop" "$data_dir/applications/nebula.desktop" "$bin_dir/nebula" "$data_dir/icons/hicolor/256x256/apps/nebula.png" <<'PYTHON'
import pathlib, sys
source, dest, executable, icon = sys.argv[1:]
escaped = executable.replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%', '%%')
text = pathlib.Path(source).read_text().replace('@EXEC@', '"' + escaped + '"').replace('Icon=nebula', 'Icon=' + icon)
pathlib.Path(dest).write_text(text)
PYTHON
# Hide the old duplicate entry while preserving existing desktop references.
if [ -f "$data_dir/applications/galaxy-linux.desktop" ]; then
    cp "$data_dir/applications/nebula.desktop" "$data_dir/applications/galaxy-linux.desktop"
    echo 'NoDisplay=true' >> "$data_dir/applications/galaxy-linux.desktop"
fi
update-desktop-database "$data_dir/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$data_dir/icons/hicolor" 2>/dev/null || true
echo "Installed: $bin_dir/nebula"
