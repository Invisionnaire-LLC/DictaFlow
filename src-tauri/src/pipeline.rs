//! Dictation pipeline: hotkey press/release → record → transcribe → cleanup → paste,
//! plus the live partial-transcription preview worker. Extracted from `main.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::audio::{self, PcmBuffer, MAX_DURATION_SECS, MIN_DURATION_SECS};
use crate::state::AppState;
use crate::{
    asr, compact_hud, emit_hud, expand_hud, history, live_transcript_enabled, paste, set_state,
    settings, AppContext,
};

const PARTIAL_MIN_SECS: f32 = 0.45;
const PARTIAL_POLL_MS: u64 = 350;
const PARTIAL_NEW_AUDIO_SECS: f32 = 0.12;
/// Audio handed to each preview pass. A little more than the 2 s Whisper keeps
/// (asr::transcribe_partial), so the high-pass filter's start-up edge is dropped.
const PARTIAL_TAIL_SECS: f32 = 2.5;
const PARTIAL_PREVIEW_CHARS: usize = 96;
/// Releases shorter than this are treated as tap-to-toggle (e.g. mouse button → Cmd+/).
const HOTKEY_HOLD_RELEASE_MS: u64 = 250;

fn release_should_stop_recording(press_at: Instant) -> bool {
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

    if !release_should_stop_recording(press_at) {
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
            emit_hud(&app_clone, "paste_blocked", "accessibility");
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
        // Clone out of the lock so a slow upload never blocks a settings change.
        let cloud = ctx.cloud.read().clone();
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

    // Previews run one at a time on this thread; a pass in flight is aborted
    // through `cancel` as soon as the hotkey is released.
    thread::spawn(move || {
        let mut last_captured_secs = 0.0f32;
        let mut last_emitted = String::new();

        loop {
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

            // Without local Whisper (cloud backend) there's no preview, but the loop
            // still watches the length cap, so ask for no audio tail.
            let transcriber = ctx.asr.lock().clone();
            let tail_secs = if transcriber.is_some() {
                PARTIAL_TAIL_SECS
            } else {
                0.0
            };
            let Ok(snapshot) = ctx.audio.snapshot(tail_secs) else {
                continue;
            };
            if snapshot.captured_secs >= MAX_DURATION_SECS - 0.05 {
                // Capture stops at the cap; finish instead of silently dropping speech.
                // On the main thread, like the hotkey handlers, so the two never race.
                tracing::info!("recording hit the {}s cap; finishing", MAX_DURATION_SECS);
                let app_finish = app.clone();
                let _ = app.run_on_main_thread(move || finish_recording(app_finish));
                break;
            }
            if snapshot.captured_secs < PARTIAL_MIN_SECS
                || snapshot.captured_secs - last_captured_secs < PARTIAL_NEW_AUDIO_SECS
            {
                continue;
            }
            last_captured_secs = snapshot.captured_secs;

            let Some(transcriber) = transcriber else {
                continue;
            };
            let partial = match transcriber.transcribe_partial(
                &snapshot.tail.samples,
                current_language_mode(),
                &cancel,
            ) {
                Ok(Some(t)) => t,
                Ok(None) => continue,
                Err(e) => {
                    tracing::debug!("partial transcription failed: {:?}", e);
                    continue;
                }
            };

            let preview = partial_preview_tail(&partial);
            if preview.is_empty() || preview == last_emitted || cancel.load(Ordering::Relaxed) {
                continue;
            }
            if live_transcript_enabled() {
                tracing::debug!("partial preview: {} chars", preview.len());
                emit_hud(&app, "partial", &preview);
            }
            last_emitted = preview;
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
        assert!(!release_should_stop_recording(press_at));
    }

    #[test]
    fn held_release_stops_recording() {
        let press_at = Instant::now() - Duration::from_millis(HOTKEY_HOLD_RELEASE_MS + 50);
        assert!(release_should_stop_recording(press_at));
    }
}
