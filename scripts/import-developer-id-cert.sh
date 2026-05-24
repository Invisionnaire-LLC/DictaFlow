#!/usr/bin/env bash
# Import a Developer ID Application .cer downloaded from developer.apple.com.
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 /path/to/DeveloperIDApplication.cer"
  exit 1
fi

CER="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=signing-common.sh
source "$ROOT/scripts/signing-common.sh"

KEY_DIR="$HOME/.appstoreconnect/dictaflow"
KEY="$KEY_DIR/DeveloperIDApplication.key"
DEST="$KEY_DIR/DeveloperIDApplication.cer"
ENV_FILE="$ROOT/scripts/apple-release.env"

if [[ ! -f "$CER" ]]; then
  echo "Certificate not found: $CER"
  exit 1
fi

cp "$CER" "$DEST"
security import "$DEST" -k ~/Library/Keychains/login.keychain-db -T /usr/bin/codesign -T /usr/bin/security

if [[ -f "$KEY" ]]; then
  security import "$KEY" -k ~/Library/Keychains/login.keychain-db -T /usr/bin/codesign -A 2>/dev/null || \
    security import "$KEY" -k ~/Library/Keychains/login.keychain-db -T /usr/bin/codesign
fi

identity="$(resolve_developer_id_identity || true)"
if [[ -z "$identity" ]]; then
  echo "Certificate imported but no Developer ID identity found."
  echo "Ensure the .cer matches the CSR private key at $KEY"
  security find-identity -v -p codesigning
  exit 1
fi

echo "Installed: $identity"
echo

if [[ -f "$ENV_FILE" ]]; then
  if grep -q '^DEVELOPER_ID_SIGNING_IDENTITY=' "$ENV_FILE"; then
    sed -i '' "s|^DEVELOPER_ID_SIGNING_IDENTITY=.*|DEVELOPER_ID_SIGNING_IDENTITY=\"$identity\"|" "$ENV_FILE"
  else
    printf '\nDEVELOPER_ID_SIGNING_IDENTITY="%s"\n' "$identity" >>"$ENV_FILE"
  fi
else
  cp "$ROOT/scripts/apple-release.env.example" "$ENV_FILE"
  printf 'DEVELOPER_ID_SIGNING_IDENTITY="%s"\n' "$identity" >>"$ENV_FILE"
fi

echo "Updated $ENV_FILE"
echo "Next: ./scripts/setup-notary-credentials.sh && ./scripts/install-macos.sh"
