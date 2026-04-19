use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};

pub fn paste(text: &str) -> Result<()> {
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
