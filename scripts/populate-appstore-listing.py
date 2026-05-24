#!/usr/bin/env python3
"""Populate DictaFlow Mac App Store Connect listing metadata."""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import jwt

APP_ID = "6772598366"
VERSION_ID = "462231f4-48c4-4ea5-bc57-7844ff035a8b"
INFO_ID = "c3467077-2484-49ff-9508-2c7b2a806337"
VERSION_LOC_ID = "c01e7439-a4f1-4e57-b7ef-029be2e13886"
INFO_LOC_ID = "621554f5-1337-4989-b7f4-88b7e663f85b"

DESCRIPTION = """DictaFlow is local-first dictation for Mac. Hold a hotkey, speak, release. Cleaned text appears at your cursor.

FEATURES
• Dictate anywhere you type with a global hotkey (default Option+Space; hold or tap to start/stop)
• Transcribes locally with Whisper — your voice stays on your Mac by default
• Optional cloud transcription (Groq or OpenAI) when you want speed over offline privacy
• Language modes: auto-detect, force English or Spanish, or translate to English
• Vocabulary: words to recognize plus auto-learned corrections
• Cleans up punctuation, fillers, and spacing with on-device rules
• Optional Ollama cleanup for smarter phrasing (runs locally)
• Menu bar app with Classic, Notch, or Cursor HUD styles and live transcript preview
• Learns corrections when you edit pasted text — no review step required
• Multi-monitor HUD follows the display you are working on

PERMISSIONS
DictaFlow asks for Microphone (record), Accessibility (paste at your cursor), and Input Monitoring (global hotkey). Local transcription and cleanup stay on your machine unless you enable a cloud backend in Settings.

Built by Invisionnaire."""

PROMO = "Speak naturally. DictaFlow transcribes locally, cleans up, and pastes at your cursor."
KEYWORDS = "dictation,transcription,whisper,voice typing,speech to text,local,privacy,productivity,ollama,mac"
WHATS_NEW = """• App Store build with tightened sandbox entitlements
• Default hotkey Option+Space (configurable in Settings)
• Cloud API keys stored in macOS Keychain
• Cursor-following HUD and optional cloud transcription
• Improved paste reliability in Electron apps like Telegram"""
SUBTITLE = "Local dictation at your cursor"
SUPPORT_URL = "https://github.com/Invisionnaire-LLC/DictaFlow/issues"
MARKETING_URL = "https://github.com/Invisionnaire-LLC/DictaFlow"
PRIVACY_POLICY_URL = "https://github.com/Invisionnaire-LLC/DictaFlow/blob/main/docs/PRIVACY-POLICY.md"


def load_env_file() -> None:
    env_path = Path(__file__).resolve().parent / "apple-release.env"
    if not env_path.is_file():
        return
    for line in env_path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        value = value.strip().strip('"').strip("'")
        os.environ.setdefault(key, value)


def contact_phone() -> str:
    phone = os.environ.get("ASC_CONTACT_PHONE", "").strip()
    if not phone:
        print(
            "WARNING: ASC_CONTACT_PHONE not set (add to scripts/apple-release.env). "
            "Skipping contactPhone update in review details.",
            file=sys.stderr,
        )
    return phone


def token() -> str:
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


def api(method: str, path: str, body: dict | None = None) -> dict:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        f"https://api.appstoreconnect.apple.com{path}",
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {token()}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read().decode()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        raise SystemExit(f"{method} {path} failed ({e.code}): {e.read().decode()}") from e


def main() -> None:
    load_env_file()

    api(
        "PATCH",
        f"/v1/appStoreVersionLocalizations/{VERSION_LOC_ID}",
        {
            "data": {
                "type": "appStoreVersionLocalizations",
                "id": VERSION_LOC_ID,
                "attributes": {
                    "description": DESCRIPTION,
                    "keywords": KEYWORDS,
                    "promotionalText": PROMO,
                    "supportUrl": SUPPORT_URL,
                    "marketingUrl": MARKETING_URL,
                },
            }
        },
    )
    print("updated version localization (description, keywords, URLs, promo)")

    api(
        "PATCH",
        f"/v1/appInfoLocalizations/{INFO_LOC_ID}",
        {
            "data": {
                "type": "appInfoLocalizations",
                "id": INFO_LOC_ID,
                "attributes": {
                    "subtitle": SUBTITLE,
                    "privacyPolicyUrl": PRIVACY_POLICY_URL,
                },
            }
        },
    )
    print(f"updated subtitle + privacy policy URL")

    api(
        "PATCH",
        f"/v1/appInfos/{INFO_ID}",
        {
            "data": {
                "type": "appInfos",
                "id": INFO_ID,
                "relationships": {
                    "primaryCategory": {
                        "data": {"type": "appCategories", "id": "PRODUCTIVITY"}
                    },
                },
            }
        },
    )
    print("set primary category: Productivity")

    api(
        "PATCH",
        f"/v1/apps/{APP_ID}",
        {
            "data": {
                "type": "apps",
                "id": APP_ID,
                "attributes": {
                    "contentRightsDeclaration": "DOES_NOT_USE_THIRD_PARTY_CONTENT"
                },
            }
        },
    )
    print("set content rights declaration")

    review_attrs = {
        "contactFirstName": "Adrian",
        "contactLastName": "Rosario",
        "contactEmail": "arosario@invisionnaire.com",
                "notes": (
                    "DictaFlow is a menu bar dictation utility. "
                    "Grant Microphone, Accessibility, and Input Monitoring when prompted. "
                    "Default hotkey is Option+Space; hold to dictate or tap twice to start/stop. "
                    "First launch downloads the Whisper model (~1.5 GB, several minutes). "
                    "Progress appears in the menu bar HUD and Settings until complete. "
                    "For faster review testing, use Settings → Transcription → Groq with an API key. "
                    "Local Whisper transcription is the default; cloud backends are optional in Settings. "
                    "Cloud API keys are stored in the macOS Keychain."
                ),
    }
    phone = contact_phone()
    if phone:
        review_attrs["contactPhone"] = phone

    review = api("GET", f"/v1/appStoreVersions/{VERSION_ID}/appStoreReviewDetail")
    review_body = {
        "data": {
            "type": "appStoreReviewDetails",
            "attributes": review_attrs,
            "relationships": {
                "appStoreVersion": {
                    "data": {"type": "appStoreVersions", "id": VERSION_ID}
                }
            },
        }
    }

    if review.get("data"):
        review_id = review["data"]["id"]
        review_body["data"]["id"] = review_id
        del review_body["data"]["relationships"]
        api("PATCH", f"/v1/appStoreReviewDetails/{review_id}", review_body)
        print("updated app review contact details")
    else:
        api("POST", "/v1/appStoreReviewDetails", review_body)
        print("created app review contact details")

    print("\nDone. Still required manually in App Store Connect:")
    print("  • Upload build 4 (.pkg) and attach it to version 1.0.0")
    print("  • Screenshots (at least one Mac screenshot)")
    print("  • App Privacy questionnaire")
    print("  • Age rating questionnaire")
    print("  • Pricing (free/paid)")
    if not phone:
        print("  • Set ASC_CONTACT_PHONE in scripts/apple-release.env and re-run for review phone")


if __name__ == "__main__":
    main()
