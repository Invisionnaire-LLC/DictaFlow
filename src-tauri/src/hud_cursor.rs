use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition, WebviewWindow};

pub const STYLE: &str = "cursor";

const CURSOR_OFFSET_X: f64 = 16.0;
const CURSOR_OFFSET_Y: f64 = -10.0;
const FOLLOW_POLL_MS: u64 = 33;
const HUD_W_LOGICAL: f64 = 110.0;
const HUD_H_LOGICAL: f64 = 40.0;

pub fn normalize_hud_style(style: &str) -> String {
    match style {
        "classic" => "classic".to_string(),
        STYLE => STYLE.to_string(),
        _ => "notch".to_string(),
    }
}

pub fn uses_waveform(style: &str) -> bool {
    style == "classic" || style == STYLE
}

pub fn is_cursor_style(style: &str) -> bool {
    style == STYLE
}

pub fn position_hud_at_cursor(window: &WebviewWindow) {
    let Some((mouse_x, mouse_y, scale)) = cursor_monitor_state(window) else {
        tracing::warn!("cursor hud: mouse position unavailable; pill may appear in wrong place");
        return;
    };

    let (w_phys, h_phys) = hud_size_physical(scale);
    let (x, y) = cursor_window_origin(mouse_x, mouse_y, w_phys, h_phys, scale, |x, y| {
        monitor_bounds_at(window, x, y)
    });

    let _ = window.set_position(PhysicalPosition::new(x, y));
    crate::mark_hud_placed_programmatically(window);
}

/// Startup sanity check — logs whether cursor tracking is likely to work.
pub fn log_tracking_readiness(window: &WebviewWindow) {
    match cursor_monitor_state(window) {
        Some((x, y, scale)) => {
            tracing::info!("cursor hud ready (mouse at physical {x}, {y}, scale {scale})");
        }
        None => {
            tracing::warn!(
                "cursor hud: mouse tracking unavailable at startup; \
                 pill will not follow cursor until this resolves"
            );
        }
    }
}

pub fn show(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(hud) = app.get_webview_window("hud") {
            position_hud_at_cursor(&hud);
            let _ = hud.show();
        }
    });
}

pub fn hide(app: &AppHandle) {
    let app = app.clone();
    crate::main_thread::run_void(app.clone(), move || {
        if let Some(hud) = app.get_webview_window("hud") {
            let _ = hud.hide();
        }
    });
}

pub fn on_recording_start(app: &AppHandle) {
    show(app);
    start_follow(app);
}

pub fn on_recording_end(app: &AppHandle) {
    stop_follow(app);
    hide(app);
}

pub fn start_follow(app: &AppHandle) {
    stop_follow(app);
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(ctx) = app.try_state::<crate::AppContext>() {
        *ctx.hud_follow_cancel.lock() = Some(cancel.clone());
    }

    if let Some(hud) = app.get_webview_window("hud") {
        let app = app.clone();
        crate::main_thread::run_void(app.clone(), move || position_hud_at_cursor(&hud));
    }

    let app = app.clone();
    thread::spawn(move || {
        while !cancel.load(Ordering::Relaxed) {
            let active = app
                .try_state::<crate::AppContext>()
                .map(|ctx| *ctx.state.lock() == crate::state::AppState::Recording)
                .unwrap_or(false);
            if !active {
                break;
            }
            if let Some(hud) = app.get_webview_window("hud") {
                let app = app.clone();
                let _ = app
                    .clone()
                    .run_on_main_thread(move || position_hud_at_cursor(&hud));
            }
            thread::sleep(Duration::from_millis(FOLLOW_POLL_MS));
        }
    });
}

pub fn stop_follow(app: &AppHandle) {
    if let Some(ctx) = app.try_state::<crate::AppContext>() {
        if let Some(cancel) = ctx.hud_follow_cancel.lock().take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn hud_size_physical(scale: f64) -> (i32, i32) {
    (
        (HUD_W_LOGICAL * scale).round() as i32,
        (HUD_H_LOGICAL * scale).round() as i32,
    )
}

fn cursor_window_origin(
    mouse_x: i32,
    mouse_y: i32,
    w_phys: i32,
    h_phys: i32,
    scale: f64,
    monitor_bounds: impl FnOnce(i32, i32) -> Option<(i32, i32, i32, i32)>,
) -> (i32, i32) {
    let mut x = mouse_x + (CURSOR_OFFSET_X * scale).round() as i32;
    let mut y = mouse_y + (CURSOR_OFFSET_Y * scale).round() as i32;

    if let Some((mx, my, mw, mh)) = monitor_bounds(mouse_x, mouse_y) {
        x = x.clamp(mx, mx + mw.saturating_sub(w_phys));
        y = y.clamp(my, my + mh.saturating_sub(h_phys));
    }

    (x, y)
}

fn monitor_bounds_at(window: &WebviewWindow, x: i32, y: i32) -> Option<(i32, i32, i32, i32)> {
    let monitors = window.app_handle().available_monitors().ok()?;
    for monitor in &monitors {
        let pos = monitor.position();
        let size = monitor.size();
        let w = size.width as i32;
        let h = size.height as i32;
        if x >= pos.x && x < pos.x + w && y >= pos.y && y < pos.y + h {
            return Some((pos.x, pos.y, w, h));
        }
    }
    monitors.first().map(|monitor| {
        let pos = monitor.position();
        let size = monitor.size();
        (pos.x, pos.y, size.width as i32, size.height as i32)
    })
}

/// Mouse in Tauri/wry physical coordinates plus the active monitor scale.
fn cursor_monitor_state(window: &WebviewWindow) -> Option<(i32, i32, f64)> {
    let (lx, ly) = tao_logical_mouse()?;
    let monitors = window.app_handle().available_monitors().ok()?;
    let monitor = monitor_at_tao_logical(&monitors, lx, ly)?;
    let scale = monitor.scale_factor();
    Some((
        (lx * scale).round() as i32,
        (ly * scale).round() as i32,
        scale,
    ))
}

fn monitor_at_tao_logical(monitors: &[tauri::Monitor], x: f64, y: f64) -> Option<tauri::Monitor> {
    for monitor in monitors {
        let scale = monitor.scale_factor();
        let pos = monitor.position();
        let size = monitor.size();
        let left = pos.x as f64 / scale;
        let top = pos.y as f64 / scale;
        let w = size.width as f64 / scale;
        let h = size.height as f64 / scale;
        if x >= left && x < left + w && y >= top && y < top + h {
            return Some(monitor.clone());
        }
    }
    monitors.first().cloned()
}

/// Global mouse in Tauri's logical desktop space (top-left origin, y down).
#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
fn tao_logical_mouse() -> Option<(f64, f64)> {
    use cocoa::foundation::NSPoint;
    use core_graphics::display::CGDisplay;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let point: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let main_h = CGDisplay::main().pixels_high() as f64;
        Some((point.x, main_h - point.y))
    }
}

#[cfg(not(target_os = "macos"))]
fn tao_logical_mouse() -> Option<(f64, f64)> {
    None
}

#[cfg(test)]
fn tao_logical_to_physical(lx: f64, ly: f64, scale: f64) -> (i32, i32) {
    ((lx * scale).round() as i32, (ly * scale).round() as i32)
}

#[cfg(test)]
fn tao_logical_from_cocoa(cocoa_x: f64, cocoa_y: f64, main_height: f64) -> (f64, f64) {
    (cocoa_x, main_height - cocoa_y)
}

pub fn position_cursor_hud(window: &WebviewWindow) {
    let _ = window.set_size(LogicalSize::new(HUD_W_LOGICAL, HUD_H_LOGICAL));
    position_hud_at_cursor(window);
}

pub fn set_cursor_hud_size(window: &WebviewWindow, expanded: bool) {
    let width = if expanded { 200.0 } else { HUD_W_LOGICAL };
    let _ = window.set_size(LogicalSize::new(width, HUD_H_LOGICAL));
    position_hud_at_cursor(window);
}

pub fn tracking_status() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        if tao_logical_mouse().is_some() {
            "ready"
        } else {
            "unavailable"
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        "unsupported"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tao_logical_flips_cocoa_y_against_main_display_height() {
        assert_eq!(
            tao_logical_from_cocoa(2491.0, 1155.0, 1117.0),
            (2491.0, -38.0)
        );
    }

    #[test]
    fn tao_logical_to_physical_uses_active_monitor_scale() {
        assert_eq!(tao_logical_to_physical(2491.0, -38.0, 1.0), (2491, -38));
        assert_eq!(tao_logical_to_physical(110.0, 55.0, 2.0), (220, 110));
    }

    #[test]
    fn cursor_origin_applies_offset_and_clamps_to_monitor() {
        let (x, y) = cursor_window_origin(500, 400, 110, 40, 1.0, |_, _| Some((0, 0, 800, 600)));
        assert_eq!(x, 516);
        assert_eq!(y, 390);

        let (x, y) = cursor_window_origin(790, 580, 110, 40, 1.0, |_, _| Some((0, 0, 800, 600)));
        assert_eq!(x, 690);
        assert_eq!(y, 560);
    }
}
