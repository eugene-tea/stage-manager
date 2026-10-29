#!/bin/bash
# Copies the signed target/Stage Manager.app to one fixed place and prints that path.
# macOS asks "allow screen access?" once per app location (replayd keys its approvals by
# bundle path), so every worktree's target/ would ask again. Dev runs and tests launch from
# here instead: one approval covers every worktree. Override with SM_DEV_APP (`make install` sets
# it to ~/Applications/Stage Manager.app).
#
# A copy running from there (any home: run.sh's, a test run's, its guardian) is never
# replaced: the script refuses first. The new copy is made beside it and swapped in, so a
# failed copy keeps the old one (review R7).
set -euo pipefail
cd "$(dirname "$0")/.."
# Never a bundle under / for want of a home (review C6).
if [ -z "${SM_DEV_APP:-}" ] && { [ -z "${HOME:-}" ] || [ "$HOME" = / ] || [ ! -d "$HOME" ]; }; then
  echo "error: HOME is not set to a folder; set it or SM_DEV_APP" >&2
  exit 1
fi
DEST="${SM_DEV_APP:-$HOME/Library/Application Support/Stage Manager Dev/Stage Manager.app}"
# It is replaced whole: only ever an app bundle.
case "$DEST" in
  /*.app) ;;
  *) echo "error: SM_DEV_APP must be an absolute path ending in .app, got: $DEST" >&2; exit 1 ;;
esac
# Any process whose executable is inside the dev copy.
if ps -axo command= | awk -v p="$DEST/Contents/MacOS/" 'index($0, p) == 1 { found = 1 } END { exit !found }'; then
  echo "error: Stage Manager is running from $DEST; quit it first (\"$DEST/Contents/MacOS/stage-manager\" ctl quit)." >&2
  exit 1
fi
DIR="$(dirname "$DEST")"
NEW="$DIR/.$(basename "$DEST").new.$$"
OLD="$DIR/.$(basename "$DEST").old.$$"
mkdir -p "$DIR"
trap 'rm -rf "$NEW"' EXIT
rm -rf "$NEW"
ditto "target/Stage Manager.app" "$NEW"
if [ -e "$DEST" ]; then
  mv "$DEST" "$OLD"
fi
if ! mv "$NEW" "$DEST"; then
  [ -e "$OLD" ] && mv "$OLD" "$DEST"
  echo "error: cannot put the new copy at $DEST" >&2
  exit 1
fi
rm -rf "$OLD"
echo "$DEST"
