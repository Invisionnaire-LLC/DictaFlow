#!/usr/bin/env python3
"""Minimal App Store Connect API helper for DictaFlow release setup."""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

import jwt

API_BASE = "https://api.appstoreconnect.apple.com/v1"
BUNDLE_ID = "com.invisionnaire.dictaflow"
APP_NAME = "DictaFlow"
SKU = "dictaflow-mac-001"


def load_config() -> tuple[str, str, Path]:
    key_path = Path(
        os.environ.get(
            "ASC_KEY_PATH",
            Path.home() / ".appstoreconnect/private_keys/AuthKey_8T4628WQ3P.p8",
        )
    )
    key_id = os.environ.get("ASC_KEY_ID", "8T4628WQ3P")
    issuer_id = os.environ.get("ASC_ISSUER_ID", "e188a5e8-bef5-4a2c-a246-69805268fd07")
    if not key_path.is_file():
        raise SystemExit(f"ASC key not found: {key_path}")
    return key_id, issuer_id, key_path


def make_token(key_id: str, issuer_id: str, key_path: Path) -> str:
    private_key = key_path.read_text()
    now = int(time.time())
    headers = {"alg": "ES256", "kid": key_id, "typ": "JWT"}
    payload = {"iss": issuer_id, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"}
    return jwt.encode(payload, private_key, algorithm="ES256", headers=headers)


def api(method: str, path: str, token: str, body: dict | None = None) -> dict:
    url = f"{API_BASE}{path}"
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read().decode()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        detail = e.read().decode()
        raise SystemExit(f"{method} {path} failed ({e.code}): {detail}") from e


def find_bundle_id(token: str) -> dict | None:
    result = api("GET", f"/bundleIds?filter[identifier]={BUNDLE_ID}", token)
    data = result.get("data") or []
    return data[0] if data else None


def create_bundle_id(token: str) -> dict:
    body = {
        "data": {
            "type": "bundleIds",
            "attributes": {
                "identifier": BUNDLE_ID,
                "name": APP_NAME,
                "platform": "MAC_OS",
            },
        }
    }
    result = api("POST", "/bundleIds", token, body)
    return result["data"]


def find_app(token: str) -> dict | None:
    result = api("GET", f"/apps?filter[bundleId]={BUNDLE_ID}", token)
    data = result.get("data") or []
    return data[0] if data else None


def create_app(token: str, bundle_id_resource: str) -> dict:
    body = {
        "data": {
            "type": "apps",
            "attributes": {
                "name": APP_NAME,
                "sku": SKU,
                "primaryLocale": "en-US",
            },
            "relationships": {
                "bundleId": {
                    "data": {"type": "bundleIds", "id": bundle_id_resource}
                }
            },
        }
    }
    result = api("POST", "/apps", token, body)
    return result["data"]


def list_certificates(token: str, cert_type: str) -> list[dict]:
    result = api("GET", f"/certificates?filter[certificateType]={cert_type}&limit=200", token)
    return result.get("data") or []


def main() -> None:
    key_id, issuer_id, key_path = load_config()
    token = make_token(key_id, issuer_id, key_path)

    print("== App Store Connect ==")
    bundle = find_bundle_id(token)
    if bundle:
        print(f"bundleId: {BUNDLE_ID} (id={bundle['id']})")
        bundle_id_resource = bundle["id"]
    else:
        bundle = create_bundle_id(token)
        bundle_id_resource = bundle["id"]
        print(f"created bundleId: {BUNDLE_ID} (id={bundle_id_resource})")

    app = find_app(token)
    if app:
        attrs = app.get("attributes") or {}
        print(f"app: {attrs.get('name')} (id={app['id']})")
    else:
        try:
            app = create_app(token, bundle_id_resource)
            attrs = app.get("attributes") or {}
            print(f"created app: {attrs.get('name')} (id={app['id']})")
        except SystemExit as e:
            print(f"could not create app record via API: {e}")
            print(
                "Create it manually in App Store Connect → Apps → + → macOS,\n"
                f"  name={APP_NAME}, bundle id={BUNDLE_ID}, sku={SKU}\n"
                "  (This API key lacks Admin / Account Holder permissions for app CREATE.)"
            )

    print("\n== Certificates ==")
    for cert_type in (
        "DEVELOPER_ID_APPLICATION",
        "MAC_INSTALLER_DISTRIBUTION",
        "MAC_APP_DISTRIBUTION",
        "DEVELOPMENT",
    ):
        certs = list_certificates(token, cert_type)
        print(f"{cert_type}: {len(certs)}")
        for cert in certs[:3]:
            attrs = cert.get("attributes") or {}
            print(f"  - {attrs.get('serialNumber')} expires {attrs.get('expirationDate')}")


if __name__ == "__main__":
    main()
