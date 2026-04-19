# DictaFlow

Local-first, cross-platform dictation. Hold a hotkey, speak, release — cleaned text lands at your cursor. All processing runs on your machine. MIT licensed. By **Invisionnaire**.

---

## Status — April 2026

| Phase | Scope | Status |
|---|---|---|
| 1 — Tracer bullet | Tray, hotkey, mic capture, synthesized paste | ✅ Done |
| 2 — Audio DSP + HUD | 16 kHz mono, DC/HPF/silence trim, RMS meter, floating pill | ✅ Done |
| 3 — ASR (Whisper) | `whisper-rs` + `large-v3-turbo` | ⚠️ Stubbed — see "ASR setup" below |
| 4 — Cleanup | Rules + Ollama (`gemma3:1b`) with 800 ms timeout + fallback | ✅ Done |
| 5 — End-to-end pipeline | Hotkey → audio → ASR → cleanup → paste | ✅ Done |
| 6 — UI polish | Tray menu, About window, state-driven tray icon | ✅ Done |
| 7 — Release infra | Code-signed builds, GitHub Actions | ⏳ Not started |
| 8 — Docs / launch | Public release | ⏳ Not started |

---

## How it works

```
F9 pressed
    │
    ▼
AudioController (cpal)  ──►  16 kHz mono f32 buffer
    │                        + live RMS → HUD bars
    ▼
F9 released
    │
    ▼
DSP:  DC-offset removal
       high-pass filter (80 Hz)
       silence trim (25 ms windows, head+tail margins)
    │
    ▼
ASR (Transcriber seam — whisper-rs behind a feature flag)
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

Each module is a leaf in `src-tauri/src/` — swap any backend by replacing one file.

---

## Requirements

| Tool | Why |
|---|---|
| Rust stable 1.78+ via rustup | Build |
| CMake ≥ 3.20 | **Only** required to build real ASR (Phase 3) |
| LLVM ≥ 15 (libclang) | **Only** required to build real ASR (Phase 3) |
| Visual Studio Build Tools (Windows) / Xcode CLT (macOS) | C++ toolchain for real ASR |
| Ollama | Optional — enables LLM cleanup. Without it, rules-only cleanup still runs. |

---

## Install (Windows)

```powershell
winget install Rustlang.Rustup
winget install Kitware.CMake
winget install LLVM.LLVM
# Visual Studio Build Tools should have installed with rustup.
```

Reopen your terminal. Then:

```bash
cd DictaFlow/src-tauri
cargo build --release
```

Release build output: `src-tauri/target/release/dictaflow.exe`.

For dev:

```bash
cargo run
```

---

## Install (macOS)

```bash
brew install rustup cmake llvm ollama
rustup-init
cd DictaFlow/src-tauri
cargo run
```

---

## Ollama setup (optional but recommended)

```bash
ollama serve &                # leave running in the background
ollama pull gemma3:1b
```

Without Ollama, DictaFlow silently falls back to rules-only cleanup. The app never errors on this path.

---

## ASR setup

DictaFlow ships with the ASR module **stubbed** so the rest of the app builds cleanly on any system. The seam is at [src-tauri/src/asr.rs](src-tauri/src/asr.rs) — the `Transcriber` struct is the only touchpoint for the rest of the app.

### Path A — whisper-rs (recommended)

1. Install CMake + LLVM + Visual Studio Build Tools (see Requirements).
2. If you have Windows Defender Application Control / Smart App Control enabled and see `OS error 4551`, add an exclusion for this directory:
   ```powershell
   Add-MpPreference -ExclusionPath "D:\path\to\DictaFlow"
   ```
   (Requires admin.)
3. In [src-tauri/Cargo.toml](src-tauri/Cargo.toml), uncomment:
   ```toml
   whisper-rs = { version = "0.14", default-features = false }
   ```
4. Replace [src-tauri/src/asr.rs](src-tauri/src/asr.rs) with the real implementation kept in git history as `asr.rs.whisper-rs`.
5. Set `LIBCLANG_PATH`:
   ```bash
   export LIBCLANG_PATH="C:\Users\<you>\OneDrive\Documents\LLVM\bin"   # Windows
   export LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib                     # macOS
   ```
6. `cargo build --release`. First run will download `ggml-large-v3-turbo.bin` (~1.5 GB) to `%LOCALAPPDATA%\DictaFlow\models\`.

### Path B — candle-whisper (pure Rust, no C++ toolchain)

Swap `whisper-rs` for `candle-transformers`. Slower than whisper.cpp on CPU, but builds without CMake/LLVM/MSVC. TODO — spec for this lives in docs.

### Path C — whisper.cpp subprocess

Run whisper.cpp's `whisper-server` locally (bundled with whisper.cpp release binaries), and point `asr.rs` at its HTTP endpoint. Works on any system with zero Rust-side C++ compilation.

---

## Daily use

1. Click into any text field (Notes, Slack, browser, terminal — anywhere).
2. Hold **F9**, speak, release.
3. Cleaned text appears at your cursor.

HUD pill at the bottom of your screen shows state:

- **Idle:** dim mic, static bars
- **Recording:** blue mic, RMS-driven bar animation
- **Processing:** amber spinner
- **Error:** red mic briefly, then back to idle

---

## Data locations

| Item | Path (Windows) | Path (macOS) |
|---|---|---|
| Recordings (debugging) | `%LOCALAPPDATA%\DictaFlow\recordings\` | `~/Library/Application Support/DictaFlow/recordings/` |
| Whisper model | `%LOCALAPPDATA%\DictaFlow\models\` | `~/Library/Application Support/DictaFlow/models/` |
| Logs | stdout (terminal) | stdout (terminal) |

---

## Development

```bash
cargo check            # fast type-check
cargo build            # full debug build
cargo build --release  # optimized build
cargo test --bins      # unit tests (may be blocked by WDAC — see notes)
```

Unit tests cover the pure DSP and cleaner logic in [audio.rs](src-tauri/src/audio.rs), [cleanup/rules.rs](src-tauri/src/cleanup/rules.rs), and [cleanup/ollama.rs](src-tauri/src/cleanup/ollama.rs).

---

## Project layout

```
DictaFlow/
├── dist/
│   ├── hud.html          # floating HUD pill (RMS-driven bars)
│   └── about.html        # About window
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── icons/            # tray + bundle icons
│   └── src/
│       ├── main.rs       # Tauri bootstrap + pipeline orchestration
│       ├── state.rs      # idle / recording / processing / error
│       ├── audio.rs      # cpal capture + DSP
│       ├── asr.rs        # Transcriber seam (currently stubbed)
│       ├── cleanup/
│       │   ├── mod.rs    # HybridCleaner
│       │   ├── rules.rs  # deterministic rules (ported 1:1 from Swift)
│       │   └── ollama.rs # local LLM via HTTP
│       └── paste.rs      # clipboard + synthesized paste
```

---

## License

MIT. See [LICENSE](LICENSE).
