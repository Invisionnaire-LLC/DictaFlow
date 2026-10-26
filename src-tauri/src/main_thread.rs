//! Run UI work on Tauri's main thread and block until it completes.
use std::sync::mpsc;
use std::time::Duration;

use tauri::AppHandle;

const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `f` on the main thread and wait (up to 5 s) for it to finish.
pub fn run_void(app: AppHandle, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::sync_channel(1);
    if app
        .clone()
        .run_on_main_thread(move || {
            f();
            let _ = tx.send(());
        })
        .is_ok()
    {
        let _ = rx.recv_timeout(MAIN_THREAD_TIMEOUT);
    }
}
