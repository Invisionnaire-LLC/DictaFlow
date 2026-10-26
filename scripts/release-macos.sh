#!/usr/bin/env bash
# Build a signed DictaFlow.app from the repo root (Developer ID when available).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=signing-common.sh
source "$ROOT/scripts/signing-common.sh"
load_release_env "$ROOT"

cd "$ROOT/src-tauri"

IDENTITY="$(resolve_signing_identity || true)"
if [[ -n "$IDENTITY" ]]; then
  SIGNING_CONFIG="$(write_tauri_signing_config "$ROOT" "$IDENTITY")"
  echo "== DictaFlow macOS release build =="
  echo "Signing identity: $IDENTITY"
  if is_developer_id_identity "$IDENTITY" && export_notary_env; then
    echo "Notarization: enabled"
  fi
  cargo tauri build --bundles app,dmg --config "$SIGNING_CONFIG"
else
  echo "== DictaFlow macOS release build (no signing identity) =="
  cargo tauri build --bundles app,dmg
fi

APP="$ROOT/src-tauri/target/release/bundle/macos/DictaFlow.app"
echo
echo "== Verify signature =="
print_signature_summary "$APP"
tcc_requirement_hint "$APP"
echo
echo "Built: $APP"
echo
if ! is_developer_id_identity "${IDENTITY:-}"; then
  echo "For rebuild-proof Accessibility: ./scripts/setup-developer-id.sh"
fi
