#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="$ROOT/scripts/apple-release.env"
if [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE"
fi

: "${ASC_KEY_PATH:=$HOME/.appstoreconnect/private_keys/AuthKey_8T4628WQ3P.p8}"
: "${ASC_KEY_ID:=8T4628WQ3P}"
: "${ASC_ISSUER_ID:=e188a5e8-bef5-4a2c-a246-69805268fd07}"

PROFILE="${NOTARY_PROFILE:-DictaFlow}"

xcrun notarytool store-credentials "$PROFILE" \
  --key "$ASC_KEY_PATH" \
  --key-id "$ASC_KEY_ID" \
  --issuer "$ASC_ISSUER_ID"

echo "Stored notary credentials profile: $PROFILE"
