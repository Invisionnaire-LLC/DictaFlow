#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=signing-common.sh
source "$ROOT/scripts/signing-common.sh"
load_release_env "$ROOT"
require_asc_env

PROFILE="${NOTARY_PROFILE:-DictaFlow}"

xcrun notarytool store-credentials "$PROFILE" \
  --key "$ASC_KEY_PATH" \
  --key-id "$ASC_KEY_ID" \
  --issuer "$ASC_ISSUER_ID"

echo "Stored notary credentials profile: $PROFILE"
