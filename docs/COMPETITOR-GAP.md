# DictaFlow vs Wispr Flow — feature gap analysis

Last updated: May 2026

This doc compares **DictaFlow** to **[Wispr Flow](https://wisprflow.ai)** (the AI dictation app). It is the canonical place to track product gaps — not the phase table in [README.md](../README.md), which tracks *implementation milestones* for this repo.

---

## DictaFlow today

What we ship or have in code on `main`:

| Area | Status |
|---|---|
| Hold hotkey → speak → release → paste at cursor | Done |
| Local Whisper ASR (`large-v3-turbo`, Metal on macOS) | Done |
| Audio DSP (DC offset, HPF, silence trim) | Done |
| Rules cleanup (fillers, spoken punctuation, capitalization) | Done |
| Optional Ollama LLM cleanup (configurable model) | Done |
| Menu bar agent, tray menu, HUD, Settings window | Done |
| Settings: hotkey, mic, HUD position, Ollama model, launch at login | Done |
| Single-instance lock | Done |
| macOS `.app` bundle | Done |
| Windows / Linux | Scaffolded, untested |
| Release CI, notarization, DMG | Not started (README Phase 9) |

**Positioning:** local-first, MIT-licensed, no subscription. All audio and transcription stay on the machine; Ollama is optional and also local.

---

## Feature comparison

Legend: ✅ shipped · ⚠️ partial · ❌ missing · — not applicable

### Core dictation loop

| Feature | Wispr Flow | DictaFlow | Notes |
|---|---|---|---|
| Paste into any app via synthetic keystroke | ✅ | ✅ | DictaFlow uses ⌘V + clipboard restore |
| Configurable global hotkey | ✅ | ✅ | Settings UI |
| Hold-to-record mode | ✅ | ✅ | Default |
| Toggle-to-record mode | ✅ | ❌ | Tap once to start, again to stop |
| Real-time / streaming transcription while speaking | ✅ | ❌ | DictaFlow transcribes only after release |
| Floating status indicator | ✅ | ✅ | HUD pill in menu bar zone |

### Cleanup & intelligence

| Feature | Wispr Flow | DictaFlow | Notes |
|---|---|---|---|
| Remove filler words | ✅ | ✅ | Rules engine |
| Spoken punctuation (“period”, “new line”) | ✅ | ✅ | Rules engine |
| LLM polish pass | ✅ (cloud) | ✅ (local Ollama) | DictaFlow falls back to rules on timeout |
| Tone / rephrase presets (formal, casual, funny, …) | ✅ | ❌ | |
| Command Mode — edit *selected* text by voice | ✅ Pro | ❌ | e.g. “make this more formal”, “bullet list” |
| Summary templates | ✅ (~15) | ❌ | |
| Context-aware formatting per app | ✅ | ❌ | Wispr uses cloud context; we are app-agnostic |
| Translation to target language | ✅ | ❌ | Whisper is multilingual but we have no translate UX |

### Vocabulary & memory

| Feature | Wispr Flow | DictaFlow | Notes |
|---|---|---|---|
| User-editable custom dictionary | ✅ | ❌ | We seed Whisper `vocabulary.txt` with brand names only |
| Snippets / text expansions | ✅ | ❌ | |
| Searchable transcription history | ✅ | ❌ | We save debug WAVs; no history UI |
| Replay audio with word-by-word highlight | ✅ | ❌ | |
| Usage stats / word counts | ✅ | ❌ | App log only |

### Settings & platform

| Feature | Wispr Flow | DictaFlow | Notes |
|---|---|---|---|
| Microphone picker | ✅ | ✅ | Tray submenu + Settings |
| Whisper / model picker in UI | ✅ (cloud models) | ❌ | Hardcoded `large-v3-turbo` |
| Language picker / auto-detect UX | ✅ | ❌ | |
| Cross-device sync | ✅ | ❌ | Local `settings.json` only |
| Launch at login | ✅ | ✅ | macOS LaunchAgent |
| macOS menu bar / no Dock icon | ✅ | ✅ | `LSUIElement` |
| Windows | ✅ | ⚠️ | Cargo target exists; untested |
| Linux | — | ⚠️ | Untested; Wayland hotkey/paste risks |
| iOS / Android | ✅ | ❌ | |
| Auto-update | ✅ | ❌ | Manual rebuild / reinstall |
| Notarized installer / release CI | ✅ | ❌ | README Phase 9 |

### Privacy & business model

| Feature | Wispr Flow | DictaFlow | Notes |
|---|---|---|---|
| Fully offline transcription | Optional | ✅ | Always local Whisper |
| Cloud processing option | ✅ | ❌ | By design |
| Open source | ❌ | ✅ | MIT |
| Subscription | ✅ freemium + Pro | ❌ | Free; user supplies Ollama/GPU |

---

## Priority gaps (suggested)

Grouped by impact if the goal is “Wispr-like daily driver, but local.”

### High — core UX parity

1. **Toggle recording mode** — alongside hold-to-talk
2. **Transcription history** — searchable list of past dictations (text + optional audio)
3. **Custom dictionary UI** — user terms → Whisper vocab + rules exceptions
4. **Windows/Linux validation** — actually ship and test Phase 8

### Medium — power-user features

5. **Command Mode** — select text, hotkey, speak an edit instruction (local Ollama)
6. **Tone / rephrase presets** — prompt templates in cleanup layer
7. **Language selection** — explicit source language + optional translate-via-Ollama
8. **Whisper model picker** — tiny/base/turbo/large tradeoffs in Settings

### Lower — polish & distribution

9. **Streaming partial results** in HUD while recording
10. **Snippets** — spoken triggers → expanded text
11. **Usage stats** in Settings or About
12. **Release infra** — GitHub Actions, Developer ID notarization, DMG, auto-update

---

## What we should *not* copy blindly

- **Cloud-first processing** — conflicts with local-first positioning unless offered as an explicit opt-in.
- **Cross-device sync** — large scope; needs account infra or iCloud-style design.
- **Mobile apps** — separate products; out of scope until desktop is solid on all targets.

---

## Related docs

| Doc | Purpose |
|---|---|
| [README.md](../README.md) | Setup, architecture, phase roadmap |
| [AGENTS.md](../AGENTS.md) | Contributor/agent guide (macOS TCC, signing, build) |

When closing a gap, update the tables above and add a line to README Phase table if it maps to a numbered phase.
