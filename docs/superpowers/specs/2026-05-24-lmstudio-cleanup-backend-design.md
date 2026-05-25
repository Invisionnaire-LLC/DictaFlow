# LM Studio as a Cleanup Backend — Design

**Date:** 2026-05-24
**Status:** Approved (pending spec review)

## Summary

DictaFlow's text-cleanup step (the pass that polishes a raw transcript before pasting)
currently runs against a local **Ollama** server. This change adds **LM Studio** as a
second, equally-supported cleanup backend. The user picks which engine to use in
Settings; Ollama remains the default so existing installs are unaffected.

LM Studio and Ollama play the **same role** here — a local LLM that cleans text. They
differ only in wire protocol:

| | Ollama | LM Studio |
|---|---|---|
| Generate | `POST /api/generate` (`{response}`) | `POST /v1/chat/completions` (OpenAI shape, `choices[0].message.content`) |
| List models | `GET /api/tags` | `GET /v1/models` |
| Default base URL | `http://127.0.0.1:11434` | `http://localhost:1234/v1` |
| Auth | none (local) | none (local) |

### Out of scope (explicitly)

- **Transcription (ASR) via LM Studio.** LM Studio's server is LLM-only; its
  OpenAI-compatible API exposes `chat/completions`, `completions`, `embeddings`, and
  `models`, but **no `/v1/audio/transcriptions`** (open feature request
  lmstudio-bug-tracker#1715). Transcription stays on Local Whisper / Groq / OpenAI,
  unchanged. This work touches only the cleanup step.

## Architecture

Today `cleanup/mod.rs` hardcodes `OllamaCleaner`. Introduce a `Cleaner` trait so the
cleanup step is backend-agnostic, and swap the active backend at runtime the same way
`ctx.cloud` (the transcription backend) is swapped today.

```rust
pub struct ModelInfo {
    pub name: String,
    pub size: u64, // 0 when the backend doesn't report size (LM Studio)
}

pub trait Cleaner: Send + Sync {
    fn clean(&self, raw: &str) -> Result<String>;
    fn warmup(&self) -> Result<()>;
    fn model(&self) -> String;
    fn set_model(&self, model: String);
    fn list_models(&self) -> Result<Vec<ModelInfo>>;
}
```

### Components

- **`cleanup/prompt.rs` (new, extracted)** — the shared, backend-independent text logic
  that currently lives in `ollama.rs`: `PROMPT_HEADER`, `build_prompt`,
  `num_predict_cap`, `strip_preamble`, and the preamble constants. Both backends produce
  identical cleanup behavior because they share this. The existing unit tests for
  `strip_preamble` / `num_predict_cap` move here with it.

- **`cleanup/ollama.rs`** — `OllamaCleaner` implements `Cleaner`. Behavior unchanged;
  it now calls into `cleanup::prompt` instead of owning the prompt logic. Returns
  `ModelInfo` instead of the old `OllamaModel` (or `OllamaModel` is renamed/aliased).

- **`cleanup/lmstudio.rs` (new)** — `LmStudioCleaner` implements `Cleaner`:
  - `clean`: `POST {base_url}/chat/completions` with
    `{ model, messages: [{role:"user", content: build_prompt(raw)}], temperature: 0.0,
    max_tokens: num_predict_cap(raw), stream: false }`. Reads
    `choices[0].message.content`, then runs it through `strip_preamble`.
  - `list_models`: `GET {base_url}/models`, mapping each `data[].id` to
    `ModelInfo { name: id, size: 0 }`.
  - `warmup`: same cheap `clean("hello")` call as Ollama.
  - Holds `base_url` + `model` (both behind `RwLock` for runtime updates, mirroring
    `OllamaCleaner`'s `model: RwLock<String>`).

- **`cleanup/mod.rs` — `HybridCleaner`** — holds the active backend behind a lock:
  `backend: RwLock<Box<dyn Cleaner>>`. Constructed from the persisted `cleanup_backend`
  setting. The rules + dictionary pass and the rules-only fallback are unchanged; only
  the LLM call now goes through the trait object. Adds a `set_backend(...)` that
  rebuilds the boxed cleaner when the user switches engines.

## Data flow

1. On startup, `main.rs` reads `cleanup_backend` (default `"ollama"`) plus the relevant
   model / base-URL settings, builds the matching `Cleaner`, and stores it in
   `HybridCleaner` inside `AppContext`.
2. The dictation pipeline calls `ctx.cleaner.clean(raw)` exactly as today — it has no
   knowledge of which backend is active.
3. When the user changes engine or model in Settings, the relevant command updates
   `HybridCleaner` in place and persists to `settings.json`.

## Settings (new / changed fields)

```jsonc
{
  "cleanup_backend": "ollama",                     // NEW: "ollama" | "lmstudio"; default "ollama"
  "ollama_model": "gemma3:1b",                      // unchanged
  "lmstudio_model": null,                            // NEW: LM Studio model id (null = first available)
  "lmstudio_base_url": "http://localhost:1234/v1"   // NEW: configurable (port varies); default shown
}
```

- Models are stored **per backend** (Ollama model ids like `gemma3:1b` are unrelated to
  LM Studio model ids), so switching engines preserves each one's last choice.
- No Keychain: both backends are local with no API key.
- Persistence goes through the existing `Settings::update` locked-write path. New fields
  use `#[serde(default)]` so old `settings.json` files load cleanly.

## Tauri commands & UI

Generalize the three Ollama-specific commands into cleanup-backend-aware ones, mirroring
the existing `get/set_transcription_config` pattern:

| Old | New |
|---|---|
| `get_ollama_model` | `get_cleanup_config` → `{ backend, model, resolved_model, base_url }` |
| `set_ollama_model` | `set_cleanup_model` |
| `list_ollama_models` | `list_cleanup_models` (lists models for the **active** backend) |
| — | `set_cleanup_backend` (switches engine; also accepts LM Studio base_url) |

- `set_cleanup_backend` rebuilds the `HybridCleaner` backend, persists `cleanup_backend`
  (and `lmstudio_base_url` when given), and returns the new active config.
- Update `generate_handler!` in `main.rs` accordingly.

**Settings UI (`dist/settings.html` + `settings.js`):**
- Add a "Cleanup engine" selector (Ollama / LM Studio).
- Selecting an engine swaps which controls are shown: the model dropdown (populated via
  `list_cleanup_models` for the active backend) and, for LM Studio, an editable
  base-URL field defaulting to `http://localhost:1234/v1`.
- Reuse the existing model-picker styling and the "couldn't reach server" empty state.

## Error handling

Unchanged contract: if the selected LLM is unreachable, returns an error, or times out,
`HybridCleaner::clean` logs a warning and falls back to the rules-only output — the user
always gets text. `list_cleanup_models` surfaces a reachability error to the UI (so the
dropdown can show "couldn't reach LM Studio at <url>"). Switching the default to neither
backend is not introduced; Ollama stays the default.

## Testing

- `cleanup/prompt.rs`: the moved `strip_preamble` / `num_predict_cap` tests continue to
  pass in their new home.
- `cleanup/lmstudio.rs`: unit tests for request-body construction and for parsing a
  sample `chat/completions` response (`choices[0].message.content`) and a `models`
  response, plus that `strip_preamble` is applied to the result. Follow the no-network
  pattern already used in the cloud/ollama tests (test pure functions; don't hit a live
  server).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all green.

## Files touched

- `src-tauri/src/cleanup/mod.rs` — `Cleaner` trait, `ModelInfo`, `HybridCleaner` holds
  boxed backend, `set_backend`.
- `src-tauri/src/cleanup/prompt.rs` — **new**, extracted shared prompt/strip logic + tests.
- `src-tauri/src/cleanup/ollama.rs` — implement `Cleaner`, use `cleanup::prompt`.
- `src-tauri/src/cleanup/lmstudio.rs` — **new**, `LmStudioCleaner`.
- `src-tauri/src/settings.rs` — new fields + defaults.
- `src-tauri/src/main.rs` — startup wiring, new commands, `generate_handler!` update,
  warmup wiring.
- `dist/settings.html`, `dist/settings.js` — cleanup-engine selector + base-URL field.
- `AGENTS.md` — document the new cleanup backend option and settings fields.
