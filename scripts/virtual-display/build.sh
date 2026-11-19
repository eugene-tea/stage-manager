#!/bin/bash
# Builds target/virtual-display (scripts/virtual-display/main.swift) when missing or older
# than its source; prints its path.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$here/../../target/virtual-display"
mkdir -p "$(dirname "$out")"
if [ ! -x "$out" ] || [ "$here/main.swift" -nt "$out" ] || [ "$here/CGVirtualDisplay.h" -nt "$out" ]; then
  swiftc -O -import-objc-header "$here/CGVirtualDisplay.h" "$here/main.swift" -o "$out" >&2
fi
echo "$out"
