#!/bin/bash
# `make release`: the public DMG, Developer ID signed and notarized.
#
#   DEVELOPER_ID    the signing identity: "Developer ID Application: Name (TEAMID)" or its SHA-1
#   NOTARY_PROFILE  the notarytool keychain profile (xcrun notarytool store-credentials …)
#   DRY_RUN=1       print the signing and notary commands instead of running them; everything
#                   else (build, bundle, DMG, cask) runs, so the flow can be checked without
#                   a certificate. The dry-run DMG holds an unsigned app: never publish it.
#
# The release builds exactly the committed tree: it refuses uncommitted or untracked files
# (a dry run only warns), builds with --locked (Cargo.lock as committed) and the toolchain
# rust-toolchain.toml pins, and writes "~" for the home folder into the binary's debug paths.
#
# Output: target/release/Stage-Manager.dmg (the name never changes; the release tag carries
# the version), a versioned copy beside it, and target/release/stage-manager.rb, the Homebrew
# cask with this DMG's version and sha256. One-time setup and the upload: docs/RELEASING.md.
# Local dev builds keep using scripts/sign.sh (the self-signed identity); this never does.
set -euo pipefail
cd "$(dirname "$0")/.."

DRY_RUN="${DRY_RUN:-0}"
APP="target/Stage Manager.app"
OUT="target/release"
DMG="$OUT/Stage-Manager.dmg"
ENTITLEMENTS="packaging/entitlements.plist"

fail() { echo "error: $*" >&2; exit 1; }
run() { # a step that needs the certificate or Apple's notary service
  if [ "$DRY_RUN" = 1 ]; then printf 'would run:'; printf ' %q' "$@"; echo; else "$@"; fi
}

if [ "$DRY_RUN" != 1 ]; then
  [ -n "${DEVELOPER_ID:-}" ] || fail "DEVELOPER_ID is not set: the \"Developer ID Application: …\" identity (docs/RELEASING.md)"
  [ -n "${NOTARY_PROFILE:-}" ] || fail "NOTARY_PROFILE is not set: the notarytool keychain profile (docs/RELEASING.md)"
  security find-identity -v -p codesigning | grep -qF -- "$DEVELOPER_ID" \
    || fail "no valid code-signing identity matching \"$DEVELOPER_ID\" in the keychain"
  case "$DEVELOPER_ID" in
    *"Developer ID Application"*|[0-9A-F][0-9A-F][0-9A-F][0-9A-F]*) ;;
    *) fail "DEVELOPER_ID must be a \"Developer ID Application: …\" identity (or its SHA-1)" ;;
  esac
fi
DIRTY="$(git status --porcelain --untracked-files=normal)"
if [ -n "$DIRTY" ]; then
  if [ "$DRY_RUN" = 1 ]; then
    echo "warning: uncommitted or untracked files (a real release refuses them):" >&2
    echo "$DIRTY" >&2
  else
    echo "$DIRTY" >&2
    fail "the tree has uncommitted or untracked files: commit or stash them; a release builds the committed tree only"
  fi
fi
IDENTITY="${DEVELOPER_ID:-Developer ID Application: (dry run)}"
PROFILE="${NOTARY_PROFILE:-(dry run)}"
plutil -lint -s "$ENTITLEMENTS"

# A fresh bundle: never re-sign a bundle the local identity signed. The binary's debug paths
# (backtraces) name the home folder as "~", not the account.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~"
nice -n 18 cargo build --release --locked
scripts/bundle.sh app
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
echo "Releasing Stage Manager $VERSION"

# The app: hardened runtime, secure timestamp, the (empty) entitlements.
run codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" -s "$IDENTITY" "$APP"
run codesign --verify --strict --deep --verbose=2 "$APP"

scripts/dmg.sh "$APP"
run codesign --force --timestamp -i dev.eugene.stagemanager.dmg -s "$IDENTITY" "$DMG"

# Notarize the DMG (it covers the app inside), then staple the ticket to it.
NOTARY_JSON="$OUT/notary.json"
if [ "$DRY_RUN" = 1 ]; then
  run xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait --output-format json
else
  xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait --output-format json | tee "$NOTARY_JSON"
  STATUS="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("status",""))' "$NOTARY_JSON")"
  if [ "$STATUS" != "Accepted" ]; then
    ID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id",""))' "$NOTARY_JSON")"
    echo "error: notarization ${STATUS:-failed}. The log:" >&2
    [ -n "$ID" ] && xcrun notarytool log "$ID" --keychain-profile "$PROFILE" >&2
    exit 1
  fi
fi
run xcrun stapler staple "$DMG"
run xcrun stapler validate "$DMG"

# Gatekeeper's view: the DMG as a download, and the app.
run spctl -a -t open --context context:primary-signature -vv "$DMG"
run spctl -a -t exec -vv "$APP"

cp "$DMG" "$OUT/Stage-Manager-$VERSION.dmg"
SHA="$(shasum -a 256 "$DMG" | cut -d' ' -f1)"
sed -e "s/@VERSION@/$VERSION/" -e "s/@SHA256@/$SHA/" packaging/homebrew/stage-manager.rb \
  | grep -v '^# ' > "$OUT/stage-manager.rb"

echo
echo "Done: $DMG (Stage Manager $VERSION, sha256 $SHA)"
[ "$DRY_RUN" = 1 ] && echo "DRY RUN: the app and DMG are unsigned and not notarized; do not publish them."
echo "Homebrew cask ($OUT/stage-manager.rb):"
echo
cat "$OUT/stage-manager.rb"
