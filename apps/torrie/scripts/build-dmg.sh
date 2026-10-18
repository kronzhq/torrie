#!/bin/sh
# Builds target/Torrie.app (scripts/bundle.sh), then wraps it in a styled drag-to-install
# target/Torrie.dmg with dmgbuild (needs uv, for `uvx dmgbuild`). Ad-hoc signed, for testing; scripts/release.sh
# builds the signed, notarized one.
set -eu
cd "$(dirname "$0")/.."
scripts/bundle.sh

# Drag-to-install window: layout and background live in scripts/dmg-settings.py.
dmg=../../target/Torrie.dmg
rm -f "$dmg"
uvx dmgbuild -s scripts/dmg-settings.py -D app=../../target/Torrie.app Torrie "$dmg"
echo "Built $dmg"
