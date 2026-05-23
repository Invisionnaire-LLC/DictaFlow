#!/usr/bin/env python3
"""Create and install a Developer ID Application certificate via App Store Connect API."""

from __future__ import annotations

import base64
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import jwt

API_BASE = "https://api.appstoreconnect.apple.com/v1"
KEY_DIR = Path.home() / ".appstoreconnect" / "dictaflow"
KEY_FILE = KEY_DIR / "DeveloperIDApplication.key"
CSR_FILE = KEY_DIR / "DeveloperIDApplication.csr"
CERT_FILE = KEY_DIR / "DeveloperIDApplication.cer"


def asc_token() -> str:
    key_path = Path(
        os.environ.get(
            "ASC_KEY_PATH",
            Path.home() / ".appstoreconnect/private_keys/AuthKey_8T4628WQ3P.p8",
        )
    )
    key_id = os.environ.get("ASC_KEY_ID", "8T4628WQ3P")
    issuer_id = os.environ.get("ASC_ISSUER_ID", "e188a5e8-bef5-4a2c-a246-69805268fd07")
    now = int(time.time())
    return jwt.encode(
        {"iss": issuer_id, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"},
        key_path.read_text(),
        algorithm="ES256",
        headers={"alg": "ES256", "kid": key_id, "typ": "JWT"},
    )


def api_post(path: str, token: str, body: dict) -> dict:
    req = urllib.request.Request(
        f"{API_BASE}{path}",
        data=json.dumps(body).encode(),
        method="POST",
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode())


def ensure_csr() -> str:
    KEY_DIR.mkdir(parents=True, exist_ok=True)
    if not KEY_FILE.exists():
        subprocess.run(
            [
                "openssl",
                "req",
                "-new",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-keyout",
                str(KEY_FILE),
                "-out",
                str(CSR_FILE),
                "-subj",
                "/CN=Developer ID Application: Invisionnaire/O=Invisionnaire/C=US",
            ],
            check=True,
        )
        KEY_FILE.chmod(0o600)
    return CSR_FILE.read_text()


def install_cert(content_b64: str) -> None:
    der = base64.b64decode(content_b64)
    CERT_FILE.write_bytes(der)
    subprocess.run(["security", "import", str(CERT_FILE), "-k", "login.keychain"], check=True)
    subprocess.run(["security", "import", str(KEY_FILE), "-k", "login.keychain"], check=True)
    print(f"installed certificate -> {CERT_FILE}")


def main() -> None:
    token = asc_token()
    csr = ensure_csr()
    body = {
        "data": {
            "type": "certificates",
            "attributes": {
                "certificateType": "DEVELOPER_ID_APPLICATION",
                "csrContent": csr,
            },
        }
    }
    try:
        result = api_post("/certificates", token, body)
    except urllib.error.HTTPError as e:
        detail = e.read().decode()
        raise SystemExit(f"create certificate failed ({e.code}): {detail}") from e

    attrs = result["data"]["attributes"]
    install_cert(attrs["certificateContent"])
    print(f"Developer ID Application certificate created (expires {attrs['expirationDate']})")


if __name__ == "__main__":
    main()
