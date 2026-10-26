#!/usr/bin/env bash
# Build DictaFlow for Mac App Store, sign, package as .pkg, upload to App Store Connect.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SIGNING="$ROOT/scripts/.signing"
# shellcheck source=signing-common.sh
source "$ROOT/scripts/signing-common.sh"
load_release_env "$ROOT"

: "${ASC_APP_ID:?Set ASC_APP_ID (App Store Connect app id) in scripts/apple-release.env}"
MARKETING_VERSION="1.0.0"
BUILD_NUMBER="${BUILD_NUMBER:-4}"

# Identities come from apple-release.env, or the first matching cert in the keychain.
APP_SIGN_ID="${APPSTORE_APP_SIGN_ID:-$(security find-identity -v -p codesigning 2>/dev/null \
  | awk -F'"' '/3rd Party Mac Developer Application:/ { print $2; exit }')}"
INSTALLER_SIGN_ID="${APPSTORE_INSTALLER_SIGN_ID:-$(security find-identity -v 2>/dev/null \
  | awk -F'"' '/3rd Party Mac Developer Installer:/ { print $2; exit }')}"

if [[ ! -f "$SIGNING/DictaFlow.provisionprofile" ]]; then
  echo "Missing provisioning profile. Run ./scripts/setup-appstore-signing.sh first."
  exit 1
fi

if [[ -z "$APP_SIGN_ID" || -z "$INSTALLER_SIGN_ID" ]]; then
  echo "Mac App Store signing identities not in keychain. Run ./scripts/setup-appstore-signing.sh"
  exit 1
fi

cd "$ROOT/src-tauri"

echo "== DictaFlow Mac App Store build =="
echo "Marketing version: $MARKETING_VERSION  Build: $BUILD_NUMBER"
echo

APPLE_SIGNING_IDENTITY="$APP_SIGN_ID" cargo tauri build --bundles app --config tauri.appstore.conf.json --ci

APP="$ROOT/src-tauri/target/release/bundle/macos/DictaFlow.app"
PKG="$ROOT/src-tauri/target/release/bundle/macos/DictaFlow.pkg"
ENT="$ROOT/src-tauri/Entitlements.appstore.plist"

if [[ ! -d "$APP" ]]; then
  echo "Build failed: $APP not found"
  exit 1
fi

echo
echo "== Embed provisioning profile + full icon + re-sign =="
cp "$SIGNING/DictaFlow.provisionprofile" "$APP/Contents/embedded.provisionprofile"
cp "$ROOT/src-tauri/icons/icon.icns" "$APP/Contents/Resources/DictaFlow.icns"

# Sign nested binaries first, then the bundle.
/usr/bin/codesign --force --options runtime --timestamp \
  --sign "$APP_SIGN_ID" \
  --entitlements "$ENT" \
  "$APP/Contents/MacOS/dictaflow"

/usr/bin/codesign --force --deep --options runtime --timestamp \
  --sign "$APP_SIGN_ID" \
  --entitlements "$ENT" \
  "$APP"

/usr/bin/codesign --verify --deep --strict "$APP"
echo "App signature OK"

echo
echo "== Create signed .pkg =="
rm -f "$PKG"
/usr/bin/productbuild \
  --product "$ROOT/scripts/macos-appstore-product.plist" \
  --component "$APP" /Applications \
  --sign "$INSTALLER_SIGN_ID" \
  "$PKG"

echo "Built: $PKG"
ls -lh "$PKG"

echo
echo "== Upload to App Store Connect =="
if ! command -v asc >/dev/null 2>&1; then
  echo "asc CLI not installed. Upload manually with Transporter:"
  echo "  open -a Transporter \"$PKG\""
  exit 0
fi

asc builds upload \
  --app "$ASC_APP_ID" \
  --pkg "$PKG" \
  --version "$MARKETING_VERSION" \
  --build-number "$BUILD_NUMBER" \
  --wait

BUILD_ID="$(asc builds list --app "$ASC_APP_ID" --output json | python3 -c "
import json, sys
data = json.load(sys.stdin).get('data') or []
for item in sorted(data, key=lambda x: x.get('attributes', {}).get('uploadedDate', ''), reverse=True):
    attrs = item.get('attributes') or {}
    if str(attrs.get('version')) == '$BUILD_NUMBER' and attrs.get('processingState') == 'VALID':
        print(item['id'])
        break
")"

if [[ -n "${BUILD_ID:-}" && -n "${ASC_VERSION_ID:-}" ]]; then
  asc versions attach-build \
    --version-id "$ASC_VERSION_ID" \
    --build "$BUILD_ID"
  echo "Attached build $BUILD_NUMBER to App Store version $MARKETING_VERSION"
fi

echo
echo "Done."
echo "  1. App Store Connect → DictaFlow → 1.0 → select build $BUILD_NUMBER"
echo "  2. The store icon appears after this build finishes processing."
echo "  3. Still manual: screenshots, App Privacy, age rating, pricing."
