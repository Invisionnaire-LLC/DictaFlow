# DictaFlow

DictaFlow is a dictation app that lives in your menu bar. Hold **Option+Space**
(or any hotkey you pick), say what you want to write, and release. It
transcribes what you said with Whisper, fixes the punctuation, drops the "um"s
and "uh"s, and pastes the text wherever your cursor is: Notes, Slack, a
browser, your editor, a terminal.

Everything runs on your Mac by default. Your voice never leaves it unless you
choose a cloud backend in Settings.

## What's in here

| Part | What it is |
|------|------------|
| **The app** (`src-tauri`) | A [Tauri](https://tauri.app) app written in Rust: the hotkey, the microphone, Whisper, the cleanup and the paste. |
| **The windows** (`dist`) | Plain HTML and JavaScript, no framework: the floating HUD, Settings, History and About. |
| **Release tooling** (`scripts`) | Signing, notarization and Mac App Store scripts for macOS builds. |

## What it does

### Talk anywhere you can type

- **Hold to talk.** Hold the hotkey while you speak and let go when you're
  done.
- **Or tap to toggle.** A quick tap starts recording and the next tap stops
  it, for longer thoughts.
- **It pastes where you were.** DictaFlow remembers which app was in front
  when you started and pastes there, even if you clicked away.
- **Auto-send** (optional) presses Enter right after pasting, for chat apps.

### A small HUD that shows what's happening

A pill floats on screen while you dictate: a blue mic while it listens, an
amber spinner while it thinks, red if something went wrong. Pick one of three
styles under **Settings → Appearance**:

- **Notch**: a slim bar at the top of the screen.
- **Classic**: a pill near the bottom, with a live waveform. Drag it anywhere.
- **Cursor**: follows your mouse, across monitors.

With **live transcript** on, the HUD shows the words as you say them.

### Cleanup that stays out of the way

Every dictation goes through two passes:

1. **Rules**, which are instant and always on: they remove fillers ("um",
   "uh"), collapse stutters ("the the"), capitalize sentences, add the final
   period, and turn spoken commands into punctuation ("new line", "new
   paragraph", "question mark", and "period" or "comma" when you clearly mean
   them).
2. **A local LLM** (optional): [Ollama](https://ollama.com) with `gemma3:1b`
   by default, or [LM Studio](https://lmstudio.ai), polishes the phrasing.
   It gets 800 ms. If it's not running, too slow, or returns nothing, you get
   the rules version. You always get text back.

### Your words, spelled your way

Under **Settings → Vocabulary**:

- **Words to recognize**: names and terms Whisper should expect (`McKenzie,
  Anthropic, CUDA`). They bias transcription before you speak.
- **Corrections**: "heard → wanted" pairs (`use cloud` → `use Claude`),
  applied to every dictation. Their "wanted" side also goes into Whisper's
  hint list.

### Languages

Auto-detect and keep whatever language you speak, force English or Spanish,
or **translate** anything you say into English. Switch from the menu bar or
Settings.

### History

**History…** in the menu lists your last 500 dictations: search them, fix the
text, copy the cleaned or raw version, or delete them.

## How it works

```
Hotkey down ──▶ microphone (cpal) ──▶ every 350 ms: last 2 s ──▶ Whisper ──▶ HUD live text
     │
Hotkey up
     │
     ▼
16 kHz mono, DC removed, 80 Hz high-pass, silence trimmed
     │
     ▼
Whisper large-v3-turbo on your GPU (Metal)   or   Groq / OpenAI, if you chose them
     │
     ▼
Rules ──▶ your corrections ──▶ Ollama / LM Studio (800 ms, optional)
     │
     ▼
Paste at the cursor ──▶ saved to History
```

- **Transcription** runs [whisper.cpp](https://github.com/ggerganov/whisper.cpp)
  through `whisper-rs`, with the `large-v3-turbo` model (about 1.5 GB). It
  downloads once on first launch; the HUD shows the progress.
- **Live preview** only ever transcribes the last couple of seconds, so it
  costs the same at second 50 as at second 5, and it stops the moment you
  release the key so the final transcription never waits behind it.
- **Pasting** first tries to insert the text directly through Accessibility,
  then falls back to the clipboard and ⌘V. It checks whether the text
  actually landed, and only then puts your old clipboard back. Terminals
  always get plain ⌘V, since they can't be checked and anything else would
  paste twice.
- **One dictation** can be up to 60 seconds long; at 60 seconds it finishes
  on its own and pastes what you said.
- **Nothing phones home.** No account, no analytics, no tracking. The only
  network calls are the model download and the cloud backend, if you turn it
  on. See the [privacy policy](docs/PRIVACY-POLICY.md).

### Cloud transcription (optional)

Prefer speed over offline? Pick **Groq** or **OpenAI** under **Settings →
Models** and paste your API key. On a Mac the key goes into the
Keychain, and the app's windows only ever see a masked hint (`gsk_…Ez77`).
Audio is then sent to that provider on each dictation, and the local Whisper
model is unloaded to free memory.

## Get started

You need:

- A Mac with Apple silicon. Intel Macs, Windows and Linux build too, but get
  much less testing (see below).
- [Rust](https://rustup.rs) (stable), [CMake](https://cmake.org) and the Xcode
  Command Line Tools (`xcode-select --install`).
- Optional: [Ollama](https://ollama.com) or [LM Studio](https://lmstudio.ai)
  for the LLM cleanup pass.

**1. Build and run it:**

```bash
git clone https://github.com/Invisionnaire-LLC/DictaFlow.git
cd DictaFlow/src-tauri
cargo run
```

The first launch downloads the Whisper model, which takes a few minutes.

**2. Grant two permissions** when macOS asks, or from **Settings →
Permissions**:

| Permission | Why |
|------------|-----|
| **Microphone** | To hear you. |
| **Accessibility** | To paste at your cursor. Without it, the text is copied instead and the HUD says so. |

**3. (Optional) Turn on the LLM cleanup:**

```bash
ollama pull gemma3:1b
```

DictaFlow finds Ollama on its own. For LM Studio, pick it under **Settings →
Models** and set its server address.

<details>
<summary><b>Install it as a real app</b> in /Applications</summary>

```bash
./scripts/install-macos.sh
```

This builds `DictaFlow.app`, signs it with the best certificate in your
keychain (Developer ID, then Apple Development, then ad-hoc), and installs it.

> [!NOTE]
> macOS ties the Accessibility permission to the app's signature. With
> ad-hoc or Apple Development signing, every rebuild can silently revoke it:
> the HUD shows the text was copied instead of pasted. Re-grant it under
> **Settings → Permissions**. A Developer ID certificate makes the grant
> survive rebuilds. [AGENTS.md](AGENTS.md) explains why and how to diagnose
> it.

</details>

<details>
<summary><b>Windows and Linux</b></summary>

Both build from the same code, with Whisper on CUDA (Windows) or Vulkan
(Linux). They're less tested than macOS:

- **Windows**: see [docs/BUILD-WINDOWS.md](docs/BUILD-WINDOWS.md). The default
  build needs the NVIDIA CUDA Toolkit; the doc shows how to switch to a CPU
  build.
- **Linux**: see [LINUX.md](LINUX.md), including Wayland, where global
  hotkeys depend on the compositor and you can bind a toggle command to a key
  instead.

</details>

### Where your data lives

Everything is in one folder: settings, history, your vocabulary and
corrections, the Whisper model, and the recordings behind each History entry
(deleted along with it).

| | Data | Log |
|---|---|---|
| macOS | `~/Library/Application Support/DictaFlow` | `~/Library/Logs/DictaFlow/app.log` |
| Windows | `%LOCALAPPDATA%\DictaFlow` | `%LOCALAPPDATA%\DictaFlow\logs\app.log` |
| Linux | `~/.local/share/DictaFlow` | `~/.local/share/DictaFlow/logs/app.log` |

## Development

In `src-tauri`:

| Command | What it does |
|---------|--------------|
| `cargo run` | Runs DictaFlow from the repo. |
| `cargo test` | The unit tests: audio DSP, cleanup rules, dictionary, paste checks, history, settings. |
| `cargo clippy --all-targets -- -D warnings` | The lint CI runs, along with `cargo fmt --check`. |
| `cargo tauri build --bundles app` | Builds `DictaFlow.app` (needs `cargo install tauri-cli --version "^2.0"`). |
| `../scripts/release-macos.sh` | A signed (and, with Developer ID, notarized) release build. |

Release scripts read their Apple credentials from `scripts/apple-release.env`,
which is gitignored: copy
[`scripts/apple-release.env.example`](scripts/apple-release.env.example) and
fill it in. Mac App Store builds use `setup-appstore-signing.sh` and
`release-macos-appstore.sh`.

```
src-tauri/src/   main.rs (tray, HUD, Settings commands), pipeline.rs (hotkey →
                 record → transcribe → clean → paste), audio.rs, asr.rs (Whisper),
                 cloud_asr.rs (Groq/OpenAI), cleanup/ (rules, Ollama, LM Studio),
                 paste.rs, dictionary.rs, history.rs, settings.rs, secrets.rs
dist/            the HUD, Settings, History and About windows; common.js
scripts/         signing, notarization and App Store tooling
docs/            privacy policy, Windows build notes
```

[AGENTS.md](AGENTS.md) (in Spanish) is the guide for working on the code:
macOS signing and permissions pitfalls, settings format, and project rules.

## What's next

- **A signed, notarized download**, so you can install it without building.
- **Real testing on Windows and Linux.**
- **Keep everything on the clipboard**: today, pasting saves and restores
  only text, so a copied image is replaced.
- **Faster live preview** by reusing Whisper's working memory between passes.
- **Longer dictations** than 60 seconds.
- **A checksum on the model download**, not just its size.

## License

DictaFlow is free software under the [GNU General Public License v3](LICENSE),
with one added term in [NOTICE](NOTICE): fork it, change it and share it, as
long as your version stays open source under the same license and keeps the
credit "DictaFlow by Invisionnaire LLC" with a link back here, in its About
screen and its README.

## Support DictaFlow

DictaFlow is free and made in spare time. If it saves you some typing, you
can buy me a coffee:

[![Support me on Ko-fi](https://img.shields.io/badge/Ko--fi-Buy%20me%20a%20coffee-FF5E5B?logo=ko-fi&logoColor=white)](https://ko-fi.com/adrianrosariopr)
