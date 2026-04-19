use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use tauri::AppHandle;

pub fn paste(app: &AppHandle, text: &str) -> Result<()> {
    let text = text.to_string();
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let result = paste_on_main(&text);
        let _ = tx.send(result);
    })
    .map_err(|e| anyhow!("schedule main-thread paste: {}", e))?;

    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|e| anyhow!("paste worker timed out: {}", e))?
}

fn paste_on_main(text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().context("clipboard init")?;
    let previous = clipboard.get_text().ok();

    clipboard
        .set_text(text.to_string())
        .context("clipboard write")?;

    thread::sleep(Duration::from_millis(30));
    synth_paste().context("synth paste")?;
    thread::sleep(Duration::from_millis(50));

    if let Some(prev) = previous {
        let _ = clipboard.set_text(prev);
    }

    Ok(())
}

fn synth_paste() -> Result<()> {
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

