#!/usr/bin/env bash
# Install Kaelis for the current user (no root).
#
#   ./packaging/linux/install.sh           # build and install
#   ./packaging/linux/install.sh --no-build # install an existing release binary
#
# Layout follows the XDG base directory spec:
#   ~/.local/bin/kaelis
#   ~/.local/share/applications/kaelis.desktop
#   ~/.local/share/icons/hicolor/scalable/apps/kaelis.svg
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin"
APPLICATIONS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/scalable/apps"

if [[ "${1:-}" != "--no-build" ]]; then
  cargo build --release -p kaelis-launcher
fi

install -d "$BIN" "$APPLICATIONS" "$ICONS"
install -m 755 "$ROOT/target/release/kaelis" "$BIN/kaelis"
install -m 644 "$ROOT/packaging/linux/kaelis.desktop" "$APPLICATIONS/kaelis.desktop"
install -m 644 "$ROOT/site/assets/kaelis-mark.svg" "$ICONS/kaelis.svg"

# Refresh the desktop database when the helper exists; a minimal install
# without it still picks the entry up on the next login.
if command -v update-desktop-database >/dev/null; then
  update-desktop-database "$APPLICATIONS" >/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null || true
fi

echo "Installed $BIN/kaelis"
echo "Make sure $BIN is on PATH, then launch Kaelis from your application menu."
