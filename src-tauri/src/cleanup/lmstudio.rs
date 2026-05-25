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
        let json =
            r#"{"choices":[{"message":{"role":"assistant","content":"Output: Hello world."}}]}"#;
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
