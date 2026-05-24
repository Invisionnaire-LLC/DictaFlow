//! Secure storage for cloud transcription API keys (Keychain on macOS).

use anyhow::{Context, Result};

const KEYCHAIN_SERVICE: &str = "com.invisionnaire.dictaflow";
const KEYCHAIN_ACCOUNT: &str = "transcription-api-key";

#[cfg(target_os = "macos")]
pub fn get_transcription_api_key() -> String {
    use security_framework::passwords::get_generic_password;

    match get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
        Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

#[cfg(target_os = "macos")]
pub fn set_transcription_api_key(key: &str) -> Result<()> {
    use security_framework::passwords::{delete_generic_password, set_generic_password};

    let key = key.trim();
    if key.is_empty() {
        let _ = delete_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
        return Ok(());
    }
    set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, key.as_bytes())
        .context("store transcription API key in Keychain")?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn get_transcription_api_key() -> String {
    crate::settings::legacy_transcription_api_key().unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
pub fn set_transcription_api_key(key: &str) -> Result<()> {
    crate::settings::set_legacy_transcription_api_key(key.trim())
}

/// Move a plaintext key from settings.json into secure storage (one-time migration).
pub fn migrate_transcription_api_key_from_settings() {
    let Some(legacy) = crate::settings::take_legacy_transcription_api_key() else {
        return;
    };
    if legacy.is_empty() {
        return;
    }
    if let Err(e) = set_transcription_api_key(&legacy) {
        tracing::warn!("could not migrate API key to secure storage: {:?}", e);
        let _ = crate::settings::Settings::update(|saved| {
            saved.transcription_api_key = Some(legacy);
        });
    } else {
        tracing::info!("migrated transcription API key from settings.json to secure storage");
    }
}
