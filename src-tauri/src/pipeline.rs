//! Dictation pipeline: hotkey press/release → record → transcribe → cleanup → paste,
//! plus the live partial-transcription preview worker. Extracted from `main.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager};

use crate::audio::{self, PcmBuffer, MIN_DURATION_SECS, TARGET_SAMPLE_RATE};
use crate::state::AppState;
use crate::{
    asr, compact_hud, emit_hud, expand_hud, history, live_transcript_enabled, paste, set_state,
    settings, AppContext,
};

const PARTIAL_MIN_SECS: f32 = 0.45;
const PARTIAL_POLL_MS: u64 = 350;
const PARTIAL_NEW_AUDIO_SECS: f32 = 0.12;
const PARTIAL_PREVIEW_CHARS: usize = 96;
/// Releases shorter than this are treated as tap-to-toggle (e.g. mouse button → Cmd+/).
const HOTKEY_HOLD_RELEASE_MS: u64 = 250;

fn release_should_stop_recording(press_at: Instant, ignore_release: bool) -> bool {
    if ignore_release {
        return false;
    }
    press_at.elapsed() >= Duration::from_millis(HOTKEY_HOLD_RELEASE_MS)
}

pub(crate) fn handle_press(app: AppHandle) {
    let ctx = match app.try_state::<AppContext>() {
        Some(c) => c,
        None => return,
    };

    let state = *ctx.state.lock();
    if state == AppState::Recording {
        *ctx.ignore_hotkey_release.lock() = true;
        finish_recording(app);
        return;
    }
    if state != AppState::Idle {
        tracing::debug!("ignore press while not idle");
        return;
    }

    *ctx.hotkey_press_at.lock() = Some(Instant::now());
    *ctx.ignore_hotkey_release.lock() = false;
    *ctx.paste_target_pid.lock() = paste::capture_frontmost_pid();

    if let Err(e) = ctx.audio.start() {
        tracing::error!("audio start failed: {:?}", e);
        set_state(&app, AppState::Error);
        return;
    }
    set_state(&app, AppState::Recording);
    expand_hud(&app);
    start_partial_transcription(app);
}

pub(crate) fn handle_release(app: AppHandle) {
    let ctx = match app.try_state::<AppContext>() {
        Some(c) => c,
        None => return,
    };
    if *ctx.state.lock() != AppState::Recording {
        tracing::debug!("ignore release while not recording");
        return;
    }

    if *ctx.ignore_hotkey_release.lock() {
        *ctx.ignore_hotkey_release.lock() = false;
        tracing::debug!("ignore release after toggle stop");
        return;
    }

    let press_at = match *ctx.hotkey_press_at.lock() {
        Some(t) => t,
        None => {
            tracing::debug!("ignore release without press timestamp");
            return;
        }
    };

    if !release_should_stop_recording(press_at, false) {
        tracing::debug!(
            "ignore quick release ({:?}ms); tap hotkey again to stop",
            press_at.elapsed().as_millis()
        );
        return;
    }

    finish_recording(app);
}

fn finish_recording(app: AppHandle) {
    let ctx = match app.try_state::<AppContext>() {
        Some(c) => c,
        None => return,
    };
    if *ctx.state.lock() != AppState::Recording {
        tracing::debug!("ignore finish while not recording");
        return;
    }
    stop_partial_transcription(&app);
    set_state(&app, AppState::Processing);

    let app_clone = app.clone();
    thread::spawn(move || {
        let ctx = match app_clone.try_state::<AppContext>() {
            Some(c) => c,
            None => return,
        };

        let pcm = match ctx.audio.stop() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("audio stop: {:?}", e);
                set_state(&app_clone, AppState::Error);
                reset_to_idle_soon(app_clone.clone());
                return;
            }
        };

        if pcm.duration_secs() < MIN_DURATION_SECS {
            tracing::info!("clip too short ({:.2}s); skipping", pcm.duration_secs());
            set_state(&app_clone, AppState::Idle);
            return;
        }

        let text = match transcribe(&ctx, &pcm) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("transcription: {:?}", e);
                set_state(&app_clone, AppState::Error);
                reset_to_idle_soon(app_clone.clone());
                return;
            }
        };

        if text.trim().is_empty() {
            tracing::info!("empty transcription; skipping paste");
            set_state(&app_clone, AppState::Idle);
            return;
        }

        let cleaned = ctx.cleaner.clean(&text);

        if !crate::is_accessibility_trusted() {
            tracing::error!(
                "accessibility permission missing; text copied to clipboard, paste skipped"
            );
            let _ = paste::copy_only(&app_clone, &cleaned);
            let _ = app_clone.emit("paste_blocked", "accessibility");
            set_state(&app_clone, AppState::Error);
            reset_to_idle_soon(app_clone.clone());
            return;
        }

        let target_pid = app_clone
            .try_state::<AppContext>()
            .map(|ctx| *ctx.paste_target_pid.lock())
            .unwrap_or(None);

        if let Err(e) = paste::paste(&app_clone, &cleaned, target_pid) {
            tracing::error!("paste: {:?}", e);
            set_state(&app_clone, AppState::Error);
            reset_to_idle_soon(app_clone.clone());
            return;
        }

        if settings::Settings::load().auto_submit {
            // Submit immediately (chat-style).
            thread::sleep(Duration::from_millis(120));
            if let Err(e) = paste::press_enter(&app_clone) {
                tracing::warn!("auto-submit enter failed: {:?}", e);
            }
        }

        if let Some(ctx) = app_clone.try_state::<AppContext>() {
            *ctx.paste_target_pid.lock() = None;
        }

        tracing::info!(
            "pipeline ok: raw={} chars, cleaned={} chars",
            text.len(),
            cleaned.len()
        );
        set_state(&app_clone, AppState::Idle);

        let duration_secs = pcm.duration_secs();
        thread::spawn(move || {
            let audio_path = audio::save_wav(&pcm)
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            if let Err(e) = history::append(&text, &cleaned, duration_secs, audio_path) {
                tracing::warn!("history append failed: {:?}", e);
            }
        });
    });
}

pub(crate) fn current_language_mode() -> settings::LanguageMode {
    settings::LanguageMode::parse(
        settings::Settings::load()
            .transcription_language
            .as_deref()
            .unwrap_or(settings::DEFAULT_LANGUAGE_MODE),
    )
}

fn transcribe(ctx: &AppContext, pcm: &PcmBuffer) -> anyhow::Result<String> {
    let lang = current_language_mode();
    {
        let cloud = ctx.cloud.read();
        if cloud.is_cloud() {
            return cloud.transcribe(&pcm.samples, pcm.sample_rate, lang, &asr::initial_prompt());
        }
    }
    let transcriber = {
        let guard = ctx.asr.lock();
        guard.clone()
    };
    match transcriber {
        Some(t) => t.transcribe(&pcm.samples, lang),
        None => {
            tracing::warn!("whisper not ready yet");
            Err(anyhow::anyhow!(
                "whisper model still loading, please try again"
            ))
        }
    }
}

fn partial_preview_tail(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let char_count = trimmed.chars().count();
    if char_count <= PARTIAL_PREVIEW_CHARS {
        return trimmed.to_string();
    }
    trimmed
        .chars()
        .skip(char_count - PARTIAL_PREVIEW_CHARS)
        .collect()
}

fn start_partial_transcription(app: AppHandle) {
    cancel_partial_worker(&app);
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(ctx) = app.try_state::<AppContext>() {
        *ctx.partial_cancel.lock() = Some(cancel.clone());
    }

    thread::spawn(move || {
        let min_new_samples = (TARGET_SAMPLE_RATE as f32 * PARTIAL_NEW_AUDIO_SECS) as usize;
        let mut last_sample_count = 0usize;
        let busy = Arc::new(AtomicBool::new(false));
        let last_emitted = Arc::new(Mutex::new(String::new()));

        loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(PARTIAL_POLL_MS));
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            let ctx = match app.try_state::<AppContext>() {
                Some(c) => c,
                None => break,
            };
            if *ctx.state.lock() != AppState::Recording {
                break;
            }
            if busy.load(Ordering::Relaxed) {
                continue;
            }

            let pcm = match ctx.audio.snapshot() {
                Ok(p) => p,
                Err(_) => continue,
            };

            if pcm.duration_secs() < PARTIAL_MIN_SECS {
                continue;
            }
            if pcm.samples.len().saturating_sub(last_sample_count) < min_new_samples {
                continue;
            }
            last_sample_count = pcm.samples.len();

            let transcriber = ctx.asr.lock().clone();
            let Some(transcriber) = transcriber else {
                continue;
            };

            busy.store(true, Ordering::Release);
            let samples = pcm.samples.clone();
            let app_worker = app.clone();
            let cancel_worker = cancel.clone();
            let busy_worker = busy.clone();
            let last_emitted_worker = last_emitted.clone();
            thread::spawn(move || {
                struct BusyGuard(Arc<AtomicBool>);
                impl Drop for BusyGuard {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _busy_guard = BusyGuard(busy_worker);

                if cancel_worker.load(Ordering::Relaxed) {
                    return;
                }

                let partial =
                    match transcriber.transcribe_partial(&samples, current_language_mode()) {
                        Ok(Some(t)) => t,
                        Ok(None) => return,
                        Err(e) => {
                            tracing::debug!("partial transcription failed: {:?}", e);
                            return;
                        }
                    };

                let preview = partial_preview_tail(&partial);
                if preview.is_empty() {
                    return;
                }

                {
                    let mut last = last_emitted_worker.lock();
                    if *last == preview {
                        return;
                    }
                    *last = preview.clone();
                }

                if !live_transcript_enabled() {
                    return;
                }
                tracing::debug!("partial preview: {} chars", preview.len());
                emit_hud(&app_worker, "partial", &preview);
            });
        }
    });
}

pub(crate) fn cancel_partial_worker(app: &AppHandle) {
    if let Some(ctx) = app.try_state::<AppContext>() {
        if let Some(cancel) = ctx.partial_cancel.lock().take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn stop_partial_transcription(app: &AppHandle) {
    cancel_partial_worker(app);
    compact_hud(app);
}

fn reset_to_idle_soon(app: AppHandle) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1200));
        set_state(&app, AppState::Idle);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_release_is_toggle_not_hold() {
        let press_at = Instant::now();
        assert!(!release_should_stop_recording(press_at, false));
    }

    #[test]
    fn held_release_stops_recording() {
        let press_at = Instant::now() - Duration::from_millis(HOTKEY_HOLD_RELEASE_MS + 50);
        assert!(release_should_stop_recording(press_at, false));
    }

    #[test]
    fn toggle_stop_ignores_release() {
        let press_at = Instant::now() - Duration::from_millis(500);
        assert!(!release_should_stop_recording(press_at, true));
    }
}
