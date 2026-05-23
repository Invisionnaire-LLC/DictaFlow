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

DESCRIPTION = """DictaFlow is local-first dictation for Mac. Hold a hotkey, speak, release — cleaned text appears at your cursor. No cloud account required.

FEATURES
• Dictate anywhere you type with a global hotkey (hold or tap to start/stop)
• Transcribes locally with Whisper — your voice stays on your Mac
• Cleans up punctuation, fillers, and spacing with on-device rules
• Optional Ollama cleanup for smarter phrasing (runs locally)
• Menu bar app with a draggable HUD pill and live transcript preview
• Choose your hotkey, HUD style, and Ollama model in Settings

PERMISSIONS
DictaFlow asks for Microphone (record), Accessibility (paste at your cursor), and Input Monitoring (global hotkey). Processing stays on your machine.

Built by Invisionnaire."""

PROMO = "Speak naturally. DictaFlow transcribes and pastes locally — no cloud, no subscription."
KEYWORDS = "dictation,transcription,whisper,voice typing,speech to text,local,privacy,productivity,ollama"
WHATS_NEW = "Initial Mac release: local Whisper transcription, Ollama cleanup, global hotkey dictation, and menu bar HUD."
SUBTITLE = "Local dictation at your cursor"
SUPPORT_URL = "https://github.com/Invisionnaire-LLC/DictaFlow/issues"
MARKETING_URL = "https://github.com/Invisionnaire-LLC/DictaFlow"


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
    print("updated version localization (description, keywords, URLs, what's new)")

    api(
        "PATCH",
        f"/v1/appInfoLocalizations/{INFO_LOC_ID}",
        {
            "data": {
                "type": "appInfoLocalizations",
                "id": INFO_LOC_ID,
                "attributes": {"subtitle": SUBTITLE},
            }
        },
    )
    print(f"updated subtitle: {SUBTITLE}")

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
    print("set primary category: Productivity / Utilities")

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

    review = api("GET", f"/v1/appStoreVersions/{VERSION_ID}/appStoreReviewDetail")
    review_body = {
        "data": {
            "type": "appStoreReviewDetails",
            "attributes": {
                "contactFirstName": "Adrian",
                "contactLastName": "Rosario",
                "contactEmail": "arosario@invisionnaire.com",
                "contactPhone": "+1 312 555 0100",
                "notes": (
                    "DictaFlow is a menu bar dictation utility. "
                    "Grant Microphone, Accessibility, and Input Monitoring when prompted. "
                    "Default hotkey is F9; hold to dictate or tap twice to start/stop. "
                    "All transcription runs locally with Whisper; no login required."
                ),
            },
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
        api("PATCH", f"/v1/appStoreReviewDetails/{review_id}", review_body)
        print("updated app review contact details")
    else:
        api("POST", "/v1/appStoreReviewDetails", review_body)
        print("created app review contact details")

    print("\nDone. Still required manually in App Store Connect:")
    print("  • Upload a Mac build (.pkg) and attach it to version 1.0")
    print("  • Screenshots (at least one Mac screenshot)")
    print("  • App Privacy questionnaire")
    print("  • Age rating questionnaire")
    print("  • Pricing (free/paid)")


if __name__ == "__main__":
    main()
