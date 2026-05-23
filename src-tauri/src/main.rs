#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(unexpected_cfgs)]

mod asr;
mod audio;
mod cleanup;
mod login_item;
mod paste;
mod settings;
mod state;

use std::fs::{File, OpenOptions};
use std::io;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use std::str::FromStr;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Runtime,
    WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::audio::{AudioController, PcmBuffer, MIN_DURATION_SECS};
use crate::cleanup::HybridCleaner;
use crate::settings::{Hotkey, HudPosition, DEFAULT_OLLAMA_MODEL};
use crate::state::AppState;

pub struct AppContext {
    pub audio: AudioController,
    pub cleaner: HybridCleaner,
    pub asr: Mutex<Option<Arc<asr::Transcriber>>>,
    pub state: Mutex<AppState>,
    pub current_shortcut: Mutex<Shortcut>,
    pub current_hotkey: Mutex<Hotkey>,
}

fn parse_modifiers(mods: &[String]) -> Result<Option<Modifiers>, String> {
    if mods.is_empty() {
        return Ok(None);
    }
    let mut out = Modifiers::empty();
    for m in mods {
        match m.as_str() {
            "Control" | "Ctrl" => out |= Modifiers::CONTROL,
            "Alt" | "Option" => out |= Modifiers::ALT,
            "Shift" => out |= Modifiers::SHIFT,
            "Meta" | "Command" | "Cmd" | "Super" => out |= Modifiers::SUPER,
            other => return Err(format!("unknown modifier: {}", other)),
        }
    }
    Ok(Some(out))
}

fn hotkey_to_shortcut(h: &Hotkey) -> Result<Shortcut, String> {
    let code = Code::from_str(&h.code).map_err(|_| format!("unknown key code: {}", h.code))?;
    let mods = parse_modifiers(&h.modifiers)?;
    Ok(Shortcut::new(mods, code))
}

const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray.png");

static TRAY_ICON_DECODED: once_cell::sync::Lazy<(Vec<u8>, u32, u32)> =
    once_cell::sync::Lazy::new(|| {
        let img = image::load_from_memory(TRAY_ICON_BYTES).expect("decode tray icon");
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        (rgba.into_raw(), w, h)
    });

fn tray_icon_image() -> Image<'static> {
    let (pixels, w, h) = &*TRAY_ICON_DECODED;
    Image::new_owned(pixels.clone(), *w, *h)
}

fn set_state(app: &AppHandle, new_state: AppState) {
    if let Some(ctx) = app.try_state::<AppContext>() {
        *ctx.state.lock() = new_state;
    }
    let _ = app.emit("state", new_state.as_str());
    tracing::debug!("state -> {:?}", new_state);
}

fn acquire_single_instance_lock() -> io::Result<File> {
    let dir = dirs::data_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no app data dir"))?
        .join("DictaFlow");
    std::fs::create_dir_all(&dir)?;
    let lock_path = dir.join("dictaflow.lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&file)?;
    Ok(file)
}

fn main() {
    let _instance_lock = match acquire_single_instance_lock() {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            eprintln!(
                "DictaFlow is already running. Quit the existing instance from the menu bar tray icon."
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Could not acquire single-instance lock: {e}");
            std::process::exit(1);
        }
    };

    let log_file = dirs::home_dir()
        .map(|h| h.join("Library/Logs/DictaFlow"))
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(dir.join("app.log"))
                .ok()
        });

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,dictaflow=debug"));

    match log_file {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(env_filter).init(),
    }

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
        .invoke_handler(tauri::generate_handler![
            get_hotkey,
            set_hotkey,
            get_ollama_model,
            list_ollama_models,
            set_ollama_model,
            get_launch_at_login,
            set_launch_at_login,
            close_settings,
            hud_edit_start,
            hud_edit_save,
            hud_edit_cancel,
            check_accessibility,
            prompt_accessibility,
            open_accessibility_settings,
            open_microphone_settings
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(ActivationPolicy::Accessory);

            let (audio, level_rx) = AudioController::spawn();
            let saved = settings::Settings::load();
            let ollama_model = saved
                .ollama_model
                .clone()
                .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string());
            let cleaner = HybridCleaner::new(Duration::from_millis(800), ollama_model.clone());
            audio.set_device(saved.input_device.clone());
            if let Some(d) = &saved.input_device {
                tracing::info!("settings: using input device '{}'", d);
            }
            tracing::info!("settings: using ollama model '{}'", ollama_model);

            let hotkey = saved.hotkey.clone().unwrap_or_default();
            let shortcut = match hotkey_to_shortcut(&hotkey) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("saved hotkey invalid ({}), using default F9", e);
                    Shortcut::new(None, Code::F9)
                }
            };

            let ctx = AppContext {
                audio,
                cleaner,
                asr: Mutex::new(None),
                state: Mutex::new(AppState::Idle),
                current_shortcut: Mutex::new(shortcut),
                current_hotkey: Mutex::new(hotkey.clone()),
            };
            app.manage(ctx);

            app.global_shortcut().register(shortcut)?;
            tracing::info!(
                "registered global shortcut: {} (hold to dictate)",
                hotkey.display()
            );

            build_tray(app.handle())?;
            build_hud(app.handle())?;

            spawn_rms_forwarder(app.handle().clone(), level_rx);
            spawn_initialization(app.handle().clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri run failed");
}

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    current_device: Option<&str>,
) -> tauri::Result<Menu<R>> {
    let about_item = MenuItem::with_id(app, "about", "About DictaFlow", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;

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
    let mic_submenu =
        Submenu::with_id_and_items(app, "mic_submenu", "Microphone", true, &mic_items)?;

    Menu::with_items(
        app,
        &[
            &about_item,
            &sep1,
            &mic_submenu,
            &sep2,
            &settings_item,
            &sep3,
            &quit_item,
        ],
    )
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let current = app
        .try_state::<AppContext>()
        .and_then(|ctx| ctx.audio.device.lock().clone());
    let menu = build_menu(app, current.as_deref())?;

    let hotkey_label = app
        .try_state::<AppContext>()
        .map(|ctx| ctx.current_hotkey.lock().display())
        .unwrap_or_else(|| "F9".to_string());

    TrayIconBuilder::with_id("main")
        .icon(tray_icon_image())
        .icon_as_template(true)
        .tooltip(format!("DictaFlow — hold {} to dictate", hotkey_label))
        .menu(&menu)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            match id {
                "about" => {
                    if let Err(e) = show_about(app.clone()) {
                        tracing::error!("about window: {:?}", e);
                    }
                }
                "settings" => {
                    if let Err(e) = show_settings(app.clone()) {
                        tracing::error!("settings window: {:?}", e);
                    }
                }
                "quit" => app.exit(0),
                id if id.starts_with("mic:") => {
                    let name = &id["mic:".len()..];
                    let selection = if name == "default" {
                        None
                    } else {
                        Some(name.to_string())
                    };
                    if let Some(ctx) = app.try_state::<AppContext>() {
                        ctx.audio.set_device(selection.clone());
                    }
                    let mut saved = settings::Settings::load();
                    saved.input_device = selection.clone();
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
    let width = 64.0;
    let height = 36.0;

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
    #[cfg(target_os = "macos")]
    raise_hud_level(&window);

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

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
}

#[tauri::command]
fn check_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        AXIsProcessTrusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[tauri::command]
fn prompt_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        use core_foundation::{
            base::TCFType, boolean::CFBoolean, dictionary::CFDictionary, string::CFString,
        };
        let key = CFString::from_static_string("AXTrustedCheckOptionPrompt");
        let value = CFBoolean::true_value();
        let dict = CFDictionary::from_CFType_pairs(&[(key, value)]);
        unsafe { AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef() as *const _) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[tauri::command]
fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
    }
}

#[tauri::command]
fn open_microphone_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn();
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
fn raise_hud_level(window: &tauri::WebviewWindow) {
    use cocoa::base::id;
    use objc::{msg_send, sel, sel_impl};

    let ns_window = match window.ns_window() {
        Ok(ptr) => ptr as id,
        Err(e) => {
            tracing::warn!("could not access NSWindow for HUD: {:?}", e);
            return;
        }
    };
    if ns_window.is_null() {
        return;
    }
    // NSStatusWindowLevel = 25 — above normal windows and the menu bar.
    const NS_STATUS_WINDOW_LEVEL: i64 = 25;
    unsafe {
        let _: () = msg_send![ns_window, setLevel: NS_STATUS_WINDOW_LEVEL];
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
fn set_hud_draggable(window: &tauri::WebviewWindow, draggable: bool) {
    use cocoa::base::{id, BOOL, NO, YES};
    use objc::{msg_send, sel, sel_impl};

    let ns_window = match window.ns_window() {
        Ok(ptr) => ptr as id,
        Err(e) => {
            tracing::warn!("could not access NSWindow for HUD: {:?}", e);
            return;
        }
    };
    if ns_window.is_null() {
        return;
    }
    let flag: BOOL = if draggable { YES } else { NO };
    unsafe {
        let _: () = msg_send![ns_window, setMovable: flag];
        let _: () = msg_send![ns_window, setMovableByWindowBackground: flag];
    }
}

#[cfg(not(target_os = "macos"))]
fn set_hud_draggable(_window: &tauri::WebviewWindow, _draggable: bool) {}

fn position_hud(window: &tauri::WebviewWindow) {
    let saved = settings::Settings::load().hud_position;
    if let Some(pos) = saved {
        let _ = window.set_position(PhysicalPosition::new(pos.x, pos.y));
    } else if let Some(monitor) = window.current_monitor().ok().flatten() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        let hud_w_physical = (64.0 * scale) as i32;
        // Sit just to the left of the notch, in the menu bar zone.
        // Notch is ~200 logical px wide, centered. Right edge of HUD lands
        // ~6 logical px left of the notch's left edge.
        let center_x = size.width as i32 / 2;
        let notch_half_physical = (100.0 * scale) as i32;
        let gap_physical = (6.0 * scale) as i32;
        let x = center_x - notch_half_physical - gap_physical - hud_w_physical;
        let y = (2.0 * scale) as i32;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    let _ = window.set_size(LogicalSize::new(64.0, 36.0));
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

fn show_settings(app: AppHandle) -> tauri::Result<()> {
    if let Some(existing) = app.get_webview_window("settings") {
        existing.set_focus().ok();
        existing.show().ok();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App("settings.html".into()))
        .title("DictaFlow Settings")
        .inner_size(520.0, 820.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .build()?;
    Ok(())
}

#[tauri::command]
fn get_hotkey(app: AppHandle) -> Hotkey {
    if let Some(ctx) = app.try_state::<AppContext>() {
        return ctx.current_hotkey.lock().clone();
    }
    Hotkey::default()
}

#[tauri::command]
fn set_hotkey(app: AppHandle, hotkey: Hotkey) -> Result<Hotkey, String> {
    let new_shortcut = hotkey_to_shortcut(&hotkey)?;

    let ctx = app
        .try_state::<AppContext>()
        .ok_or_else(|| "app context missing".to_string())?;

    let old_shortcut = *ctx.current_shortcut.lock();
    let old_hotkey = ctx.current_hotkey.lock().clone();

    if new_shortcut == old_shortcut {
        return Ok(old_hotkey);
    }

    let gs = app.global_shortcut();
    gs.unregister(old_shortcut)
        .map_err(|e| format!("unregister failed: {}", e))?;

    if let Err(e) = gs.register(new_shortcut) {
        // roll back
        let _ = gs.register(old_shortcut);
        return Err(format!(
            "OS rejected that shortcut (likely in use by another app): {}",
            e
        ));
    }

    *ctx.current_shortcut.lock() = new_shortcut;
    *ctx.current_hotkey.lock() = hotkey.clone();

    let mut saved = settings::Settings::load();
    saved.hotkey = Some(hotkey.clone());
    if let Err(e) = saved.save() {
        tracing::warn!("could not persist hotkey: {:?}", e);
    }

    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!(
            "DictaFlow — hold {} to dictate",
            hotkey.display()
        )));
    }

    tracing::info!("hotkey changed to: {}", hotkey.display());
    Ok(hotkey)
}

#[tauri::command]
fn get_ollama_model(app: AppHandle) -> String {
    if let Some(ctx) = app.try_state::<AppContext>() {
        return ctx.cleaner.ollama_model();
    }
    settings::Settings::load()
        .ollama_model
        .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string())
}

#[tauri::command]
fn list_ollama_models(app: AppHandle) -> Result<Vec<cleanup::ollama::OllamaModel>, String> {
    let ctx = app
        .try_state::<AppContext>()
        .ok_or_else(|| "app context missing".to_string())?;
    ctx.cleaner
        .list_ollama_models()
        .map_err(|e| format!("could not list Ollama models: {}", e))
}

#[tauri::command]
fn set_ollama_model(app: AppHandle, model: String) -> Result<String, String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err("Enter an Ollama model first.".to_string());
    }

    let ctx = app
        .try_state::<AppContext>()
        .ok_or_else(|| "app context missing".to_string())?;

    let selected = trimmed.to_string();
    ctx.cleaner.set_ollama_model(selected.clone());

    let mut saved = settings::Settings::load();
    saved.ollama_model = Some(selected.clone());
    if let Err(e) = saved.save() {
        tracing::warn!("could not persist ollama model: {:?}", e);
    }

    tracing::info!("ollama model changed to: {}", selected);
    Ok(selected)
}

#[tauri::command]
fn get_launch_at_login() -> bool {
    login_item::is_enabled()
}

#[tauri::command]
fn set_launch_at_login(enabled: bool) -> Result<bool, String> {
    login_item::set_enabled(enabled).map_err(|e| format!("could not update login item: {}", e))?;

    let mut saved = settings::Settings::load();
    saved.launch_at_login = enabled;
    if let Err(e) = saved.save() {
        tracing::warn!("could not persist launch at login: {:?}", e);
    }

    tracing::info!("launch at login changed to: {}", enabled);
    Ok(enabled)
}

#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.close();
    }
}

#[tauri::command]
fn hud_edit_start(app: AppHandle) -> Result<(), String> {
    let hud = app
        .get_webview_window("hud")
        .ok_or_else(|| "HUD window missing".to_string())?;
    let _ = hud.emit("hud_edit", true);
    set_hud_draggable(&hud, true);
    let _ = hud.show();
    Ok(())
}

#[tauri::command]
fn hud_edit_save(app: AppHandle) -> Result<HudPosition, String> {
    let hud = app
        .get_webview_window("hud")
        .ok_or_else(|| "HUD window missing".to_string())?;
    let pos = hud
        .outer_position()
        .map_err(|e| format!("read hud position: {}", e))?;
    let hud_pos = HudPosition { x: pos.x, y: pos.y };
    let mut saved = settings::Settings::load();
    saved.hud_position = Some(hud_pos);
    saved.save().map_err(|e| format!("save: {}", e))?;
    set_hud_draggable(&hud, false);
    let _ = hud.emit("hud_edit", false);
    Ok(hud_pos)
}

#[tauri::command]
fn hud_edit_cancel(app: AppHandle) -> Result<(), String> {
    let hud = app
        .get_webview_window("hud")
        .ok_or_else(|| "HUD window missing".to_string())?;
    position_hud(&hud);
    set_hud_draggable(&hud, false);
    let _ = hud.emit("hud_edit", false);
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

        if !check_accessibility() {
            tracing::error!(
                "accessibility permission missing; text copied to clipboard, paste skipped"
            );
            let _ = paste::copy_only(&app_clone, &cleaned);
            let _ = app_clone.emit("paste_blocked", "accessibility");
            open_accessibility_settings();
            set_state(&app_clone, AppState::Error);
            reset_to_idle_soon(app_clone.clone());
            return;
        }

        if let Err(e) = paste::paste(&app_clone, &cleaned) {
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
