use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
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
    /// Cleanup backend: "ollama" (default) or "lmstudio".
    #[serde(default)]
    pub cleanup_backend: Option<String>,
    /// LM Studio model id (None = first available).
    #[serde(default)]
    pub lmstudio_model: Option<String>,
    /// LM Studio server base URL (port varies); None = the default shown in the UI.
    #[serde(default)]
    pub lmstudio_base_url: Option<String>,
    /// Transcription backend: "local" (offline Whisper, default), "groq", or "openai".
    #[serde(default)]
    pub transcription_backend: Option<String>,
    /// Legacy field — migrated to Keychain on macOS; never written on save.
    #[cfg_attr(target_os = "macos", serde(default, skip_serializing))]
    #[cfg_attr(not(target_os = "macos"), serde(default))]
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

/// In-memory source of truth for settings reads. The app is single-instance (see the
/// single-instance lock in main), and every write goes through `Settings::update`, so the
/// cache stays coherent with disk while avoiding a JSON file read on every hot-path access
/// (language mode, live-transcript, auto-submit are read per dictation).
static CACHE: Lazy<RwLock<Settings>> = Lazy::new(|| RwLock::new(load_from_disk()));

fn load_from_disk() -> Settings {
    match settings_path() {
        Some(path) => read_settings_file(&path).unwrap_or_default(),
        None => Settings::default(),
    }
}

impl Settings {
    /// Cheap clone of the cached settings (no disk read).
    pub fn load() -> Self {
        CACHE.read().clone()
    }

    /// Reload settings under an exclusive lock, apply `patch`, write atomically, and refresh
    /// the in-memory cache.
    pub fn update<F>(patch: F) -> Result<()>
    where
        F: FnOnce(&mut Self),
    {
        let path = settings_path().ok_or_else(|| anyhow!("no settings path"))?;
        crate::fs_util::with_file_lock(&path, "settings.lock", || {
            let mut settings = read_settings_file(&path).unwrap_or_default();
            patch(&mut settings);
            if cfg!(target_os = "macos") {
                settings.transcription_api_key = None;
            }
            let json = serde_json::to_string_pretty(&settings)?;
            crate::fs_util::write_atomic(&path, &json)?;
            *CACHE.write() = settings;
            tracing::info!("settings updated at {}", path.display());
            Ok(())
        })
    }

    /// Cleanup backend name as saved ("ollama" when unset; not yet normalized).
    pub fn cleanup_backend_or_default(&self) -> String {
        self.cleanup_backend
            .clone()
            .unwrap_or_else(|| "ollama".to_string())
    }

    pub fn ollama_model_or_default(&self) -> String {
        self.ollama_model
            .clone()
            .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string())
    }

    pub fn lmstudio_base_url_or_default(&self) -> String {
        self.lmstudio_base_url
            .clone()
            .unwrap_or_else(|| crate::cleanup::lmstudio::DEFAULT_BASE_URL.to_string())
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
            crate::fs_util::backup_corrupt_file(path, "settings");
            tracing::warn!(
                "settings parse failed at {}: {:?}; backed up and using defaults",
                path.display(),
                e
            );
            Ok(Settings::default())
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(crate::fs_util::app_dir()?.join("settings.json"))
}

/// Read a plaintext API key from settings.json (macOS: legacy, pre-Keychain;
/// other platforms: where the key lives).
pub fn legacy_transcription_api_key() -> Option<String> {
    let path = settings_path()?;
    let settings = read_settings_file(&path).ok()?;
    settings
        .transcription_api_key
        .filter(|k| !k.trim().is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_mode_parses_known_codes() {
        assert_eq!(LanguageMode::parse("en"), LanguageMode::English);
        assert_eq!(LanguageMode::parse("ENGLISH"), LanguageMode::English);
        assert_eq!(LanguageMode::parse("es"), LanguageMode::Spanish);
        assert_eq!(LanguageMode::parse("spanish"), LanguageMode::Spanish);
        assert_eq!(
            LanguageMode::parse("translate_en"),
            LanguageMode::TranslateEnglish
        );
        assert_eq!(
            LanguageMode::parse("translate"),
            LanguageMode::TranslateEnglish
        );
        assert_eq!(LanguageMode::parse("auto"), LanguageMode::Auto);
        assert_eq!(LanguageMode::parse("whatever"), LanguageMode::Auto);
        assert_eq!(LanguageMode::parse(""), LanguageMode::Auto);
    }

    #[test]
    fn language_mode_roundtrips_through_as_str() {
        for mode in [
            LanguageMode::Auto,
            LanguageMode::English,
            LanguageMode::Spanish,
            LanguageMode::TranslateEnglish,
        ] {
            assert_eq!(LanguageMode::parse(mode.as_str()), mode);
        }
    }

    #[test]
    fn translate_english_has_no_source_language_but_sets_translate() {
        let m = LanguageMode::TranslateEnglish;
        assert_eq!(m.whisper_language(), None);
        assert!(m.translate_to_english());
    }

    #[test]
    fn forced_languages_map_to_codes_and_dont_translate() {
        assert_eq!(LanguageMode::English.whisper_language(), Some("en"));
        assert_eq!(LanguageMode::Spanish.whisper_language(), Some("es"));
        assert!(!LanguageMode::English.translate_to_english());
        assert!(!LanguageMode::Auto.translate_to_english());
        assert_eq!(LanguageMode::Auto.whisper_language(), None);
    }

    #[test]
    fn hotkey_display_joins_modifiers_and_code() {
        let hk = Hotkey {
            code: "Space".into(),
            modifiers: vec!["Alt".into()],
        };
        assert_eq!(hk.display(), "Alt + Space");
        let plain = Hotkey {
            code: "F9".into(),
            modifiers: vec![],
        };
        assert_eq!(plain.display(), "F9");
    }
}
