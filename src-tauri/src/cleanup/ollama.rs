use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use parking_lot::RwLock;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use super::prompt::{build_prompt, num_predict_cap, strip_preamble};

pub const ENDPOINT: &str = "http://127.0.0.1:11434/api/generate";
pub const TAGS_ENDPOINT: &str = "http://127.0.0.1:11434/api/tags";

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: String,
    stream: bool,
    keep_alive: i32,
    options: GenerateOptions,
}

#[derive(Serialize)]
struct GenerateOptions {
    temperature: f32,
    top_k: i32,
    num_predict: i32,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OllamaModel {
    pub name: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    size: Option<u64>,
}

pub struct OllamaCleaner {
    client: Client,
    model: RwLock<String>,
}

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

    pub fn model(&self) -> String {
        self.model.read().clone()
    }

    pub fn set_model(&self, model: String) {
        *self.model.write() = model;
    }

    pub fn list_models(&self) -> Result<Vec<OllamaModel>> {
        list_models_with_client(&self.client)
    }

    pub fn clean(&self, raw_text: &str) -> Result<String> {
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

    /// Warmup — fire a cheap request to pin the model in RAM.
    pub fn warmup(&self) -> Result<()> {
        let _ = self.clean("hello")?;
        Ok(())
    }
}

fn list_models_with_client(client: &Client) -> Result<Vec<OllamaModel>> {
    let resp = client.get(TAGS_ENDPOINT).send().context("ollama tags")?;
    if !resp.status().is_success() {
        return Err(anyhow!("ollama tags http status {}", resp.status()));
    }
    let decoded: TagsResponse = resp.json().context("ollama tags decode")?;
    let mut models: Vec<OllamaModel> = decoded
        .models
        .into_iter()
        .map(|m| OllamaModel {
            name: m.name,
            size: m.size.unwrap_or_default(),
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}
