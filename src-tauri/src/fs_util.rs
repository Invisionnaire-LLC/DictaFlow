//! Small filesystem helpers shared across persistence modules.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;

/// Per-user data directory (`~/Library/Application Support/DictaFlow` on macOS).
/// Holds settings, history, dictionary, the Whisper model, and recordings.
pub fn app_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("DictaFlow"))
}

/// Write `contents` to `path` atomically: write to a sibling `.tmp` then rename,
/// so a crash mid-write never leaves a half-written file at `path`.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Run `f` while holding an exclusive lock on `lock_name` next to `path`, so
/// read-modify-write cycles on `path` never interleave.
pub fn with_file_lock<T>(path: &Path, lock_name: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    fs::create_dir_all(dir)?;
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(lock_name))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("lock {lock_name}"))?;
    f()
}

/// Move an unparseable file aside (`<name>.<label>.broken-<unix>.json`) so the
/// next save starts fresh instead of failing forever.
pub fn backup_corrupt_file(path: &Path, label: &str) {
    if !path.exists() {
        return;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_extension(format!("{label}.broken-{stamp}.json"));
    match fs::rename(path, &backup) {
        Ok(()) => tracing::warn!("backed up corrupt file to {}", backup.display()),
        Err(e) => tracing::warn!("could not back up corrupt {}: {:?}", path.display(), e),
    }
}
