#!/usr/bin/env bash
# Build dist/Kaelis-macos-universal.dmg from the bundle that bundle.sh created.
#
#   ./packaging/macos/bundle.sh --universal
#   ./packaging/macos/dmg.sh
#
# The disk image holds Kaelis.app and an Applications shortcut, so the install
# is one drag. hdiutil is part of macOS, so this adds no tooling to CI.
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/kaelis-launcher/Cargo.toml | head -1)"
VERSION="${VERSION:-0.0.0}"
APP="$ROOT/dist/Kaelis.app"
STAGE="$ROOT/target/dmg"
DMG="$ROOT/dist/Kaelis-macos-universal.dmg"

if [[ ! -d "$APP" ]]; then
  echo "missing $APP; run ./packaging/macos/bundle.sh --universal first" >&2
  exit 1
fi

rm -rf "$STAGE"
mkdir -p "$STAGE"
# `ditto` preserves the executable bit, the bundle metadata, and the ad-hoc
# signature. `cp -R` can drop extended attributes on some filesystems.
ditto "$APP" "$STAGE/Kaelis.app"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create \
  -volname "Kaelis $VERSION" \
  -srcfolder "$STAGE" \
  -fs HFS+ \
  -format UDZO \
  -ov \
  "$DMG" >/dev/null

echo "Packaged $DMG"
ls -lh "$DMG"
