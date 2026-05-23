#!/usr/bin/env bash
# Build a signed DictaFlow.app from the repo root.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="$ROOT/scripts/apple-release.env"

if [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE"
fi

cd "$ROOT/src-tauri"

echo "== DictaFlow macOS release build =="
echo "Signing identity from tauri.conf.json (see scripts/apple-release.env for notary vars)"
echo

cargo tauri build --bundles app

APP="$ROOT/src-tauri/target/release/bundle/macos/DictaFlow.app"
echo
echo "== Verify signature =="
codesign --verify --deep --strict "$APP"
codesign -dv "$APP" 2>&1 | grep -E "Identifier|Authority|TeamIdentifier|Signature" || true

echo
echo "Built: $APP"
echo
echo "Next steps (manual):"
echo "  1. Account Holder: create Developer ID Application cert → update tauri.conf.json signingIdentity"
echo "  2. App Store Connect: Apps → + → macOS → DictaFlow (bundle com.invisionnaire.dictaflow)"
echo "  3. Re-run this script — Tauri will notarize when Developer ID + APPLE_API_* env are set"
echo "  4. After first signed build: grant Accessibility once in Settings → Permissions"
