#!/bin/sh
# Builds target/Torrie.app (ad-hoc signed) at the workspace root. Usage: apps/torrie/scripts/bundle.sh
# BIN=<path> bundles a prebuilt binary instead of building one; SIGN=<identity> signs for distribution
# (hardened runtime, secure timestamp) instead of ad hoc. scripts/release.sh sets both.
set -eu
cd "$(dirname "$0")/.."
target=../../target
if [ -z "${BIN:-}" ]; then
  cargo build --release
  BIN=$target/release/torrie
fi
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=$target/Torrie.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$BIN" "$app/Contents/MacOS/Torrie"
# Compiled from macos/AppIcon.icon (Icon Composer layers) so this needs no Xcode; after editing it, run
# xcrun actool macos/AppIcon.icon --compile macos --platform macosx --minimum-deployment-target 12.0 \
#   --app-icon AppIcon --output-partial-info-plist /dev/null
cp macos/Assets.car macos/AppIcon.icns "$app/Contents/Resources/"
sed "s/__VERSION__/$version/g" macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
if [ -n "${SIGN:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$SIGN" "$app"
else
  codesign --force --sign - "$app"
fi
echo "Built $app"
