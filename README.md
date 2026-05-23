# DictaFlow

Local-first, cross-platform dictation. Hold a hotkey, speak, release — cleaned text lands at your cursor. All processing runs on your machine. MIT licensed. By **Invisionnaire**.

---

## Status — April 2026

| Phase | Scope | Status |
|---|---|---|
| 1 — Tracer bullet | Tray, hotkey, mic capture, synthesized paste | Done |
| 2 — Audio DSP + HUD | 16 kHz mono, DC/HPF/silence trim, RMS meter, floating pill | Done |
| 3 — ASR | `whisper-rs` + `large-v3-turbo`, Metal on macOS | Done |
| 4 — Cleanup | Rules + Ollama (`gemma3:1b`) with 800 ms timeout + fallback | Done |
| 5 — End-to-end pipeline | Hotkey → audio → ASR → cleanup → paste | Done |
| 6 — UI polish | Tray menu, HUD, settings window, About window | Done |
| 7 — macOS bundling | Signed `.app`, LSUIElement, mic usage string, Accessibility gate | Done |
| 8 — Windows / Linux | Target-conditional whisper features (CUDA / CPU) | Scaffolded, untested |
| 9 — Release infra | GitHub Actions, notarization, DMG | Not started |

See [docs/COMPETITOR-GAP.md](docs/COMPETITOR-GAP.md) for a Wispr Flow feature comparison and prioritized gap list.

---

## How it works

```
Hotkey pressed  (default F9 — configurable in Settings)
    │
    ▼
AudioController (cpal)  ──►  16 kHz mono f32 buffer
    │                        + live RMS → HUD bars
    ▼
Hotkey released
    │
    ▼
DSP:  DC-offset removal
       high-pass filter (80 Hz)
       silence trim (25 ms windows, head+tail margins)
    │
    ▼
ASR (whisper-rs + ggml-large-v3-turbo, GPU accel on macOS via Metal)
    │
    ▼
HybridCleaner
    ├── RulesCleaner  (deterministic: fillers, spoken commands,
    │                  capitalization, repeated-word collapse)
    └── OllamaCleaner (local `gemma3:1b`, 800 ms timeout,
                       falls back to rules output on failure)
    │
    ▼
Paster (arboard + enigo) — save clipboard, write text,
                            synth Ctrl/Cmd+V, restore clipboard
    │
    ▼
Paste at cursor
```

Each module is a leaf in [src-tauri/src/](src-tauri/src/) — swap any backend by replacing one file.

---

## Requirements

| Tool | Why |
|---|---|
| Rust stable 1.78+ via rustup | Build |
| CMake ≥ 3.20 | Builds `whisper-rs` C++ backend |
| LLVM ≥ 15 (libclang) | `whisper-rs` bindgen |
| Xcode Command Line Tools (macOS) / Visual Studio Build Tools (Windows) | C++ toolchain |
| Tauri CLI (`cargo install tauri-cli --version "^2.0"`) | Only for `.app` / installer builds — `cargo run` works without it |
| Ollama | Optional — enables LLM cleanup. Without it, rules-only cleanup still runs. |
| NVIDIA CUDA toolkit | Optional, Windows only — required if you want GPU-accelerated Whisper |

---

## Running from source

### macOS

```bash
brew install rustup cmake llvm ollama
rustup-init
git clone https://github.com/Invisionnaire-LLC/DictaFlow.git
cd DictaFlow/src-tauri
cargo run
```

First run downloads the Whisper model (~1.5 GB) to `~/Library/Application Support/DictaFlow/models/`.

For a packaged menu-bar app (instead of running from terminal):

```bash
cargo install tauri-cli --version "^2.0"   # one-time
cargo tauri build --bundles app
cp -R target/release/bundle/macos/DictaFlow.app /Applications/
open /Applications/DictaFlow.app
```

The signing identity in [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json) points to a specific Apple Development cert. Change it to your own via `security find-identity -v -p codesigning`, or remove the `signingIdentity` key to fall back to ad-hoc signing (you'll re-grant TCC permissions on every rebuild).

### Windows

```powershell
winget install Rustlang.Rustup
winget install Kitware.CMake
winget install LLVM.LLVM
# Visual Studio Build Tools should have installed with rustup.
```

Optional, for GPU transcription: install the [NVIDIA CUDA toolkit](https://developer.nvidia.com/cuda-downloads) and ensure `nvcc` is on `PATH`. Without it, the Windows build will fail at link time because [Cargo.toml](src-tauri/Cargo.toml) enables the `cuda` feature of `whisper-rs` on Windows targets.

```bash
cd DictaFlow/src-tauri
cargo run
```

If you see `OS error 4551` from Windows Defender Application Control / Smart App Control, whitelist the repo:

```powershell
Add-MpPreference -ExclusionPath "D:\path\to\DictaFlow"
```

### Linux

X11 works out of the box. Wayland may block global hotkeys and synthetic keystrokes depending on your compositor. Whisper currently runs on CPU on Linux; swap the feature to `cuda` or `vulkan` in the Linux `[target.'cfg(target_os = "linux")'.dependencies]` block of [Cargo.toml](src-tauri/Cargo.toml) if you have the matching toolkit installed.

---

## macOS permissions

DictaFlow needs three TCC grants. On first run, macOS will prompt for each:

| Permission | Why | Where to grant |
|---|---|---|
| Microphone | Capture your voice | System Settings → Privacy & Security → Microphone |
| Accessibility | Synthesize ⌘V to paste transcribed text | System Settings → Privacy & Security → Accessibility |
| Input Monitoring | Register the global hotkey | System Settings → Privacy & Security → Input Monitoring |

If a grant ever gets stale (e.g., you change the signing identity or toggle hardened runtime), the paste path falls back gracefully: your transcription lands in the clipboard and System Settings → Accessibility opens so you can re-grant. Quit and relaunch after re-granting.

---

## Ollama setup (optional but recommended)

```bash
ollama serve &                # leave running in the background
ollama pull gemma3:1b
```

Without Ollama, DictaFlow silently falls back to rules-only cleanup. The app never errors on this path.

---

## Daily use

1. Click into any text field (Notes, Slack, browser, terminal — anywhere).
2. Hold your hotkey (default **F9**, change in Settings), speak, release.
3. Cleaned text appears at your cursor.

The tray icon (frog in your menu bar / system tray) is the home for the app — right-click it for the menu:

- **About DictaFlow** — version info
- **Settings…** — change hotkey, select microphone, reposition the HUD
- **Quit**

The floating HUD pill shows state:

- **Idle:** dim mic, static bars
- **Recording:** blue mic, RMS-driven bar animation
- **Processing:** amber spinner
- **Error:** red mic briefly, then back to idle

---

## Data locations

| Item | macOS | Windows |
|---|---|---|
| Whisper model | `~/Library/Application Support/DictaFlow/models/` | `%LOCALAPPDATA%\DictaFlow\models\` |
| Settings (hotkey, mic, HUD position) | `~/Library/Application Support/DictaFlow/settings.json` | `%APPDATA%\DictaFlow\settings.json` |
| Recording debug artifacts | `~/Library/Application Support/DictaFlow/recordings/` | `%LOCALAPPDATA%\DictaFlow\recordings\` |
| Logs | `~/Library/Logs/DictaFlow/app.log` | `%LOCALAPPDATA%\DictaFlow\Logs\app.log` |

---

## Development

```bash
cargo check            # fast type-check
cargo build            # full debug build
cargo build --release  # optimized build
cargo run              # launch dev build (inherits parent terminal's TCC grants)
cargo tauri build      # produce signed .app + DMG (macOS)
cargo test --bins      # unit tests for pure DSP and cleaner logic
```

Unit tests cover the pure DSP and cleaner logic in [audio.rs](src-tauri/src/audio.rs), [cleanup/rules.rs](src-tauri/src/cleanup/rules.rs), and [cleanup/ollama.rs](src-tauri/src/cleanup/ollama.rs).

---

## Project layout

```
DictaFlow/
├── dist/
│   ├── hud.html              # floating HUD pill (RMS-driven bars)
│   ├── settings.html         # settings window
│   └── about.html            # About window
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json       # Tauri config: bundle targets, macOS signing
│   ├── Info.plist            # LSUIElement + NSMicrophoneUsageDescription
│   ├── build.rs
│   ├── capabilities/         # Tauri 2 capability manifests
│   ├── icons/                # tray, macOS/Windows/iOS/Android icon sets
│   └── src/
│       ├── main.rs           # Tauri bootstrap + pipeline orchestration
│       ├── state.rs          # idle / recording / processing / error
│       ├── settings.rs       # settings load/save
│       ├── audio.rs          # cpal capture + DSP
│       ├── asr.rs            # whisper-rs Transcriber
│       ├── cleanup/
│       │   ├── mod.rs        # HybridCleaner
│       │   ├── rules.rs      # deterministic rules
│       │   └── ollama.rs     # local LLM via HTTP
│       └── paste.rs          # clipboard + synthesized paste
```

---

## License

MIT.
