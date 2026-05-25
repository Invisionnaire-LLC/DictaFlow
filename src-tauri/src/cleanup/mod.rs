pub mod lmstudio;
pub mod ollama;
pub mod prompt;
pub mod rules;

use std::time::Duration;

use anyhow::Result;
use parking_lot::RwLock;
use serde::Serialize;

use self::lmstudio::{LmStudioCleaner, DEFAULT_BASE_URL as LMSTUDIO_DEFAULT_BASE_URL};
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

fn normalize_for_compare(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn should_skip_llm(raw: &str, rules_output: &str) -> bool {
    let raw_n = normalize_for_compare(raw);
    let rules_n = normalize_for_compare(rules_output);
    if raw_n == rules_n {
        return true;
    }
    // Very short utterances rarely benefit from an LLM pass.
    rules_n.split_whitespace().count() <= 2
}
