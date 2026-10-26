#!/usr/bin/env bash
# Build DictaFlow and install to /Applications with the best available signature.
# Developer ID Application → stable Accessibility (TCC) across rebuilds.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/src-tauri/target/release/bundle/macos/DictaFlow.app"
DST="/Applications/DictaFlow.app"
LSREG="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"

# shellcheck source=signing-common.sh
source "$ROOT/scripts/signing-common.sh"
load_release_env "$ROOT"

IDENTITY="$(resolve_signing_identity || true)"
USE_DEV_ID=false
if [[ -n "$IDENTITY" ]] && is_developer_id_identity "$IDENTITY"; then
  USE_DEV_ID=true
fi

echo "== Build =="
cd "$ROOT/src-tauri"

if [[ -n "$IDENTITY" ]]; then
  SIGNING_CONFIG="$(write_tauri_signing_config "$ROOT" "$IDENTITY")"
  echo "Signing identity: $IDENTITY"
  if $USE_DEV_ID && export_notary_env; then
    echo "Notarization: enabled (APPLE_API_* from apple-release.env)"
  elif $USE_DEV_ID; then
    echo "Notarization: skipped (set APPLE_API_* in scripts/apple-release.env)"
  fi
  cargo tauri build --bundles app --config "$SIGNING_CONFIG"
else
  echo "WARNING: No signing identity found. Build may be ad-hoc."
  echo "Run ./scripts/setup-developer-id.sh for stable Accessibility."
  cargo tauri build --bundles app
fi

if [[ ! -d "$SRC" ]]; then
  echo "Build failed: $SRC not found"
  exit 1
fi

echo
echo "== Verify build signature =="
print_signature_summary "$SRC"
tcc_requirement_hint "$SRC"
echo

echo "== Install to /Applications =="
if [[ -d "$DST" ]]; then
  backup="$DST.backup-$(date +%Y%m%d-%H%M%S)"
  echo "Backing up current app to $backup"
  cp -R "$DST" "$backup"
fi

osascript -e 'tell application "DictaFlow" to quit' 2>/dev/null || true
sleep 1
pkill -f "$DST/Contents/MacOS/dictaflow" 2>/dev/null || true
sleep 1

rm -rf "$DST"
cp -R "$SRC" "$DST"

# Re-sign installed copy only if verification fails (same identity, never ad-hoc).
if [[ -n "$IDENTITY" ]] && ! verify_app_signature "$DST"; then
  echo "Re-signing installed app with $IDENTITY"
  resign_app "$DST" "$IDENTITY"
elif [[ -z "$IDENTITY" ]]; then
  echo "Ad-hoc signing installed app (Accessibility will reset on next rebuild)"
  codesign --force --deep -s - --identifier "$IDENTIFIER" "$DST"
  codesign --verify --deep --strict "$DST"
fi

"$LSREG" -u "$SRC" 2>/dev/null || true
"$LSREG" -f "$DST" 2>/dev/null || true

echo
echo "== Installed signature =="
print_signature_summary "$DST"
tcc_requirement_hint "$DST"

open "$DST"
echo
echo "Installed: $DST"
if $USE_DEV_ID; then
  echo "Developer ID build — re-grant Accessibility only if this is the first Developer ID install."
else
  echo "If paste breaks, re-grant Accessibility in Settings → Permissions."
  echo "For rebuild-proof grants: ./scripts/setup-developer-id.sh"
fi

# Keep the three most recent backups; remove older ones.
if ls /Applications/DictaFlow.app.backup-* >/dev/null 2>&1; then
  ls -dt /Applications/DictaFlow.app.backup-* | tail -n +4 | while read -r old; do
    echo "Removing old backup: $old"
    rm -rf "$old"
  done
fi
