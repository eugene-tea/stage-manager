#!/bin/bash
# Copies the signed target/Stage Manager.app to one fixed place and prints that path.
# macOS asks "allow screen access?" once per app location (replayd keys its approvals by
# bundle path), so every worktree's target/ would ask again. Dev runs and tests launch from
# here instead: one approval covers every worktree. Override with SM_DEV_APP.
set -euo pipefail
cd "$(dirname "$0")/.."
DEST="${SM_DEV_APP:-$HOME/Library/Application Support/Stage Manager Dev/Stage Manager.app}"
# It is replaced whole (rm -rf): only ever an app bundle.
case "$DEST" in
  /*.app) ;;
  *) echo "error: SM_DEV_APP must be an absolute path ending in .app, got: $DEST" >&2; exit 1 ;;
esac
mkdir -p "$(dirname "$DEST")"
rm -rf "$DEST"
ditto "target/Stage Manager.app" "$DEST"
echo "$DEST"
