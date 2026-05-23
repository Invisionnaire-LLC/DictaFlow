use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 500;

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

pub fn append(raw: &str, cleaned: &str, duration_secs: f32, audio_path: Option<String>) -> Result<HistoryEntry> {
    let entry = HistoryEntry {
        id: new_id(),
        created_at: chrono::Local::now().to_rfc3339(),
        raw: raw.to_string(),
        cleaned: cleaned.to_string(),
        duration_secs,
        audio_path,
    };

    let mut entries = load_all()?;
    entries.insert(0, entry.clone());
    entries.truncate(MAX_ENTRIES);
    save_all(&entries)?;
    Ok(entry)
}

pub fn load_all() -> Result<Vec<HistoryEntry>> {
    let path = history_path().ok_or_else(|| anyhow!("no history path"))?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path).context("read history")?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).context("parse history")
}

pub fn search(query: &str) -> Result<Vec<HistoryEntry>> {
    let q = query.trim().to_lowercase();
    let entries = load_all()?;
    if q.is_empty() {
        return Ok(entries);
    }
    Ok(entries
        .into_iter()
        .filter(|e| {
            e.raw.to_lowercase().contains(&q) || e.cleaned.to_lowercase().contains(&q)
        })
        .collect())
}

pub fn delete(id: &str) -> Result<bool> {
    let mut entries = load_all()?;
    let before = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() == before {
        return Ok(false);
    }
    save_all(&entries)?;
    Ok(true)
}

pub fn clear_all() -> Result<()> {
    let path = history_path().ok_or_else(|| anyhow!("no history path"))?;
    if path.exists() {
        fs::remove_file(&path).context("remove history")?;
    }
    Ok(())
}

fn save_all(entries: &[HistoryEntry]) -> Result<()> {
    let path = history_path().ok_or_else(|| anyhow!("no history path"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("create history dir")?;
    }
    let json = serde_json::to_string_pretty(entries)?;
    fs::write(&path, json).context("write history")?;
    Ok(())
}

fn history_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DICTAFLOW_HISTORY_PATH") {
        return Some(PathBuf::from(path));
    }
    let dir = dirs::data_local_dir()?.join("DictaFlow");
    Some(dir.join("history.json"))
}

fn new_id() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    )
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
        let _ = fs::remove_file(&path);
        std::env::set_var("DICTAFLOW_HISTORY_PATH", path.to_string_lossy().to_string());
        f();
        let _ = fs::remove_file(&path);
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
}
