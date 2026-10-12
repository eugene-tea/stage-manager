#!/bin/bash
# Creates a self-signed code-signing identity in the login keychain, once.
# A stable identity keeps the Accessibility and Screen Recording grants across rebuilds;
# ad-hoc signatures change with every build and macOS forgets the grants.
set -euo pipefail

NAME="${CERT_NAME:-Stage Manager Local Signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$NAME" "$KEYCHAIN" >/dev/null 2>&1; then
  echo "Identity '$NAME' already exists."
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/cert.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
CNF

# LibreSSL (/usr/bin/openssl) writes PKCS#12 with ciphers that `security import` reads.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$WORK/key.pem" -out "$WORK/cert.pem" -config "$WORK/cert.cnf" 2>/dev/null
/usr/bin/openssl pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
  -name "$NAME" -passout pass:stagemanager -out "$WORK/id.p12"

security import "$WORK/id.p12" -k "$KEYCHAIN" -P stagemanager -T /usr/bin/codesign

echo "Created '$NAME' in the login keychain."
echo "The first signing may ask to use the key: choose Always Allow."
