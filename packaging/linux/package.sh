#!/usr/bin/env bash
# Package Kaelis for Linux: tarball, Debian package, and AppImage.
#
#   ./packaging/linux/package.sh            # build a release binary first
#   ./packaging/linux/package.sh --no-build # package target/release/kaelis
#
# The tarball stays the minimal, dependency-free option. The .deb adds a
# desktop entry, icon, and package-managed uninstall. The AppImage is one file
# that runs from anywhere without root.
#
# The AppImage tool is pinned by version and SHA-256, because it is downloaded
# at packaging time rather than trusted from PATH. Set APPIMAGETOOL to use an
# already verified copy.
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/kaelis-launcher/Cargo.toml | head -1)"
VERSION="${VERSION:-0.0.0}"
ARCH="x86_64"
BIN="$ROOT/target/release/kaelis"
DIST="$ROOT/dist"
STAGE="$ROOT/target/linux-package"
APPIMAGETOOL_VERSION="1.9.1"
APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/appimagetool-x86_64.AppImage"

if [[ "${1:-}" != "--no-build" ]]; then
  cargo build --release -p kaelis-launcher
fi
if [[ ! -x "$BIN" ]]; then
  echo "missing $BIN; build it first or run without --no-build" >&2
  exit 1
fi

rm -rf "$STAGE"
mkdir -p "$STAGE" "$DIST"

# 1. Tarball: binary, desktop entry, icon, licence, readme.
TARBALL="$STAGE/kaelis-linux-x64"
mkdir -p "$TARBALL"
install -m 755 "$BIN" "$TARBALL/kaelis"
install -m 644 "$ROOT/packaging/linux/kaelis.desktop" "$TARBALL/kaelis.desktop"
install -m 644 "$ROOT/site/assets/kaelis-mark.svg" "$TARBALL/kaelis.svg"
install -m 644 "$ROOT/LICENSE" "$TARBALL/LICENSE"
install -m 644 "$ROOT/site/downloads/README.md" "$TARBALL/README.md"
tar -C "$STAGE" -czf "$DIST/Kaelis-linux-x64.tar.gz" kaelis-linux-x64

# 2. Debian package. dpkg-shlibdeps reads the real binary, so the dependency
#    list follows the libraries this build actually links instead of a guess
#    that rots when a window library changes name.
DEB="$STAGE/deb"
mkdir -p "$DEB/DEBIAN" \
  "$DEB/usr/bin" \
  "$DEB/usr/share/applications" \
  "$DEB/usr/share/icons/hicolor/scalable/apps" \
  "$DEB/usr/share/doc/kaelis"
install -m 755 "$BIN" "$DEB/usr/bin/kaelis"
install -m 644 "$ROOT/packaging/linux/kaelis.desktop" "$DEB/usr/share/applications/kaelis.desktop"
install -m 644 "$ROOT/site/assets/kaelis-mark.svg" "$DEB/usr/share/icons/hicolor/scalable/apps/kaelis.svg"
install -m 644 "$ROOT/LICENSE" "$DEB/usr/share/doc/kaelis/copyright"
install -m 644 "$ROOT/site/downloads/README.md" "$DEB/usr/share/doc/kaelis/README.md"
# The fallback names the desktop libraries the binary links on a normal
# Debian/Ubuntu install. t64 alternatives cover Ubuntu 24.04's rename without
# excluding the older packages. When dpkg-shlibdeps can resolve the real
# symbols it replaces the fallback with the exact list for the build host.
DEPENDS="libc6 (>= 2.31), libgcc-s1, libgtk-3-0 | libgtk-3-0t64, libxdo3, libayatana-appindicator3-1 | libappindicator3-1, libxkbcommon0, libwayland-client0, libx11-6, libxcursor1, libxrandr2, libxi6"
if command -v dpkg-shlibdeps >/dev/null; then
  computed="$(cd "$STAGE" && dpkg-shlibdeps -O "$BIN" 2>/dev/null \
    | sed -n 's/^shlibs:Depends=//p' || true)"
  if [[ -n "$computed" ]]; then
    DEPENDS="$computed"
  fi
fi
INSTALLED_SIZE="$(du -sk "$DEB/usr" | cut -f1)"
cat > "$DEB/DEBIAN/control" <<EOF
Package: kaelis
Version: $VERSION
Section: games
Priority: optional
Architecture: amd64
Installed-Size: $INSTALLED_SIZE
Depends: $DEPENDS
Recommends: libayatana-appindicator3-1 | libappindicator3-1
Maintainer: Kaelis Labs <kaelis@users.noreply.github.com>
Homepage: https://github.com/fuvji/kaelis
Description: Native Rust Minecraft launcher
 One workspace for Minecraft instances, mods, resource packs, shaders,
 modpacks, and accounts. Instances launch on a managed Java runtime, content
 installs from Modrinth and CurseForge with hash verification, and junk is
 cleaned automatically. No telemetry, no account required for the launcher.
EOF
# A deterministic maintainer script keeps `apt remove` from leaving the
# desktop database stale.
cat > "$DEB/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
fi
EOF
cat > "$DEB/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null; then
  update-desktop-database -q /usr/share/applications || true
fi
EOF
chmod 755 "$DEB/DEBIAN/postinst" "$DEB/DEBIAN/postrm"
dpkg-deb --build --root-owner-group "$DEB" "$DIST/Kaelis-linux-x64.deb" >/dev/null

# 3. AppImage. The launcher is a single static Rust binary apart from the
#    desktop libraries every distribution already ships, so the AppDir carries
#    the binary, its entry, and its icon without duplicating GTK.
APPDIR="$STAGE/Kaelis.AppDir"
mkdir -p "$APPDIR/usr/bin" \
  "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/512x512/apps"
install -m 755 "$BIN" "$APPDIR/usr/bin/kaelis"
install -m 644 "$ROOT/packaging/linux/kaelis.desktop" "$APPDIR/kaelis.desktop"
install -m 644 "$ROOT/packaging/linux/kaelis.desktop" "$APPDIR/usr/share/applications/kaelis.desktop"
install -m 644 "$ROOT/site/assets/kaelis-mark-512.png" "$APPDIR/kaelis.png"
install -m 644 "$ROOT/site/assets/kaelis-mark-512.png" "$APPDIR/usr/share/icons/hicolor/512x512/apps/kaelis.png"
install -m 644 "$ROOT/LICENSE" "$APPDIR/LICENSE"
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/kaelis" "$@"
EOF
chmod 755 "$APPDIR/AppRun"
ln -sf kaelis.png "$APPDIR/.DirIcon"

TOOL="${APPIMAGETOOL:-$ROOT/target/tools/appimagetool-x86_64.AppImage}"
if [[ ! -x "$TOOL" ]]; then
  mkdir -p "$(dirname "$TOOL")"
  echo "Downloading appimagetool $APPIMAGETOOL_VERSION..."
  curl -fsSL -o "$TOOL" "$APPIMAGETOOL_URL"
  echo "$APPIMAGETOOL_SHA256  $TOOL" | sha256sum -c - >/dev/null
  chmod +x "$TOOL"
fi
# CI has no FUSE. --appimage-extract-and-run unpacks the tool once and runs it,
# which also works on a developer machine without libfuse.
ARCH="$ARCH" "$TOOL" --appimage-extract-and-run "$APPDIR" "$DIST/Kaelis-linux-x64.AppImage" >/dev/null

echo "Packaged Kaelis $VERSION"
ls -lh "$DIST/Kaelis-linux-x64.tar.gz" "$DIST/Kaelis-linux-x64.deb" "$DIST/Kaelis-linux-x64.AppImage"
