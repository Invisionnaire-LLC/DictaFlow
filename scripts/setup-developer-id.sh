#!/usr/bin/env bash
# Prepare DictaFlow for Developer ID signing (stable Accessibility / TCC grants).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=signing-common.sh
source "$ROOT/scripts/signing-common.sh"

KEY_DIR="$HOME/.appstoreconnect/dictaflow"
CSR="$KEY_DIR/DeveloperIDApplication.csr"
KEY="$KEY_DIR/DeveloperIDApplication.key"
ENV_FILE="$ROOT/scripts/apple-release.env"
VENV="$ROOT/scripts/.venv"

echo "== DictaFlow Developer ID setup =="
echo

existing="$(resolve_developer_id_identity || true)"
if [[ -n "$existing" ]]; then
  echo "Developer ID certificate already installed:"
  echo "  $existing"
  echo
  if [[ -f "$ENV_FILE" ]] && grep -q '^DEVELOPER_ID_SIGNING_IDENTITY=' "$ENV_FILE" 2>/dev/null; then
    :
  else
    echo "Add to scripts/apple-release.env:"
    echo "DEVELOPER_ID_SIGNING_IDENTITY=\"$existing\""
  fi
  echo
  echo "Run ./scripts/install-macos.sh to build, notarize, and install with this identity."
  exit 0
fi

echo "No Developer ID Application certificate found in your login keychain."
echo
echo "This certificate requires your Apple Developer **Account Holder** (one-time setup)."
echo "After it is installed, ./scripts/install-macos.sh will sign with Developer ID"
echo "and Accessibility grants will survive rebuilds."
echo

mkdir -p "$KEY_DIR"
if [[ ! -f "$CSR" || ! -f "$KEY" ]]; then
  echo "Generating CSR + private key..."
  openssl req -new -newkey rsa:2048 -nodes \
    -keyout "$KEY" -out "$CSR" \
    -subj "/CN=Developer ID Application: Invisionnaire/O=Invisionnaire/C=US"
  chmod 600 "$KEY"
fi

echo "CSR ready for upload:"
echo "  $CSR"
echo
echo "Account Holder steps (developer.apple.com):"
echo "  1. Certificates, Identifiers & Profiles → Certificates → +"
echo "  2. Developer ID Application → Continue"
echo "  3. Upload the CSR above → Download the .cer file"
echo "  4. Run: ./scripts/import-developer-id-cert.sh ~/Downloads/developerID_application.cer"
echo

if [[ -d "$VENV" ]] || python3 -m venv "$VENV" 2>/dev/null; then
  "$VENV/bin/pip" install -q 'PyJWT[crypto]' 2>/dev/null || true
  if "$VENV/bin/python" "$ROOT/scripts/create_developer_id_cert.py" 2>/dev/null; then
    existing="$(resolve_developer_id_identity || true)"
    if [[ -n "$existing" ]]; then
      echo
      echo "Certificate created via API. Add to scripts/apple-release.env:"
      echo "DEVELOPER_ID_SIGNING_IDENTITY=\"$existing\""
      exit 0
    fi
  fi
fi

echo "API auto-create skipped or failed (only Account Holder can create this cert)."
echo "Use the manual CSR steps above, then re-run this script to verify."
