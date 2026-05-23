#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(unexpected_cfgs)]

mod asr;
mod audio;
mod cleanup;
mod history;
mod login_item;
mod paste;
mod settings;
mod state;

use std::fs::{File, OpenOptions};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use std::str::FromStr;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Runtime,
    WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::audio::{AudioController, PcmBuffer, MIN_DURATION_SECS};
use crate::cleanup::HybridCleaner;
use crate::settings::{Hotkey, HudAnchor, HudPosition, DEFAULT_OLLAMA_MODEL};
use crate::state::AppState;

const HUD_COMPACT_W: f64 = 64.0;
const HUD_COMPACT_H: f64 = 36.0;
const HUD_EXPANDED_W: f64 = 200.0;
const HUD_EXPANDED_H: f64 = HUD_COMPACT_H;
const PARTIAL_MIN_SECS: f32 = 0.45;
const PARTIAL_POLL_MS: u64 = 350;
const PARTIAL_NEW_AUDIO_SECS: f32 = 0.12;
const PARTIAL_PREVIEW_CHARS: usize = 96;

static HUD_LAST_PLACED: Mutex<Option<Instant>> = Mutex::new(None);

pub struct AppContext {
    pub audio: AudioController,
    pub cleaner: HybridCleaner,
    pub asr: Mutex<Option<Arc<asr::Transcriber>>>,
    pub state: Mutex<AppState>,
    pub current_shortcut: Mutex<Shortcut>,
    pub current_hotkey: Mutex<Hotkey>,
    pub partial_cancel: Mutex<Option<Arc<AtomicBool>>>,
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
            close_history,
            search_history,
            delete_history_entry,
            clear_history,
            copy_text,
            get_hud_style,
            set_hud_style,
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
                partial_cancel: Mutex::new(None),
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
        .always_on_top(true)
        .resizable(false)
        .skip_taskbar(true)
        .focused(false)
        .shadow(false)
        .build()?;

    position_hud(&window);
    persist_legacy_hud_anchor(&window);
    let _ = window.emit("hud_style", style.as_str());
    #[cfg(target_os = "macos")]
    raise_hud_level(&window);
    set_hud_draggable(&window, true);

    let app_clone = app.clone();
    window.on_window_event(move |event| {
        match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                if let Some(w) = app_clone.get_webview_window("hud") {
                    let _ = w.hide();
                }
            }
            WindowEvent::Moved(_) => {
                if hud_move_should_persist() {
                    if let Some(hud) = app_clone.get_webview_window("hud") {
                        save_hud_position_from_window(&hud);
                    }
                }
            }
            _ => {}
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

#[cfg(target_os = "macos")]
fn activate_application() {
    use cocoa::base::{id, YES};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let ns_app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: YES];
    }
}

#[cfg(target_os = "macos")]
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
        if matches!(event, WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed) {
            restore_menu_bar_activation_policy(&app_for_close);
        }
    });

    present_auxiliary_window(app, &window);
    Ok(())
}

fn hud_style() -> String {
    settings::Settings::load()
        .hud_style
        .unwrap_or_else(|| "notch".to_string())
}

fn hud_size(style: &str) -> (f64, f64) {
    if style == "notch" {
        (HUD_EXPANDED_W, HUD_COMPACT_H)
    } else {
        (110.0, 40.0)
    }
}

fn mark_hud_placed_programmatically() {
    *HUD_LAST_PLACED.lock() = Some(Instant::now());
}

fn hud_move_should_persist() -> bool {
    HUD_LAST_PLACED
        .lock()
        .map(|t| t.elapsed() > Duration::from_millis(350))
        .unwrap_or(true)
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
    let compact_w = (HUD_COMPACT_W * scale).round() as i32;
    let center_x = size.width as i32 / 2;
    let notch_half_physical = (100.0 * scale).round() as i32;
    let gap_physical = (6.0 * scale).round() as i32;
    let compact_x = center_x - notch_half_physical - gap_physical - compact_w;
    HudAnchor {
        right: compact_x + compact_w,
        top: (2.0 * scale).round() as i32,
    }
}

fn position_hud(window: &tauri::WebviewWindow) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let settings = settings::Settings::load();
    let style = settings.hud_style.as_deref().unwrap_or("notch");

    if style == "classic" {
        let (w, h) = hud_size("classic");
        let w_phys = (w * scale).round() as i32;
        let h_phys = (h * scale).round() as i32;
        let _ = window.set_size(LogicalSize::new(w, h));
        if let Some(pos) = settings.hud_position {
            let _ = window.set_position(PhysicalPosition::new(pos.x, pos.y));
            mark_hud_placed_programmatically();
            return;
        }
        let Some(monitor) = window.current_monitor().ok().flatten() else {
            return;
        };
        let size = monitor.size();
        let x = (size.width as i32 - w_phys) / 2;
        let y = size.height as i32 - h_phys - (120.0 * scale).round() as i32;
        let _ = window.set_position(PhysicalPosition::new(x, y));
        mark_hud_placed_programmatically();
        return;
    }

    let expanded_w_phys = (HUD_EXPANDED_W * scale).round() as i32;
    let anchor = resolved_hud_anchor(&settings, scale).or_else(|| {
        window
            .current_monitor()
            .ok()
            .flatten()
            .map(|monitor| default_hud_anchor(&monitor, scale))
    });

    if let Some(anchor) = anchor {
        let x = anchor.right - expanded_w_phys;
        let _ = window.set_position(PhysicalPosition::new(x, anchor.top));
    }
    let _ = window.set_size(LogicalSize::new(HUD_EXPANDED_W, HUD_COMPACT_H));
    mark_hud_placed_programmatically();
}

fn persist_legacy_hud_anchor(window: &tauri::WebviewWindow) {
    let mut saved = settings::Settings::load();
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
    let mut saved = settings::Settings::load();
    let style = saved.hud_style.as_deref().unwrap_or("notch");
    let compact_w = (HUD_COMPACT_W * scale).round() as i32;
    let expanded_w = (HUD_EXPANDED_W * scale).round() as i32;

    if style == "classic" {
        saved.hud_position = Some(HudPosition {
            x: outer.x,
            y: outer.y,
        });
        saved.hud_anchor = None;
    } else {
        let anchor = HudAnchor {
            right: outer.x + expanded_w,
            top: outer.y,
        };
        let unchanged = saved
            .hud_anchor
            .is_some_and(|a| a.right == anchor.right && a.top == anchor.top);
        if unchanged {
            return;
        }
        saved.hud_anchor = Some(anchor);
        saved.hud_position = Some(HudPosition {
            x: anchor.right - compact_w,
            y: anchor.top,
        });
    }

    if let Err(e) = saved.save() {
        tracing::warn!("could not save hud position: {:?}", e);
    } else if style == "notch" {
        if let Some(anchor) = saved.hud_anchor {
            tracing::info!(
                "hud anchor saved: right={}, top={}",
                anchor.right,
                anchor.top
            );
        }
    }
}

fn expand_hud(app: &AppHandle) {
    emit_hud(app, "hud_layout", "expanded");
    emit_hud(app, "partial", "Listening…");
}

fn compact_hud(app: &AppHandle) {
    emit_hud(app, "partial", "");
    emit_hud(app, "hud_layout", "compact");
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
        820.0,
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
    let mut saved = settings::Settings::load();
    saved.hud_style = Some(style.clone());
    saved.hud_position = None;
    saved.hud_anchor = None;
    saved.save().map_err(|e| format!("save: {}", e))?;
    if let Some(hud) = app.get_webview_window("hud") {
        position_hud(&hud);
        let _ = hud.emit("hud_style", &style);
    }
    Ok(style)
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

        let app_warm = app.clone();
        thread::spawn(move || {
            if let Some(ctx) = app_warm.try_state::<AppContext>() {
                ctx.cleaner.warmup();
            }
        });
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

fn transcribe(ctx: &AppContext, pcm: &PcmBuffer) -> anyhow::Result<String> {
    let transcriber = {
        let guard = ctx.asr.lock();
        guard.clone()
    };
    match transcriber {
        Some(t) => t.transcribe(&pcm.samples),
        None => {
            tracing::warn!("whisper not ready yet");
            Err(anyhow::anyhow!("whisper model still loading, please try again"))
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

                let partial = match transcriber.transcribe_partial(&samples) {
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
