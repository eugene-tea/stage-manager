#!/bin/bash
# Signs a bundle with the stable local identity, found by SHA-1. Never ad-hoc: an ad-hoc
# signature changes every build and macOS forgets the Accessibility / Screen Recording
# grants.
set -euo pipefail

BUNDLE="${1:?usage: $0 <bundle>}"
NAME="${CERT_NAME:-Stage Manager Local Signing}"

# Not -v: the self-signed identity is listed as CSSMERR_TP_NOT_TRUSTED, which is fine.
SHA="$(security find-identity -p codesigning | awk -v n="\"$NAME\"" 'index($0, n) { print $2; exit }')"
if [ -z "$SHA" ]; then
  echo "error: code-signing identity \"$NAME\" not found in the keychain." >&2
  echo "       Create it once with scripts/make-cert.sh, then run make again." >&2
  exit 1
fi

# A new key needs one approval: if macOS asks to let codesign use the key, choose "Always
# Allow" (until then codesign waits on that dialog).
echo "Signing $BUNDLE (if macOS asks to use the \"$NAME\" key: Always Allow)"
codesign --force --options runtime --timestamp=none -s "$SHA" "$BUNDLE" &
CS=$!
# Unattended runs must not wait forever on the keychain dialog.
for _ in $(seq 1 "$(( ${SIGN_TIMEOUT:-300} * 2 ))"); do
  kill -0 "$CS" 2>/dev/null || break
  sleep 0.5
done
if kill -0 "$CS" 2>/dev/null; then
  kill "$CS" 2>/dev/null
  echo "error: codesign waited ${SIGN_TIMEOUT:-300} s for keychain approval of \"$NAME\"." >&2
  echo "       Approve it once (Always Allow), then run make again." >&2
  exit 1
fi
wait "$CS" || exit 1
echo "Signed $BUNDLE with \"$NAME\" ($SHA)"
