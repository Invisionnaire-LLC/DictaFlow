use std::time::Duration;

use anyhow::Result;
use parking_lot::RwLock;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use super::prompt::{build_prompt, num_predict_cap};
use super::{finish_output, get_json, http_client, post_json, sorted_models, Cleaner, ModelInfo};

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
        OllamaCleaner {
            client: http_client(timeout),
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
        let tags: TagsResponse = get_json(&self.client, TAGS_ENDPOINT, "ollama tags")?;
        Ok(sorted_models(tags.models.into_iter().map(|m| ModelInfo {
            name: m.name,
            size: m.size.unwrap_or_default(),
        })))
    }

    fn request(&self, raw_text: &str, timeout: Option<Duration>) -> Result<String> {
        let model = self.model();
        let body = GenerateRequest {
            model: &model,
            prompt: build_prompt(raw_text),
            stream: false,
            // Keep the model resident so later dictations skip the load.
            keep_alive: -1,
            options: GenerateOptions {
                temperature: 0.0,
                top_k: 1,
                num_predict: num_predict_cap(raw_text),
            },
        };
        let decoded: GenerateResponse =
            post_json(&self.client, ENDPOINT, &body, timeout, "ollama")?;
        finish_output("ollama", &decoded.response)
    }
}
