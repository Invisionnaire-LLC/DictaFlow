use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::{NoExpand, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

const MAX_REPLACEMENTS: usize = 200;
const MAX_PROMPT_CHARS: usize = 800;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DictionaryFile {
    #[serde(default)]
    replacements: Vec<Replacement>,
}

static DICT: Lazy<Mutex<DictionaryFile>> = Lazy::new(|| Mutex::new(DictionaryFile::load()));

/// Bumped whenever the dictionary mutates, so the compiled-regex cache rebuilds lazily.
static DICT_GEN: AtomicU64 = AtomicU64::new(0);

struct CompiledReplacement {
    re: Regex,
    to: String,
}

/// `(generation, compiled)` — rebuilt only when `DICT_GEN` changes, so we don't recompile
/// every regex on every transcription.
static REGEX_CACHE: Lazy<Mutex<(u64, Vec<CompiledReplacement>)>> =
    Lazy::new(|| Mutex::new((u64::MAX, Vec::new())));

fn bump_dictionary_generation() {
    DICT_GEN.fetch_add(1, Ordering::Release);
}

/// Snapshot the dictionary and compile each replacement into a `\b…\b` regex once.
fn compile_replacements() -> Vec<CompiledReplacement> {
    let mut ordered = {
        let dict = DICT.lock();
        dict.replacements.clone()
    };
    // Longest `from` first so multi-word phrases win over their sub-words.
    ordered.sort_by_key(|r| std::cmp::Reverse(r.from.len()));

    let mut compiled = Vec::new();
    for rep in ordered {
        if rep.from.is_empty() || rep.from.eq_ignore_ascii_case(&rep.to) {
            continue;
        }
        if is_ambiguous_common_word(&rep.from) && !rep.from.contains(' ') {
            continue;
        }
        let pattern = format!(r"(?i)\b{}\b", regex::escape(&rep.from));
        if let Ok(re) = RegexBuilder::new(&pattern).build() {
            compiled.push(CompiledReplacement { re, to: rep.to });
        }
    }
    compiled
}

impl DictionaryFile {
    fn load() -> Self {
        match dictionary_path() {
            Some(path) if path.exists() => fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            _ => Self::default(),
        }
    }

    fn save(&self) -> Result<()> {
        let path = dictionary_path().ok_or_else(|| anyhow!("no dictionary path"))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).context("create dictionary dir")?;
        }
        let json = serde_json::to_string_pretty(self)?;
        crate::fs_util::write_atomic(&path, &json).context("write dictionary")
    }

    fn upsert_replacement(&mut self, from: String, to: String) {
        if let Some(existing) = self
            .replacements
            .iter_mut()
            .find(|r| r.from.eq_ignore_ascii_case(&from))
        {
            existing.to = to;
            return;
        }
        self.replacements.push(Replacement { from, to });
        if self.replacements.len() > MAX_REPLACEMENTS {
            let overflow = self.replacements.len() - MAX_REPLACEMENTS;
            self.replacements.drain(0..overflow);
        }
    }
}

pub fn apply_replacements(text: &str) -> String {
    let generation = DICT_GEN.load(Ordering::Acquire);
    let mut cache = REGEX_CACHE.lock();
    if cache.0 != generation {
        cache.1 = compile_replacements();
        cache.0 = generation;
    }
    if cache.1.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for rep in &cache.1 {
        // `replace_all` only allocates when the rule actually matches.
        if let Cow::Owned(replaced) = rep.re.replace_all(&out, NoExpand(&rep.to)) {
            out = replaced;
        }
    }
    out
}

pub fn add_replacement(from: &str, to: &str) -> Result<()> {
    let from = from.trim();
    let to = to.trim();
    if from.is_empty() || to.is_empty() {
        return Err(anyhow!("both heard and wanted text are required"));
    }
    if from.eq_ignore_ascii_case(to) {
        return Err(anyhow!("heard and wanted text must differ"));
    }
    let mut dict = DICT.lock();
    dict.upsert_replacement(from.to_string(), to.to_string());
    dict.save()?;
    drop(dict);
    bump_dictionary_generation();
    Ok(())
}

pub fn list_replacements() -> Vec<Replacement> {
    DICT.lock().replacements.clone()
}

pub fn delete_replacement(from: &str) -> Result<bool> {
    let key = from.trim();
    if key.is_empty() {
        return Ok(false);
    }
    let mut dict = DICT.lock();
    let before = dict.replacements.len();
    dict.replacements
        .retain(|r| !r.from.eq_ignore_ascii_case(key));
    if dict.replacements.len() == before {
        return Ok(false);
    }
    dict.save()?;
    drop(dict);
    bump_dictionary_generation();
    Ok(true)
}

pub fn whisper_prompt() -> String {
    let dict = DICT.lock();
    let mut words: Vec<String> = dict
        .replacements
        .iter()
        .map(|r| r.to.trim().to_string())
        .filter(|w| !w.is_empty())
        .collect();
    words.sort_by_key(|w| w.to_ascii_lowercase());
    words.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    if words.is_empty() {
        return String::new();
    }

    let mut out = words[0].clone();
    for word in words.iter().skip(1) {
        if out.len() + word.len() + 2 > MAX_PROMPT_CHARS {
            break;
        }
        out.push_str(", ");
        out.push_str(word);
    }
    out.push('.');
    out
}

/// Words a single-word replacement is skipped for, to avoid clobbering ordinary
/// English (e.g. "cloud" in weather talk). Multi-word phrases are still applied.
fn is_ambiguous_common_word(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "cloud"
            | "clouds"
            | "son"
            | "sun"
            | "piece"
            | "peace"
            | "right"
            | "write"
            | "read"
            | "red"
            | "bear"
            | "bare"
            | "rain"
            | "reign"
            | "mail"
            | "male"
            | "sale"
            | "sail"
            | "code"
    )
}

fn dictionary_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DICTAFLOW_DICTIONARY_PATH") {
        return Some(PathBuf::from(path));
    }
    Some(crate::fs_util::app_dir()?.join("dictionary.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_replacement_rejects_empty_or_identical() {
        assert!(add_replacement("", "Claude").is_err());
        assert!(add_replacement("cloud", "").is_err());
        assert!(add_replacement("Claude", "Claude").is_err());
    }

    #[test]
    fn preserves_literal_cloud_in_cleanup() {
        let mut dict = DictionaryFile::default();
        dict.replacements.push(Replacement {
            from: "cloud".into(),
            to: "Claude".into(),
        });
        let ordered = dict.replacements;
        let mut out = "This is a cloud in the sky.".to_string();
        for rep in ordered {
            if is_ambiguous_common_word(&rep.from) && !rep.from.contains(' ') {
                continue;
            }
            let pattern = format!(r"(?i)\b{}\b", regex::escape(&rep.from));
            let re = RegexBuilder::new(&pattern).build().unwrap();
            out = re.replace_all(&out, NoExpand(&rep.to)).into_owned();
        }
        assert_eq!(out, "This is a cloud in the sky.");
    }

    #[test]
    fn applies_phrase_cloud_replacement() {
        let pattern = format!(r"(?i)\b{}\b", regex::escape("use cloud"));
        let re = RegexBuilder::new(&pattern).build().unwrap();
        let out = re.replace_all("Let's use cloud for AI", NoExpand("use Claude"));
        assert_eq!(out, "Let's use Claude for AI");
    }

    #[test]
    fn apply_replacement_is_case_insensitive() {
        let rep = Replacement {
            from: "draught".into(),
            to: "Draft".into(),
        };
        let pattern = format!(r"(?i)\b{}\b", regex::escape(&rep.from));
        let re = RegexBuilder::new(&pattern).build().unwrap();
        let out = re.replace_all("Save the draught", rep.to.as_str());
        assert_eq!(out, "Save the Draft");
    }

    #[test]
    fn replacement_preserves_dollar_signs() {
        let rep = Replacement {
            from: "price".into(),
            to: "$5".into(),
        };
        let mut dict = DictionaryFile::default();
        dict.replacements.push(rep);
        drop(dict);
        // apply via same regex path as production
        let pattern = format!(r"(?i)\b{}\b", regex::escape("price"));
        let re = RegexBuilder::new(&pattern).build().unwrap();
        let out = re.replace_all("The price is good", NoExpand("$5"));
        assert_eq!(out, "The $5 is good");
    }

    #[test]
    fn delete_replacement_does_not_deadlock_while_listing() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        const KEY: &str = "_deadlock_test_";
        {
            let mut dict = DICT.lock();
            dict.replacements.retain(|r| r.from != KEY);
            dict.replacements.push(Replacement {
                from: KEY.into(),
                to: "ok".into(),
            });
        }

        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = delete_replacement(KEY);
            let _ = tx.send(());
        });

        let start = Instant::now();
        loop {
            let _ = list_replacements();
            if rx.try_recv().is_ok() {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "delete_replacement deadlocked while holding DICT lock"
            );
            thread::sleep(Duration::from_millis(5));
        }
        handle.join().expect("delete thread panicked");
    }
}
