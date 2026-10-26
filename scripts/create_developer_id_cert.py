#!/usr/bin/env python3
"""Create and install a Developer ID Application certificate via App Store Connect API."""

from __future__ import annotations

import base64
import os
import subprocess
from pathlib import Path

from asc_api import api, make_token

KEY_DIR = Path.home() / ".appstoreconnect" / "dictaflow"
KEY_FILE = KEY_DIR / "DeveloperIDApplication.key"
CSR_FILE = KEY_DIR / "DeveloperIDApplication.csr"
CERT_FILE = KEY_DIR / "DeveloperIDApplication.cer"


def ensure_csr() -> str:
    """Generate the CSR + key once. Run after make_token(), which loads apple-release.env."""
    # Apple replaces the subject with the team name on the issued cert.
    subject = os.environ.get("CSR_SUBJECT", "/CN=DictaFlow Developer ID/C=US")
    KEY_DIR.mkdir(parents=True, exist_ok=True)
    if not KEY_FILE.exists():
        subprocess.run(
            [
                "openssl",
                "req",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                str(KEY_FILE),
                "-out",
                str(CSR_FILE),
                "-subj",
                subject,
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
    token = make_token()
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
        result = api("POST", "/v1/certificates", token, body)
    except SystemExit as e:
        if "(403)" in str(e):
            raise SystemExit(
                "create certificate failed (403): Account Holder must create this cert.\n"
                "Run ./scripts/setup-developer-id.sh and upload the CSR at developer.apple.com\n"
                f"Details: {e}"
            ) from e
        raise

    attrs = result["data"]["attributes"]
    install_cert(attrs["certificateContent"])
    print(f"Developer ID Application certificate created (expires {attrs['expirationDate']})")


if __name__ == "__main__":
    main()
