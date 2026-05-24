use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use tauri::AppHandle;

#[cfg(target_os = "macos")]
pub fn read_focused_field_text(target_pid: Option<i32>) -> Option<String> {
    macos::read_focused_text(target_pid)
}

#[cfg(not(target_os = "macos"))]
pub fn read_focused_field_text(_target_pid: Option<i32>) -> Option<String> {
    None
}

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
        macos::ensure_target_active(target_pid);

        let before = macos::read_focused_text(target_pid);
        let field_unreadable = before.is_none();
        // Terminals accept ⌘V reliably but their AX text can't be verified, so
        // running AX insert and/or the typing fallback paints the text twice.
        let is_terminal = macos::is_terminal_pid(target_pid);

        // Only use AX insert when the field is readable (Cursor, Notes, etc.).
        // Electron apps (Telegram, Slack) often report AX success without inserting,
        // which blocks the clipboard fallback. Never stop on Unverified for those.
        if !field_unreadable
            && !is_terminal
            && macos::try_ax_insert_text(text, target_pid)
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

        thread::sleep(Duration::from_millis(30));
        macos::synth_paste_cgevent(target_pid).context("synth paste")?;
        thread::sleep(Duration::from_millis(250));

        let mut outcome = macos::measure_paste_outcome(text, before.as_deref(), target_pid);
        if !is_terminal
            && (outcome == PasteOutcome::Failed
                || (outcome == PasteOutcome::Unverified
                    && field_unreadable
                    && macos::is_browser_pid(target_pid)))
        {
            tracing::debug!("clipboard paste unconfirmed in browser; trying direct typing");
            macos::type_text_directly(text).context("direct typing")?;
            outcome = macos::measure_paste_outcome(text, before.as_deref(), target_pid);
            tracing::info!("paste via direct typing ({}, {:?})", text.len(), outcome);
        } else {
            tracing::info!("paste via clipboard+cmd+v ({}, {:?})", text.len(), outcome);
        }

        restore_clipboard_unless_confirmed(&mut clipboard, previous.as_deref(), outcome);

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
        // No reliable field verification off macOS — leave transcript on clipboard.
        restore_clipboard_unless_confirmed(
            &mut clipboard,
            previous.as_deref(),
            PasteOutcome::Unverified,
        );
        Ok(())
    }
}

fn restore_clipboard_unless_confirmed(
    clipboard: &mut Clipboard,
    previous: Option<&str>,
    outcome: PasteOutcome,
) {
    if should_restore_previous_clipboard(outcome) {
        if let Some(prev) = previous {
            let _ = clipboard.set_text(prev.to_string());
        }
        return;
    }

    tracing::info!(
        "paste {:?}; transcript left on clipboard for manual paste",
        outcome
    );
}

fn should_restore_previous_clipboard(outcome: PasteOutcome) -> bool {
    outcome == PasteOutcome::Confirmed
}

#[cfg(not(target_os = "macos"))]
fn synth_paste_enigo() -> Result<()> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};

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

/// Press Enter/Return (used by the "auto-submit after dictation" option).
pub fn press_enter(app: &AppHandle) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(press_enter_sync());
    })
    .map_err(|e| anyhow!("schedule main-thread enter: {}", e))?;

    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|e| anyhow!("enter worker timed out: {}", e))?
}

fn press_enter_sync() -> Result<()> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};

    let mut enigo = Enigo::new(&Settings::default()).context("enigo init")?;
    enigo.key(Key::Return, Direction::Click)?;
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

pub fn try_learn_from_field_edit(
    pasted: &str,
    before: Option<&str>,
    after: &str,
) -> Result<Vec<crate::dictionary::LearnedUpdate>> {
    let pasted = pasted.trim();
    let after = after.trim();
    if pasted.is_empty() || after.is_empty() || pasted == after {
        return Ok(Vec::new());
    }

    let before = before.map(str::trim).unwrap_or("");
    let post_paste = synthetic_after_paste(before, pasted);
    if post_paste.trim() == after {
        return Ok(Vec::new());
    }

    if tokenize_words(&post_paste).len() == tokenize_words(after).len() {
        let learned = crate::dictionary::learn_from_edit(&post_paste, after)?;
        if !learned.is_empty() {
            return Ok(learned);
        }
    }

    if !before.is_empty() {
        if let Some(rest) = after.strip_prefix(before) {
            let suffix = rest.trim();
            if !suffix.is_empty() && suffix != pasted {
                return crate::dictionary::learn_from_edit(pasted, suffix);
            }
        }
    }

    if let Ok(learned) = crate::dictionary::learn_pasted_in_context(pasted, after) {
        if !learned.is_empty() {
            return Ok(learned);
        }
    }

    if !before.is_empty() && after.starts_with(before) {
        let suffix = after[before.len()..].trim();
        if !suffix.is_empty() && suffix != pasted {
            return crate::dictionary::learn_from_edit(pasted, suffix);
        }
    }

    if before.is_empty() && after.starts_with(pasted) {
        return crate::dictionary::learn_from_edit(pasted, after);
    }

    Ok(Vec::new())
}

fn tokenize_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn synthetic_after_paste(before: &str, pasted: &str) -> String {
    if before.is_empty() {
        return pasted.to_string();
    }
    if before.ends_with(pasted) {
        return before.to_string();
    }
    if before.ends_with(' ') || pasted.starts_with(|c: char| c.is_ascii_punctuation()) {
        format!("{before}{pasted}")
    } else {
        format!("{before} {pasted}")
    }
}

pub fn spawn_edit_learning_watch(
    app: AppHandle,
    pasted: String,
    before: Option<String>,
    target_pid: Option<i32>,
) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1500));
        let mut stable: Option<String> = None;
        let mut stable_count = 0;
        for attempt in 0..30 {
            let current = read_focused_field_text(target_pid);
            if let Some(field) = current {
                let trimmed = field.trim().to_string();
                if stable.as_deref() == Some(trimmed.as_str()) {
                    stable_count += 1;
                    if stable_count >= 2 {
                        match try_learn_from_field_edit(&pasted, before.as_deref(), &trimmed) {
                            Ok(learned) if !learned.is_empty() => {
                                for update in learned {
                                    tracing::info!(
                                        "learned replacement: {} -> {}",
                                        update.from,
                                        update.to
                                    );
                                    crate::hud_cursor::notify_word_learned(&app, &update);
                                }
                            }
                            Ok(_) => {
                                tracing::debug!(
                                    "edit watch: no learnable diff (pasted={:?}, after={:?})",
                                    pasted,
                                    trimmed
                                );
                            }
                            Err(e) => tracing::warn!("edit watch learn failed: {:?}", e),
                        }
                        return;
                    }
                } else {
                    stable = Some(trimmed);
                    stable_count = 1;
                }
            } else if attempt == 0 {
                tracing::debug!(
                    "edit watch: focused field not readable (pid={:?})",
                    target_pid
                );
            }
            thread::sleep(Duration::from_millis(450));
        }
        tracing::debug!("edit watch: timed out without stable field text");
    });
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod macos {
    use std::ffi::c_void;

    use anyhow::{Context, Result};
    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
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
        fn AXUIElementCopyParameterizedAttributeValue(
            element: *mut c_void,
            attribute: CFStringRef,
            parameter: CFTypeRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXUIElementSetAttributeValue(
            element: *mut c_void,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> i32;
        fn AXValueCreate(type_: u32, value: *const c_void) -> *mut c_void;
    }

    #[allow(non_upper_case_globals)]
    const kAXValueCFRangeType: u32 = 5;

    type CFStringRef = *const core_foundation::string::__CFString;

    struct AxElement(*mut c_void);

    impl AxElement {
        fn new(ptr: *mut c_void) -> Self {
            Self(ptr)
        }

        fn as_ptr(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for AxElement {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0 as CFTypeRef) };
            }
        }
    }

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

    pub fn ensure_target_active(target_pid: Option<i32>) {
        let Some(pid) = target_pid else {
            return;
        };
        if capture_frontmost_pid() == Some(pid) {
            return;
        }
        if reactivate_app(pid) {
            thread::sleep(Duration::from_millis(80));
        }
    }

    pub fn is_browser_bundle_id(bundle_id: &str) -> bool {
        matches!(
            bundle_id,
            "com.apple.Safari"
                | "com.google.Chrome"
                | "company.thebrowser.Browser"
                | "org.mozilla.firefox"
                | "com.microsoft.edgemac"
                | "com.brave.Browser"
        ) || bundle_id.starts_with("com.vivaldi")
            || bundle_id.starts_with("com.operasoftware.Opera")
            || bundle_id.starts_with("com.google.Chrome.canary")
    }

    pub fn is_browser_pid(target_pid: Option<i32>) -> bool {
        target_pid
            .and_then(bundle_id_for_pid)
            .is_some_and(|id| is_browser_bundle_id(&id))
    }

    pub fn is_terminal_bundle_id(bundle_id: &str) -> bool {
        matches!(
            bundle_id,
            "com.apple.Terminal"
                | "com.googlecode.iterm2"
                | "net.kovidgoyal.kitty"
                | "com.github.wez.wezterm"
                | "io.alacritty"
                | "org.alacritty"
                | "co.zeit.hyper"
                | "com.mitchellh.ghostty"
        ) || bundle_id.starts_with("dev.warp.Warp")
    }

    /// Terminals reliably accept ⌘V but don't expose their text via Accessibility,
    /// so AX insert and the typing fallback both double the paste. Force clipboard-only.
    pub fn is_terminal_pid(target_pid: Option<i32>) -> bool {
        target_pid
            .and_then(bundle_id_for_pid)
            .is_some_and(|id| is_terminal_bundle_id(&id))
    }

    pub fn synth_paste_cgevent(target_pid: Option<i32>) -> Result<()> {
        use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        const KEY_V: CGKeyCode = 0x09;

        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("CGEventSource init failed"))?;

        let key_down = CGEvent::new_keyboard_event(source.clone(), KEY_V, true)
            .map_err(|_| anyhow::anyhow!("paste key down failed"))?;
        key_down.set_flags(CGEventFlags::CGEventFlagCommand);

        let key_up = CGEvent::new_keyboard_event(source, KEY_V, false)
            .map_err(|_| anyhow::anyhow!("paste key up failed"))?;
        key_up.set_flags(CGEventFlags::CGEventFlagCommand);

        if let Some(pid) = target_pid.filter(|pid| *pid > 0) {
            key_down.post_to_pid(pid);
            thread::sleep(Duration::from_millis(5));
            key_up.post_to_pid(pid);
        } else {
            key_down.post(CGEventTapLocation::HID);
            thread::sleep(Duration::from_millis(5));
            key_up.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    fn reactivate_app(pid: i32) -> bool {
        use cocoa::base::id;

        const NS_APPLICATION_ACTIVATE_IGNORING_OTHER_APPS: u64 = 1 << 1;

        unsafe {
            let app: id = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                return false;
            }
            let ok: bool = msg_send![
                app,
                activateWithOptions: NS_APPLICATION_ACTIVATE_IGNORING_OTHER_APPS
            ];
            if ok {
                tracing::debug!("reactivated target app pid={pid}");
            }
            ok
        }
    }

    fn bundle_id_for_pid(pid: i32) -> Option<String> {
        use cocoa::base::id;
        use std::ffi::CStr;

        unsafe {
            let app: id = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                return None;
            }
            let bundle: id = msg_send![app, bundleIdentifier];
            if bundle.is_null() {
                return None;
            }
            let cstr: *const i8 = msg_send![bundle, UTF8String];
            if cstr.is_null() {
                return None;
            }
            Some(CStr::from_ptr(cstr).to_string_lossy().into_owned())
        }
    }

    pub fn read_focused_text(target_pid: Option<i32>) -> Option<String> {
        focused_element(target_pid).and_then(read_element_tree_text)
    }

    fn read_element_tree_text(mut current: AxElement) -> Option<String> {
        for _ in 0..4 {
            if let Some(text) = read_element_text(current.as_ptr()) {
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
            current = parent_element(current.as_ptr())?;
        }
        None
    }

    fn parent_element(element: *mut c_void) -> Option<AxElement> {
        unsafe {
            let parent_attr = CFString::new("AXParent");
            let mut parent: CFTypeRef = std::ptr::null_mut();
            if AXUIElementCopyAttributeValue(
                element,
                parent_attr.as_concrete_TypeRef() as CFStringRef,
                &mut parent,
            ) != AX_ERROR_SUCCESS
                || parent.is_null()
            {
                return None;
            }
            Some(AxElement::new(parent as *mut c_void))
        }
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
                focused.as_ptr(),
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

    fn focused_element(target_pid: Option<i32>) -> Option<AxElement> {
        unsafe {
            if let Some(pid) = target_pid {
                let root = AxElement::new(AXUIElementCreateApplication(pid));
                if let Some(element) = focused_element_for_root(root.as_ptr()) {
                    return Some(element);
                }
            }
            let root = AxElement::new(AXUIElementCreateSystemWide());
            focused_element_for_root(root.as_ptr())
        }
    }

    unsafe fn focused_element_for_root(root: *mut c_void) -> Option<AxElement> {
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
        Some(AxElement::new(focused as *mut c_void))
    }

    fn read_element_text(element: *mut c_void) -> Option<String> {
        unsafe {
            for attr in ["AXSelectedText", "AXValue", "AXText"] {
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
            read_ax_full_text(element)
        }
    }

    fn read_ax_full_text(element: *mut c_void) -> Option<String> {
        unsafe {
            let count_attr = CFString::new("AXNumberOfCharacters");
            let mut count_ref: CFTypeRef = std::ptr::null_mut();
            if AXUIElementCopyAttributeValue(
                element,
                count_attr.as_concrete_TypeRef() as CFStringRef,
                &mut count_ref,
            ) != AX_ERROR_SUCCESS
                || count_ref.is_null()
            {
                return None;
            }
            let count_number = core_foundation::number::CFNumber::wrap_under_get_rule(
                count_ref as core_foundation::number::CFNumberRef,
            );
            let count = count_number.to_i64().unwrap_or(0);
            if count <= 0 {
                return None;
            }

            let range = core_foundation::base::CFRange {
                location: 0,
                length: count as isize,
            };
            let range_value =
                AXValueCreate(kAXValueCFRangeType, &range as *const _ as *const c_void);
            if range_value.is_null() {
                return None;
            }
            let _range_guard = AxValueGuard(range_value);

            let param_attr = CFString::new("AXStringForRange");
            let mut string_ref: CFTypeRef = std::ptr::null_mut();
            if AXUIElementCopyParameterizedAttributeValue(
                element,
                param_attr.as_concrete_TypeRef() as CFStringRef,
                range_value as CFTypeRef,
                &mut string_ref,
            ) != AX_ERROR_SUCCESS
                || string_ref.is_null()
            {
                return None;
            }
            cf_type_to_string(string_ref).filter(|text| !text.is_empty())
        }
    }

    struct AxValueGuard(*mut c_void);

    impl Drop for AxValueGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CFRelease(self.0 as CFTypeRef);
                }
            }
        }
    }

    unsafe fn cf_type_to_string(value: CFTypeRef) -> Option<String> {
        let cf_string = CFString::wrap_under_get_rule(value as CFStringRef);
        Some(cf_string.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_paste_result, try_learn_from_field_edit, PasteOutcome};

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

    #[test]
    fn learns_suffix_after_prefix() {
        let learned = try_learn_from_field_edit(
            "Talk to Mackenzie today",
            Some("Hello. "),
            "Hello. Talk to McKenzie today",
        )
        .unwrap();
        assert_eq!(learned.len(), 1);
        assert_eq!(learned[0].from, "Mackenzie");
    }

    #[test]
    fn learns_cloud_to_claude_as_phrase() {
        let learned = try_learn_from_field_edit(
            "Let's use cloud for this",
            Some(""),
            "Let's use Claude for this",
        )
        .unwrap();
        assert_eq!(learned.len(), 1);
        assert_eq!(learned[0].from, "use cloud");
        assert_eq!(learned[0].to, "use Claude");
    }

    #[test]
    fn learns_cloud_to_claude_inside_larger_field_without_prefix() {
        let learned = try_learn_from_field_edit(
            "Let's use cloud for this",
            None,
            "Earlier note. Let's use Claude for this.",
        )
        .unwrap();
        assert_eq!(learned.len(), 1);
        assert_eq!(learned[0].from, "use cloud");
        assert_eq!(learned[0].to, "use Claude");
    }

    #[test]
    fn learns_merged_brand_name() {
        let learned = try_learn_from_field_edit("whiz pop", Some(""), "WizPop").unwrap();
        assert_eq!(learned.len(), 1);
        assert_eq!(learned[0].from, "whiz pop");
        assert_eq!(learned[0].to, "WizPop");
    }

    #[test]
    fn restores_clipboard_only_when_paste_confirmed() {
        assert!(super::should_restore_previous_clipboard(
            PasteOutcome::Confirmed
        ));
        assert!(!super::should_restore_previous_clipboard(
            PasteOutcome::Unverified
        ));
        assert!(!super::should_restore_previous_clipboard(
            PasteOutcome::Failed
        ));
    }

    #[test]
    fn synthetic_after_paste_joins_with_space() {
        assert_eq!(
            super::synthetic_after_paste("Hello.", "whiz pop"),
            "Hello. whiz pop"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_common_browser_bundle_ids() {
        use super::macos::is_browser_bundle_id;

        assert!(is_browser_bundle_id("com.apple.Safari"));
        assert!(is_browser_bundle_id("com.google.Chrome"));
        assert!(is_browser_bundle_id("company.thebrowser.Browser"));
        assert!(!is_browser_bundle_id("com.tinyspeck.slackmacgap"));
        assert!(!is_browser_bundle_id("com.invisionnaire.dictaflow"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_common_terminal_bundle_ids() {
        use super::macos::is_terminal_bundle_id;

        assert!(is_terminal_bundle_id("com.apple.Terminal"));
        assert!(is_terminal_bundle_id("com.googlecode.iterm2"));
        assert!(is_terminal_bundle_id("dev.warp.Warp-Stable"));
        assert!(is_terminal_bundle_id("com.mitchellh.ghostty"));
        assert!(!is_terminal_bundle_id("com.apple.Safari"));
        assert!(!is_terminal_bundle_id("com.invisionnaire.dictaflow"));
    }
}
