#!/usr/bin/env bash
# Builds a direct-download release of Torrie in target/dist/: the universal app signed with a Developer ID,
# notarized and stapled; Torrie-X.Y.Z.zip of it, which the in-app updater (src/update.rs) installs; a
# drag-to-install DMG, itself signed, notarized and stapled; and latest.json, the updater's feed. Then uploads
# them to R2 kronz-dl/torrie/ (dl.kronz.xyz/torrie). Does NOT bump the version (edit Cargo.toml first) or commit.
#
#   apps/torrie/scripts/release.sh                  # full release
#   NOTES=$'Faster lists\nFixes…' …release.sh       # shown under "What's new" in the update window
#   REPLACE=1 …release.sh                           # re-release the live version (copies already on it don't update)
#
# Nothing about the developer account lives in this repo. One-time setup on the release Mac:
#   1. ~/.release.env (chmod 600; ENV_FILE overrides) exporting the Apple account: APPLE_SIGNING_IDENTITY (from
#      security find-identity -v -p codesigning), APPLE_ID, APPLE_PASSWORD (app-specific, from account.apple.com)
#      and APPLE_TEAM_ID.
#   2. brew install uv (for dmgbuild); rustup target add aarch64-apple-darwin x86_64-apple-darwin
#   3. bunx wrangler login, for the upload
set -euo pipefail
cd "$(dirname "$0")/.."

V=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
SITE="https://dl.kronz.xyz/torrie"
ENV_FILE="${ENV_FILE:-$HOME/.release.env}"
target=../../target
OUT=$target/dist
APP=$target/Torrie.app
ZIP=$OUT/Torrie-$V.zip
DMG=$OUT/Torrie-$V.dmg

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
notarize() { xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait; }

step "Checking setup"
[ -f "$ENV_FILE" ] || die "$ENV_FILE not found (see the top of this script)"
# shellcheck source=/dev/null
source "$ENV_FILE"
for v in APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
  [ -n "${!v:-}" ] || die "$v is empty in $ENV_FILE"
done
IDENTITY="$APPLE_SIGNING_IDENTITY"
command -v uvx >/dev/null || die "uvx not found (brew install uv)"
for t in aarch64-apple-darwin x86_64-apple-darwin; do
  rustup target list --installed | grep -qx "$t" || die "missing Rust target $t (rustup target add $t)"
done
LIVE=$(curl -sf "$SITE/latest.json?$(date +%s)" | sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' || true)
[ "$LIVE" != "$V" ] || [ -n "${REPLACE:-}" ] || die "$V is already live. Bump the version in Cargo.toml."
echo "Releasing Torrie $V (live now: ${LIVE:-none})"
read -r -p "Continue? [y/N] " ok
[ "$ok" = y ] || exit 1

step "Testing"
cargo test

step "Building for Apple silicon and Intel"
# macOS 12, the minimum, still runs on Intel Macs.
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
rm -rf "$OUT" && mkdir -p "$OUT"
lipo -create -output "$OUT/torrie" \
  "$target/aarch64-apple-darwin/release/torrie" "$target/x86_64-apple-darwin/release/torrie"

step "Bundling and signing the app"
BIN="$OUT/torrie" SIGN="$IDENTITY" scripts/bundle.sh
rm "$OUT/torrie"

step "Notarizing the app (a few minutes)"
ditto -c -k --keepParent "$APP" "$OUT/notarize.zip"
notarize "$OUT/notarize.zip"
rm "$OUT/notarize.zip"
xcrun stapler staple "$APP"
# Zipped again after stapling, so the update carries its ticket and opens offline.
ditto -c -k --keepParent "$APP" "$ZIP"

step "Building the DMG"
uvx dmgbuild -s scripts/dmg-settings.py -D app="$APP" "Torrie" "$DMG"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"
step "Notarizing the DMG"
notarize "$DMG"
xcrun stapler staple "$DMG"

step "Verifying"
[[ "$(spctl -a -vvv -t install "$DMG" 2>&1)" == *"Notarized Developer ID"* ]] || die "the DMG is not notarized"
[[ "$(spctl -a -vvv -t execute "$APP" 2>&1)" == *accepted* ]] || die "Gatekeeper rejects the app"
echo "The DMG and the app pass Gatekeeper"

step "Writing latest.json"
cp "$DMG" "$OUT/Torrie.dmg" # a stable name for the website's download button
NOTES="${NOTES:-}" V="$V" URL="$SITE/Torrie-$V.zip" SIZE="$(stat -f%z "$ZIP")" python3 -c '
import json, os, datetime
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
print(json.dumps({"version": os.environ["V"], "notes": os.environ["NOTES"], "pub_date": now,
                  "url": os.environ["URL"], "size": int(os.environ["SIZE"])}, indent=2))' > "$OUT/latest.json"

step "Uploading to R2 (latest.json last: it tells every copy to update)"
put() { bunx wrangler r2 object put "kronz-dl/torrie/$1" --file "$OUT/$1" --remote ${2:+--cache-control "$2"}; }
put "Torrie-$V.zip"
put "Torrie-$V.dmg"
put Torrie.dmg no-cache
put latest.json no-cache

step "Checking it's live"
# Cache-busting query: a 404 fetched too early would otherwise stay cached at the edge.
curl -sfI "$SITE/Torrie-$V.zip?$(date +%s)" -o /dev/null || die "the update zip is not reachable"
[ "$(curl -sf "$SITE/latest.json?$(date +%s)" | sed -n 's/.*"version": *"\([^"]*\)".*/\1/p')" = "$V" ] ||
  die "latest.json on $SITE doesn't say $V yet"

printf '\n\033[1;32mTorrie %s is live.\033[0m Download: %s/Torrie.dmg\n' "$V" "$SITE"
