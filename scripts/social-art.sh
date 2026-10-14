#!/bin/bash
# Renders the info page's link-preview card (`make social-art`) from art/social.py:
#   docs/assets/social-card.png   1200 × 630, for og:image and twitter:image in docs/index.html
# The PNG is committed. Rendering uses headless Chrome (never a visible window) with a
# throwaway profile; CHROME names another Chromium binary. The repository's own social
# preview on GitHub is set by hand: Settings → General → Social preview, upload this file.
set -euo pipefail
cd "$(dirname "$0")/.."

CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
[ -x "$CHROME" ] || { echo "error: no Chrome at $CHROME (set CHROME to a Chromium binary)" >&2; exit 1; }
SIZE="$(python3 -c 'import sys; sys.path.insert(0, "art"); import social; print(f"{social.WIDTH},{social.HEIGHT}")')"

WORK="target/social-art"
rm -rf "$WORK"
mkdir -p "$WORK/profile" docs/assets
python3 art/social.py "$WORK/card.html"
"$CHROME" --headless --disable-gpu --hide-scrollbars --mute-audio --no-first-run \
  --allow-file-access-from-files --user-data-dir="$PWD/$WORK/profile" --window-size="$SIZE" \
  --force-device-scale-factor=1 --screenshot="$PWD/docs/assets/social-card.png" \
  "file://$PWD/$WORK/card.html" >/dev/null 2>&1
echo "docs/assets/social-card.png"
