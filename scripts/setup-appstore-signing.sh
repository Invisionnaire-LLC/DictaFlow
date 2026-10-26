#!/usr/bin/env bash
# One-time (or yearly) App Store signing setup via asc CLI.
# Creates distribution certs, Mac App Store profile, and stores files under scripts/.signing/
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SIGNING="$ROOT/scripts/.signing"
# shellcheck source=signing-common.sh
source "$ROOT/scripts/signing-common.sh"
load_release_env "$ROOT"

mkdir -p "$SIGNING"
cd "$SIGNING"

if ! command -v asc >/dev/null 2>&1; then
  echo "Install asc first: brew tap rudrankriyam/tap && brew install asc"
  exit 1
fi

if ! asc auth status 2>/dev/null | grep -q '"credentials"'; then
  require_asc_env
  chmod 600 "$ASC_KEY_PATH" 2>/dev/null || true
  asc auth login \
    --name "DictaFlow" \
    --key-id "$ASC_KEY_ID" \
    --issuer-id "$ASC_ISSUER_ID" \
    --private-key "$ASC_KEY_PATH" \
    --network
fi

if [[ ! -f dictaflow.key ]]; then
  : "${CSR_COMMON_NAME:?Set CSR_COMMON_NAME (your name as it appears on the cert) in scripts/apple-release.env}"
  : "${CSR_EMAIL:?Set CSR_EMAIL in scripts/apple-release.env}"
  echo "Generating CSR + private key..."
  asc certificates csr generate \
    --common-name "$CSR_COMMON_NAME" \
    --email "$CSR_EMAIL" \
    --key-out ./dictaflow.key \
    --csr-out ./dictaflow.csr
  chmod 600 dictaflow.key
fi

create_cert() {
  local type="$1"
  local marker="$2"
  if [[ -f "$marker" ]]; then
    echo "  skip $type ($marker exists)"
    return
  fi
  echo "  creating $type..."
  asc certificates create --certificate-type "$type" --csr ./dictaflow.csr --output json > "$marker.json"
  python3 - <<PY
import json, base64, pathlib
data = json.load(open("$marker.json"))
attrs = data["data"]["attributes"]
pathlib.Path("$marker.cer").write_bytes(base64.b64decode(attrs["certificateContent"]))
print(f"    id={data['data']['id']} name={attrs['name']}")
PY
}

create_cert MAC_APP_DISTRIBUTION app-dist
create_cert MAC_INSTALLER_DISTRIBUTION installer-dist

echo "Importing key + certs into login keychain (may prompt once)..."
security import dictaflow.key -k ~/Library/Keychains/login.keychain-db -P "" \
  -T /usr/bin/codesign -T /usr/bin/productbuild -A 2>/dev/null || true
security import app-dist.cer -k ~/Library/Keychains/login.keychain-db \
  -T /usr/bin/codesign -T /usr/bin/productbuild -A 2>/dev/null || true
security import installer-dist.cer -k ~/Library/Keychains/login.keychain-db \
  -T /usr/bin/productbuild -A 2>/dev/null || true

PROFILE_ID="$(asc profiles list --profile-type MAC_APP_STORE --output json 2>/dev/null | python3 - <<'PY'
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for item in data.get("data", []):
    if item.get("attributes", {}).get("name") == "DictaFlow Mac App Store":
        print(item["id"])
        break
PY
)"

if [[ -z "${PROFILE_ID:-}" ]]; then
  : "${ASC_BUNDLE_ID_RESOURCE:?Set ASC_BUNDLE_ID_RESOURCE (bundle id resource from App Store Connect) in scripts/apple-release.env}"
  APP_CERT_ID="$(python3 - <<'PY'
import json
data = json.load(open("app-dist.json"))
print(data["data"]["id"])
PY
)"
  asc profiles create \
    --name "DictaFlow Mac App Store" \
    --profile-type MAC_APP_STORE \
    --bundle "$ASC_BUNDLE_ID_RESOURCE" \
    --certificate "$APP_CERT_ID" \
    --output json > profile.json
  PROFILE_ID="$(python3 - <<'PY'
import json
print(json.load(open("profile.json"))["data"]["id"])
PY
)"
fi

asc profiles download --id "$PROFILE_ID" --output "$SIGNING/DictaFlow.provisionprofile"

echo
echo "Signing identities:"
security find-identity -v -p codesigning 2>&1 | grep -E "Mac Developer|Distribution" || true
echo
echo "Profile: $SIGNING/DictaFlow.provisionprofile"
echo "Done. Run ./scripts/release-macos-appstore.sh to build, package, and upload."
