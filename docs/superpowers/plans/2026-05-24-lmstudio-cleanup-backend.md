# LM Studio Cleanup Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add LM Studio as a second, user-selectable text-cleanup backend alongside the existing Ollama backend, with Ollama remaining the default.

**Architecture:** Introduce a `Cleaner` trait so the cleanup step is backend-agnostic. `OllamaCleaner` and a new `LmStudioCleaner` both implement it; shared prompt/strip logic moves to a `cleanup::prompt` module. `HybridCleaner` holds the active backend behind `RwLock<Box<dyn Cleaner>>` and is rebuilt at runtime when the user switches engines, mirroring how the transcription backend (`ctx.cloud`) is swapped today.

**Tech Stack:** Rust (Tauri backend, `reqwest::blocking`, `parking_lot::RwLock`, `serde`), vanilla JS/HTML settings UI.

---

## File Structure

- `src-tauri/src/cleanup/prompt.rs` — **new**. Shared, backend-independent prompt logic (`PROMPT_HEADER`, `build_prompt`, `num_predict_cap`, `strip_preamble`, preamble constants) + their unit tests.
- `src-tauri/src/cleanup/mod.rs` — `Cleaner` trait, `ModelInfo`, `normalize_backend`, `make_cleaner`, and `HybridCleaner` (now backend-agnostic with `set_backend`).
- `src-tauri/src/cleanup/ollama.rs` — `OllamaCleaner` implements `Cleaner`, uses `cleanup::prompt`.
- `src-tauri/src/cleanup/lmstudio.rs` — **new**. `LmStudioCleaner` implements `Cleaner`.
- `src-tauri/src/settings.rs` — new `cleanup_backend`, `lmstudio_model`, `lmstudio_base_url` fields.
- `src-tauri/src/main.rs` — startup wiring, four cleanup-backend-aware commands, `generate_handler!` update, warmup wiring.
- `dist/settings.html`, `dist/settings.js` — cleanup-engine selector + LM Studio base-URL field.
- `AGENTS.md` — document the new backend option and settings fields.

---

## Task 1: Extract shared prompt logic into `cleanup/prompt.rs`

**Files:**
- Create: `src-tauri/src/cleanup/prompt.rs`
- Modify: `src-tauri/src/cleanup/mod.rs:1` (add module declaration)
- Modify: `src-tauri/src/cleanup/ollama.rs` (remove moved items, import from `prompt`)

- [ ] **Step 1: Create `cleanup/prompt.rs` with the moved logic and tests**

Create `src-tauri/src/cleanup/prompt.rs`:

```rust
//! Backend-independent text logic shared by every cleanup backend (Ollama,
//! LM Studio). Both backends build the same prompt and strip the same model
//! preambles, so cleanup behavior is identical regardless of which LLM runs.

const COLON_TAIL_PREAMBLES: &[&str] = &["Here is the", "Here's the", "Here is", "Here's", "Sure,"];
const FULL_PREAMBLES: &[&str] = &["Output:", "Cleaned:"];

const PROMPT_HEADER: &str = "You are a literal text editor. Fix only obvious typos, missing \
punctuation, missing capitalization, and short transcription fragments where a partial word \
appears immediately before its full form (e.g., \"dict dictating\" → \"dictating\", \
\"s sentence\" → \"sentence\"). Do NOT change real words. Do NOT rephrase. Leave legitimate \
adjacent words alone (e.g., \"in inside\", \"my myself\", \"a apple\"). Output ONLY the fixed text.

Input: i went to the store and bought some apples
Output: I went to the store and bought some apples.

Input: she said hi and then walked away
Output: She said hi and then walked away.

Input: i am dict dictating a s sentence
Output: I am dictating a sentence.

Input: we went in inside the house
Output: We went in inside the house.

";

pub fn build_prompt(input: &str) -> String {
    format!("{}Input: {}\nOutput:", PROMPT_HEADER, input)
}

pub fn num_predict_cap(input: &str) -> i32 {
    let words = input.split_whitespace().count() as i32;
    60.max(words * 2)
}

pub fn strip_preamble(input: &str) -> String {
    let mut text = input.trim().to_string();

    for preamble in COLON_TAIL_PREAMBLES {
        if !text.to_lowercase().starts_with(&preamble.to_lowercase()) {
            continue;
        }
        let after = &text[preamble.len()..];
        if let Some(colon_idx) = after.find(':') {
            let head = &after[..colon_idx];
            if head.chars().count() <= 20 {
                text = after[colon_idx + 1..].trim().to_string();
                break;
            }
        }
    }

    for preamble in FULL_PREAMBLES {
        if text.to_lowercase().starts_with(&preamble.to_lowercase()) {
            text = text[preamble.len()..].trim().to_string();
            break;
        }
    }

    if text.starts_with('"') {
        text.remove(0);
        if text.ends_with('"') {
            text.pop();
        }
    }

    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_heres_the_cleaned_text() {
        assert_eq!(
            strip_preamble("Here's the cleaned text: Hello world."),
            "Hello world."
        );
    }

    #[test]
    fn strip_output_prefix() {
        assert_eq!(strip_preamble("Output: Hello."), "Hello.");
    }

    #[test]
    fn strip_quotes() {
        assert_eq!(strip_preamble("\"Hello world.\""), "Hello world.");
    }

    #[test]
    fn preserves_heres_without_colon_tail() {
        // No colon after "Here's" → not a preamble, real content.
        assert_eq!(
            strip_preamble("Here's how I did it."),
            "Here's how I did it."
        );
    }

    #[test]
    fn num_predict_floor_60() {
        assert_eq!(num_predict_cap("hi"), 60);
    }

    #[test]
    fn num_predict_scales_with_words() {
        let s = "a b c d e f g h i j k l m n o p q r s t u v w x y z ".repeat(5);
        let cap = num_predict_cap(&s);
        assert!(cap > 60);
    }
}
```

- [ ] **Step 2: Declare the module in `mod.rs`**

In `src-tauri/src/cleanup/mod.rs`, add to the top module declarations (currently lines 1-2):

```rust
pub mod ollama;
pub mod prompt;
pub mod rules;
```

- [ ] **Step 3: Remove the moved items from `ollama.rs` and import from `prompt`**

In `src-tauri/src/cleanup/ollama.rs`:

Delete these now-moved items:
- the `COLON_TAIL_PREAMBLES` constant (line 11)
- the `FULL_PREAMBLES` constant (line 12)
- the `PROMPT_HEADER` constant (lines 14-32)
- the `build_prompt`, `num_predict_cap`, `strip_preamble` functions (lines 158-199)
- the entire `#[cfg(test)] mod tests { ... }` block (lines 201-243)

Then add this import near the top of the file (after the existing `use serde::...` line):

```rust
use super::prompt::{build_prompt, num_predict_cap, strip_preamble};
```

The three call sites inside `OllamaCleaner::clean` stay textually identical (`build_prompt(...)`, `num_predict_cap(...)`, `strip_preamble(...)`) — they now resolve to the imported functions.

- [ ] **Step 4: Run fmt, clippy, and tests**

Run:
```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```
Expected: builds clean, the six moved tests (`strip_*`, `num_predict_*`) pass in their new `prompt` module home.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/cleanup/prompt.rs src-tauri/src/cleanup/mod.rs src-tauri/src/cleanup/ollama.rs
git commit -m "refactor(cleanup): extract shared prompt logic into prompt module

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Add the `Cleaner` trait and `ModelInfo`; make `OllamaCleaner` implement it

**Files:**
- Modify: `src-tauri/src/cleanup/mod.rs` (trait + `ModelInfo` + `normalize_backend`)
- Modify: `src-tauri/src/cleanup/ollama.rs` (impl `Cleaner`, return `ModelInfo`)
- Modify: `src-tauri/src/main.rs:1587` (command return type `OllamaModel` → `ModelInfo`)

- [ ] **Step 1: Add the trait, `ModelInfo`, and `normalize_backend` to `mod.rs`**

In `src-tauri/src/cleanup/mod.rs`, replace the header region (currently lines 1-6, from the module declarations through `use self::ollama::OllamaCleaner;`) with:

```rust
pub mod ollama;
pub mod prompt;
pub mod rules;

use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

use self::ollama::OllamaCleaner;

/// A model offered by a cleanup backend. `size` is 0 when the backend doesn't
/// report it (LM Studio's `/v1/models` has no size field).
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub size: u64,
}

/// A local LLM that polishes text. Ollama and LM Studio play the same role and
/// differ only in wire protocol.
pub trait Cleaner: Send + Sync {
    fn clean(&self, raw: &str) -> Result<String>;
    fn warmup(&self) -> Result<()>;
    fn model(&self) -> String;
    fn set_model(&self, model: String);
    fn list_models(&self) -> Result<Vec<ModelInfo>>;
}

/// Canonical backend name. Anything unrecognized falls back to Ollama (default).
pub fn normalize_backend(name: &str) -> &'static str {
    match name.trim().to_ascii_lowercase().as_str() {
        "lmstudio" => "lmstudio",
        _ => "ollama",
    }
}
```

(The existing `use self::ollama::OllamaCleaner;` import is preserved. `Duration` was already imported via `use std::time::Duration;` — keep it; it's still used by `HybridCleaner::new`.)

- [ ] **Step 2: Update the `HybridCleaner` method that returns the model list**

In `src-tauri/src/cleanup/mod.rs`, the `list_ollama_models` method (currently lines 29-31) returns `Vec<ollama::OllamaModel>`. Change its return type to `Vec<ModelInfo>`:

```rust
    pub fn list_ollama_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        self.ollama.list_models()
    }
```

(Leave `new`, `ollama_model`, `set_ollama_model`, `clean`, `warmup`, and the free functions unchanged in this task. `HybridCleaner` still holds the concrete `OllamaCleaner` — it is refactored in Task 5.)

- [ ] **Step 3: Convert `OllamaCleaner` to implement `Cleaner` and return `ModelInfo`**

In `src-tauri/src/cleanup/ollama.rs`:

Add the trait import near the `use super::prompt::...` line added in Task 1:

```rust
use super::{Cleaner, ModelInfo};
```

Delete the `OllamaModel` struct (currently lines 55-59):

```rust
#[derive(Debug, Clone, Serialize)]
pub struct OllamaModel {
    pub name: String,
    pub size: u64,
}
```

Keep `use serde::{Deserialize, Serialize};` as-is — `GenerateRequest`/`GenerateOptions` still derive `Serialize`.

Move the `model`, `set_model`, `clean`, `warmup`, and `list_models` methods out of the inherent `impl OllamaCleaner` block into a new trait impl, keeping `new` inherent. The result:

```rust
impl OllamaCleaner {
    pub fn new(timeout: Duration, model: String) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client");
        OllamaCleaner {
            client,
            model: RwLock::new(model),
        }
    }
}

impl Cleaner for OllamaCleaner {
    fn model(&self) -> String {
        self.model.read().clone()
    }

    fn set_model(&self, model: String) {
        *self.model.write() = model;
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        list_models_with_client(&self.client)
    }

    fn clean(&self, raw_text: &str) -> Result<String> {
        let model = self.model();
        let body = GenerateRequest {
            model: &model,
            prompt: build_prompt(raw_text),
            stream: false,
            keep_alive: -1,
            options: GenerateOptions {
                temperature: 0.0,
                top_k: 1,
                num_predict: num_predict_cap(raw_text),
            },
        };
        let resp = self
            .client
            .post(ENDPOINT)
            .json(&body)
            .send()
            .context("ollama request")?;

        if !resp.status().is_success() {
            return Err(anyhow!("ollama http status {}", resp.status()));
        }

        let decoded: GenerateResponse = resp.json().context("ollama decode")?;
        let stripped = strip_preamble(&decoded.response);
        if stripped.is_empty() {
            return Err(anyhow!("ollama empty response"));
        }
        Ok(stripped)
    }

    fn warmup(&self) -> Result<()> {
        let _ = self.clean("hello")?;
        Ok(())
    }
}
```

Update `list_models_with_client` (currently lines 140-156) to build `ModelInfo` instead of `OllamaModel`:

```rust
fn list_models_with_client(client: &Client) -> Result<Vec<ModelInfo>> {
    let resp = client.get(TAGS_ENDPOINT).send().context("ollama tags")?;
    if !resp.status().is_success() {
        return Err(anyhow!("ollama tags http status {}", resp.status()));
    }
    let decoded: TagsResponse = resp.json().context("ollama tags decode")?;
    let mut models: Vec<ModelInfo> = decoded
        .models
        .into_iter()
        .map(|m| ModelInfo {
            name: m.name,
            size: m.size.unwrap_or_default(),
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}
```

- [ ] **Step 4: Update the command return type in `main.rs`**

In `src-tauri/src/main.rs`, the `list_ollama_models` command (line 1587) currently returns `Result<Vec<cleanup::ollama::OllamaModel>, String>`. Change the type to `cleanup::ModelInfo`:

```rust
fn list_ollama_models(app: AppHandle) -> Result<Vec<cleanup::ModelInfo>, String> {
```

- [ ] **Step 5: Run fmt, clippy, and tests**

Run:
```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```
Expected: builds clean (no `OllamaModel` references remain), all tests pass. Behavior is unchanged; this is a pure refactor.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/cleanup/mod.rs src-tauri/src/cleanup/ollama.rs src-tauri/src/main.rs
git commit -m "refactor(cleanup): add Cleaner trait and ModelInfo, impl for Ollama

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Create the `LmStudioCleaner` backend

**Files:**
- Create: `src-tauri/src/cleanup/lmstudio.rs`
- Modify: `src-tauri/src/cleanup/mod.rs:1` (add `pub mod lmstudio;`)

- [ ] **Step 1: Create `cleanup/lmstudio.rs` with the cleaner and its unit tests**

Create `src-tauri/src/cleanup/lmstudio.rs`:

```rust
//! LM Studio cleanup backend. Same role as Ollama (a local LLM that polishes
//! text), but speaks LM Studio's OpenAI-compatible wire protocol:
//!   - generate:    POST {base_url}/chat/completions  (choices[0].message.content)
//!   - list models: GET  {base_url}/models            (data[].id)
//! Default base URL is http://localhost:1234/v1 (port is configurable).

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use parking_lot::RwLock;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use super::prompt::{build_prompt, num_predict_cap, strip_preamble};
use super::{Cleaner, ModelInfo};

pub const DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    max_tokens: i32,
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Holds the LM Studio base URL and the selected model (empty = first available),
/// both behind `RwLock` for runtime updates — mirroring `OllamaCleaner`.
pub struct LmStudioCleaner {
    client: Client,
    base_url: RwLock<String>,
    model: RwLock<String>,
}

impl LmStudioCleaner {
    pub fn new(timeout: Duration, base_url: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client");
        LmStudioCleaner {
            client,
            base_url: RwLock::new(normalize_base_url(&base_url)),
            model: RwLock::new(model.trim().to_string()),
        }
    }

    pub fn base_url(&self) -> String {
        self.base_url.read().clone()
    }

    /// The selected model, or the first listed model when none is configured.
    fn resolve_model(&self) -> Result<String> {
        let model = self.model();
        if !model.is_empty() {
            return Ok(model);
        }
        let first = self
            .list_models()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("LM Studio has no models loaded"))?;
        Ok(first.name)
    }
}

impl Cleaner for LmStudioCleaner {
    fn clean(&self, raw: &str) -> Result<String> {
        let model = self.resolve_model()?;
        let prompt = build_prompt(raw);
        let body = ChatRequest {
            model: &model,
            messages: vec![ChatMessage {
                role: "user",
                content: &prompt,
            }],
            temperature: 0.0,
            max_tokens: num_predict_cap(raw),
            stream: false,
        };
        let url = format!("{}/chat/completions", self.base_url());
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .context("lmstudio request")?;

        if !resp.status().is_success() {
            return Err(anyhow!("lmstudio http status {}", resp.status()));
        }

        let decoded: ChatResponse = resp.json().context("lmstudio decode")?;
        let content = decoded
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("lmstudio empty choices"))?
            .message
            .content;
        let stripped = strip_preamble(&content);
        if stripped.is_empty() {
            return Err(anyhow!("lmstudio empty response"));
        }
        Ok(stripped)
    }

    fn warmup(&self) -> Result<()> {
        let _ = self.clean("hello")?;
        Ok(())
    }

    fn model(&self) -> String {
        self.model.read().clone()
    }

    fn set_model(&self, model: String) {
        *self.model.write() = model.trim().to_string();
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.base_url());
        let resp = self.client.get(&url).send().context("lmstudio models")?;
        if !resp.status().is_success() {
            return Err(anyhow!("lmstudio models http status {}", resp.status()));
        }
        let decoded: ModelsResponse = resp.json().context("lmstudio models decode")?;
        let mut models: Vec<ModelInfo> = decoded
            .data
            .into_iter()
            .map(|m| ModelInfo {
                name: m.id,
                size: 0,
            })
            .collect();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(models)
    }
}

fn normalize_base_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_shape() {
        let prompt = build_prompt("hi");
        let body = ChatRequest {
            model: "qwen2.5-7b",
            messages: vec![ChatMessage {
                role: "user",
                content: &prompt,
            }],
            temperature: 0.0,
            max_tokens: num_predict_cap("hi"),
            stream: false,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["model"], "qwen2.5-7b");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], prompt);
        assert_eq!(v["temperature"], 0.0);
        assert_eq!(v["stream"], false);
        assert_eq!(v["max_tokens"], 60);
    }

    #[test]
    fn parses_chat_response_and_strips_preamble() {
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"Output: Hello world."}}]}"#;
        let decoded: ChatResponse = serde_json::from_str(json).unwrap();
        let content = decoded.choices.into_iter().next().unwrap().message.content;
        assert_eq!(strip_preamble(&content), "Hello world.");
    }

    #[test]
    fn parses_models_response() {
        let json = r#"{"data":[{"id":"qwen2.5-7b"},{"id":"llama-3.2-3b"}]}"#;
        let decoded: ModelsResponse = serde_json::from_str(json).unwrap();
        let mut models: Vec<ModelInfo> = decoded
            .data
            .into_iter()
            .map(|m| ModelInfo {
                name: m.id,
                size: 0,
            })
            .collect();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(models[0].name, "llama-3.2-3b");
        assert_eq!(models[1].name, "qwen2.5-7b");
        assert_eq!(models[0].size, 0);
    }

    #[test]
    fn normalizes_base_url() {
        assert_eq!(
            normalize_base_url("http://localhost:1234/v1/"),
            "http://localhost:1234/v1"
        );
        assert_eq!(normalize_base_url("  "), DEFAULT_BASE_URL);
    }
}
```

- [ ] **Step 2: Declare the module in `mod.rs`**

In `src-tauri/src/cleanup/mod.rs`, add `lmstudio` to the module declarations at the top:

```rust
pub mod lmstudio;
pub mod ollama;
pub mod prompt;
pub mod rules;
```

- [ ] **Step 3: Run fmt, clippy, and tests**

Run:
```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```
Expected: builds clean; the four new `lmstudio::tests` pass. `LmStudioCleaner` is not yet wired in (that's Task 5). If clippy flags `base_url` or `LmStudioCleaner` as never-used, that's expected at this point — it's consumed in Task 5, so do **not** add `#[allow(dead_code)]`; instead, if clippy errors block the gate, proceed to Task 5 in the same session and run the gate there. (Trait-impl methods are not dead-code-flagged, and `pub` items are typically exempt, so this is unlikely to error.)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/cleanup/lmstudio.rs src-tauri/src/cleanup/mod.rs
git commit -m "feat(cleanup): add LmStudioCleaner backend

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Add the new settings fields

**Files:**
- Modify: `src-tauri/src/settings.rs:115` (the `Settings` struct, after `ollama_model`)

- [ ] **Step 1: Add the three new fields to `Settings`**

In `src-tauri/src/settings.rs`, the `Settings` struct already has (line 114-115):

```rust
    #[serde(default)]
    pub ollama_model: Option<String>,
```

Directly after that field, add:

```rust
    /// Cleanup backend: "ollama" (default) or "lmstudio".
    #[serde(default)]
    pub cleanup_backend: Option<String>,
    /// LM Studio model id (None = first available).
    #[serde(default)]
    pub lmstudio_model: Option<String>,
    /// LM Studio server base URL (port varies); None = the default shown in the UI.
    #[serde(default)]
    pub lmstudio_base_url: Option<String>,
```

`#[serde(default)]` ensures old `settings.json` files load cleanly; `Settings` already derives `Default`.

- [ ] **Step 2: Run fmt, clippy, and tests**

Run:
```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```
Expected: builds clean, all tests pass.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/settings.rs
git commit -m "feat(settings): add cleanup_backend, lmstudio_model, lmstudio_base_url

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Make `HybridCleaner` backend-agnostic and wire it through `main.rs`

**Files:**
- Modify: `src-tauri/src/cleanup/mod.rs` (`HybridCleaner` holds boxed backend, `set_backend`, `make_cleaner`)
- Modify: `src-tauri/src/main.rs` (startup wiring, commands, `generate_handler!`)

- [ ] **Step 1: Add `parking_lot` + `lmstudio` imports in `mod.rs`**

In `src-tauri/src/cleanup/mod.rs`, update the imports added in Task 2 to also bring in `RwLock` and the LM Studio items:

```rust
use std::time::Duration;

use anyhow::Result;
use parking_lot::RwLock;
use serde::Serialize;

use self::lmstudio::{LmStudioCleaner, DEFAULT_BASE_URL as LMSTUDIO_DEFAULT_BASE_URL};
use self::ollama::OllamaCleaner;
```

- [ ] **Step 2: Rewrite `HybridCleaner` and add `make_cleaner` in `mod.rs`**

Replace the entire `HybridCleaner` block — the `pub struct HybridCleaner { ollama: OllamaCleaner }` struct plus its whole `impl HybridCleaner { ... }` (everything from `pub struct HybridCleaner` down to the closing `}` of the impl, just above `fn normalize_for_compare`) — with:

```rust
/// Builds the boxed `Cleaner` for the named backend. Unrecognized names fall
/// back to Ollama. `lmstudio_model` empty means "first available".
pub fn make_cleaner(
    backend: &str,
    timeout: Duration,
    ollama_model: &str,
    lmstudio_model: &str,
    lmstudio_base_url: &str,
) -> Box<dyn Cleaner> {
    match normalize_backend(backend) {
        "lmstudio" => {
            let base = if lmstudio_base_url.trim().is_empty() {
                LMSTUDIO_DEFAULT_BASE_URL
            } else {
                lmstudio_base_url
            };
            Box::new(LmStudioCleaner::new(
                timeout,
                base.to_string(),
                lmstudio_model.to_string(),
            ))
        }
        _ => Box::new(OllamaCleaner::new(timeout, ollama_model.to_string())),
    }
}

/// Composes deterministic rules with an LLM pass. If the LLM fails or times
/// out, returns the rules-cleaned output. User always gets something back.
/// The active LLM backend is swappable at runtime via `set_backend`.
pub struct HybridCleaner {
    backend: RwLock<Box<dyn Cleaner>>,
    backend_name: RwLock<String>,
    /// The configured LM Studio base URL, kept so the UI can show it even while
    /// Ollama is the active backend. Empty string when never configured.
    base_url: RwLock<String>,
}

impl HybridCleaner {
    pub fn new(backend_name: String, base_url: String, cleaner: Box<dyn Cleaner>) -> Self {
        HybridCleaner {
            backend: RwLock::new(cleaner),
            backend_name: RwLock::new(normalize_backend(&backend_name).to_string()),
            base_url: RwLock::new(base_url),
        }
    }

    pub fn backend_name(&self) -> String {
        self.backend_name.read().clone()
    }

    pub fn base_url(&self) -> String {
        self.base_url.read().clone()
    }

    pub fn model(&self) -> String {
        self.backend.read().model()
    }

    pub fn set_model(&self, model: String) {
        self.backend.read().set_model(model);
    }

    pub fn list_models(&self) -> Result<Vec<ModelInfo>> {
        self.backend.read().list_models()
    }

    /// Swap the active backend (called when the user changes engine).
    pub fn set_backend(&self, backend_name: String, base_url: String, cleaner: Box<dyn Cleaner>) {
        *self.backend.write() = cleaner;
        *self.backend_name.write() = normalize_backend(&backend_name).to_string();
        *self.base_url.write() = base_url;
    }

    pub fn clean(&self, raw: &str) -> String {
        let rules_output = rules::apply(raw);
        let rules_output = crate::dictionary::apply_replacements(&rules_output);
        if should_skip_llm(raw, &rules_output) {
            tracing::debug!("hybrid source=rules_only in={}", rules_output.len());
            return rules_output;
        }
        match self.backend.read().clean(&rules_output) {
            Ok(out) => {
                tracing::info!(
                    "hybrid source=llm_ok in={} out={}",
                    rules_output.len(),
                    out.len()
                );
                out
            }
            Err(e) => {
                tracing::warn!("hybrid fallback to rules: {:?}", e);
                rules_output
            }
        }
    }

    pub fn warmup(&self) {
        if let Err(e) = self.backend.read().warmup() {
            tracing::warn!("cleanup warmup skipped: {:?}", e);
        }
    }
}
```

Leave `normalize_for_compare` and `should_skip_llm` (the free functions at the bottom of the file) unchanged.

- [ ] **Step 3: Update startup wiring in `main.rs`**

In `src-tauri/src/main.rs`, replace the `HybridCleaner` construction in `setup` (currently lines 364-368, the `let ollama_model = ...` block through `let cleaner = HybridCleaner::new(...)`) with:

```rust
            let cleanup_backend = saved
                .cleanup_backend
                .clone()
                .unwrap_or_else(|| "ollama".to_string());
            let ollama_model = saved
                .ollama_model
                .clone()
                .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string());
            let lmstudio_model = saved.lmstudio_model.clone().unwrap_or_default();
            let lmstudio_base_url = saved
                .lmstudio_base_url
                .clone()
                .unwrap_or_else(|| cleanup::lmstudio::DEFAULT_BASE_URL.to_string());
            let cleaner_box = cleanup::make_cleaner(
                &cleanup_backend,
                Duration::from_millis(800),
                &ollama_model,
                &lmstudio_model,
                &lmstudio_base_url,
            );
            let cleaner =
                HybridCleaner::new(cleanup_backend.clone(), lmstudio_base_url.clone(), cleaner_box);
```

Then update the log line at (currently) line 387, `tracing::info!("settings: using ollama model '{}'", ollama_model);`, to:

```rust
            tracing::info!(
                "settings: cleanup backend '{}' (ollama model '{}')",
                cleanup::normalize_backend(&cleanup_backend),
                ollama_model
            );
```

- [ ] **Step 4: Replace the three Ollama commands with four cleanup commands**

In `src-tauri/src/main.rs`, delete the `get_ollama_model`, `list_ollama_models`, and `set_ollama_model` commands (currently lines 1576-1618) and replace them with:

```rust
#[derive(serde::Serialize)]
struct CleanupConfig {
    backend: String,
    /// User's model selection (empty = backend default / first available).
    model: String,
    /// The model that would actually be sent.
    resolved_model: String,
    /// LM Studio base URL (always populated so the UI can prefill it).
    base_url: String,
}

#[tauri::command]
fn get_cleanup_config(app: AppHandle) -> CleanupConfig {
    if let Some(ctx) = app.try_state::<AppContext>() {
        let backend = ctx.cleaner.backend_name();
        let model = ctx.cleaner.model();
        let resolved_model = if !model.is_empty() {
            model.clone()
        } else {
            ctx.cleaner
                .list_models()
                .ok()
                .and_then(|m| m.into_iter().next())
                .map(|m| m.name)
                .unwrap_or_default()
        };
        return CleanupConfig {
            backend,
            model,
            resolved_model,
            base_url: ctx.cleaner.base_url(),
        };
    }
    let saved = settings::Settings::load();
    let backend =
        cleanup::normalize_backend(saved.cleanup_backend.as_deref().unwrap_or("ollama")).to_string();
    let model = if backend == "lmstudio" {
        saved.lmstudio_model.clone().unwrap_or_default()
    } else {
        saved
            .ollama_model
            .clone()
            .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string())
    };
    CleanupConfig {
        resolved_model: model.clone(),
        model,
        backend,
        base_url: saved
            .lmstudio_base_url
            .clone()
            .unwrap_or_else(|| cleanup::lmstudio::DEFAULT_BASE_URL.to_string()),
    }
}

#[tauri::command]
fn list_cleanup_models(app: AppHandle) -> Result<Vec<cleanup::ModelInfo>, String> {
    let ctx = app
        .try_state::<AppContext>()
        .ok_or_else(|| "app context missing".to_string())?;
    let backend = ctx.cleaner.backend_name();
    ctx.cleaner.list_models().map_err(|e| {
        if backend == "lmstudio" {
            format!("couldn't reach LM Studio at {}: {}", ctx.cleaner.base_url(), e)
        } else {
            format!("could not list Ollama models: {}", e)
        }
    })
}

#[tauri::command]
fn set_cleanup_model(app: AppHandle, model: String) -> Result<String, String> {
    let trimmed = model.trim();
    let ctx = app
        .try_state::<AppContext>()
        .ok_or_else(|| "app context missing".to_string())?;
    let backend = ctx.cleaner.backend_name();

    // Ollama always needs a model; LM Studio allows empty (= first available).
    if backend != "lmstudio" && trimmed.is_empty() {
        return Err("Enter an Ollama model first.".to_string());
    }

    let selected = trimmed.to_string();
    ctx.cleaner.set_model(selected.clone());

    if let Err(e) = settings::Settings::update(|saved| {
        if backend == "lmstudio" {
            saved.lmstudio_model = if selected.is_empty() {
                None
            } else {
                Some(selected.clone())
            };
        } else {
            saved.ollama_model = Some(selected.clone());
        }
    }) {
        tracing::warn!("could not persist cleanup model: {:?}", e);
    }

    tracing::info!("cleanup model ({}) changed to: {}", backend, selected);
    Ok(selected)
}

#[tauri::command]
fn set_cleanup_backend(
    app: AppHandle,
    backend: String,
    base_url: Option<String>,
) -> Result<CleanupConfig, String> {
    let ctx = app
        .try_state::<AppContext>()
        .ok_or_else(|| "app context missing".to_string())?;

    let normalized = cleanup::normalize_backend(&backend).to_string();
    let saved_now = settings::Settings::load();
    let lmstudio_base_url = base_url
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .or_else(|| saved_now.lmstudio_base_url.clone())
        .unwrap_or_else(|| cleanup::lmstudio::DEFAULT_BASE_URL.to_string());
    let ollama_model = saved_now
        .ollama_model
        .clone()
        .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string());
    let lmstudio_model = saved_now.lmstudio_model.clone().unwrap_or_default();

    let cleaner_box = cleanup::make_cleaner(
        &normalized,
        Duration::from_millis(800),
        &ollama_model,
        &lmstudio_model,
        &lmstudio_base_url,
    );
    ctx.cleaner
        .set_backend(normalized.clone(), lmstudio_base_url.clone(), cleaner_box);

    if let Err(e) = settings::Settings::update(|s| {
        s.cleanup_backend = Some(normalized.clone());
        s.lmstudio_base_url = Some(lmstudio_base_url.clone());
    }) {
        tracing::warn!("could not persist cleanup backend: {:?}", e);
    }
    tracing::info!("cleanup backend changed to: {}", normalized);

    // Warm up the new backend off the UI thread.
    let app_warm = app.clone();
    thread::spawn(move || {
        if let Some(ctx) = app_warm.try_state::<AppContext>() {
            ctx.cleaner.warmup();
        }
    });

    Ok(get_cleanup_config(app))
}
```

- [ ] **Step 5: Update `generate_handler!`**

In `src-tauri/src/main.rs`, in the `tauri::generate_handler![ ... ]` list (lines 316-318), replace:

```rust
            get_ollama_model,
            list_ollama_models,
            set_ollama_model,
```

with:

```rust
            get_cleanup_config,
            list_cleanup_models,
            set_cleanup_model,
            set_cleanup_backend,
```

- [ ] **Step 6: Run fmt, clippy, and tests**

Run:
```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```
Expected: builds clean (no `get_ollama_model`/`set_ollama_model`/`list_ollama_models`/`OllamaModel` references remain anywhere), all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/cleanup/mod.rs src-tauri/src/main.rs
git commit -m "feat(cleanup): swap cleanup backend at runtime via HybridCleaner

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Settings UI — cleanup engine selector + LM Studio base URL

**Files:**
- Modify: `dist/settings.html:641-668` (the `#panel-cleanup` section)
- Modify: `dist/settings.js` (element refs at 30-35, state var at 57, functions at 89-173, handlers at 381-421)

- [ ] **Step 1: Replace the cleanup panel HTML**

In `dist/settings.html`, replace the entire `#panel-cleanup` section (lines 641-668) with:

```html
      <section id="panel-cleanup" class="panel" aria-labelledby="nav-cleanup">
        <header class="panel-head">
          <h2>Cleanup</h2>
          <p>Pick the local LLM that polishes text after rules cleanup. Rules always run first.</p>
        </header>

        <div class="card">
          <div class="card-title">Cleanup engine</div>
          <div class="field">
            <label for="cleanupBackend">Engine</label>
            <select id="cleanupBackend" class="control">
              <option value="ollama">Ollama (default)</option>
              <option value="lmstudio">LM Studio</option>
            </select>
          </div>

          <div class="field" id="lmstudioUrlField" style="display:none">
            <label for="lmstudioBaseUrl">LM Studio server URL</label>
            <div class="combo-row">
              <input id="lmstudioBaseUrl" class="control" spellcheck="false" autocomplete="off" placeholder="http://localhost:1234/v1" />
              <button id="lmstudioUrlSave" type="button">Apply</button>
            </div>
          </div>

          <div class="field">
            <label for="cleanupModel" id="cleanupModelLabel">Model name</label>
            <div class="combo-wrap">
              <div class="combo-row">
                <input id="cleanupModel" class="control" spellcheck="false" autocomplete="off" placeholder="gemma3:1b" />
                <button id="cleanupToggle" class="combo-toggle" type="button" aria-label="Show models" aria-expanded="false">▾</button>
              </div>
              <div id="cleanupList" class="combo-list" role="listbox"></div>
            </div>
          </div>
          <div class="btn-row">
            <button id="cleanupSave" class="primary" disabled>Save model</button>
            <button id="cleanupRefresh" type="button">Refresh list</button>
          </div>
          <div id="cleanupMsg" class="msg"></div>
          <p class="hint" id="cleanupHint">
            Type any Ollama model, or open the list to pick from installed ones. <b>gemma3:1b</b> is always available as the DictaFlow default.
          </p>
        </div>
      </section>
```

- [ ] **Step 2: Replace the element-reference declarations in `settings.js`**

In `dist/settings.js`, replace the six `ollama*` element refs (lines 30-35):

```javascript
    const ollamaModelEl = document.getElementById("ollamaModel");
    const ollamaToggleEl = document.getElementById("ollamaToggle");
    const ollamaListEl = document.getElementById("ollamaList");
    const ollamaMsgEl = document.getElementById("ollamaMsg");
    const ollamaSaveBtn = document.getElementById("ollamaSave");
    const ollamaRefreshBtn = document.getElementById("ollamaRefresh");
```

with:

```javascript
    const cleanupBackendEl = document.getElementById("cleanupBackend");
    const lmstudioUrlFieldEl = document.getElementById("lmstudioUrlField");
    const lmstudioBaseUrlEl = document.getElementById("lmstudioBaseUrl");
    const lmstudioUrlSaveBtn = document.getElementById("lmstudioUrlSave");
    const cleanupModelEl = document.getElementById("cleanupModel");
    const cleanupToggleEl = document.getElementById("cleanupToggle");
    const cleanupListEl = document.getElementById("cleanupList");
    const cleanupMsgEl = document.getElementById("cleanupMsg");
    const cleanupModelLabelEl = document.getElementById("cleanupModelLabel");
    const cleanupHintEl = document.getElementById("cleanupHint");
    const cleanupSaveBtn = document.getElementById("cleanupSave");
    const cleanupRefreshBtn = document.getElementById("cleanupRefresh");
```

- [ ] **Step 3: Replace the state var**

In `dist/settings.js`, replace the line (57):

```javascript
    let currentOllamaModel = "";
```

with:

```javascript
    let currentCleanupModel = "";
    let cleanupBackend = "ollama";
```

- [ ] **Step 4: Replace the cleanup combo/loader functions**

In `dist/settings.js`, replace the block from `function setOllamaMessage` through the end of `loadOllamaModels();` and its trailing `}` (lines 89-173) with the rewritten cleanup logic below. (`formatBytes` at lines 77-87 stays as-is, above this block.) Note: list/hint DOM updates use `replaceChildren()` / `textContent` rather than `innerHTML` — no untrusted HTML is ever injected.

```javascript
    function setCleanupMessage(text, kind = "") {
      cleanupMsgEl.textContent = text;
      cleanupMsgEl.className = kind ? `msg ${kind}` : "msg";
    }

    function cleanupIsLmStudio() {
      return cleanupBackendEl.value === "lmstudio";
    }

    let comboItems = [{ value: "gemma3:1b", meta: "DictaFlow default" }];

    function renderComboList() {
      const selected = cleanupModelEl.value.trim();
      cleanupListEl.replaceChildren();
      for (const item of comboItems) {
        const row = document.createElement("div");
        row.className = "combo-item";
        row.setAttribute("role", "option");
        if (item.value === selected) row.classList.add("selected");
        const name = document.createElement("span");
        name.textContent = item.value;
        row.appendChild(name);
        if (item.meta) {
          const meta = document.createElement("span");
          meta.className = "meta";
          meta.textContent = item.meta;
          row.appendChild(meta);
        }
        row.addEventListener("mousedown", (e) => {
          e.preventDefault();
          cleanupModelEl.value = item.value;
          onCleanupModelInput();
          closeComboList();
          cleanupModelEl.focus();
        });
        cleanupListEl.appendChild(row);
      }
    }

    function openComboList() {
      renderComboList();
      cleanupListEl.classList.add("open");
      cleanupToggleEl.setAttribute("aria-expanded", "true");
    }

    function closeComboList() {
      cleanupListEl.classList.remove("open");
      cleanupToggleEl.setAttribute("aria-expanded", "false");
    }

    // Set the hint via safe DOM nodes (no innerHTML).
    function setCleanupHint(lm) {
      cleanupHintEl.replaceChildren();
      if (lm) {
        cleanupHintEl.textContent =
          "Pick a model loaded in LM Studio, or leave blank to use the first available. The server must be running with a model loaded.";
      } else {
        cleanupHintEl.append(
          "Type any Ollama model, or open the list to pick from installed ones. "
        );
        const b = document.createElement("b");
        b.textContent = "gemma3:1b";
        cleanupHintEl.append(b, " is always available as the DictaFlow default.");
      }
    }

    // Apply backend-specific labels/placeholders and show the URL field for LM Studio.
    function refreshCleanupFields() {
      const lm = cleanupIsLmStudio();
      lmstudioUrlFieldEl.style.display = lm ? "block" : "none";
      cleanupModelLabelEl.textContent = lm ? "LM Studio model" : "Model name";
      cleanupModelEl.placeholder = lm ? "(first available)" : "gemma3:1b";
      setCleanupHint(lm);
    }

    async function loadCleanupModels() {
      cleanupModelEl.disabled = true;
      cleanupSaveBtn.disabled = true;
      cleanupModelEl.placeholder = "Loading…";
      setCleanupMessage("");
      const lm = cleanupIsLmStudio();
      const defaultItems = lm
        ? []
        : [{ value: "gemma3:1b", meta: "DictaFlow default" }];
      let current = "";
      try {
        const [cfg, models] = await Promise.all([
          invoke("get_cleanup_config"),
          invoke("list_cleanup_models"),
        ]);
        current = cfg.model || "";
        const names = new Set(defaultItems.map((i) => i.value));
        comboItems = defaultItems.slice();
        for (const model of models) {
          if (names.has(model.name)) continue;
          names.add(model.name);
          comboItems.push({ value: model.name, meta: lm ? "" : formatBytes(model.size) });
        }
        if (current && !names.has(current)) {
          setCleanupMessage(`Current: ${current} (manual model)`, "");
        } else if (!models.length) {
          setCleanupMessage(
            lm
              ? "No models reported. Load a model in LM Studio, then Refresh."
              : "Using DictaFlow default. You can also type any Ollama model name manually.",
            lm ? "" : "ok"
          );
        } else {
          setCleanupMessage(current ? `Current: ${current}` : "Using first available.", "ok");
        }
      } catch (err) {
        current = await invoke("get_cleanup_config").then((c) => c.model || "").catch(() => "");
        comboItems = defaultItems.slice();
        setCleanupMessage(`${String(err)}. You can still type and save a model name manually.`, "err");
      }
      currentCleanupModel = current;
      cleanupModelEl.value = current;
      cleanupModelEl.placeholder = lm ? "(first available)" : "gemma3:1b";
      cleanupModelEl.disabled = false;
      cleanupSaveBtn.disabled = true;
      renderComboList();
    }

    async function loadCleanupConfig() {
      try {
        const cfg = await invoke("get_cleanup_config");
        cleanupBackend = cfg.backend || "ollama";
        cleanupBackendEl.value = cleanupBackend;
        lmstudioBaseUrlEl.value = cfg.base_url || "";
      } catch (err) {
        cleanupBackendEl.value = "ollama";
      }
      refreshCleanupFields();
      await loadCleanupModels();
    }
    loadCleanupConfig();
```

- [ ] **Step 5: Replace the cleanup event handlers**

In `dist/settings.js`, replace the handler block from `function onOllamaInput()` through `ollamaRefreshBtn.addEventListener("click", loadOllamaModels);` (lines 381-421) with:

```javascript
    function onCleanupModelInput() {
      const value = cleanupModelEl.value.trim();
      // LM Studio allows an empty model (= first available); Ollama requires one.
      const sameAsCurrent = value === currentCleanupModel;
      cleanupSaveBtn.disabled = sameAsCurrent || (!cleanupIsLmStudio() && !value);
      setCleanupMessage(
        sameAsCurrent && value ? `Current: ${currentCleanupModel}` : "",
        sameAsCurrent && value ? "ok" : ""
      );
    }
    cleanupModelEl.addEventListener("input", onCleanupModelInput);
    cleanupModelEl.addEventListener("focus", openComboList);
    cleanupToggleEl.addEventListener("click", (e) => {
      e.preventDefault();
      if (cleanupListEl.classList.contains("open")) {
        closeComboList();
      } else {
        openComboList();
        cleanupModelEl.focus();
      }
    });
    document.addEventListener("click", (e) => {
      if (!e.target.closest(".combo-wrap")) closeComboList();
    });
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") closeComboList();
    });

    // Switching engine persists immediately and reloads that backend's models.
    cleanupBackendEl.addEventListener("change", async () => {
      setCleanupMessage("Switching…");
      try {
        const cfg = await invoke("set_cleanup_backend", {
          backend: cleanupBackendEl.value,
          baseUrl: cleanupIsLmStudio() ? lmstudioBaseUrlEl.value.trim() : null,
        });
        cleanupBackend = cfg.backend;
        cleanupBackendEl.value = cfg.backend;
        lmstudioBaseUrlEl.value = cfg.base_url || "";
        refreshCleanupFields();
        await loadCleanupModels();
      } catch (err) {
        setCleanupMessage(String(err), "err");
      }
    });

    // Applying a new LM Studio URL re-points the backend, then reloads models.
    lmstudioUrlSaveBtn.addEventListener("click", async () => {
      setCleanupMessage("Applying…");
      try {
        const cfg = await invoke("set_cleanup_backend", {
          backend: "lmstudio",
          baseUrl: lmstudioBaseUrlEl.value.trim(),
        });
        lmstudioBaseUrlEl.value = cfg.base_url || "";
        await loadCleanupModels();
      } catch (err) {
        setCleanupMessage(String(err), "err");
      }
    });

    cleanupSaveBtn.addEventListener("click", async () => {
      cleanupSaveBtn.disabled = true;
      setCleanupMessage("");
      try {
        const saved = await invoke("set_cleanup_model", { model: cleanupModelEl.value.trim() });
        currentCleanupModel = saved;
        cleanupModelEl.value = saved;
        setCleanupMessage(saved ? `Saved: ${saved}` : "Saved: (first available)", "ok");
      } catch (err) {
        setCleanupMessage(String(err), "err");
        cleanupSaveBtn.disabled = false;
      }
    });

    cleanupRefreshBtn.addEventListener("click", loadCleanupModels);
```

- [ ] **Step 6: Verify no stale `ollama` references remain in the JS**

Run:
```bash
grep -n "ollama\|Ollama" dist/settings.js
```
Expected: no matches. If any remain (e.g. a leftover call), update them to the `cleanup*` equivalents.

- [ ] **Step 7: Manually verify the UI in the running app**

Build and launch the app (this is a Tauri app — use the project's documented run command, e.g. `npm run tauri dev`), open Settings → Cleanup, and confirm:
- Engine selector shows Ollama (default) and LM Studio.
- Selecting LM Studio reveals the server-URL field (default `http://localhost:1234/v1`) and the model label/hint change.
- The model picker populates from `list_cleanup_models` for the active backend, and shows the "couldn't reach LM Studio at <url>" message when the server is down.
- Switching back to Ollama hides the URL field and restores the Ollama model list.

Report what you observed. If you cannot run the GUI in this environment, say so explicitly rather than claiming it works.

- [ ] **Step 8: Commit**

```bash
git add dist/settings.html dist/settings.js
git commit -m "feat(settings-ui): add cleanup engine selector and LM Studio URL field

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Document the new backend in `AGENTS.md`

**Files:**
- Modify: `AGENTS.md` (cleanup pipeline line ~19-20, module map ~53-56, commands ~75, settings example ~92)

- [ ] **Step 1: Update the cleanup pipeline description**

In `AGENTS.md`, the cleanup step description (lines 19-20) currently reads:

```
4. Se limpia el texto: primero reglas (`cleanup/rules.rs`), luego un modelo **Ollama** local
   (default `gemma3:1b`).
```

Replace with:

```
4. Se limpia el texto: primero reglas (`cleanup/rules.rs`), luego un LLM local de limpieza —
   **Ollama** (default, `gemma3:1b`) o **LM Studio**, seleccionable en Settings.
```

- [ ] **Step 2: Update the module map**

In `AGENTS.md`, the `cleanup/` module map (lines 53-56) currently lists `mod.rs`, `ollama.rs` (and `rules.rs`). Update those lines to:

```
    cleanup/
      mod.rs             Hybrid cleaner (reglas + LLM), trait Cleaner, set_backend
      prompt.rs          Prompt/strip compartido entre backends
      ollama.rs          Cliente Ollama: /api/generate y /api/tags
      lmstudio.rs        Cliente LM Studio: /v1/chat/completions y /v1/models
```

(Preserve the existing `rules.rs` line and surrounding indentation/format.)

- [ ] **Step 3: Update the commands list**

In `AGENTS.md`, the cleanup commands line (line 75) currently reads:

```
- Cleanup (Ollama): `get_ollama_model`, `list_ollama_models`, `set_ollama_model`
```

Replace with:

```
- Cleanup: `get_cleanup_config`, `list_cleanup_models`, `set_cleanup_model`, `set_cleanup_backend`
```

- [ ] **Step 4: Update the settings example**

In `AGENTS.md`, the settings JSON example near line 92 includes `"ollama_model": "gemma3:1b",`. Add the new fields directly after it:

```
  "ollama_model": "gemma3:1b",
  "cleanup_backend": "ollama",
  "lmstudio_model": null,
  "lmstudio_base_url": "http://localhost:1234/v1",
```

- [ ] **Step 5: Commit**

```bash
git add AGENTS.md
git commit -m "docs: document LM Studio cleanup backend and settings

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Final verification

- [ ] Run the full Rust gate once more from a clean state:
```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```
Expected: all three green.

- [ ] Confirm no stale references remain across the whole tree:
```bash
grep -rn "OllamaModel\|get_ollama_model\|set_ollama_model\|list_ollama_models" src-tauri dist
```
Expected: no matches.
