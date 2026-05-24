//! Optional cloud transcription backends (Groq / OpenAI).
//!
//! Offline local Whisper is the default and stays the privacy-preserving path.
//! When a cloud backend is selected the captured PCM is encoded to an in-memory
//! WAV and POSTed to an OpenAI-compatible `audio/transcriptions` endpoint.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{multipart, Client};
use serde::Deserialize;

use crate::settings::LanguageMode;

const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/audio/transcriptions";
const GROQ_TRANSLATE_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/translations";
const OPENAI_TRANSLATE_ENDPOINT: &str = "https://api.openai.com/v1/audio/translations";

const GROQ_DEFAULT_MODEL: &str = "whisper-large-v3";
const OPENAI_DEFAULT_MODEL: &str = "gpt-4o-transcribe";

/// Cloud transcription APIs cap the bias `prompt`. Groq rejects prompts over 896
/// **bytes** with HTTP 400 (accented chars count as 2+ bytes); OpenAI truncates to
/// its token budget. We trim by byte length (at a comma boundary) with a safety
/// margin so a long custom vocabulary never errors.
const MAX_PROMPT_BYTES: usize = 870;

/// Where transcription runs. `Local` keeps everything offline (default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Local,
    Groq,
    OpenAI,
}

impl Backend {
    pub fn parse(s: &str) -> Backend {
        match s.trim().to_ascii_lowercase().as_str() {
            "groq" => Backend::Groq,
            "openai" => Backend::OpenAI,
            _ => Backend::Local,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Backend::Local => "local",
            Backend::Groq => "groq",
            Backend::OpenAI => "openai",
        }
    }

    pub fn is_cloud(&self) -> bool {
        !matches!(self, Backend::Local)
    }

    fn endpoint(&self, translate: bool) -> Option<&'static str> {
        match (self, translate) {
            (Backend::Local, _) => None,
            (Backend::Groq, false) => Some(GROQ_ENDPOINT),
            (Backend::Groq, true) => Some(GROQ_TRANSLATE_ENDPOINT),
            (Backend::OpenAI, false) => Some(OPENAI_ENDPOINT),
            (Backend::OpenAI, true) => Some(OPENAI_TRANSLATE_ENDPOINT),
        }
    }

    fn default_model(&self) -> &'static str {
        match self {
            Backend::Local => "",
            Backend::Groq => GROQ_DEFAULT_MODEL,
            Backend::OpenAI => OPENAI_DEFAULT_MODEL,
        }
    }
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// Holds the selected cloud backend, credentials, and a reusable HTTP client.
pub struct CloudTranscriber {
    client: Client,
    backend: Backend,
    api_key: String,
    /// User override; empty means use the provider default.
    model: String,
}

impl CloudTranscriber {
    pub fn new(backend: Backend, api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        CloudTranscriber {
            client,
            backend,
            api_key: api_key.trim().to_string(),
            model: model.trim().to_string(),
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn is_cloud(&self) -> bool {
        self.backend.is_cloud()
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// The model that will actually be sent (resolved against the provider default).
    pub fn resolved_model(&self) -> String {
        if self.model.is_empty() {
            self.backend.default_model().to_string()
        } else {
            self.model.clone()
        }
    }

    /// The raw user override (may be empty).
    pub fn model_override(&self) -> &str {
        &self.model
    }

    pub fn transcribe(
        &self,
        samples: &[f32],
        sample_rate: u32,
        lang: LanguageMode,
        prompt: &str,
    ) -> Result<String> {
        let translate = lang.translate_to_english();
        let endpoint = self
            .backend
            .endpoint(translate)
            .ok_or_else(|| anyhow!("local backend has no cloud endpoint"))?;
        if self.api_key.is_empty() {
            return Err(anyhow!(
                "missing API key for {} backend",
                self.backend.as_str()
            ));
        }

        let wav = encode_wav(samples, sample_rate).context("encode wav for upload")?;
        let model = self.resolved_model();

        let part = multipart::Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .context("multipart part")?;
        let mut form = multipart::Form::new()
            .part("file", part)
            .text("model", model.clone())
            .text("response_format", "json")
            .text("temperature", "0");
        // The translations endpoint always targets English, so no language param there.
        if !translate {
            if let Some(code) = lang.whisper_language() {
                form = form.text("language", code);
            }
        }
        let prompt = cap_prompt(prompt.trim());
        if !prompt.is_empty() {
            form = form.text("prompt", prompt);
        }

        let start = std::time::Instant::now();
        let resp = self
            .client
            .post(endpoint)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .with_context(|| format!("{} transcription request", self.backend.as_str()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            let snippet = body.chars().take(300).collect::<String>();
            return Err(anyhow!(
                "{} transcription http {}: {}",
                self.backend.as_str(),
                status,
                snippet
            ));
        }

        let decoded: TranscriptionResponse = resp.json().context("transcription decode")?;
        let text = decoded.text.trim().to_string();
        tracing::info!(
            "cloud transcription ({}/{}) {} chars in {}ms",
            self.backend.as_str(),
            model,
            text.len(),
            start.elapsed().as_millis()
        );
        Ok(text)
    }
}

/// Trim the bias prompt to `MAX_PROMPT_BYTES`, cutting at the last comma. Works in
/// byte units (the API limits bytes), but only ever cuts on UTF-8 char boundaries
/// so it never panics or splits a multi-byte char.
fn cap_prompt(prompt: &str) -> String {
    if prompt.len() <= MAX_PROMPT_BYTES {
        return prompt.to_string();
    }
    // Largest byte index <= budget that lands on a char boundary.
    let mut end = 0;
    for (i, c) in prompt.char_indices() {
        if i + c.len_utf8() > MAX_PROMPT_BYTES {
            break;
        }
        end = i + c.len_utf8();
    }
    let truncated = &prompt[..end];
    let trimmed = match truncated.rfind(',') {
        Some(i) => &truncated[..i],
        None => truncated,
    };
    let trimmed = trimmed.trim();
    tracing::debug!(
        "cloud prompt capped from {} to {} bytes",
        prompt.len(),
        trimmed.len()
    );
    trimmed.to_string()
}

/// Encode mono f32 PCM into a 16-bit WAV in memory.
fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).context("wav writer")?;
        for s in samples {
            let clamped = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer.write_sample(clamped)?;
        }
        writer.finalize().context("wav finalize")?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_backend_is_case_insensitive_and_defaults_local() {
        assert_eq!(Backend::parse("Groq"), Backend::Groq);
        assert_eq!(Backend::parse("OPENAI"), Backend::OpenAI);
        assert_eq!(Backend::parse("local"), Backend::Local);
        assert_eq!(Backend::parse("whatever"), Backend::Local);
        assert_eq!(Backend::parse(""), Backend::Local);
    }

    #[test]
    fn cloud_flag_matches_backend() {
        assert!(!Backend::Local.is_cloud());
        assert!(Backend::Groq.is_cloud());
        assert!(Backend::OpenAI.is_cloud());
    }

    #[test]
    fn resolved_model_falls_back_to_provider_default() {
        let t = CloudTranscriber::new(Backend::Groq, "k".into(), "".into());
        assert_eq!(t.resolved_model(), GROQ_DEFAULT_MODEL);
        let t = CloudTranscriber::new(Backend::OpenAI, "k".into(), "  ".into());
        assert_eq!(t.resolved_model(), OPENAI_DEFAULT_MODEL);
        let t = CloudTranscriber::new(Backend::Groq, "k".into(), "custom-model".into());
        assert_eq!(t.resolved_model(), "custom-model");
    }

    #[test]
    fn cap_prompt_leaves_short_prompts_untouched() {
        assert_eq!(cap_prompt("DictaFlow, José, café"), "DictaFlow, José, café");
    }

    #[test]
    fn cap_prompt_trims_long_prompts_under_byte_limit_at_comma() {
        let word = "palabra, ";
        let long = word.repeat(200); // ~1800 bytes
        let capped = cap_prompt(&long);
        assert!(capped.len() <= MAX_PROMPT_BYTES);
        assert!(!capped.ends_with(','));
        // Trimmed at a comma boundary → ends on a whole word.
        assert!(capped.ends_with("palabra"));
    }

    #[test]
    fn cap_prompt_byte_limit_counts_multibyte_chars() {
        // Accented chars are 2 bytes each — the cap must measure bytes, not chars,
        // because Groq's 896 limit is in bytes.
        let long = "ñoño, ".repeat(300); // multi-byte, ~2100 bytes
        let capped = cap_prompt(&long);
        assert!(capped.len() <= MAX_PROMPT_BYTES);
        // Must remain valid UTF-8 (no split char) — this would panic if not.
        assert!(capped.is_char_boundary(capped.len()));
    }

    #[test]
    fn encode_wav_has_riff_header() {
        let wav = encode_wav(&[0.0, 0.5, -0.5, 1.0], 16_000).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }
}
