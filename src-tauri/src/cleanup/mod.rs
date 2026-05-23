pub mod ollama;
pub mod rules;

use std::time::Duration;

use self::ollama::OllamaCleaner;

/// Composes deterministic rules with an LLM pass. If the LLM fails or times
/// out, returns the rules-cleaned output. User always gets something back.
pub struct HybridCleaner {
    ollama: OllamaCleaner,
}

impl HybridCleaner {
    pub fn new(llm_timeout: Duration, model: String) -> Self {
        HybridCleaner {
            ollama: OllamaCleaner::new(llm_timeout, model),
        }
    }

    pub fn ollama_model(&self) -> String {
        self.ollama.model()
    }

    pub fn set_ollama_model(&self, model: String) {
        self.ollama.set_model(model);
    }

    pub fn list_ollama_models(&self) -> anyhow::Result<Vec<ollama::OllamaModel>> {
        self.ollama.list_models()
    }

    pub fn clean(&self, raw: &str) -> String {
        let rules_output = rules::apply(raw);
        match self.ollama.clean(&rules_output) {
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
        if let Err(e) = self.ollama.warmup() {
            tracing::warn!("ollama warmup skipped: {:?}", e);
        }
    }
}
