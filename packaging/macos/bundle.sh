#!/usr/bin/env bash
# Build Kaelis.app for macOS.
#
#   ./packaging/macos/bundle.sh            # native architecture
#   ./packaging/macos/bundle.sh --universal # arm64 + x86_64 (needs both targets)
#
# Requires the Rust toolchain and Xcode command line tools (for `sips`,
# `iconutil`, and `codesign`). The bundle is ad-hoc signed, which is enough to
# run locally; a distributed build needs a Developer ID identity.
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/kaelis-launcher/Cargo.toml | head -1)"
VERSION="${VERSION:-0.2.1}"
APP="$ROOT/dist/Kaelis.app"

if [[ "${1:-}" == "--universal" ]]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  cargo build --release -p kaelis-launcher --target aarch64-apple-darwin
  cargo build --release -p kaelis-launcher --target x86_64-apple-darwin
  BINARY="$ROOT/target/kaelis-universal"
  lipo -create \
    "$ROOT/target/aarch64-apple-darwin/release/kaelis" \
    "$ROOT/target/x86_64-apple-darwin/release/kaelis" \
    -output "$BINARY"
else
  cargo build --release -p kaelis-launcher
  BINARY="$ROOT/target/release/kaelis"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 755 "$BINARY" "$APP/Contents/MacOS/kaelis"

# Icon: draw the 1024 px master from the same mark geometry and let iconutil
# assemble the .icns. Skipped when the master PNG is missing.
MASTER="$ROOT/assets/kaelis-mark.png"
if [[ -f "$MASTER" ]] && command -v iconutil >/dev/null; then
  ICONSET="$ROOT/target/Kaelis.iconset"
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$MASTER" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$MASTER" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Kaelis.icns"
fi

sed "s/__VERSION__/$VERSION/g" "$ROOT/packaging/macos/Info.plist" > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc signature: required on Apple silicon, and it makes the bundle launch
# without the quarantine dialog when built locally.
codesign --force --deep --sign - "$APP" >/dev/null

echo "Built $APP"
