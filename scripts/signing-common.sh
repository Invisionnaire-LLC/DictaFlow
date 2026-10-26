#!/usr/bin/env bash
# Shared macOS code-signing helpers for DictaFlow install/release scripts.
# shellcheck shell=bash

IDENTIFIER="com.invisionnaire.dictaflow"

load_release_env() {
  local root="$1"
  local env_file="$root/scripts/apple-release.env"
  if [[ -f "$env_file" ]]; then
    # shellcheck disable=SC1090
    source "$env_file"
  fi
}

# App Store Connect API key (Users and Access → Integrations → Keys).
require_asc_env() {
  : "${ASC_KEY_ID:?Set ASC_KEY_ID in scripts/apple-release.env (see apple-release.env.example)}"
  : "${ASC_ISSUER_ID:?Set ASC_ISSUER_ID in scripts/apple-release.env}"
  : "${ASC_KEY_PATH:=$HOME/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID}.p8}"
}

export_notary_env() {
  if [[ -n "${APPLE_API_KEY_PATH:-}" && -n "${APPLE_API_ISSUER:-}" ]]; then
    export APPLE_API_KEY="${APPLE_API_KEY:-$ASC_KEY_ID}"
    export APPLE_API_KEY_PATH
    export APPLE_API_ISSUER
    return 0
  fi
  if [[ -n "${ASC_KEY_PATH:-}" && -n "${ASC_ISSUER_ID:-}" && -n "${ASC_KEY_ID:-}" ]]; then
    export APPLE_API_KEY="$ASC_KEY_ID"
    export APPLE_API_KEY_PATH="$ASC_KEY_PATH"
    export APPLE_API_ISSUER="$ASC_ISSUER_ID"
    return 0
  fi
  return 1
}

resolve_developer_id_identity() {
  if [[ -n "${DEVELOPER_ID_SIGNING_IDENTITY:-}" ]]; then
    if security find-identity -v -p codesigning 2>/dev/null | grep -Fq "$DEVELOPER_ID_SIGNING_IDENTITY"; then
      echo "$DEVELOPER_ID_SIGNING_IDENTITY"
      return 0
    fi
  fi

  security find-identity -v -p codesigning 2>/dev/null \
    | awk -F'"' '/Developer ID Application:/ { print $2; exit }'
}

resolve_signing_identity() {
  local dev_id
  dev_id="$(resolve_developer_id_identity)"
  if [[ -n "$dev_id" ]]; then
    echo "$dev_id"
    return 0
  fi

  security find-identity -v -p codesigning 2>/dev/null \
    | awk -F'"' '/Apple Development:/ { print $2; exit }'
}

is_developer_id_identity() {
  [[ "$1" == Developer\ ID\ Application:* ]]
}

write_tauri_signing_config() {
  local root="$1"
  local identity="$2"
  local out="$root/src-tauri/.tauri-signing.local.json"
  cat >"$out" <<EOF
{
  "bundle": {
    "macOS": {
      "signingIdentity": "$identity"
    }
  }
}
EOF
  echo "$out"
}

verify_app_signature() {
  local app="$1"
  codesign --verify --deep --strict "$app" 2>/dev/null
}

resign_app() {
  local app="$1"
  local identity="$2"
  codesign --force --deep --options runtime --timestamp \
    --identifier "$IDENTIFIER" \
    -s "$identity" \
    "$app"
  codesign --verify --deep --strict "$app"
}

print_signature_summary() {
  local app="$1"
  codesign -dv --verbose=4 "$app" 2>&1 | grep -E "Identifier|Authority|TeamIdentifier|Signature|Format|Notarization" || true
}

tcc_requirement_hint() {
  local app="$1"
  local info
  info="$(codesign -dv --verbose=4 "$app" 2>&1)"
  if grep -q "Authority=Developer ID Application" <<<"$info"; then
    echo "TCC: Accessibility grants should survive rebuilds (Developer ID anchor)."
  elif grep -q "Authority=Apple Development" <<<"$info"; then
    echo "TCC: Apple Development signing — grant may reset when the build changes. Prefer Developer ID."
  else
    echo "TCC: ad-hoc signing — Accessibility breaks on every rebuild. Install Developer ID cert."
  fi
}
