#!/usr/bin/env python3
"""Populate DictaFlow Mac App Store Connect listing metadata."""

from __future__ import annotations

import os
import sys

from asc_api import api as asc_request, make_token, require_env

_TOKEN: str | None = None


DESCRIPTION = """DictaFlow is local-first dictation for Mac. Hold a hotkey, speak, release. Cleaned text appears at your cursor.

FEATURES
• Dictate anywhere you type with a global hotkey (default Option+Space; hold or tap to start/stop)
• Transcribes locally with Whisper — your voice stays on your Mac by default
• Optional cloud transcription (Groq or OpenAI) when you want speed over offline privacy
• Language modes: auto-detect, force English or Spanish, or translate to English
• Vocabulary: words to recognize plus your own corrections
• Cleans up punctuation, fillers, and spacing with on-device rules
• Optional Ollama cleanup for smarter phrasing (runs locally)
• Menu bar app with Classic, Notch, or Cursor HUD styles and live transcript preview
• Multi-monitor HUD follows the display you are working on

PERMISSIONS
DictaFlow asks for Microphone (record) and Accessibility (paste at your cursor). Local transcription and cleanup stay on your machine unless you enable a cloud backend in Settings.

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


def api(method: str, path: str, body: dict | None = None) -> dict:
    global _TOKEN
    if _TOKEN is None:
        _TOKEN = make_token()
    return asc_request(method, path, _TOKEN, body)


def en_us(items: list[dict]) -> str:
    for item in items:
        if (item.get("attributes") or {}).get("locale") == "en-US":
            return item["id"]
    raise SystemExit("no en-US localization found")


def contact_phone() -> str:
    phone = os.environ.get("ASC_CONTACT_PHONE", "").strip()
    if not phone:
        print(
            "WARNING: ASC_CONTACT_PHONE not set (add to scripts/apple-release.env). "
            "Skipping contactPhone update in review details.",
            file=sys.stderr,
        )
    return phone


def main() -> None:
    make_token()  # loads apple-release.env and validates the API key up front
    app_id = require_env("ASC_APP_ID")
    version_id = require_env("ASC_VERSION_ID")
    version_loc_id = en_us(
        api("GET", f"/v1/appStoreVersions/{version_id}/appStoreVersionLocalizations").get("data") or []
    )
    infos = api("GET", f"/v1/apps/{app_id}/appInfos").get("data") or []
    if not infos:
        raise SystemExit(f"no appInfos for app {app_id}")
    info_id = infos[0]["id"]
    info_loc_id = en_us(api("GET", f"/v1/appInfos/{info_id}/appInfoLocalizations").get("data") or [])

    api(
        "PATCH",
        f"/v1/appStoreVersionLocalizations/{version_loc_id}",
        {
            "data": {
                "type": "appStoreVersionLocalizations",
                "id": version_loc_id,
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
        f"/v1/appInfoLocalizations/{info_loc_id}",
        {
            "data": {
                "type": "appInfoLocalizations",
                "id": info_loc_id,
                "attributes": {
                    "subtitle": SUBTITLE,
                    "privacyPolicyUrl": PRIVACY_POLICY_URL,
                },
            }
        },
    )
    print("updated subtitle + privacy policy URL")

    api(
        "PATCH",
        f"/v1/appInfos/{info_id}",
        {
            "data": {
                "type": "appInfos",
                "id": info_id,
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
        f"/v1/apps/{app_id}",
        {
            "data": {
                "type": "apps",
                "id": app_id,
                "attributes": {
                    "contentRightsDeclaration": "DOES_NOT_USE_THIRD_PARTY_CONTENT"
                },
            }
        },
    )
    print("set content rights declaration")

    review_attrs = {
        "contactFirstName": require_env("ASC_CONTACT_FIRST_NAME"),
        "contactLastName": require_env("ASC_CONTACT_LAST_NAME"),
        "contactEmail": require_env("ASC_CONTACT_EMAIL"),
                "notes": (
                    "DictaFlow is a menu bar dictation utility. "
                    "Grant Microphone and Accessibility when prompted. "
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

    review = api("GET", f"/v1/appStoreVersions/{version_id}/appStoreReviewDetail")
    review_body = {
        "data": {
            "type": "appStoreReviewDetails",
            "attributes": review_attrs,
            "relationships": {
                "appStoreVersion": {
                    "data": {"type": "appStoreVersions", "id": version_id}
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
