#!/bin/bash
# Builds the installer DMG from a signed app bundle:
#   scripts/dmg.sh "<path>/Stage Manager.app"   -> target/release/Stage-Manager.dmg
# The file name never changes between releases (the version lives in the release tag), so
# .../releases/latest/download/Stage-Manager.dmg always serves the newest. A versioned copy,
# Stage-Manager-<version>.dmg, goes alongside. The volume is named "Stage Manager".
#
# dmgbuild runs from a private virtualenv under target/ (created on first use); it writes the
# Finder layout directly, so no AppleScript and no Automation approval. DMG_THEME=dark uses the
# dark background (Finder shows one background in both appearances; light is the default).
set -euo pipefail
cd "$(dirname "$0")/.."

APP="${1:?usage: $0 <app bundle>}"
[ -d "$APP" ] || { echo "error: $APP missing" >&2; exit 1; }
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
OUT_DIR="target/release"
DMG="$OUT_DIR/Stage-Manager.dmg"

VENV="target/dmgbuild-venv"
DMGBUILD_VERSION="1.6.5"
if [ ! -x "$VENV/bin/dmgbuild" ] || ! "$VENV/bin/pip" show dmgbuild 2>/dev/null | grep -q "^Version: $DMGBUILD_VERSION$"; then
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install --quiet --disable-pip-version-check "dmgbuild==$DMGBUILD_VERSION"
fi

mkdir -p "$OUT_DIR"
rm -f "$DMG"
"$VENV/bin/dmgbuild" -s packaging/dmg_settings.py \
  -D app="$APP" -D root="$PWD" -D theme="${DMG_THEME:-light}" \
  "Stage Manager" "$DMG"
cp "$DMG" "$OUT_DIR/Stage-Manager-$VERSION.dmg"
echo "Built $DMG (Stage Manager $VERSION; copy: $OUT_DIR/Stage-Manager-$VERSION.dmg)"
