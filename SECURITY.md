# Security Policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Use GitHub's private reporting instead:
**Security → Report a vulnerability** on this repository. Include the version (Settings → About),
your OS, and steps to reproduce.

We aim to acknowledge reports within a few days and will credit you in the fix unless you'd rather
stay anonymous.

## Supported versions

Only the latest release receives security fixes.

## Security model

DictaFlow is a local-first dictation app. The things it is trusted with, and how it handles them:

| Area | How it's handled |
|---|---|
| **Audio & transcripts** | Processed on-device by default (Whisper). Stored only under the app's data folder. Nothing is sent anywhere unless you pick a cloud backend. |
| **Cloud API keys** (Groq / OpenAI) | macOS Keychain; never logged and never sent to the webview (the UI only gets a masked hint like `gsk_…Ez77`). On Linux/Windows the key lives in `settings.json`. |
| **Network** | Cloud transcription goes over HTTPS straight from Rust (`reqwest`), never from the webview. The cleanup LLM is a local Ollama/LM Studio server; LM Studio URLs must be `http(s)://`. |
| **Webview** | Strict CSP (no remote scripts or connections) and minimal Tauri capabilities. Transcribed text is escaped or set as text, never injected as HTML. |
| **Other apps** | Accessibility is used only to paste at the cursor and read the focused field to confirm the paste. |
| **Telemetry** | None: no analytics, crash reporting, or tracking SDKs. |

See [docs/PRIVACY-POLICY.md](docs/PRIVACY-POLICY.md) for exactly what is stored on disk.
