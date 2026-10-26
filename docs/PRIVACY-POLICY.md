# DictaFlow Privacy Policy

**Last updated:** October 25, 2026

Invisionnaire ("we", "us") built DictaFlow as a local-first dictation app for macOS. This policy explains what data the app handles and how.

## Summary

- **Local transcription (default):** Your voice is processed on your Mac. Audio is not sent to our servers.
- **Optional cloud transcription:** If you enable Groq or OpenAI in Settings, audio is sent to that provider using your API key. We do not receive that audio.
- **No account required:** DictaFlow does not require login to Invisionnaire or any third-party service for local use.

## Data stored on your device

DictaFlow may store the following in `~/Library/Application Support/DictaFlow/`:

| Data | Purpose |
|------|---------|
| Settings (hotkey, HUD, language mode) | Your preferences |
| Whisper model (~1.5 GB) | Offline speech recognition |
| Vocabulary | Words you add + corrections learned from edits |
| Dictation history | Past transcriptions (local search), up to the last 500 |
| Recordings | Audio of each dictation (`recordings/`), deleted with its history entry |
| Cloud API key | Stored in macOS Keychain when you use Groq/OpenAI |

We do not sync this data to Invisionnaire servers.

## Microphone and accessibility

DictaFlow requests:

- **Microphone** — to record speech while you hold the dictation hotkey.
- **Accessibility** — to paste transcribed text at your cursor.

These permissions are used only for dictation functionality.

## Optional cloud backends

When you choose Groq or OpenAI transcription:

- Audio is encoded and sent directly to that provider's API.
- Your API key is stored locally (Keychain on macOS).
- Provider privacy policies apply: [Groq](https://groq.com/privacy-policy/), [OpenAI](https://openai.com/policies/privacy-policy/).

## Optional Ollama cleanup

If Ollama is installed and configured, dictated text may be sent to your local Ollama instance for cleanup. This stays on your machine unless you configure Ollama otherwise.

## Analytics and tracking

DictaFlow does not include analytics, advertising, or third-party tracking SDKs.

## Children's privacy

DictaFlow is not directed at children under 13.

## Changes

We may update this policy. The "Last updated" date will change when we do.

## Contact

Questions: [GitHub Issues](https://github.com/Invisionnaire-LLC/DictaFlow/issues) or arosario@invisionnaire.com
