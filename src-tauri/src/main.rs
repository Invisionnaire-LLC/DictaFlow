#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(unexpected_cfgs)]

mod asr;
mod audio;
mod cleanup;
mod cloud_asr;
mod dictionary;
mod history;
mod login_item;
mod paste;
mod settings;
mod state;

use std::fs::{File, OpenOptions};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use std::str::FromStr;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, RunEvent, Runtime, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

// macOS-only: gate the import so non-macOS targets (e.g. Windows) still compile.
#[cfg(target_os = "macos")]
use tauri::ActivationPolicy;

use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::audio::{AudioController, PcmBuffer, MIN_DURATION_SECS};
use crate::cleanup::HybridCleaner;
use crate::dictionary::LearnedUpdate;
use crate::settings::{Hotkey, HudAnchor, HudPosition, DEFAULT_OLLAMA_MODEL};
use crate::state::AppState;

const HUD_COMPACT_W: f64 = 64.0;
const HUD_COMPACT_H: f64 = 36.0;
const HUD_EXPANDED_W: f64 = 200.0;
const PARTIAL_MIN_SECS: f32 = 0.45;
const PARTIAL_POLL_MS: u64 = 350;
const PARTIAL_NEW_AUDIO_SECS: f32 = 0.12;
const PARTIAL_PREVIEW_CHARS: usize = 96;
/// Releases shorter than this are treated as tap-to-toggle (e.g. mouse button → Cmd+/).
const HOTKEY_HOLD_RELEASE_MS: u64 = 250;

struct HudPlacedMark {
    x: i32,
    y: i32,
    at: Instant,
}

static HUD_LAST_PLACED: Mutex<Option<HudPlacedMark>> = Mutex::new(None);
static HUD_SAVE_GEN: AtomicU64 = AtomicU64::new(0);

pub struct AppContext {
    pub audio: AudioController,
    pub cleaner: HybridCleaner,
    pub asr: Mutex<Option<Arc<asr::Transcriber>>>,
    /// Cloud transcription backend (Local by default — keeps everything offline).
    pub cloud: RwLock<cloud_asr::CloudTranscriber>,
    pub state: Mutex<AppState>,
    pub current_shortcut: Mutex<Shortcut>,
    pub current_hotkey: Mutex<Hotkey>,
    pub partial_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// When the current recording session's hotkey was pressed (hold vs tap detection).
    hotkey_press_at: Mutex<Option<Instant>>,
    /// Ignore the release that follows a toggle-stop press.
    ignore_hotkey_release: Mutex<bool>,
    /// App that was frontmost when dictation started (where paste should land).
    paste_target_pid: Mutex<Option<i32>>,
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
    set_hud_elevated(app, hud_should_elevate(new_state));
    tracing::debug!("state -> {:?}", new_state);
}

fn hud_always_on_top_enabled() -> bool {
    settings::Settings::load().hud_always_on_top.unwrap_or(true)
}

fn hud_should_elevate(state: AppState) -> bool {
    if hud_always_on_top_enabled() {
        return true;
    }
    matches!(
        state,
        AppState::Recording | AppState::Processing | AppState::Error
    )
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
            get_transcription_config,
            set_transcription_config,
            get_transcription_language,
            set_transcription_language,
            get_auto_submit,
            set_auto_submit,
            get_vocabulary,
            set_vocabulary,
            get_launch_at_login,
            set_launch_at_login,
            close_settings,
            close_history,
            search_history,
            delete_history_entry,
            clear_history,
            copy_text,
            update_history_entry,
            list_dictionary,
            delete_dictionary_entry,
            get_hud_style,
            set_hud_style,
            get_live_transcript,
            set_live_transcript,
            get_hud_always_on_top,
            set_hud_always_on_top,
            reset_hud_position,
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

            let cloud_backend = cloud_asr::Backend::parse(
                saved.transcription_backend.as_deref().unwrap_or("local"),
            );
            let cloud = cloud_asr::CloudTranscriber::new(
                cloud_backend,
                saved.transcription_api_key.clone().unwrap_or_default(),
                saved.transcription_model.clone().unwrap_or_default(),
            );
            tracing::info!(
                "settings: transcription backend '{}'",
                cloud_backend.as_str()
            );

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
                cloud: RwLock::new(cloud),
                state: Mutex::new(AppState::Idle),
                current_shortcut: Mutex::new(shortcut),
                current_hotkey: Mutex::new(hotkey.clone()),
                partial_cancel: Mutex::new(None),
                hotkey_press_at: Mutex::new(None),
                ignore_hotkey_release: Mutex::new(false),
                paste_target_pid: Mutex::new(None),
            };
            app.manage(ctx);

            app.global_shortcut().register(shortcut)?;
            tracing::info!(
                "registered global shortcut: {} (hold or tap to dictate)",
                hotkey.display()
            );

            build_tray(app.handle())?;
            build_hud(app.handle())?;

            spawn_rms_forwarder(app.handle().clone(), level_rx);
            spawn_initialization(app.handle().clone());

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("tauri build failed")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(hud) = app.get_webview_window("hud") {
                    save_hud_position_from_window(&hud);
                }
            }
        });
}

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    current_device: Option<&str>,
) -> tauri::Result<Menu<R>> {
    let about_item = MenuItem::with_id(app, "about", "About DictaFlow", true, None::<&str>)?;
    let history_item = MenuItem::with_id(app, "history", "History…", true, None::<&str>)?;
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
            &history_item,
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
        .tooltip(format!(
            "DictaFlow: hold or tap {} to dictate",
            hotkey_label
        ))
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
                "history" => {
                    if let Err(e) = show_history(app.clone()) {
                        tracing::error!("history window: {:?}", e);
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
    let style = hud_style();
    let (width, height) = if style == "notch" {
        (HUD_EXPANDED_W, HUD_COMPACT_H)
    } else {
        hud_size("classic")
    };

    let window = WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("hud.html".into()))
        .title("DictaFlow HUD")
        .inner_size(width, height)
        .decorations(false)
        .transparent(true)
        .always_on_top(false)
        .resizable(false)
        .skip_taskbar(true)
        .focused(false)
        .shadow(false)
        .build()?;

    position_hud(&window);
    persist_legacy_hud_anchor(&window);
    let _ = window.emit("hud_style", style.as_str());
    let _ = window.emit("live_transcript", live_transcript_enabled());
    set_hud_elevated(app, hud_always_on_top_enabled());
    set_hud_draggable(&window, true);

    let app_clone = app.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            if let Some(w) = app_clone.get_webview_window("hud") {
                let _ = w.hide();
            }
        }
        WindowEvent::Moved(_) => {
            if let Some(hud) = app_clone.get_webview_window("hud") {
                if hud_move_should_persist(&hud) {
                    save_hud_position_from_window(&hud);
                }
                schedule_hud_position_save(app_clone.clone());
            }
        }
        _ => {}
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

fn set_hud_elevated(app: &AppHandle, elevated: bool) {
    let app = app.clone();
    if let Err(e) = app.clone().run_on_main_thread(move || {
        if let Some(hud) = app.get_webview_window("hud") {
            let _ = hud.show();
            let _ = hud.set_always_on_top(elevated);
            #[cfg(target_os = "macos")]
            set_hud_window_level(&hud, elevated);
        }
    }) {
        tracing::warn!("set_hud_elevated dispatch failed: {:?}", e);
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
fn set_hud_window_level(window: &tauri::WebviewWindow, elevated: bool) {
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
    const NS_NORMAL_WINDOW_LEVEL: i64 = 0;
    const NS_FLOATING_WINDOW_LEVEL: i64 = 3;
    let level = if elevated {
        NS_FLOATING_WINDOW_LEVEL
    } else {
        NS_NORMAL_WINDOW_LEVEL
    };
    unsafe {
        let _: () = msg_send![ns_window, setLevel: level];
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

#[cfg(target_os = "macos")]
#[allow(deprecated)] // cocoa::base aliases; objc2 migration tracked in #7
fn activate_application() {
    use cocoa::base::{id, YES};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let ns_app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: YES];
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // cocoa::base aliases; objc2 migration tracked in #7
fn order_window_front(window: &WebviewWindow) {
    use cocoa::base::{id, nil};
    use objc::{msg_send, sel, sel_impl};

    let ns_window = match window.ns_window() {
        Ok(ptr) => ptr as id,
        Err(e) => {
            tracing::warn!("could not access NSWindow for focus: {:?}", e);
            return;
        }
    };
    if ns_window.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![ns_window, makeKeyAndOrderFront: nil];
    }
}

fn auxiliary_windows_visible(app: &AppHandle) -> bool {
    ["settings", "about", "history"].iter().any(|label| {
        app.get_webview_window(label)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    })
}

fn restore_menu_bar_activation_policy(app: &AppHandle) {
    if auxiliary_windows_visible(app) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(ActivationPolicy::Accessory);
    }
}

fn present_auxiliary_window(app: &AppHandle, window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(ActivationPolicy::Regular);
        activate_application();
    }
    let _ = window.unminimize();
    let _ = window.show();
    #[cfg(target_os = "macos")]
    order_window_front(window);
    let _ = window.set_focus();
}

fn open_auxiliary_window(
    app: &AppHandle,
    label: &str,
    url: &str,
    title: &str,
    width: f64,
    height: f64,
) -> tauri::Result<()> {
    if let Some(existing) = app.get_webview_window(label) {
        present_auxiliary_window(app, &existing);
        return Ok(());
    }

    let app_for_close = app.clone();
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(width, height)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .center()
        .build()?;

    window.on_window_event(move |event| {
        if matches!(
            event,
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
        ) {
            restore_menu_bar_activation_policy(&app_for_close);
        }
    });

    present_auxiliary_window(app, &window);
    Ok(())
}

fn hud_style() -> String {
    settings::Settings::load()
        .hud_style
        .unwrap_or_else(|| "classic".to_string())
}

fn hud_size(style: &str) -> (f64, f64) {
    if style == "notch" {
        (HUD_EXPANDED_W, HUD_COMPACT_H)
    } else {
        (110.0, 40.0)
    }
}

fn hud_size_physical(style: &str, scale: f64) -> (i32, i32) {
    let (w, h) = hud_size(style);
    ((w * scale).round() as i32, (h * scale).round() as i32)
}

fn hud_position_visible(window: &WebviewWindow, x: i32, y: i32, w: i32, h: i32) -> bool {
    let monitors = window.app_handle().available_monitors().unwrap_or_default();
    if monitors.is_empty() {
        return x >= -w && y >= -h && x < 16_000 && y < 16_000;
    }
    let cx = x + w / 2;
    let cy = y + h / 2;
    monitors.iter().any(|monitor| {
        let origin = monitor.position();
        let size = monitor.size();
        cx >= origin.x
            && cx < origin.x + size.width as i32
            && cy >= origin.y
            && cy < origin.y + size.height as i32
    })
}

fn clear_saved_hud_placement() {
    if let Err(e) = settings::Settings::update(|saved| {
        saved.hud_position = None;
        saved.hud_anchor = None;
    }) {
        tracing::warn!("could not clear invalid hud placement: {:?}", e);
    }
}

fn default_classic_hud_position(monitor: &tauri::Monitor, scale: f64) -> HudPosition {
    let (w_phys, h_phys) = hud_size_physical("classic", scale);
    let size = monitor.size();
    let origin = monitor.position();
    HudPosition {
        x: origin.x + (size.width as i32 - w_phys) / 2,
        y: origin.y + size.height as i32 - h_phys - (120.0 * scale).round() as i32,
    }
}

fn mark_hud_placed_programmatically(window: &tauri::WebviewWindow) {
    let mark = window.outer_position().ok().map(|pos| HudPlacedMark {
        x: pos.x,
        y: pos.y,
        at: Instant::now(),
    });
    *HUD_LAST_PLACED.lock() = mark;
}

fn hud_move_should_persist(window: &tauri::WebviewWindow) -> bool {
    let Ok(outer) = window.outer_position() else {
        return false;
    };
    let guard = HUD_LAST_PLACED.lock();
    let Some(mark) = guard.as_ref() else {
        return true;
    };
    if outer.x != mark.x || outer.y != mark.y {
        return true;
    }
    mark.at.elapsed() > Duration::from_millis(350)
}

fn schedule_hud_position_save(app: AppHandle) {
    let gen = HUD_SAVE_GEN.fetch_add(1, Ordering::Relaxed) + 1;
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(400));
        if HUD_SAVE_GEN.load(Ordering::Relaxed) != gen {
            return;
        }
        if let Some(hud) = app.get_webview_window("hud") {
            save_hud_position_from_window(&hud);
        }
    });
}

fn resolved_hud_anchor(settings: &settings::Settings, scale: f64) -> Option<HudAnchor> {
    if let Some(anchor) = settings.hud_anchor {
        return Some(anchor);
    }
    settings.hud_position.map(|pos| {
        let compact_w = (HUD_COMPACT_W * scale).round() as i32;
        HudAnchor {
            right: pos.x + compact_w,
            top: pos.y,
        }
    })
}

fn default_hud_anchor(monitor: &tauri::Monitor, scale: f64) -> HudAnchor {
    let size = monitor.size();
    let origin = monitor.position();
    let compact_w = (HUD_COMPACT_W * scale).round() as i32;
    let center_x = size.width as i32 / 2;
    let notch_half_physical = (100.0 * scale).round() as i32;
    let gap_physical = (6.0 * scale).round() as i32;
    let compact_x = center_x - notch_half_physical - gap_physical - compact_w;
    HudAnchor {
        right: origin.x + compact_x + compact_w,
        top: origin.y + (2.0 * scale).round() as i32,
    }
}

/// The monitor the user is currently on (under the mouse cursor), so the HUD
/// follows them across displays. Falls back to the window's / primary monitor.
fn active_monitor(window: &tauri::WebviewWindow) -> Option<tauri::Monitor> {
    if let Ok(pos) = window.cursor_position() {
        if let Ok(Some(m)) = window.monitor_from_point(pos.x, pos.y) {
            return Some(m);
        }
    }
    window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
}

/// True if the HUD (placed at `pos`, size `w`×`h` physical px) is centered on `monitor`.
fn position_on_monitor(
    pos: HudPosition,
    w_phys: i32,
    h_phys: i32,
    monitor: &tauri::Monitor,
) -> bool {
    let mp = monitor.position();
    let ms = monitor.size();
    let cx = pos.x + w_phys / 2;
    let cy = pos.y + h_phys / 2;
    cx >= mp.x && cx < mp.x + ms.width as i32 && cy >= mp.y && cy < mp.y + ms.height as i32
}

fn position_hud(window: &tauri::WebviewWindow) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let settings = settings::Settings::load();
    let style = settings.hud_style.as_deref().unwrap_or("classic");
    let monitor = active_monitor(window);

    if style == "classic" {
        if settings.hud_anchor.is_some() {
            let _ = settings::Settings::update(|saved| {
                saved.hud_anchor = None;
            });
        }
        let (w_phys, h_phys) = hud_size_physical("classic", scale);
        let _ = window.set_size(LogicalSize::new(
            hud_size("classic").0,
            hud_size("classic").1,
        ));
        // Drop a genuinely off-screen saved position; keep it otherwise.
        let saved_visible = settings
            .hud_position
            .filter(|p| hud_position_visible(window, p.x, p.y, w_phys, h_phys));
        if settings.hud_position.is_some() && saved_visible.is_none() {
            tracing::warn!("saved classic hud position is off-screen; resetting");
            clear_saved_hud_placement();
        }
        // Honor the dragged position only while on the monitor it was placed on;
        // otherwise show the pill on the monitor the user is currently using.
        let pos = saved_visible
            .filter(|p| {
                monitor
                    .as_ref()
                    .map(|m| position_on_monitor(*p, w_phys, h_phys, m))
                    .unwrap_or(true)
            })
            .or_else(|| {
                monitor
                    .as_ref()
                    .map(|m| default_classic_hud_position(m, scale))
            });
        if let Some(pos) = pos {
            let _ = window.set_position(PhysicalPosition::new(pos.x, pos.y));
        }
        mark_hud_placed_programmatically(window);
        let _ = window.show();
        return;
    }

    let expanded_w_phys = (HUD_EXPANDED_W * scale).round() as i32;
    let h_phys = (HUD_COMPACT_H * scale).round() as i32;
    let anchor = if let Some(anchor) = settings.hud_anchor {
        let x = anchor.right - expanded_w_phys;
        if hud_position_visible(window, x, anchor.top, expanded_w_phys, h_phys) {
            Some(anchor)
        } else {
            tracing::warn!(
                "saved notch hud anchor (right={}, top={}) is off-screen; resetting",
                anchor.right,
                anchor.top
            );
            clear_saved_hud_placement();
            None
        }
    } else if let Some(legacy) = resolved_hud_anchor(&settings, scale) {
        let x = legacy.right - expanded_w_phys;
        if hud_position_visible(window, x, legacy.top, expanded_w_phys, h_phys) {
            Some(legacy)
        } else if let Some(pos) = settings.hud_position {
            tracing::warn!(
                "saved notch hud position ({}, {}) is off-screen; resetting",
                pos.x,
                pos.y
            );
            clear_saved_hud_placement();
            None
        } else {
            None
        }
    } else {
        None
    };
    let anchor = anchor.or_else(|| monitor.as_ref().map(|m| default_hud_anchor(m, scale)));

    if let Some(anchor) = anchor {
        let x = anchor.right - expanded_w_phys;
        let _ = window.set_position(PhysicalPosition::new(x, anchor.top));
    }
    let _ = window.set_size(LogicalSize::new(HUD_EXPANDED_W, HUD_COMPACT_H));
    mark_hud_placed_programmatically(window);
    let _ = window.show();
}

fn persist_legacy_hud_anchor(window: &tauri::WebviewWindow) {
    let mut saved = settings::Settings::load();
    if saved.hud_style.as_deref().unwrap_or("classic") == "classic" {
        return;
    }
    if saved.hud_anchor.is_some() || saved.hud_position.is_none() {
        return;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let Some(anchor) = resolved_hud_anchor(&saved, scale) else {
        return;
    };
    let compact_w = (HUD_COMPACT_W * scale).round() as i32;
    saved.hud_anchor = Some(anchor);
    saved.hud_position = Some(HudPosition {
        x: anchor.right - compact_w,
        y: anchor.top,
    });
    if let Err(e) = saved.save() {
        tracing::warn!("could not migrate hud anchor: {:?}", e);
    } else {
        tracing::info!(
            "migrated hud anchor: right={}, top={}",
            anchor.right,
            anchor.top
        );
    }
}

fn save_hud_position_from_window(window: &tauri::WebviewWindow) {
    let Ok(outer) = window.outer_position() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let style = hud_style();
    let compact_w = (HUD_COMPACT_W * scale).round() as i32;
    let expanded_w = (HUD_EXPANDED_W * scale).round() as i32;

    let (anchor, position) = if style == "classic" {
        (
            None,
            Some(HudPosition {
                x: outer.x,
                y: outer.y,
            }),
        )
    } else {
        let anchor = HudAnchor {
            right: outer.x + expanded_w,
            top: outer.y,
        };
        (
            Some(anchor),
            Some(HudPosition {
                x: anchor.right - compact_w,
                y: anchor.top,
            }),
        )
    };

    let (w_phys, h_phys) = hud_size_physical(&style, scale);
    let visible = if style == "classic" {
        position
            .as_ref()
            .is_some_and(|pos| hud_position_visible(window, pos.x, pos.y, w_phys, h_phys))
    } else {
        anchor.as_ref().is_some_and(|a| {
            let x = a.right - expanded_w;
            hud_position_visible(window, x, a.top, expanded_w, h_phys)
        })
    };
    if !visible {
        if let Some(pos) = position {
            tracing::warn!(
                "ignore hud save: position off-screen ({}, {})",
                pos.x,
                pos.y
            );
        }
        return;
    }

    if let Some(a) = anchor {
        let saved = settings::Settings::load();
        if saved
            .hud_anchor
            .is_some_and(|existing| existing.right == a.right && existing.top == a.top)
        {
            return;
        }
    } else if let Some(pos) = position {
        let saved = settings::Settings::load();
        if saved
            .hud_position
            .is_some_and(|existing| existing.x == pos.x && existing.y == pos.y)
            && saved.hud_anchor.is_none()
        {
            return;
        }
    }

    let result = settings::Settings::update(|saved| {
        saved.hud_style.get_or_insert_with(|| style.clone());
        if style == "classic" {
            saved.hud_position = position;
            saved.hud_anchor = None;
        } else if let (Some(anchor), Some(position)) = (anchor, position) {
            saved.hud_anchor = Some(anchor);
            saved.hud_position = Some(position);
        }
    });

    match result {
        Ok(()) => {
            if let Some(anchor) = anchor {
                tracing::info!(
                    "hud anchor saved: right={}, top={}",
                    anchor.right,
                    anchor.top
                );
            } else if let Some(pos) = position {
                tracing::info!("hud position saved: x={}, y={}", pos.x, pos.y);
            }
        }
        Err(e) => tracing::warn!("could not save hud position: {:?}", e),
    }
}

fn live_transcript_enabled() -> bool {
    settings::Settings::load().live_transcript.unwrap_or(true)
}

fn expand_hud(app: &AppHandle) {
    let style = hud_style();
    let transcript = live_transcript_enabled();
    let should_expand = style == "notch" || (style == "classic" && transcript);
    if should_expand {
        emit_hud(app, "hud_layout", "expanded");
        if transcript {
            emit_hud(app, "partial", "Listening…");
        }
    }
    if style == "classic" && transcript {
        let app = app.clone();
        let height = hud_size("classic").1;
        if let Err(e) = app.clone().run_on_main_thread(move || {
            if let Some(hud) = app.get_webview_window("hud") {
                let _ = hud.set_size(LogicalSize::new(HUD_EXPANDED_W, height));
            }
        }) {
            tracing::warn!("expand_hud resize dispatch failed: {:?}", e);
        }
    }
}

fn compact_hud(app: &AppHandle) {
    emit_hud(app, "partial", "");
    let style = hud_style();
    let transcript = live_transcript_enabled();
    if style == "notch" || (style == "classic" && transcript) {
        emit_hud(app, "hud_layout", "compact");
    }
    if style == "classic" {
        let app = app.clone();
        let (w, h) = hud_size("classic");
        if let Err(e) = app.clone().run_on_main_thread(move || {
            if let Some(hud) = app.get_webview_window("hud") {
                let _ = hud.set_size(LogicalSize::new(w, h));
            }
        }) {
            tracing::warn!("compact_hud resize dispatch failed: {:?}", e);
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

fn emit_hud(app: &AppHandle, event: &str, payload: &str) {
    let _ = app.emit(event, payload);
}

fn show_about(app: AppHandle) -> tauri::Result<()> {
    open_auxiliary_window(&app, "about", "about.html", "About DictaFlow", 420.0, 300.0)
}

fn show_settings(app: AppHandle) -> tauri::Result<()> {
    open_auxiliary_window(
        &app,
        "settings",
        "settings.html",
        "DictaFlow Settings",
        520.0,
        880.0,
    )
}

fn show_history(app: AppHandle) -> tauri::Result<()> {
    open_auxiliary_window(
        &app,
        "history",
        "history.html",
        "DictaFlow History",
        760.0,
        620.0,
    )
}

#[tauri::command]
fn search_history(query: String) -> Result<Vec<history::HistoryEntry>, String> {
    history::search(&query).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_history_entry(id: String) -> Result<bool, String> {
    history::delete(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_history() -> Result<(), String> {
    history::clear_all().map_err(|e| e.to_string())
}

#[tauri::command]
fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    paste::copy_only(&app, &text).map_err(|e| e.to_string())
}

#[tauri::command]
fn update_history_entry(
    app: AppHandle,
    id: String,
    cleaned: String,
) -> Result<Vec<LearnedUpdate>, String> {
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return Err("text cannot be empty".into());
    }
    let (before, _updated) = history::update_cleaned(&id, &cleaned)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "history entry not found".to_string())?;
    let learned = dictionary::learn_from_edit(&before, &cleaned).map_err(|e| e.to_string())?;
    for update in &learned {
        let _ = app.emit("word_learned", update);
    }
    Ok(learned)
}

#[tauri::command]
fn list_dictionary() -> Result<Vec<dictionary::Replacement>, String> {
    Ok(dictionary::list_replacements())
}

#[tauri::command]
fn delete_dictionary_entry(from: String) -> Result<bool, String> {
    dictionary::delete_replacement(&from).map_err(|e| e.to_string())
}

#[tauri::command]
fn close_history(app: AppHandle) {
    if let Some(w) = app.get_webview_window("history") {
        let _ = w.close();
    }
    restore_menu_bar_activation_policy(&app);
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
            "DictaFlow: hold or tap {} to dictate",
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

#[derive(serde::Serialize)]
struct TranscriptionConfig {
    backend: String,
    /// User's model override (empty = provider default).
    model: String,
    /// The model that would actually be sent (resolved against provider default).
    resolved_model: String,
    /// API key is returned so the local settings page can show/edit it.
    api_key: String,
}

#[tauri::command]
fn get_transcription_config(app: AppHandle) -> TranscriptionConfig {
    if let Some(ctx) = app.try_state::<AppContext>() {
        let cloud = ctx.cloud.read();
        return TranscriptionConfig {
            backend: cloud.backend().as_str().to_string(),
            model: cloud.model_override().to_string(),
            resolved_model: cloud.resolved_model(),
            api_key: cloud.api_key().to_string(),
        };
    }
    let saved = settings::Settings::load();
    let backend =
        cloud_asr::Backend::parse(saved.transcription_backend.as_deref().unwrap_or("local"));
    let transcriber = cloud_asr::CloudTranscriber::new(
        backend,
        saved.transcription_api_key.unwrap_or_default(),
        saved.transcription_model.unwrap_or_default(),
    );
    TranscriptionConfig {
        backend: backend.as_str().to_string(),
        model: transcriber.model_override().to_string(),
        resolved_model: transcriber.resolved_model(),
        api_key: transcriber.api_key().to_string(),
    }
}

#[tauri::command]
fn set_transcription_config(
    app: AppHandle,
    backend: String,
    api_key: String,
    model: String,
) -> Result<String, String> {
    let parsed = cloud_asr::Backend::parse(&backend);
    let key = api_key.trim().to_string();
    let model = model.trim().to_string();

    if parsed.is_cloud() && key.is_empty() {
        return Err(format!(
            "Enter an API key to use the {} backend.",
            parsed.as_str()
        ));
    }

    let ctx = app
        .try_state::<AppContext>()
        .ok_or_else(|| "app context missing".to_string())?;

    let was_cloud = ctx.cloud.read().is_cloud();

    let transcriber = cloud_asr::CloudTranscriber::new(parsed, key.clone(), model.clone());
    let resolved = transcriber.resolved_model();
    *ctx.cloud.write() = transcriber;

    let mut saved = settings::Settings::load();
    saved.transcription_backend = Some(parsed.as_str().to_string());
    saved.transcription_api_key = if key.is_empty() { None } else { Some(key) };
    saved.transcription_model = if model.is_empty() { None } else { Some(model) };
    if let Err(e) = saved.save() {
        tracing::warn!("could not persist transcription config: {:?}", e);
    }

    // Free the ~1.5 GB local model when switching to cloud; reload it when going local.
    if parsed.is_cloud() {
        if let Some(t) = ctx.asr.lock().take() {
            drop(t);
            tracing::info!("switched to cloud backend; released local Whisper model");
        }
    } else if was_cloud && ctx.asr.lock().is_none() {
        tracing::info!("switched to local backend; loading Whisper model");
        spawn_initialization(app.clone());
    }

    tracing::info!(
        "transcription backend set to '{}' (model '{}')",
        parsed.as_str(),
        resolved
    );
    Ok(parsed.as_str().to_string())
}

#[tauri::command]
fn get_transcription_language() -> String {
    current_language_mode().as_str().to_string()
}

#[tauri::command]
fn set_transcription_language(language: String) -> Result<String, String> {
    let mode = settings::LanguageMode::parse(&language);
    let mut saved = settings::Settings::load();
    saved.transcription_language = Some(mode.as_str().to_string());
    saved
        .save()
        .map_err(|e| format!("could not save language: {}", e))?;
    tracing::info!("transcription language set to '{}'", mode.as_str());
    Ok(mode.as_str().to_string())
}

#[tauri::command]
fn get_auto_submit() -> bool {
    settings::Settings::load().auto_submit
}

#[tauri::command]
fn set_auto_submit(enabled: bool) -> Result<bool, String> {
    settings::Settings::update(|s| s.auto_submit = enabled)
        .map_err(|e| format!("could not save auto-submit: {}", e))?;
    tracing::info!("auto-submit set to {}", enabled);
    Ok(enabled)
}

#[tauri::command]
fn get_vocabulary() -> String {
    asr::read_vocabulary()
}

#[tauri::command]
fn set_vocabulary(words: String) -> Result<String, String> {
    let cleaned = words.trim().to_string();
    asr::write_vocabulary(&cleaned).map_err(|e| format!("could not save words: {}", e))?;
    tracing::info!("custom vocabulary updated ({} chars)", cleaned.len());
    Ok(cleaned)
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
    restore_menu_bar_activation_policy(&app);
}

#[tauri::command]
fn get_hud_style() -> String {
    hud_style()
}

#[tauri::command]
fn set_hud_style(app: AppHandle, style: String) -> Result<String, String> {
    let style = if style == "classic" {
        "classic".to_string()
    } else {
        "notch".to_string()
    };
    settings::Settings::update(|saved| {
        saved.hud_style = Some(style.clone());
    })
    .map_err(|e| format!("save: {}", e))?;
    if let Some(hud) = app.get_webview_window("hud") {
        position_hud(&hud);
        let _ = hud.emit("hud_style", &style);
    }
    Ok(style)
}

#[tauri::command]
fn get_live_transcript() -> bool {
    live_transcript_enabled()
}

#[tauri::command]
fn set_live_transcript(app: AppHandle, enabled: bool) -> Result<bool, String> {
    settings::Settings::update(|saved| {
        saved.live_transcript = Some(enabled);
    })
    .map_err(|e| format!("save: {}", e))?;
    if let Some(hud) = app.get_webview_window("hud") {
        let _ = hud.emit("live_transcript", enabled);
    }
    Ok(enabled)
}

#[tauri::command]
fn get_hud_always_on_top() -> bool {
    hud_always_on_top_enabled()
}

#[tauri::command]
fn set_hud_always_on_top(app: AppHandle, enabled: bool) -> Result<bool, String> {
    settings::Settings::update(|saved| {
        saved.hud_always_on_top = Some(enabled);
    })
    .map_err(|e| format!("save: {}", e))?;
    let state = app
        .try_state::<AppContext>()
        .map(|ctx| *ctx.state.lock())
        .unwrap_or(AppState::Idle);
    set_hud_elevated(&app, hud_should_elevate(state));
    Ok(enabled)
}

#[tauri::command]
fn reset_hud_position(app: AppHandle) -> Result<(), String> {
    clear_saved_hud_placement();
    if let Some(hud) = app.get_webview_window("hud") {
        position_hud(&hud);
        let state = app
            .try_state::<AppContext>()
            .map(|ctx| *ctx.state.lock())
            .unwrap_or(AppState::Idle);
        set_hud_elevated(&app, hud_should_elevate(state));
    }
    Ok(())
}

fn spawn_rms_forwarder(app: AppHandle, rx: mpsc::Receiver<f32>) {
    thread::spawn(move || {
        while let Ok(level) = rx.recv() {
            let _ = app.emit("rms", level);
        }
    });
}

/// Loads the local Whisper model (unless a cloud backend is active) and warms
/// up the Ollama cleaner, all in the background.
fn spawn_initialization(app: AppHandle) {
    thread::spawn(move || {
        let cloud = app
            .try_state::<AppContext>()
            .map(|ctx| ctx.cloud.read().is_cloud())
            .unwrap_or(false);

        if cloud {
            tracing::info!("cloud transcription backend active; skipping local Whisper model");
        } else {
            load_local_whisper(&app);
        }

        // The Ollama cleaner runs regardless of which transcription backend is used.
        let app_warm = app.clone();
        thread::spawn(move || {
            if let Some(ctx) = app_warm.try_state::<AppContext>() {
                ctx.cleaner.warmup();
            }
        });
    });
}

/// Downloads (if needed), loads, and prewarms the local Whisper model in memory.
fn load_local_whisper(app: &AppHandle) {
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
            let transcriber_warm = transcriber.clone();
            thread::spawn(move || {
                if let Err(e) = transcriber_warm.prewarm() {
                    tracing::warn!("whisper prewarm failed: {:?}", e);
                } else {
                    tracing::info!("whisper prewarm complete");
                }
            });
        }
        Err(e) => tracing::error!("whisper init failed: {:?}", e),
    }
}

fn release_should_stop_recording(press_at: Instant, ignore_release: bool) -> bool {
    if ignore_release {
        return false;
    }
    press_at.elapsed() >= Duration::from_millis(HOTKEY_HOLD_RELEASE_MS)
}

fn handle_press(app: AppHandle) {
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

fn handle_release(app: AppHandle) {
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

        if !check_accessibility() {
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

        let before_field = paste::read_focused_field_text(target_pid);

        if let Err(e) = paste::paste(&app_clone, &cleaned, target_pid) {
            tracing::error!("paste: {:?}", e);
            set_state(&app_clone, AppState::Error);
            reset_to_idle_soon(app_clone.clone());
            return;
        }

        if settings::Settings::load().auto_submit {
            // Submit immediately (chat-style). Skip edit-learning since the field clears.
            thread::sleep(Duration::from_millis(120));
            if let Err(e) = paste::press_enter(&app_clone) {
                tracing::warn!("auto-submit enter failed: {:?}", e);
            }
        } else {
            paste::spawn_edit_learning_watch(
                app_clone.clone(),
                cleaned.clone(),
                before_field,
                target_pid,
            );
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

fn current_language_mode() -> settings::LanguageMode {
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

fn start_partial_transcription(app: AppHandle) {
    cancel_partial_worker(&app);
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(ctx) = app.try_state::<AppContext>() {
        *ctx.partial_cancel.lock() = Some(cancel.clone());
    }

    thread::spawn(move || {
        let min_new_samples = (audio::TARGET_SAMPLE_RATE as f32 * PARTIAL_NEW_AUDIO_SECS) as usize;
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

fn cancel_partial_worker(app: &AppHandle) {
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
mod hotkey_tests {
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
