#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(unexpected_cfgs)]

mod asr;
mod audio;
mod cleanup;
mod cloud_asr;
mod dictionary;
mod fs_util;
mod history;
mod hud_cursor;
mod login_item;
mod main_thread;
mod paste;
mod pipeline;
mod secrets;
mod settings;
mod state;

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
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

use crate::audio::AudioController;
use crate::cleanup::HybridCleaner;
use crate::settings::{Hotkey, HudAnchor, HudPosition};
use crate::state::AppState;

const HUD_COMPACT_W: f64 = 64.0;
const HUD_COMPACT_H: f64 = 36.0;
const HUD_EXPANDED_W: f64 = 200.0;
/// Max time to wait on the LLM cleanup pass before falling back to rules-only output.
const CLEANUP_TIMEOUT_MS: u64 = 800;

struct HudPlacedMark {
    x: i32,
    y: i32,
    at: Instant,
}

static HUD_LAST_PLACED: Mutex<Option<HudPlacedMark>> = Mutex::new(None);
static HUD_SAVE_GEN: AtomicU64 = AtomicU64::new(0);
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

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
    pub(crate) hotkey_press_at: Mutex<Option<Instant>>,
    /// Ignore the release that follows a toggle-stop press.
    pub(crate) ignore_hotkey_release: Mutex<bool>,
    /// App that was frontmost when dictation started (where paste should land).
    pub(crate) paste_target_pid: Mutex<Option<i32>>,
    /// Stops the cursor-follow HUD polling thread.
    hud_follow_cancel: Mutex<Option<Arc<AtomicBool>>>,
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

fn default_dictation_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::ALT), Code::Space)
}

fn hotkey_to_shortcut(h: &Hotkey) -> Result<Shortcut, String> {
    let code = Code::from_str(&h.code).map_err(|_| format!("unknown key code: {}", h.code))?;
    let mods = parse_modifiers(&h.modifiers)?;
    Ok(Shortcut::new(mods, code))
}

const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray.png");

static HUD_STYLE_CACHE: once_cell::sync::Lazy<Mutex<String>> =
    once_cell::sync::Lazy::new(|| Mutex::new(load_hud_style_from_disk()));

static MODEL_DOWNLOADING: AtomicBool = AtomicBool::new(false);

fn load_hud_style_from_disk() -> String {
    hud_cursor::normalize_hud_style(
        &settings::Settings::load()
            .hud_style
            .unwrap_or_else(|| "notch".to_string()),
    )
}

fn set_hud_style_cache(style: &str) {
    *HUD_STYLE_CACHE.lock() = hud_cursor::normalize_hud_style(style);
}

pub(crate) fn set_state(app: &AppHandle, new_state: AppState) {
    if let Some(ctx) = app.try_state::<AppContext>() {
        *ctx.state.lock() = new_state;
    }
    emit_hud(app, "state", new_state.as_str());
    if hud_cursor::is_cursor_style(&hud_style()) {
        match new_state {
            AppState::Recording => hud_cursor::on_recording_start(app),
            _ => hud_cursor::on_recording_end(app),
        }
    }
    set_hud_elevated(app, hud_should_elevate(new_state));
    tracing::debug!("state -> {:?}", new_state);
}

fn hud_always_on_top_enabled() -> bool {
    settings::Settings::load().hud_always_on_top.unwrap_or(true)
}

fn current_state(app: &AppHandle) -> AppState {
    app.try_state::<AppContext>()
        .map(|ctx| *ctx.state.lock())
        .unwrap_or(AppState::Idle)
}

/// Re-apply HUD stacking for the current state after a setting or position change.
fn refresh_hud_elevation(app: &AppHandle) {
    set_hud_elevated(app, hud_should_elevate(current_state(app)));
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

fn app_dir_created() -> io::Result<std::path::PathBuf> {
    let dir = fs_util::app_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no app data dir"))?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn acquire_single_instance_lock() -> io::Result<File> {
    let dir = app_dir_created()?;
    let lock_path = dir.join("dictaflow.lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn control_socket_path() -> io::Result<std::path::PathBuf> {
    Ok(app_dir_created()?.join("control.sock"))
}

#[cfg(unix)]
fn spawn_control_socket(app: AppHandle) {
    let path = match control_socket_path() {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!("control socket path unavailable: {e}");
            return;
        }
    };

    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::warn!("control socket bind failed at {}: {e}", path.display());
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!("control socket chmod failed at {}: {e}", path.display());
    }

    tracing::info!("control socket listening at {}", path.display());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::warn!("control socket accept failed: {e}");
                    continue;
                }
            };

            let mut command = String::new();
            if let Err(e) = stream.read_to_string(&mut command) {
                let _ = writeln!(stream, "ERR read: {e}");
                continue;
            }

            match command.trim() {
                "toggle" => {
                    let app_for_toggle = app.clone();
                    if let Err(e) =
                        app.run_on_main_thread(move || pipeline::handle_press(app_for_toggle))
                    {
                        let _ = writeln!(stream, "ERR toggle: {e}");
                    } else {
                        let _ = writeln!(stream, "OK");
                    }
                }
                other => {
                    let _ = writeln!(stream, "ERR unknown command: {other}");
                }
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_control_socket(_app: AppHandle) {}

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

    // ~/Library/Logs/DictaFlow on macOS (where Console.app looks); the app dir elsewhere.
    let log_dir = if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs/DictaFlow"))
    } else {
        fs_util::app_dir().map(|d| d.join("logs"))
    };
    let log_file = log_dir.and_then(|dir| {
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
                        ShortcutState::Pressed => pipeline::handle_press(app),
                        ShortcutState::Released => pipeline::handle_release(app),
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_hotkey,
            set_hotkey,
            get_cleanup_config,
            list_cleanup_models,
            set_cleanup_model,
            set_cleanup_backend,
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
            search_history,
            delete_history_entry,
            clear_history,
            copy_text,
            update_history_entry,
            list_dictionary,
            add_dictionary_entry,
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
            open_microphone_settings,
            launch_at_login_supported,
            get_whisper_model_status,
            get_build_info,
            get_cursor_hud_status
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(ActivationPolicy::Accessory);

            let (audio, level_rx) = AudioController::spawn();
            secrets::migrate_transcription_api_key_from_settings();
            let saved = settings::Settings::load();
            let cleanup_backend = saved.cleanup_backend_or_default();
            let lmstudio_base_url = saved.lmstudio_base_url_or_default();
            let cleaner = HybridCleaner::new(
                cleanup_backend.clone(),
                lmstudio_base_url.clone(),
                make_cleaner(&saved, &cleanup_backend, &lmstudio_base_url),
            );

            let cloud = cloud_asr::CloudTranscriber::from_settings(&saved);
            tracing::info!(
                "settings: transcription backend '{}'",
                cloud.backend().as_str()
            );

            audio.set_device(saved.input_device.clone());
            if let Some(d) = &saved.input_device {
                tracing::info!("settings: using input device '{}'", d);
            }
            tracing::info!(
                "settings: cleanup backend '{}' (ollama model '{}')",
                cleanup::normalize_backend(&cleanup_backend),
                saved.ollama_model_or_default()
            );
            if let Err(e) = login_item::reconcile(saved.launch_at_login) {
                tracing::warn!("launch at login reconcile: {:?}", e);
            }

            let hotkey = saved.hotkey.clone().unwrap_or_default();
            let shortcut = match hotkey_to_shortcut(&hotkey) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("saved hotkey invalid ({}), using default Option+Space", e);
                    default_dictation_shortcut()
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
                hud_follow_cancel: Mutex::new(None),
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
            spawn_control_socket(app.handle().clone());
            spawn_initialization(app.handle().clone());

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("tauri build failed")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                prepare_shutdown(app);
            }
        });
}

fn make_cleaner(
    saved: &settings::Settings,
    backend: &str,
    lmstudio_base_url: &str,
) -> Box<dyn cleanup::Cleaner> {
    cleanup::make_cleaner(
        backend,
        Duration::from_millis(CLEANUP_TIMEOUT_MS),
        &saved.ollama_model_or_default(),
        saved.lmstudio_model.as_deref().unwrap_or_default(),
        lmstudio_base_url,
    )
}

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    current_device: Option<&str>,
) -> tauri::Result<Menu<R>> {
    let current_language = pipeline::current_language_mode();
    let about_item = MenuItem::with_id(app, "about", "About DictaFlow", true, None::<&str>)?;
    let history_item = MenuItem::with_id(app, "history", "History…", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let language_items = [
        (settings::LanguageMode::Auto, "Auto"),
        (settings::LanguageMode::Spanish, "Español (Focus)"),
        (settings::LanguageMode::English, "English (Focus)"),
        (settings::LanguageMode::TranslateEnglish, "Translate"),
    ]
    .into_iter()
    .map(|(mode, label)| {
        CheckMenuItem::with_id(
            app,
            format!("lang:{}", mode.as_str()),
            label,
            true,
            current_language == mode,
            None::<&str>,
        )
    })
    .collect::<tauri::Result<Vec<_>>>()?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let sep4 = PredefinedMenuItem::separator(app)?;

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
    mic_items.extend(device_items.iter().map(|i| i as &dyn IsMenuItem<R>));
    let mic_submenu =
        Submenu::with_id_and_items(app, "mic_submenu", "Microphone", true, &mic_items)?;

    let mut items: Vec<&dyn IsMenuItem<R>> = vec![&about_item, &sep1];
    items.extend(language_items.iter().map(|i| i as &dyn IsMenuItem<R>));
    items.extend([
        &sep2 as &dyn IsMenuItem<R>,
        &mic_submenu,
        &sep3,
        &history_item,
        &settings_item,
        &sep4,
        &quit_item,
    ]);
    Menu::with_items(app, &items)
}

fn current_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let device = app
        .try_state::<AppContext>()
        .and_then(|ctx| ctx.audio.device.lock().clone());
    build_menu(app, device.as_deref())
}

fn tray_tooltip(hotkey: &Hotkey) -> String {
    format!("DictaFlow: hold or tap {} to dictate", hotkey.display())
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = current_menu(app)?;
    let hotkey = app
        .try_state::<AppContext>()
        .map(|ctx| ctx.current_hotkey.lock().clone())
        .unwrap_or_default();

    TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(TRAY_ICON_BYTES)?)
        .icon_as_template(true)
        .tooltip(tray_tooltip(&hotkey))
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
                id if id.starts_with("lang:") => {
                    let mode = settings::LanguageMode::parse(&id["lang:".len()..]);
                    match settings::Settings::update(|saved| {
                        saved.transcription_language = Some(mode.as_str().to_string());
                    }) {
                        Ok(()) => {
                            tracing::info!("transcription language set to '{}'", mode.as_str())
                        }
                        Err(e) => tracing::warn!("could not save language: {:?}", e),
                    }
                    if let Err(e) = refresh_tray_menu(app) {
                        tracing::warn!("could not refresh tray menu: {:?}", e);
                    }
                }
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
                    if let Err(e) = settings::Settings::update(|saved| {
                        saved.input_device = selection.clone();
                    }) {
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
    let menu = current_menu(app)?;
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
        hud_size(&style)
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
    if hud_cursor::is_cursor_style(&style) {
        hud_cursor::log_tracking_readiness(&window);
    }
    let _ = window.emit("hud_style", style.as_str());
    let _ = window.emit("live_transcript", live_transcript_enabled());
    set_hud_elevated(app, hud_always_on_top_enabled());
    set_hud_draggable(app, &window, !hud_cursor::is_cursor_style(&style));

    let app_clone = app.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            if let Some(w) = app_clone.get_webview_window("hud") {
                let _ = w.hide();
            }
        }
        WindowEvent::Moved(_) => {
            if hud_cursor::is_cursor_style(&hud_style()) {
                return;
            }
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
    is_accessibility_trusted()
}

/// Plain helper (no Tauri command macro) so other modules can call it without
/// pulling the generated command macros into their namespace.
pub(crate) fn is_accessibility_trusted() -> bool {
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

/// Open a pane of System Settings → Privacy & Security (e.g. `Privacy_Microphone`).
fn open_privacy_pane(pane: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{pane}"
            ))
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = pane;
}

#[tauri::command]
fn open_accessibility_settings() {
    open_privacy_pane("Privacy_Accessibility");
}

#[tauri::command]
fn open_microphone_settings() {
    open_privacy_pane("Privacy_Microphone");
}

#[tauri::command]
fn launch_at_login_supported() -> bool {
    login_item::launch_at_login_supported()
}

#[tauri::command]
fn get_whisper_model_status() -> String {
    if MODEL_DOWNLOADING.load(Ordering::Relaxed) {
        return "downloading".to_string();
    }
    if asr::is_model_downloaded() {
        "ready".to_string()
    } else {
        "missing".to_string()
    }
}

fn set_hud_elevated(app: &AppHandle, elevated: bool) {
    if SHUTTING_DOWN.load(Ordering::Relaxed) {
        return;
    }
    let app = app.clone();
    let cursor_style = hud_cursor::is_cursor_style(&hud_style());
    main_thread::run_void(app.clone(), move || {
        if let Some(hud) = app.get_webview_window("hud") {
            // Cursor mode visibility is owned by hud_cursor (show/hide on record).
            // Only adjust stacking here so we never race reposition with a blind show().
            if !cursor_style {
                let _ = hud.show();
            }
            if hud.set_always_on_top(elevated).is_err() {
                tracing::warn!("set_always_on_top({elevated}) failed");
            }
        }
    });
}

fn set_hud_draggable(app: &AppHandle, window: &tauri::WebviewWindow, draggable: bool) {
    let label = window.label().to_string();
    let app = app.clone();
    main_thread::run_void(app.clone(), move || {
        if let Some(hud) = app.get_webview_window(&label) {
            set_hud_draggable_ns(&hud, draggable);
        }
    });
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
fn set_hud_draggable_ns(window: &tauri::WebviewWindow, draggable: bool) {
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
fn set_hud_draggable_ns(_window: &tauri::WebviewWindow, _draggable: bool) {}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)] // cocoa::base aliases; objc2 migration tracked in #7
fn activate_application() {
    use cocoa::base::{id, YES};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let ns_app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: YES];
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)] // cocoa::base aliases; objc2 migration tracked in #7
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
    if !auxiliary_windows_visible(app) {
        #[cfg(target_os = "macos")]
        {
            let _ = app.set_activation_policy(ActivationPolicy::Accessory);
        }
    }
}

fn present_auxiliary_window(_app: &AppHandle, window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let _ = _app.set_activation_policy(ActivationPolicy::Regular);
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
    HUD_STYLE_CACHE.lock().clone()
}

fn hud_size(style: &str) -> (f64, f64) {
    match style {
        "notch" => (HUD_EXPANDED_W, HUD_COMPACT_H),
        "classic" | "cursor" => (110.0, 40.0),
        _ => (HUD_EXPANDED_W, HUD_COMPACT_H),
    }
}

/// Logical → physical pixels.
fn to_phys(logical: f64, scale: f64) -> i32 {
    (logical * scale).round() as i32
}

fn hud_size_physical(style: &str, scale: f64) -> (i32, i32) {
    let (w, h) = hud_size(style);
    (to_phys(w, scale), to_phys(h, scale))
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
        y: origin.y + size.height as i32 - h_phys - to_phys(120.0, scale),
    }
}

pub(crate) fn mark_hud_placed_programmatically(window: &tauri::WebviewWindow) {
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
        let compact_w = to_phys(HUD_COMPACT_W, scale);
        HudAnchor {
            right: pos.x + compact_w,
            top: pos.y,
        }
    })
}

fn default_hud_anchor(monitor: &tauri::Monitor, scale: f64) -> HudAnchor {
    let size = monitor.size();
    let origin = monitor.position();
    let compact_w = to_phys(HUD_COMPACT_W, scale);
    let center_x = size.width as i32 / 2;
    let notch_half_physical = to_phys(100.0, scale);
    let gap_physical = to_phys(6.0, scale);
    let compact_x = center_x - notch_half_physical - gap_physical - compact_w;
    HudAnchor {
        right: origin.x + compact_x + compact_w,
        top: origin.y + to_phys(2.0, scale),
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
    let style = hud_style();
    let monitor = active_monitor(window);

    if hud_cursor::is_cursor_style(&style) {
        hud_cursor::position_cursor_hud(window);
        if current_state(window.app_handle()) == AppState::Recording {
            let _ = window.show();
        } else {
            let _ = window.hide();
        }
        return;
    }

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

    let expanded_w_phys = to_phys(HUD_EXPANDED_W, scale);
    let h_phys = to_phys(HUD_COMPACT_H, scale);
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
    if hud_style() == "classic" {
        return;
    }
    let saved = settings::Settings::load();
    if saved.hud_anchor.is_some() || saved.hud_position.is_none() {
        return;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let Some(anchor) = resolved_hud_anchor(&saved, scale) else {
        return;
    };
    let compact_w = to_phys(HUD_COMPACT_W, scale);
    if let Err(e) = settings::Settings::update(|saved| {
        saved.hud_anchor = Some(anchor);
        saved.hud_position = Some(HudPosition {
            x: anchor.right - compact_w,
            y: anchor.top,
        });
    }) {
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
    let style = hud_style();
    if hud_cursor::is_cursor_style(&style) {
        return;
    }
    let Ok(outer) = window.outer_position() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let compact_w = to_phys(HUD_COMPACT_W, scale);
    let expanded_w = to_phys(HUD_EXPANDED_W, scale);

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

pub(crate) fn live_transcript_enabled() -> bool {
    settings::Settings::load().live_transcript.unwrap_or(true)
}

/// Notch always toggles layout; classic/cursor only when they show live text.
fn hud_layout_toggles(style: &str, transcript: bool) -> bool {
    style == "notch" || (hud_cursor::uses_waveform(style) && transcript)
}

/// Resize a waveform-style HUD (classic/cursor) between compact and expanded.
fn resize_waveform_hud(app: &AppHandle, style: &str, expanded: bool) {
    let cursor = hud_cursor::is_cursor_style(style);
    let (compact_w, height) = hud_size("classic");
    let width = if expanded { HUD_EXPANDED_W } else { compact_w };
    let app_main = app.clone();
    if let Err(e) = app.run_on_main_thread(move || {
        if let Some(hud) = app_main.get_webview_window("hud") {
            if cursor {
                hud_cursor::set_cursor_hud_size(&hud, expanded);
            } else {
                let _ = hud.set_size(LogicalSize::new(width, height));
            }
        }
    }) {
        tracing::warn!("hud resize dispatch failed: {:?}", e);
    }
}

pub(crate) fn expand_hud(app: &AppHandle) {
    let style = hud_style();
    let transcript = live_transcript_enabled();
    if hud_layout_toggles(&style, transcript) {
        emit_hud(app, "hud_layout", "expanded");
        if transcript {
            emit_hud(app, "partial", "Listening…");
        }
    }
    if hud_cursor::uses_waveform(&style) && transcript {
        resize_waveform_hud(app, &style, true);
    }
}

pub(crate) fn compact_hud(app: &AppHandle) {
    emit_hud(app, "partial", "");
    let style = hud_style();
    if hud_layout_toggles(&style, live_transcript_enabled()) {
        emit_hud(app, "hud_layout", "compact");
    }
    if hud_cursor::uses_waveform(&style) {
        resize_waveform_hud(app, &style, false);
    }
}

/// HUD-only events (state, level meter, live text) go to the HUD window alone,
/// not to every open Settings/History/About webview.
pub(crate) fn emit_hud<S: serde::Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to("hud", event, payload);
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
        820.0,
        640.0,
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

/// Run blocking work (HTTP, disk, Keychain) on Tauri's blocking pool. Plain sync
/// commands run on the main thread, and blocking `reqwest` must not run on an
/// async runtime worker (it panics in debug builds).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("background task failed: {e}"))
}

#[tauri::command]
async fn search_history(query: String) -> Result<Vec<history::HistoryEntry>, String> {
    blocking(move || history::search(&query).map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn delete_history_entry(id: String) -> Result<bool, String> {
    blocking(move || history::delete(&id).map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn clear_history() -> Result<(), String> {
    blocking(|| history::clear_all().map_err(|e| e.to_string())).await?
}

#[tauri::command]
fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    paste::copy_only(&app, &text).map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_history_entry(id: String, cleaned: String) -> Result<(), String> {
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return Err("text cannot be empty".into());
    }
    match blocking(move || history::update_cleaned(&id, &cleaned)).await? {
        Ok(true) => Ok(()),
        Ok(false) => Err("history entry not found".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn list_dictionary() -> Result<Vec<dictionary::Replacement>, String> {
    blocking(dictionary::list_replacements).await
}

#[tauri::command]
async fn delete_dictionary_entry(from: String) -> Result<bool, String> {
    blocking(move || dictionary::delete_replacement(&from).map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn add_dictionary_entry(from: String, to: String) -> Result<(), String> {
    blocking(move || dictionary::add_replacement(&from, &to).map_err(|e| e.to_string())).await?
}

#[tauri::command]
fn get_hotkey(app: AppHandle) -> Hotkey {
    app.state::<AppContext>().current_hotkey.lock().clone()
}

#[tauri::command]
fn set_hotkey(app: AppHandle, hotkey: Hotkey) -> Result<Hotkey, String> {
    let new_shortcut = hotkey_to_shortcut(&hotkey)?;
    let ctx = app.state::<AppContext>();

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

    if let Err(e) = settings::Settings::update(|saved| {
        saved.hotkey = Some(hotkey.clone());
    }) {
        tracing::warn!("could not persist hotkey: {:?}", e);
    }

    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(tray_tooltip(&hotkey)));
    }

    tracing::info!("hotkey changed to: {}", hotkey.display());
    Ok(hotkey)
}

#[derive(serde::Serialize)]
struct CleanupConfig {
    backend: String,
    /// User's model selection (empty = backend default / first available).
    model: String,
    /// The model that would actually be sent.
    resolved_model: String,
    /// LM Studio base URL (always populated so the UI can prefill it).
    base_url: String,
}

/// May query the cleanup server (LM Studio with no model picked), so callers
/// run it via `blocking`.
fn cleanup_config(app: &AppHandle) -> CleanupConfig {
    let ctx = app.state::<AppContext>();
    let backend = ctx.cleaner.backend_name();
    let model = ctx.cleaner.model();
    let resolved_model = if !model.is_empty() {
        model.clone()
    } else {
        ctx.cleaner
            .list_models()
            .ok()
            .and_then(|m| m.into_iter().next())
            .map(|m| m.name)
            .unwrap_or_default()
    };
    CleanupConfig {
        backend,
        model,
        resolved_model,
        base_url: ctx.cleaner.base_url(),
    }
}

#[tauri::command]
async fn get_cleanup_config(app: AppHandle) -> Result<CleanupConfig, String> {
    blocking(move || cleanup_config(&app)).await
}

#[tauri::command]
async fn list_cleanup_models(app: AppHandle) -> Result<Vec<cleanup::ModelInfo>, String> {
    blocking(move || list_cleanup_models_blocking(&app)).await?
}

fn list_cleanup_models_blocking(app: &AppHandle) -> Result<Vec<cleanup::ModelInfo>, String> {
    let ctx = app.state::<AppContext>();
    let backend = ctx.cleaner.backend_name();
    ctx.cleaner.list_models().map_err(|e| {
        if backend == "lmstudio" {
            format!(
                "couldn't reach LM Studio at {}: {}",
                ctx.cleaner.base_url(),
                e
            )
        } else {
            format!("could not list Ollama models: {}", e)
        }
    })
}

#[tauri::command]
fn set_cleanup_model(app: AppHandle, model: String) -> Result<String, String> {
    let trimmed = model.trim();
    let ctx = app.state::<AppContext>();
    let backend = ctx.cleaner.backend_name();

    // Ollama always needs a model; LM Studio allows empty (= first available).
    if backend != "lmstudio" && trimmed.is_empty() {
        return Err("Enter an Ollama model first.".to_string());
    }

    let selected = trimmed.to_string();
    ctx.cleaner.set_model(selected.clone());

    if let Err(e) = settings::Settings::update(|saved| {
        if backend == "lmstudio" {
            saved.lmstudio_model = if selected.is_empty() {
                None
            } else {
                Some(selected.clone())
            };
        } else {
            saved.ollama_model = Some(selected.clone());
        }
    }) {
        tracing::warn!("could not persist cleanup model: {:?}", e);
    }

    tracing::info!("cleanup model ({}) changed to: {}", backend, selected);
    Ok(selected)
}

#[tauri::command]
async fn set_cleanup_backend(
    app: AppHandle,
    backend: String,
    base_url: Option<String>,
) -> Result<CleanupConfig, String> {
    blocking(move || set_cleanup_backend_blocking(&app, &backend, base_url)).await?
}

fn set_cleanup_backend_blocking(
    app: &AppHandle,
    backend: &str,
    base_url: Option<String>,
) -> Result<CleanupConfig, String> {
    let ctx = app.state::<AppContext>();

    let normalized = cleanup::normalize_backend(backend).to_string();
    let saved_now = settings::Settings::load();
    let lmstudio_base_url = base_url
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| saved_now.lmstudio_base_url_or_default());
    cleanup::lmstudio::validate_base_url(&lmstudio_base_url).map_err(|e| e.to_string())?;

    ctx.cleaner.set_backend(
        normalized.clone(),
        lmstudio_base_url.clone(),
        make_cleaner(&saved_now, &normalized, &lmstudio_base_url),
    );

    if let Err(e) = settings::Settings::update(|s| {
        s.cleanup_backend = Some(normalized.clone());
        s.lmstudio_base_url = Some(lmstudio_base_url.clone());
    }) {
        tracing::warn!("could not persist cleanup backend: {:?}", e);
    }
    tracing::info!("cleanup backend changed to: {}", normalized);

    spawn_cleaner_warmup(app.clone());
    Ok(cleanup_config(app))
}

#[derive(serde::Serialize)]
struct BuildInfo {
    version: &'static str,
    git_hash: &'static str,
}

#[tauri::command]
fn get_build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        git_hash: env!("DICTAFLOW_GIT_HASH"),
    }
}

#[derive(serde::Serialize)]
struct TranscriptionConfig {
    backend: String,
    /// User's model override (empty = provider default).
    model: String,
    /// The model that would actually be sent (resolved against provider default).
    resolved_model: String,
    /// Whether a cloud API key is stored. The raw key is NEVER sent to the webview.
    has_key: bool,
    /// Masked hint for display only (e.g. `gsk_…Ez77`), empty when no key is stored.
    api_key_hint: String,
}

/// Mask an API key for display: first 4 + ellipsis + last 4. Never reveals the middle.
fn mask_api_key(key: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "••••".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[tauri::command]
fn get_transcription_config(app: AppHandle) -> TranscriptionConfig {
    let ctx = app.state::<AppContext>();
    let cloud = ctx.cloud.read();
    TranscriptionConfig {
        backend: cloud.backend().as_str().to_string(),
        model: cloud.model_override().to_string(),
        resolved_model: cloud.resolved_model(),
        has_key: !cloud.api_key().is_empty(),
        api_key_hint: mask_api_key(cloud.api_key()),
    }
}

#[tauri::command]
async fn set_transcription_config(
    app: AppHandle,
    backend: String,
    api_key: String,
    model: String,
) -> Result<String, String> {
    blocking(move || set_transcription_config_blocking(&app, &backend, &api_key, &model)).await?
}

/// Touches the Keychain and builds an HTTP client, so it runs via `blocking`.
fn set_transcription_config_blocking(
    app: &AppHandle,
    backend: &str,
    api_key: &str,
    model: &str,
) -> Result<String, String> {
    let parsed = cloud_asr::Backend::parse(backend);
    // A blank key field means "keep the stored key" — never wipes an existing key.
    let provided_key = api_key.trim().to_string();
    let model = model.trim().to_string();

    let effective_key = if provided_key.is_empty() {
        secrets::get_transcription_api_key()
    } else {
        provided_key.clone()
    };

    if parsed.is_cloud() && effective_key.is_empty() {
        return Err(format!(
            "Enter an API key to use the {} backend.",
            parsed.as_str()
        ));
    }

    let ctx = app.state::<AppContext>();
    let was_cloud = ctx.cloud.read().is_cloud();

    let transcriber =
        cloud_asr::CloudTranscriber::new(parsed, effective_key.clone(), model.clone());
    let resolved = transcriber.resolved_model();
    *ctx.cloud.write() = transcriber;

    // Only touch stored storage when the user actually typed a new key.
    if !provided_key.is_empty() {
        if let Err(e) = secrets::set_transcription_api_key(&provided_key) {
            tracing::warn!("could not store transcription API key: {:?}", e);
        }
    }
    let backend_str = parsed.as_str().to_string();
    let model_opt = if model.is_empty() { None } else { Some(model) };
    #[cfg(not(target_os = "macos"))]
    let key_opt = if effective_key.is_empty() {
        None
    } else {
        Some(effective_key.clone())
    };
    if let Err(e) = settings::Settings::update(|s| {
        s.transcription_backend = Some(backend_str.clone());
        s.transcription_model = model_opt.clone();
        #[cfg(not(target_os = "macos"))]
        {
            s.transcription_api_key = key_opt.clone();
        }
    }) {
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
    pipeline::current_language_mode().as_str().to_string()
}

#[tauri::command]
fn set_transcription_language(language: String) -> Result<String, String> {
    let mode = settings::LanguageMode::parse(&language);
    settings::Settings::update(|s| s.transcription_language = Some(mode.as_str().to_string()))
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
fn get_cursor_hud_status() -> String {
    hud_cursor::tracking_status().to_string()
}

#[tauri::command]
fn get_launch_at_login() -> bool {
    login_item::is_enabled()
}

#[tauri::command]
async fn set_launch_at_login(enabled: bool) -> Result<bool, String> {
    blocking(move || set_launch_at_login_blocking(enabled)).await?
}

fn set_launch_at_login_blocking(enabled: bool) -> Result<bool, String> {
    login_item::set_enabled(enabled).map_err(|e| format!("could not update login item: {}", e))?;

    if let Err(e) = settings::Settings::update(|saved| {
        saved.launch_at_login = enabled;
    }) {
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
    let style = hud_cursor::normalize_hud_style(&style);
    set_hud_style_cache(&style);
    settings::Settings::update(|saved| {
        saved.hud_style = Some(style.clone());
    })
    .map_err(|e| format!("save: {}", e))?;
    hud_cursor::stop_follow(&app);
    if let Some(hud) = app.get_webview_window("hud") {
        position_hud(&hud);
        set_hud_draggable(&app, &hud, !hud_cursor::is_cursor_style(&style));
        let _ = hud.emit("hud_style", &style);
    }
    if hud_cursor::is_cursor_style(&style) {
        if current_state(&app) == AppState::Recording {
            hud_cursor::on_recording_start(&app);
        } else {
            hud_cursor::on_recording_end(&app);
        }
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
    refresh_hud_elevation(&app);
    Ok(enabled)
}

#[tauri::command]
fn reset_hud_position(app: AppHandle) -> Result<(), String> {
    clear_saved_hud_placement();
    if let Some(hud) = app.get_webview_window("hud") {
        position_hud(&hud);
        refresh_hud_elevation(&app);
    }
    Ok(())
}

/// Minimum spacing between "rms" events sent to the webview. The audio
/// callback produces ~100 readings/s; the HUD meter only needs ~20/s.
const RMS_EMIT_INTERVAL: Duration = Duration::from_millis(50);

fn spawn_rms_forwarder(app: AppHandle, rx: mpsc::Receiver<f32>) {
    thread::spawn(move || {
        let mut last_emit: Option<Instant> = None;
        while let Ok(level) = rx.recv() {
            if let Some(t) = last_emit {
                let elapsed = t.elapsed();
                if elapsed < RMS_EMIT_INTERVAL {
                    thread::sleep(RMS_EMIT_INTERVAL - elapsed);
                }
            }
            // Fold everything that queued up while we waited into one peak.
            let level = audio::drain_peak(&rx, level);
            emit_hud(&app, "rms", level);
            last_emit = Some(Instant::now());
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

        // The cleanup LLM runs regardless of which transcription backend is used.
        spawn_cleaner_warmup(app);
    });
}

/// Load the cleanup model into memory in the background so the first dictation is fast.
fn spawn_cleaner_warmup(app: AppHandle) {
    thread::spawn(move || {
        if let Some(ctx) = app.try_state::<AppContext>() {
            ctx.cleaner.warmup();
        }
    });
}

/// Downloads (if needed), loads, and prewarms the local Whisper model in memory.
fn load_local_whisper(app: &AppHandle) {
    if !asr::is_model_downloaded() {
        tracing::info!("whisper model not found locally; downloading (~1.5 GB)");
        MODEL_DOWNLOADING.store(true, Ordering::Relaxed);
        let app_for_progress = app.clone();
        let _ = app_for_progress.emit("model_download_start", ());
        let _ = app_for_progress.emit("model_download_progress", 0u32);
        let last_pct = std::cell::Cell::new(0u32);

        let download_result = asr::download_model(|done, total| {
            if total == 0 {
                return;
            }
            let pct = ((done as f64 / total as f64) * 100.0).min(100.0) as u32;
            if pct != last_pct.get() || done == total {
                last_pct.set(pct);
                tracing::info!("whisper download: {}% ({} / {})", pct, done, total);
                let _ = app_for_progress.emit("model_download_progress", pct);
            }
        });

        MODEL_DOWNLOADING.store(false, Ordering::Relaxed);
        match download_result {
            Ok(_) => {
                tracing::info!("whisper model download complete");
                let _ = app.emit("model_download_progress", 100u32);
                let _ = app.emit("model_download_complete", ());
            }
            Err(e) => {
                tracing::error!("whisper download failed: {:?}", e);
                let _ = app.emit("model_download_failed", e.to_string());
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

fn prepare_shutdown(app: &AppHandle) {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    hud_cursor::stop_follow(app);
    pipeline::cancel_partial_worker(app);
    // Brief pause so follow/partial threads observe the cancel flag before teardown.
    thread::sleep(Duration::from_millis(100));
}
