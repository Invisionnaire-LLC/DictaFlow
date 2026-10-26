//! Secure storage for cloud transcription API keys (Keychain on macOS).

use anyhow::Result;

#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "com.invisionnaire.dictaflow";
#[cfg(target_os = "macos")]
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
    use anyhow::Context;
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

/// Move a plaintext key from settings.json into the Keychain (one-time migration).
/// The key is cleared from settings.json only after the Keychain write succeeds.
#[cfg(target_os = "macos")]
pub fn migrate_transcription_api_key_from_settings() {
    let Some(legacy) = crate::settings::legacy_transcription_api_key() else {
        return;
    };
    if let Err(e) = set_transcription_api_key(&legacy) {
        tracing::warn!("could not migrate API key to secure storage: {:?}", e);
        return;
    }
    // `Settings::update` never writes the legacy field on macOS, so a no-op patch drops it.
    match crate::settings::Settings::update(|_| {}) {
        Ok(()) => tracing::info!("migrated transcription API key from settings.json to Keychain"),
        Err(e) => tracing::warn!(
            "API key copied to Keychain but settings.json not cleared: {:?}",
            e
        ),
    }
}

/// Off macOS the key stays in settings.json, so there is nothing to migrate.
#[cfg(not(target_os = "macos"))]
pub fn migrate_transcription_api_key_from_settings() {}
