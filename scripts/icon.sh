#!/bin/bash
# Regenerates the app icon (`make icon`). Needs Xcode 26 or later; `make` itself does not, as
# the outputs are committed.
#
#   art/icon.py          -> art/AppIcon.icon      Icon Composer document: icon.json + SVG layers
#   actool               -> resources/Assets.car  the Liquid Glass icon (CFBundleIconName, macOS 26+)
#   ictool + icon_frame  -> resources/AppIcon.icns the fallback for older macOS (CFBundleIconFile)
#                        -> docs/assets/app-icon.png, app-icon-dark.png  the info page's icons (512 px)
#
# The tools are called inside XCODE_DEVELOPER_DIR (default /Applications/Xcode.app/...), never
# through xcode-select. ICON_VARIANT picks another variant from art/icon.py.
set -euo pipefail
cd "$(dirname "$0")/.."

DEV="${XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
ACTOOL="$DEV/usr/bin/actool"
ICTOOL="$DEV/../Applications/Icon Composer.app/Contents/Executables/ictool"
for tool in "$ACTOOL" "$ICTOOL"; do
  [ -x "$tool" ] || { echo "error: $tool missing (Xcode 26 or later needed)" >&2; exit 1; }
done

# ICON_FROM_DOCUMENT=1 compiles art/AppIcon.icon as it is (after editing it in Icon Composer).
[ "${ICON_FROM_DOCUMENT:-0}" = 1 ] || python3 art/icon.py ${ICON_VARIANT:+--variant "$ICON_VARIANT"}

WORK="target/icon"
rm -rf "$WORK"
mkdir -p "$WORK/car" resources docs/assets

DEVELOPER_DIR="$DEV" "$ACTOOL" art/AppIcon.icon --compile "$WORK/car" --app-icon AppIcon \
  --platform macosx --target-device mac --minimum-deployment-target 14.0 \
  --output-partial-info-plist "$WORK/car/partial.plist" \
  --errors --warnings --output-format human-readable-text >"$WORK/actool.log"
grep -q "Assets.car" "$WORK/actool.log" || { cat "$WORK/actool.log" >&2; exit 1; }
cp "$WORK/car/Assets.car" resources/Assets.car

# actool's own .icns stops at 256 px; the fallback is built from Icon Composer's render instead.
DEVELOPER_DIR="$DEV" "$ICTOOL" art/AppIcon.icon --export-image --output-file "$WORK/render.png" \
  --platform macOS --rendition Default --width 1024 --height 1024 --scale 1 >/dev/null
swiftc -O -o "$WORK/icon_frame" art/tools/icon_frame.swift
"$WORK/icon_frame" "$WORK/render.png" "$WORK/AppIcon.iconset"
iconutil -c icns -o resources/AppIcon.icns "$WORK/AppIcon.iconset"
cp "$WORK/AppIcon.iconset/icon_256x256@2x.png" docs/assets/app-icon.png

# The info page's dark-appearance icon.
DEVELOPER_DIR="$DEV" "$ICTOOL" art/AppIcon.icon --export-image --output-file "$WORK/render-dark.png" \
  --platform macOS --rendition Dark --width 1024 --height 1024 --scale 1 >/dev/null
"$WORK/icon_frame" "$WORK/render-dark.png" "$WORK/Dark.iconset"
cp "$WORK/Dark.iconset/icon_256x256@2x.png" docs/assets/app-icon-dark.png

echo "Icon: resources/Assets.car, resources/AppIcon.icns, docs/assets/app-icon.png"
