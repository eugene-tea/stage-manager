#!/bin/bash
# Assembles an app bundle under target/.
#   scripts/bundle.sh app      -> target/Stage Manager.app (from target/release/stage-manager)
#   scripts/bundle.sh fixture  -> target/SM Fixture.app    (compiles scripts/fixture/*.swift)
# FIXTURE_ID and FIXTURE_APP give the fixture another bundle ID and path, so test runs can use
# a fixture no other run manages (scripts/test-stages.sh does).
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

write_plist() { # path id name executable extra-keys
  cat > "$1" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>$2</string>
	<key>CFBundleName</key>
	<string>$3</string>
	<key>CFBundleDisplayName</key>
	<string>$3</string>
	<key>CFBundleExecutable</key>
	<string>$4</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>14.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
$5
</dict>
</plist>
PLIST
  plutil -lint -s "$1"
}

case "${1:-}" in
  app)
    APP="target/Stage Manager.app"
    BIN="target/release/stage-manager"
    [ -x "$BIN" ] || { echo "error: $BIN missing; run make build" >&2; exit 1; }
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    cp "$BIN" "$APP/Contents/MacOS/stage-manager"
    write_plist "$APP/Contents/Info.plist" dev.eugene.stagemanager "Stage Manager" stage-manager \
      $'\t<key>LSUIElement</key>\n\t<true/>'
    echo "Bundled $APP ($VERSION)"
    ;;
  fixture)
    APP="${FIXTURE_APP:-target/SM Fixture.app}"
    FIXTURE_ID="${FIXTURE_ID:-dev.eugene.stagemanager.fixture}"
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    swiftc -O -swift-version 5 -o "$APP/Contents/MacOS/sm-fixture" scripts/fixture/main.swift
    write_plist "$APP/Contents/Info.plist" "$FIXTURE_ID" "SM Fixture" sm-fixture ""
    echo "Bundled $APP ($VERSION)"
    ;;
  *)
    echo "usage: $0 app|fixture" >&2
    exit 2
    ;;
esac
