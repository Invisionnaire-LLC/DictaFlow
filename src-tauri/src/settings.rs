use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub const DEFAULT_OLLAMA_MODEL: &str = "gemma3:1b";
pub const DEFAULT_LANGUAGE_MODE: &str = "auto";

/// How transcription handles spoken language. Applies to both local Whisper and
/// the cloud backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageMode {
    /// Auto-detect and keep the spoken language ("Normal").
    Auto,
    /// Force English transcription.
    English,
    /// Force Spanish transcription.
    Spanish,
    /// Translate any spoken language into English text.
    TranslateEnglish,
}

impl LanguageMode {
    pub fn parse(s: &str) -> LanguageMode {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => LanguageMode::English,
            "es" | "spanish" => LanguageMode::Spanish,
            "translate_en" | "translate" => LanguageMode::TranslateEnglish,
            _ => LanguageMode::Auto,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LanguageMode::Auto => "auto",
            LanguageMode::English => "en",
            LanguageMode::Spanish => "es",
            LanguageMode::TranslateEnglish => "translate_en",
        }
    }

    /// Whisper source language code; `None` means auto-detect.
    pub fn whisper_language(&self) -> Option<&'static str> {
        match self {
            LanguageMode::English => Some("en"),
            LanguageMode::Spanish => Some("es"),
            LanguageMode::Auto | LanguageMode::TranslateEnglish => None,
        }
    }

    pub fn translate_to_english(&self) -> bool {
        matches!(self, LanguageMode::TranslateEnglish)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotkey {
    pub code: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self {
            code: "F9".to_string(),
            modifiers: Vec::new(),
        }
    }
}

impl Hotkey {
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = self.modifiers.iter().map(|s| s.as_str()).collect();
        parts.push(&self.code);
        parts.join(" + ")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HudPosition {
    pub x: i32,
    pub y: i32,
}

/// Physical screen coords for the HUD window's top-right corner (stable across resizes).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HudAnchor {
    pub right: i32,
    pub top: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    pub input_device: Option<String>,
    #[serde(default)]
    pub hotkey: Option<Hotkey>,
    #[serde(default)]
    pub hud_position: Option<HudPosition>,
    /// HUD appearance: "classic" (bottom pill + waveform) or "notch" (top minimal bar).
    #[serde(default)]
    pub hud_style: Option<String>,
    /// Show Whisper partial text in the HUD while recording.
    #[serde(default)]
    pub live_transcript: Option<bool>,
    /// Keep the HUD pill above other windows (when false, it only floats up while dictating).
    #[serde(default)]
    pub hud_always_on_top: Option<bool>,
    #[serde(default)]
    pub hud_anchor: Option<HudAnchor>,
    #[serde(default)]
    pub ollama_model: Option<String>,
    /// Transcription backend: "local" (offline Whisper, default), "groq", or "openai".
    #[serde(default)]
    pub transcription_backend: Option<String>,
    /// API key for the selected cloud transcription backend. Stored locally in plaintext.
    #[serde(default)]
    pub transcription_api_key: Option<String>,
    /// Optional model override for the cloud backend (provider default used when empty).
    #[serde(default)]
    pub transcription_model: Option<String>,
    /// Language handling: "auto" (default), "en", "es", or "translate_en".
    #[serde(default)]
    pub transcription_language: Option<String>,
    /// Press Enter automatically after pasting the dictated text.
    #[serde(default)]
    pub auto_submit: bool,
    #[serde(default)]
    pub launch_at_login: bool,
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("settings parse failed, using defaults: {:?}", e);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path().ok_or_else(|| anyhow!("no settings path"))?;
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json)?;
        tracing::info!("settings saved to {}", path.display());
        Ok(())
    }

    /// Reload settings under an exclusive lock, apply `patch`, then write atomically.
    pub fn update<F>(patch: F) -> Result<()>
    where
        F: FnOnce(&mut Self),
    {
        let path = settings_path().ok_or_else(|| anyhow!("no settings path"))?;
        let dir = path
            .parent()
            .ok_or_else(|| anyhow!("settings path has no parent"))?;
        fs::create_dir_all(dir)?;

        let lock_path = dir.join("settings.lock");
        let lock_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock_file
            .lock_exclusive()
            .map_err(|e| anyhow!("settings lock: {}", e))?;

        let mut settings = match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                tracing::warn!("settings parse failed during update: {:?}", e);
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        patch(&mut settings);
        let json = serde_json::to_string_pretty(&settings)?;
        fs::write(&path, json)?;
        tracing::info!("settings updated at {}", path.display());
        Ok(())
    }
}

fn settings_path() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("DictaFlow");
    fs::create_dir_all(&dir).ok();
    Some(dir.join("settings.json"))
}
