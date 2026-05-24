//! Run UI work on Tauri's main thread and block until it completes.
use std::sync::mpsc;
use std::time::Duration;

use tauri::AppHandle;

const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run<R: Send + 'static>(app: AppHandle, f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    let (tx, rx) = mpsc::sync_channel(1);
    if app
        .clone()
        .run_on_main_thread(move || {
            let _ = tx.send(f());
        })
        .is_err()
    {
        return None;
    }
    rx.recv_timeout(MAIN_THREAD_TIMEOUT).ok()
}

pub fn run_void(app: AppHandle, f: impl FnOnce() + Send + 'static) {
    let _ = run(app, f);
}
