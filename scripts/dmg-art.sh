#!/bin/bash
# Renders the DMG window's background (`make dmg-art`) from art/dmg.py and art/dmg/text.json:
#   art/dmg/background.png, background@2x.png          light (the one the DMG uses)
#   art/dmg/dark/background.png, background@2x.png     dark (DMG_THEME=dark)
# The PNGs are committed, so `make dmg` needs no browser. Rendering uses headless Chrome
# (never a visible window) with a throwaway profile; CHROME names another Chromium binary.
set -euo pipefail
cd "$(dirname "$0")/.."

CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
[ -x "$CHROME" ] || { echo "error: no Chrome at $CHROME (set CHROME to a Chromium binary)" >&2; exit 1; }
SIZE="$(python3 -c 'import sys; sys.path.insert(0, "art"); import dmg; print(f"{dmg.WIDTH},{dmg.HEIGHT}")')"

WORK="target/dmg-art"
rm -rf "$WORK"
mkdir -p "$WORK/profile" art/dmg/dark
for theme in light dark; do
  python3 art/dmg.py --theme "$theme" "$WORK/$theme.html"
  dir="art/dmg"; [ "$theme" = dark ] && dir="art/dmg/dark"
  for scale in 1 2; do
    name="background.png"; [ "$scale" = 2 ] && name="background@2x.png"
    "$CHROME" --headless --disable-gpu --hide-scrollbars --mute-audio --no-first-run \
      --user-data-dir="$PWD/$WORK/profile" --window-size="$SIZE" --force-device-scale-factor="$scale" \
      --screenshot="$PWD/$dir/$name" "file://$PWD/$WORK/$theme.html" >/dev/null 2>&1
    echo "$dir/$name"
  done
done
