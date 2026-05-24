use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 500;

static ID_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub id: String,
    pub created_at: String,
    pub raw: String,
    pub cleaned: String,
    pub duration_secs: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_path: Option<String>,
}

pub fn append(
    raw: &str,
    cleaned: &str,
    duration_secs: f32,
    audio_path: Option<String>,
) -> Result<HistoryEntry> {
    let entry = HistoryEntry {
        id: new_id(),
        created_at: chrono::Local::now().to_rfc3339(),
        raw: raw.to_string(),
        cleaned: cleaned.to_string(),
        duration_secs,
        audio_path,
    };

    with_locked_history(|entries| {
        entries.insert(0, entry.clone());
        if entries.len() > MAX_ENTRIES {
            let dropped: Vec<_> = entries.drain(MAX_ENTRIES..).collect();
            for dropped in dropped {
                remove_audio_file(dropped.audio_path.as_deref());
            }
        }
        Ok(((), true))
    })?;

    Ok(entry)
}

#[allow(dead_code)]
pub fn load_all() -> Result<Vec<HistoryEntry>> {
    with_locked_history(|entries| Ok((entries.clone(), false)))
}

pub fn search(query: &str) -> Result<Vec<HistoryEntry>> {
    let q = query.trim().to_lowercase();
    with_locked_history(|entries| {
        if q.is_empty() {
            return Ok((entries.clone(), false));
        }
        Ok((
            entries
                .iter()
                .filter(|e| {
                    e.raw.to_lowercase().contains(&q) || e.cleaned.to_lowercase().contains(&q)
                })
                .cloned()
                .collect(),
            false,
        ))
    })
}

pub fn delete(id: &str) -> Result<bool> {
    let mut removed_audio: Option<Option<String>> = None;
    let changed = with_locked_history(|entries| {
        let before = entries.len();
        if let Some(idx) = entries.iter().position(|e| e.id == id) {
            removed_audio = Some(entries[idx].audio_path.clone());
            entries.remove(idx);
        }
        Ok((entries.len() != before, entries.len() != before))
    })?;
    if changed {
        if let Some(path) = removed_audio.flatten() {
            remove_audio_file(Some(&path));
        }
    }
    Ok(changed)
}

pub fn update_cleaned(id: &str, cleaned: &str) -> Result<Option<(String, HistoryEntry)>> {
    with_locked_history(|entries| {
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
            return Ok((None, false));
        };
        let before = entry.cleaned.clone();
        entry.cleaned = cleaned.to_string();
        let updated = entry.clone();
        Ok((Some((before, updated)), true))
    })
}

pub fn clear_all() -> Result<()> {
    with_locked_history(|entries| {
        for entry in entries.drain(..) {
            remove_audio_file(entry.audio_path.as_deref());
        }
        Ok(((), true))
    })
}

fn with_locked_history<F, T>(mut f: F) -> Result<T>
where
    F: FnMut(&mut Vec<HistoryEntry>) -> Result<(T, bool)>,
{
    let path = history_path().ok_or_else(|| anyhow!("no history path"))?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("history path has no parent"))?;
    fs::create_dir_all(dir)?;

    let lock_path = dir.join("history.lock");
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file
        .lock_exclusive()
        .map_err(|e| anyhow!("history lock: {}", e))?;

    let mut entries = read_history_file(&path)?;
    let (result, dirty) = f(&mut entries)?;
    if dirty {
        let json = serde_json::to_string_pretty(&entries)?;
        write_atomic(&path, &json)?;
    }
    Ok(result)
}

fn read_history_file(path: &Path) -> Result<Vec<HistoryEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).context("read history")?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).context("parse history")
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

fn remove_audio_file(path: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    if path.is_empty() {
        return;
    }
    match fs::remove_file(path) {
        Ok(()) => tracing::debug!("removed recording {}", path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("could not remove recording {}: {:?}", path, e),
    }
}

fn history_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DICTAFLOW_HISTORY_PATH") {
        return Some(PathBuf::from(path));
    }
    let dir = dirs::data_local_dir()?.join("DictaFlow");
    Some(dir.join("history.json"))
}

fn new_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{ms}-{seq}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn temp_history_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dictaflow-history-test-{}-{}.json",
            label,
            std::process::id()
        ))
    }

    fn with_temp_history<F: FnOnce()>(label: &str, f: F) {
        let _guard = TEST_LOCK.lock().unwrap();
        let path = temp_history_path(label);
        let lock = path.with_extension("lock");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&lock);
        std::env::set_var("DICTAFLOW_HISTORY_PATH", path.to_string_lossy().to_string());
        f();
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&lock);
        std::env::remove_var("DICTAFLOW_HISTORY_PATH");
    }

    #[test]
    fn append_and_search_matches_cleaned_text() {
        with_temp_history("search", || {
            append("hello world", "Hello world.", 1.2, None).unwrap();
            append("foo bar", "Foo bar.", 0.8, None).unwrap();
            let hits = search("foo").unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].cleaned, "Foo bar.");
        });
    }

    #[test]
    fn delete_removes_entry() {
        with_temp_history("delete", || {
            let entry = append("x", "X.", 0.5, None).unwrap();
            assert!(delete(&entry.id).unwrap());
            assert!(load_all().unwrap().is_empty());
        });
    }

    #[test]
    fn new_ids_are_unique_in_sequence() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
    }
}
