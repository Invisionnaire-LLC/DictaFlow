#!/usr/bin/env bash
# One-time (or yearly) App Store signing setup via asc CLI.
# Creates distribution certs, Mac App Store profile, and stores files under scripts/.signing/
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SIGNING="$ROOT/scripts/.signing"
BUNDLE_ID_RESOURCE="2F22H477C3"

mkdir -p "$SIGNING"
cd "$SIGNING"

if ! command -v asc >/dev/null 2>&1; then
  echo "Install asc first: brew tap rudrankriyam/tap && brew install asc"
  exit 1
fi

if ! asc auth status 2>/dev/null | grep -q '"credentials"'; then
  ENV_FILE="$ROOT/scripts/apple-release.env"
  if [[ -f "$ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    source "$ENV_FILE"
  fi
  chmod 600 "${ASC_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_8T4628WQ3P.p8}" 2>/dev/null || true
  asc auth login \
    --name "DictaFlow" \
    --key-id "${ASC_KEY_ID:-8T4628WQ3P}" \
    --issuer-id "${ASC_ISSUER_ID:-e188a5e8-bef5-4a2c-a246-69805268fd07}" \
    --private-key "${ASC_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_8T4628WQ3P.p8}" \
    --network
fi

if [[ ! -f dictaflow.key ]]; then
  echo "Generating CSR + private key..."
  asc certificates csr generate \
    --common-name "Adrian Rosario" \
    --email "arosario@invisionnaire.com" \
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
  APP_CERT_ID="$(python3 - <<'PY'
import json
data = json.load(open("app-dist.json"))
print(data["data"]["id"])
PY
)"
  asc profiles create \
    --name "DictaFlow Mac App Store" \
    --profile-type MAC_APP_STORE \
    --bundle "$BUNDLE_ID_RESOURCE" \
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
