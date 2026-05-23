use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use tauri::AppHandle;

pub fn paste(app: &AppHandle, text: &str, target_pid: Option<i32>) -> Result<()> {
    let text = text.to_string();
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let result = paste_on_main(&text, target_pid);
        let _ = tx.send(result);
    })
    .map_err(|e| anyhow!("schedule main-thread paste: {}", e))?;

    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|e| anyhow!("paste worker timed out: {}", e))?
}

pub fn copy_only(app: &AppHandle, text: &str) -> Result<()> {
    let text = text.to_string();
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let result = Clipboard::new()
            .context("clipboard init")
            .and_then(|mut cb| cb.set_text(text).context("clipboard write"));
        let _ = tx.send(result);
    })
    .map_err(|e| anyhow!("schedule main-thread copy: {}", e))?;

    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|e| anyhow!("copy worker timed out: {}", e))?
}

#[cfg(target_os = "macos")]
pub fn capture_frontmost_pid() -> Option<i32> {
    macos::capture_frontmost_pid()
}

#[cfg(not(target_os = "macos"))]
pub fn capture_frontmost_pid() -> Option<i32> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteOutcome {
    Confirmed,
    Unverified,
    Failed,
}

fn paste_on_main(text: &str, target_pid: Option<i32>) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let before = macos::read_focused_text(target_pid);

        // Only use AX when we can confirm text landed (Cursor, Notes, etc.).
        // Never stop on Unverified — that broke Electron apps like Telegram.
        if macos::try_ax_insert_text(text, target_pid)
            && macos::measure_paste_outcome(text, before.as_deref(), target_pid)
                == PasteOutcome::Confirmed
        {
            tracing::info!("paste via accessibility insert ({})", text.len());
            return Ok(());
        }

        let mut clipboard = Clipboard::new().context("clipboard init")?;
        let previous = clipboard.get_text().ok();
        clipboard
            .set_text(text.to_string())
            .context("clipboard write")?;

        // Original path that worked in Telegram/Outlook before the paste rework.
        thread::sleep(Duration::from_millis(30));
        synth_paste_enigo().context("synth paste")?;
        thread::sleep(Duration::from_millis(200));

        if let Some(prev) = &previous {
            let _ = clipboard.set_text(prev.clone());
        }

        let outcome = macos::measure_paste_outcome(text, before.as_deref(), target_pid);
        if outcome == PasteOutcome::Failed {
            tracing::debug!("clipboard paste did not stick; trying direct typing");
            macos::type_text_directly(text).context("direct typing")?;
            tracing::info!("paste via direct typing ({})", text.len());
        } else {
            tracing::info!("paste via clipboard+cmd+v ({}, {:?})", text.len(), outcome);
        }

        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = target_pid;
        let mut clipboard = Clipboard::new().context("clipboard init")?;
        let previous = clipboard.get_text().ok();
        clipboard
            .set_text(text.to_string())
            .context("clipboard write")?;
        thread::sleep(Duration::from_millis(30));
        synth_paste_enigo().context("synth paste")?;
        thread::sleep(Duration::from_millis(50));
        if let Some(prev) = previous {
            let _ = clipboard.set_text(prev);
        }
        Ok(())
    }
}

fn synth_paste_enigo() -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default()).context("enigo init")?;

    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    enigo.key(modifier, Direction::Press)?;
    enigo.key(Key::Unicode('v'), Direction::Click)?;
    enigo.key(modifier, Direction::Release)?;
    Ok(())
}

fn classify_paste_result(
    expected: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> PasteOutcome {
    let expected = expected.trim();
    if expected.is_empty() {
        return PasteOutcome::Confirmed;
    }

    let Some(after) = after else {
        return PasteOutcome::Unverified;
    };
    let after = after.trim();

    if !after.contains(expected) {
        return PasteOutcome::Failed;
    }

    if let Some(before) = before.map(str::trim) {
        if after == before {
            return PasteOutcome::Failed;
        }
        if after.len() <= before.len() && after.ends_with(expected) {
            return PasteOutcome::Failed;
        }
    }

    PasteOutcome::Confirmed
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod macos {
    use std::ffi::c_void;

    use anyhow::{Context, Result};
    use core_foundation::base::{CFTypeRef, TCFType};
    use core_foundation::string::CFString;
    use enigo::{Enigo, Keyboard, Settings};
    use objc::{class, msg_send, sel, sel_impl};
    use std::thread;
    use std::time::Duration;

    use super::{classify_paste_result, PasteOutcome};

    const AX_ERROR_SUCCESS: i32 = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> *mut c_void;
        fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
        fn AXUIElementCopyAttributeValue(
            element: *mut c_void,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXUIElementSetAttributeValue(
            element: *mut c_void,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> i32;
    }

    type CFStringRef = *const core_foundation::string::__CFString;

    pub fn capture_frontmost_pid() -> Option<i32> {
        use cocoa::base::id;

        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front: id = msg_send![workspace, frontmostApplication];
            if front.is_null() {
                return None;
            }
            let pid: i32 = msg_send![front, processIdentifier];
            let self_pid = std::process::id() as i32;
            if pid == self_pid || pid <= 0 {
                None
            } else {
                Some(pid)
            }
        }
    }

    pub fn read_focused_text(target_pid: Option<i32>) -> Option<String> {
        focused_element(target_pid).and_then(|element| read_element_text(element))
    }

    pub fn measure_paste_outcome(
        expected: &str,
        before: Option<&str>,
        target_pid: Option<i32>,
    ) -> PasteOutcome {
        thread::sleep(Duration::from_millis(80));
        let after = read_focused_text(target_pid);
        classify_paste_result(expected, before, after.as_deref())
    }

    pub fn try_ax_insert_text(text: &str, target_pid: Option<i32>) -> bool {
        if text.is_empty() {
            return false;
        }

        let Some(focused) = focused_element(target_pid) else {
            return false;
        };

        let text_cf = CFString::new(text);

        unsafe {
            let selected_attr = CFString::new("AXSelectedText");
            AXUIElementSetAttributeValue(
                focused,
                selected_attr.as_concrete_TypeRef() as CFStringRef,
                text_cf.as_concrete_TypeRef() as CFTypeRef,
            ) == AX_ERROR_SUCCESS
        }
    }

    pub fn type_text_directly(text: &str) -> Result<()> {
        let mut enigo = Enigo::new(&Settings::default()).context("enigo init")?;
        enigo.text(text).context("enigo text")?;
        Ok(())
    }

    fn focused_element(target_pid: Option<i32>) -> Option<*mut c_void> {
        unsafe {
            if let Some(pid) = target_pid {
                if let Some(element) = focused_element_for_root(AXUIElementCreateApplication(pid)) {
                    return Some(element);
                }
            }
            focused_element_for_root(AXUIElementCreateSystemWide())
        }
    }

    unsafe fn focused_element_for_root(root: *mut c_void) -> Option<*mut c_void> {
        if root.is_null() {
            return None;
        }
        let focused_attr = CFString::new("AXFocusedUIElement");
        let mut focused: CFTypeRef = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(
            root,
            focused_attr.as_concrete_TypeRef() as CFStringRef,
            &mut focused,
        ) != AX_ERROR_SUCCESS
            || focused.is_null()
        {
            return None;
        }
        Some(focused as *mut c_void)
    }

    fn read_element_text(element: *mut c_void) -> Option<String> {
        unsafe {
            for attr in ["AXValue", "AXSelectedText"] {
                let attr_cf = CFString::new(attr);
                let mut value: CFTypeRef = std::ptr::null_mut();
                if AXUIElementCopyAttributeValue(
                    element,
                    attr_cf.as_concrete_TypeRef() as CFStringRef,
                    &mut value,
                ) != AX_ERROR_SUCCESS
                    || value.is_null()
                {
                    continue;
                }
                if let Some(text) = cf_type_to_string(value) {
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
        None
    }

    unsafe fn cf_type_to_string(value: CFTypeRef) -> Option<String> {
        let cf_string = CFString::wrap_under_get_rule(value as CFStringRef);
        Some(cf_string.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_paste_result, PasteOutcome};

    #[test]
    fn confirmed_when_text_appears() {
        assert_eq!(
            classify_paste_result("hello", Some(""), Some("hello")),
            PasteOutcome::Confirmed
        );
    }

    #[test]
    fn unverified_when_field_is_not_readable() {
        assert_eq!(
            classify_paste_result("hello", Some(""), None),
            PasteOutcome::Unverified
        );
    }

    #[test]
    fn failed_when_text_unchanged() {
        assert_eq!(
            classify_paste_result("hello", Some("hello"), Some("hello")),
            PasteOutcome::Failed
        );
    }
}
