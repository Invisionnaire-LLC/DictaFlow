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
    pub fn new(llm_timeout: Duration) -> Self {
        HybridCleaner {
            ollama: OllamaCleaner::new(llm_timeout),
        }
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
