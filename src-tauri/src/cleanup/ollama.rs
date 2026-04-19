use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

pub const MODEL_NAME: &str = "gemma3:1b";
pub const ENDPOINT: &str = "http://127.0.0.1:11434/api/generate";

const COLON_TAIL_PREAMBLES: &[&str] = &[
    "Here is the",
    "Here's the",
    "Here is",
    "Here's",
    "Sure,",
];
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

pub struct OllamaCleaner {
    client: Client,
}

impl OllamaCleaner {
    pub fn new(timeout: Duration) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client");
        OllamaCleaner { client }
    }

    pub fn clean(&self, raw_text: &str) -> Result<String> {
        let body = GenerateRequest {
            model: MODEL_NAME,
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
