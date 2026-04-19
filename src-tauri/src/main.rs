#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod asr;
mod audio;
mod cleanup;
mod paste;
mod settings;
mod state;

use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Runtime, WebviewUrl,
    WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};

use crate::audio::{AudioController, PcmBuffer, MIN_DURATION_SECS};
use crate::cleanup::HybridCleaner;
use crate::state::AppState;

pub struct AppContext {
    pub audio: AudioController,
    pub cleaner: HybridCleaner,
    pub asr: Mutex<Option<Arc<asr::Transcriber>>>,
    pub state: Mutex<AppState>,
}

fn tray_icon_image(state: AppState) -> Image<'static> {
    const SIZE: u32 = 32;
    let (r, g, b) = match state {
        AppState::Recording => (33u8, 150, 243),
        AppState::Processing => (255, 183, 77),
        AppState::Error => (229, 115, 115),
        AppState::Idle => (120, 128, 140),
    };
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        pixels.extend_from_slice(&[r, g, b, 255]);
    }
    Image::new_owned(pixels, SIZE, SIZE)
}

fn set_state(app: &AppHandle, new_state: AppState) {
    if let Some(ctx) = app.try_state::<AppContext>() {
        *ctx.state.lock() = new_state;
    }
    let _ = app.emit("state", new_state.as_str());
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_icon(Some(tray_icon_image(new_state)));
    }
    tracing::debug!("state -> {:?}", new_state);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,dictaflow=debug")),
        )
        .init();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    let app = app.clone();
                    match event.state() {
                        ShortcutState::Pressed => handle_press(app),
                        ShortcutState::Released => handle_release(app),
                    }
                })
                .build(),
        )
        .setup(|app| {
            let (audio, level_rx) = AudioController::spawn();
            let cleaner = HybridCleaner::new(Duration::from_millis(800));

            let saved = settings::Settings::load();
            audio.set_device(saved.input_device.clone());
            if let Some(d) = &saved.input_device {
                tracing::info!("settings: using input device '{}'", d);
            }

            let ctx = AppContext {
                audio,
                cleaner,
                asr: Mutex::new(None),
                state: Mutex::new(AppState::Idle),
            };
            app.manage(ctx);

            let shortcut = Shortcut::new(None, Code::F9);
            app.global_shortcut().register(shortcut)?;
            tracing::info!("registered global shortcut: F9 (hold to dictate)");

            build_tray(app.handle())?;
            build_hud(app.handle())?;

            spawn_rms_forwarder(app.handle().clone(), level_rx);
            spawn_initialization(app.handle().clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri run failed");
}

fn build_menu<R: Runtime>(app: &AppHandle<R>, current_device: Option<&str>) -> tauri::Result<Menu<R>> {
    let about_item = MenuItem::with_id(app, "about", "About DictaFlow", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let default_mic = CheckMenuItem::with_id(
        app,
        "mic:default",
        "(System Default)",
        true,
        current_device.is_none(),
        None::<&str>,
    )?;
    let devices = audio::list_input_devices();
    let device_items: Vec<CheckMenuItem<R>> = devices
        .iter()
        .map(|d| {
            CheckMenuItem::with_id(
                app,
                format!("mic:{}", d),
                d,
                true,
                current_device == Some(d.as_str()),
                None::<&str>,
            )
        })
        .collect::<tauri::Result<Vec<_>>>()?;

    let mut mic_items: Vec<&dyn IsMenuItem<R>> = vec![&default_mic];
    for item in &device_items {
        mic_items.push(item);
    }
    let mic_submenu = Submenu::with_id_and_items(app, "mic_submenu", "Microphone", true, &mic_items)?;

    Menu::with_items(
        app,
        &[&about_item, &sep1, &mic_submenu, &sep2, &quit_item],
    )
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let current = app
        .try_state::<AppContext>()
        .and_then(|ctx| ctx.audio.device.lock().clone());
    let menu = build_menu(app, current.as_deref())?;

    TrayIconBuilder::with_id("main")
        .icon(tray_icon_image(AppState::Idle))
        .tooltip("DictaFlow — hold F9 to dictate")
        .menu(&menu)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            match id {
                "about" => {
                    if let Err(e) = show_about(app.clone()) {
                        tracing::error!("about window: {:?}", e);
                    }
                }
                "quit" => app.exit(0),
                id if id.starts_with("mic:") => {
                    let name = &id["mic:".len()..];
                    let selection = if name == "default" { None } else { Some(name.to_string()) };
                    if let Some(ctx) = app.try_state::<AppContext>() {
                        ctx.audio.set_device(selection.clone());
                    }
                    let saved = settings::Settings {
                        input_device: selection.clone(),
                    };
                    if let Err(e) = saved.save() {
                        tracing::warn!("could not save settings: {:?}", e);
                    }
                    tracing::info!("microphone switched to: {:?}", selection);
                    if let Err(e) = refresh_tray_menu(app) {
                        tracing::warn!("could not refresh tray menu: {:?}", e);
                    }
                }
                _ => {}
            }
        })
        .build(app)?;
    Ok(())
}

fn refresh_tray_menu(app: &AppHandle) -> tauri::Result<()> {
    let current = app
        .try_state::<AppContext>()
        .and_then(|ctx| ctx.audio.device.lock().clone());
    let menu = build_menu(app, current.as_deref())?;
    if let Some(tray) = app.tray_by_id("main") {
        tray.set_menu(Some(menu))?;
    }
    Ok(())
}

fn build_hud(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window("hud").is_some() {
        return Ok(());
    }
    let width = 110.0;
    let height = 40.0;

    let window = WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("hud.html".into()))
        .title("DictaFlow HUD")
        .inner_size(width, height)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .resizable(false)
        .skip_taskbar(true)
        .focused(false)
        .shadow(false)
        .build()?;

    position_hud(&window);

    let app_clone = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Some(w) = app_clone.get_webview_window("hud") {
                let _ = w.hide();
            }
        }
    });

    Ok(())
}

fn position_hud(window: &tauri::WebviewWindow) {
    if let Some(monitor) = window.current_monitor().ok().flatten() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        let hud_w_physical = (110.0 * scale) as i32;
        let hud_h_physical = (40.0 * scale) as i32;
        let x = (size.width as i32 - hud_w_physical) / 2;
        let y = size.height as i32 - hud_h_physical - (50.0 * scale) as i32;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    let _ = window.set_size(LogicalSize::new(110.0, 40.0));
}

fn show_about(app: AppHandle) -> tauri::Result<()> {
    if let Some(existing) = app.get_webview_window("about") {
        existing.set_focus().ok();
        existing.show().ok();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, "about", WebviewUrl::App("about.html".into()))
        .title("About DictaFlow")
        .inner_size(420.0, 300.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .build()?;
    Ok(())
}

fn spawn_rms_forwarder(app: AppHandle, rx: mpsc::Receiver<f32>) {
    thread::spawn(move || {
        while let Ok(level) = rx.recv() {
            let _ = app.emit("rms", level);
        }
    });
}

/// Downloads the Whisper model in the background and loads it into memory.
fn spawn_initialization(app: AppHandle) {
    thread::spawn(move || {
        if !asr::is_model_downloaded() {
            tracing::info!("whisper model not found locally; downloading (~1.5 GB)");
            let app_for_progress = app.clone();
            match asr::download_model(|done, total| {
                if total > 0 && done % (64 * 1024 * 1024) < (1 << 16) {
                    let pct = (done as f64 / total as f64 * 100.0) as u32;
                    tracing::info!("whisper download: {}% ({} / {})", pct, done, total);
                    let _ = app_for_progress.emit("download_progress", pct);
                }
            }) {
                Ok(_) => tracing::info!("whisper model download complete"),
                Err(e) => {
                    tracing::error!("whisper download failed: {:?}", e);
                    return;
                }
            }
        }

        let path = match asr::model_path() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("no model path: {:?}", e);
                return;
            }
        };

        match asr::Transcriber::new(&path) {
            Ok(t) => {
                let transcriber = Arc::new(t);
                if let Some(ctx) = app.try_state::<AppContext>() {
                    *ctx.asr.lock() = Some(transcriber.clone());
                }
                tracing::info!("whisper model loaded; prewarming");
                let _ = transcriber.prewarm();
                tracing::info!("whisper prewarm complete");
            }
            Err(e) => tracing::error!("whisper init failed: {:?}", e),
        }

        if let Some(ctx) = app.try_state::<AppContext>() {
            ctx.cleaner.warmup();
        }
    });
}

fn handle_press(app: AppHandle) {
    let ctx = match app.try_state::<AppContext>() {
        Some(c) => c,
        None => return,
    };
    if *ctx.state.lock() != AppState::Idle {
        tracing::debug!("ignore press while not idle");
        return;
    }
    if let Err(e) = ctx.audio.start() {
        tracing::error!("audio start failed: {:?}", e);
        set_state(&app, AppState::Error);
        return;
    }
    set_state(&app, AppState::Recording);
}

fn handle_release(app: AppHandle) {
    let ctx = match app.try_state::<AppContext>() {
        Some(c) => c,
        None => return,
    };
    if *ctx.state.lock() != AppState::Recording {
        tracing::debug!("ignore release while not recording");
        return;
    }
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

        let _ = audio::save_wav(&pcm); // best-effort debugging artifact

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

        if let Err(e) = paste::paste(&cleaned) {
            tracing::error!("paste: {:?}", e);
            set_state(&app_clone, AppState::Error);
            reset_to_idle_soon(app_clone.clone());
            return;
        }

        tracing::info!(
            "pipeline ok: raw={} chars, cleaned={} chars",
            text.len(),
            cleaned.len()
        );
        set_state(&app_clone, AppState::Idle);
    });
}

fn transcribe(ctx: &AppContext, pcm: &PcmBuffer) -> anyhow::Result<String> {
    let transcriber = {
        let guard = ctx.asr.lock();
        guard.clone()
    };
    match transcriber {
        Some(t) => t.transcribe(&pcm.samples),
        None => {
            tracing::warn!("whisper not ready yet; returning placeholder");
            Ok(String::from(
                "whisper model still loading, please try again in a moment",
            ))
        }
    }
}

fn reset_to_idle_soon(app: AppHandle) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1200));
        set_state(&app, AppState::Idle);
    });
}
