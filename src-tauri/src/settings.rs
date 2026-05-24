use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
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
            code: "Space".to_string(),
            modifiers: vec!["Alt".to_string()],
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
    /// HUD appearance: "classic" (bottom pill + waveform), "notch" (top bar), or "cursor" (follows mouse).
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
    /// Legacy field — migrated to Keychain on macOS; never written on save.
    #[serde(default, skip_serializing)]
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
        read_settings_file(&path).unwrap_or_default()
    }

    /// Persist settings under an exclusive lock (all writes must go through here).
    pub fn save(&self) -> Result<()> {
        let mut replacement = self.clone();
        replacement.transcription_api_key = None;
        Self::update(|saved| *saved = replacement)
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

        let mut settings = read_settings_file(&path).unwrap_or_default();
        patch(&mut settings);
        settings.transcription_api_key = None;
        let json = serde_json::to_string_pretty(&settings)?;
        write_atomic(&path, &json)?;
        tracing::info!("settings updated at {}", path.display());
        Ok(())
    }
}

fn read_settings_file(path: &Path) -> Result<Settings> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(e.into()),
    };

    match serde_json::from_str::<Settings>(&raw) {
        Ok(settings) => Ok(settings),
        Err(e) => {
            backup_corrupt_file(path, "settings");
            tracing::warn!(
                "settings parse failed at {}: {:?}; backed up and using defaults",
                path.display(),
                e
            );
            Ok(Settings::default())
        }
    }
}

fn backup_corrupt_file(path: &Path, label: &str) {
    if !path.exists() {
        return;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_extension(format!("{label}.broken-{stamp}.json"));
    match fs::rename(path, &backup) {
        Ok(()) => tracing::warn!("backed up corrupt file to {}", backup.display()),
        Err(e) => tracing::warn!("could not back up corrupt {}: {:?}", path.display(), e),
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

fn settings_path() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("DictaFlow");
    fs::create_dir_all(&dir).ok();
    Some(dir.join("settings.json"))
}

/// Read a legacy plaintext API key still present in settings.json (pre-Keychain).
#[cfg(not(target_os = "macos"))]
pub fn legacy_transcription_api_key() -> Option<String> {
    let path = settings_path()?;
    let settings = read_settings_file(&path).ok()?;
    settings
        .transcription_api_key
        .filter(|k| !k.trim().is_empty())
}

/// Remove and return a legacy plaintext API key from settings.json.
pub fn take_legacy_transcription_api_key() -> Option<String> {
    let mut taken = None;
    let _ = Settings::update(|saved| {
        if saved.transcription_api_key.is_some() {
            taken = saved.transcription_api_key.take();
        }
    });
    taken.filter(|k| !k.trim().is_empty())
}

#[cfg(not(target_os = "macos"))]
pub fn set_legacy_transcription_api_key(key: &str) -> Result<()> {
    Settings::update(|saved| {
        saved.transcription_api_key = if key.is_empty() {
            None
        } else {
            Some(key.to_string())
        };
    })
}
