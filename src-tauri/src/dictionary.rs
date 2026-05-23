use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::RegexBuilder;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearnedUpdate {
    pub from: String,
    pub to: String,
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
        fs::write(&path, json).context("write dictionary")?;
        Ok(())
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
    let dict = DICT.lock();
    if dict.replacements.is_empty() {
        return text.to_string();
    }
    let mut ordered = dict.replacements.clone();
    ordered.sort_by_key(|r| std::cmp::Reverse(r.from.len()));

    let mut out = text.to_string();
    for rep in ordered {
        if rep.from.is_empty() || rep.from.eq_ignore_ascii_case(&rep.to) {
            continue;
        }
        let pattern = format!(r"(?i)\b{}\b", regex::escape(&rep.from));
        let Ok(re) = RegexBuilder::new(&pattern).build() else {
            continue;
        };
        out = re.replace_all(&out, rep.to.as_str()).into_owned();
    }
    out
}

pub fn learn_from_edit(before: &str, after: &str) -> Result<Vec<LearnedUpdate>> {
    let pairs = edits_to_learn(before, after);
    if pairs.is_empty() {
        return Ok(Vec::new());
    }

    let mut learned = Vec::new();
    {
        let mut dict = DICT.lock();
        for (from, to) in pairs {
            dict.upsert_replacement(from.clone(), to.clone());
            learned.push(LearnedUpdate { from, to });
        }
        dict.save()?;
    }
    sync_whisper_vocabulary()?;
    Ok(learned)
}

pub fn edits_to_learn(before: &str, after: &str) -> Vec<(String, String)> {
    let before = before.trim();
    let after = after.trim();
    if before.is_empty() || after.is_empty() || before == after {
        return Vec::new();
    }

    let before_words = tokenize(before);
    let after_words = tokenize(after);
    if before_words.len() != after_words.len() || before_words.is_empty() {
        return Vec::new();
    }

    before_words
        .iter()
        .zip(after_words.iter())
        .filter_map(|(from, to)| candidate_pair(from, to))
        .collect()
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
    sync_whisper_vocabulary()?;
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

    let mut out = String::from("DictaFlow, Invisionnaire");
    for word in words {
        if out.len() + word.len() + 2 > MAX_PROMPT_CHARS {
            break;
        }
        out.push_str(", ");
        out.push_str(&word);
    }
    out.push('.');
    out
}

fn sync_whisper_vocabulary() -> Result<()> {
    let prompt = whisper_prompt();
    if let Some(path) = vocabulary_path() {
        fs::write(&path, format!("{prompt}\n")).context("write vocabulary.txt")?;
    }
    Ok(())
}

fn vocabulary_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DICTAFLOW_VOCABULARY_PATH") {
        return Some(PathBuf::from(path));
    }
    let dir = dirs::data_local_dir()?.join("DictaFlow");
    Some(dir.join(crate::asr::VOCAB_FILENAME))
}

fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| {
                matches!(
                    c,
                    '.' | ',' | '!' | '?' | ';' | ':' | '"' | '\'' | '(' | ')' | '[' | ']'
                )
            })
        })
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn candidate_pair(from: &str, to: &str) -> Option<(String, String)> {
    if from.eq_ignore_ascii_case(to) || is_stopword(from) {
        return None;
    }
    if from.len() > 60 || to.len() > 60 {
        return None;
    }
    Some((from.to_string(), to.to_string()))
}

fn is_stopword(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "a" | "an"
            | "the"
            | "and"
            | "or"
            | "but"
            | "to"
            | "of"
            | "in"
            | "on"
            | "at"
            | "for"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "it"
            | "its"
            | "i"
            | "me"
            | "my"
            | "you"
            | "your"
            | "we"
            | "our"
            | "they"
            | "their"
            | "this"
            | "that"
            | "with"
            | "as"
            | "by"
            | "from"
            | "so"
            | "if"
            | "not"
            | "no"
            | "yes"
    )
}

fn dictionary_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DICTAFLOW_DICTIONARY_PATH") {
        return Some(PathBuf::from(path));
    }
    let dir = dirs::data_local_dir()?.join("DictaFlow");
    Some(dir.join("dictionary.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learns_matching_word_count() {
        let pairs = edits_to_learn("Talk to Mackenzie today", "Talk to McKenzie today");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "Mackenzie");
        assert_eq!(pairs[0].1, "McKenzie");
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
    fn ignores_stopword_changes() {
        assert!(edits_to_learn("this is fine", "this was fine").is_empty());
    }

    #[test]
    fn ignores_length_mismatch() {
        assert!(edits_to_learn("one two", "one two three").is_empty());
    }
}
