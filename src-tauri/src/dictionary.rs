use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::{NoExpand, RegexBuilder};
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
        if is_ambiguous_common_word(&rep.from) && !rep.from.contains(' ') {
            continue;
        }
        let pattern = format!(r"(?i)\b{}\b", regex::escape(&rep.from));
        let Ok(re) = RegexBuilder::new(&pattern).build() else {
            continue;
        };
        out = re.replace_all(&out, NoExpand(&rep.to)).into_owned();
    }
    out
}

pub fn learn_from_edit(before: &str, after: &str) -> Result<Vec<LearnedUpdate>> {
    let pairs = edits_to_learn(before, after);
    learn_pairs(pairs)
}

/// Find learnable edits when `pasted` appears inside a larger field value.
pub fn learn_pasted_in_context(pasted: &str, after: &str) -> Result<Vec<LearnedUpdate>> {
    if let Some(pairs) = edits_in_pasted_context(pasted, after) {
        return learn_pairs(pairs);
    }
    Ok(Vec::new())
}

fn edits_in_pasted_context(pasted: &str, after: &str) -> Option<Vec<(String, String)>> {
    let pasted = pasted.trim();
    let after = after.trim();
    if pasted.is_empty() || after.is_empty() || pasted.eq_ignore_ascii_case(after) {
        return None;
    }

    let pasted_words = tokenize(pasted);
    let after_words = tokenize(after);
    if pasted_words.is_empty() {
        return None;
    }

    if pasted_words.len() == after_words.len() {
        let pairs = edits_to_learn(pasted, after);
        if !pairs.is_empty() {
            return Some(pairs);
        }
    } else if after_words.len() < pasted_words.len() {
        let pairs = edits_to_learn(pasted, after);
        if !pairs.is_empty() {
            return Some(pairs);
        }
        return None;
    }

    let n = pasted_words.len();
    let mut best: Option<Vec<(String, String)>> = None;
    for i in 0..=after_words.len().saturating_sub(n) {
        let chunk = join_words(&after_words[i..i + n]);
        let pairs = edits_to_learn(pasted, &chunk);
        if pairs.is_empty() {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|current| pair_score(&pairs) < pair_score(current))
        {
            best = Some(pairs);
        }
    }
    best
}

fn pair_score(pairs: &[(String, String)]) -> (usize, usize) {
    let words = pairs.iter().map(|(from, _)| tokenize(from).len()).sum();
    (pairs.len(), words)
}

fn learn_pairs(pairs: Vec<(String, String)>) -> Result<Vec<LearnedUpdate>> {
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
    // Learned terms reach the Whisper prompt via `whisper_prompt()` (see
    // asr::initial_prompt); they must NOT overwrite the user's custom words file.
    Ok(learned)
}

const MAX_LEARNED_PAIRS: usize = 3;

pub fn edits_to_learn(before: &str, after: &str) -> Vec<(String, String)> {
    let before = before.trim();
    let after = after.trim();
    if before.is_empty() || after.is_empty() || before == after {
        return Vec::new();
    }

    let before_words = tokenize(before);
    let after_words = tokenize(after);
    if before_words.is_empty() || after_words.is_empty() {
        return Vec::new();
    }

    if before_words.len() == after_words.len() {
        let pairs: Vec<_> = (0..before_words.len())
            .filter_map(|idx| candidate_pair_with_context(&before_words, &after_words, idx))
            .collect();
        return cap_learned_pairs(pairs);
    }

    let prefix_len = before_words
        .iter()
        .zip(after_words.iter())
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count();
    let suffix_len = before_words
        .iter()
        .rev()
        .zip(after_words.iter().rev())
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count();

    let bw_len = before_words.len();
    let aw_len = after_words.len();
    if prefix_len + suffix_len < bw_len && prefix_len + suffix_len < aw_len {
        let from_mid = join_words(&before_words[prefix_len..bw_len - suffix_len]);
        let to_mid = join_words(&after_words[prefix_len..aw_len - suffix_len]);
        if let Some(pair) = candidate_phrase(&from_mid, &to_mid) {
            return vec![pair];
        }
        let from_mid_words = &before_words[prefix_len..bw_len - suffix_len];
        let to_mid_words = &after_words[prefix_len..aw_len - suffix_len];
        if from_mid_words.len() == to_mid_words.len() && !from_mid_words.is_empty() {
            let pairs: Vec<_> = (0..from_mid_words.len())
                .filter_map(|idx| candidate_pair_with_context(from_mid_words, to_mid_words, idx))
                .collect();
            return cap_learned_pairs(pairs);
        }
    }

    Vec::new()
}

fn cap_learned_pairs(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    if pairs.len() > MAX_LEARNED_PAIRS {
        tracing::debug!(
            "skipping learn: {} word changes exceeds limit of {}",
            pairs.len(),
            MAX_LEARNED_PAIRS
        );
        return Vec::new();
    }
    pairs
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

fn join_words(words: &[String]) -> String {
    words.join(" ")
}

fn candidate_phrase(from: &str, to: &str) -> Option<(String, String)> {
    if from.is_empty() || to.is_empty() || from.eq_ignore_ascii_case(to) {
        return None;
    }
    if from.split_whitespace().all(is_stopword) {
        return None;
    }
    if from.len() > 80 || to.len() > 80 {
        return None;
    }
    Some((from.to_string(), to.to_string()))
}

fn candidate_pair(from: &str, to: &str) -> Option<(String, String)> {
    if from.eq_ignore_ascii_case(to) || is_stopword(from) {
        return None;
    }
    if is_ambiguous_common_word(from) && !from.contains(' ') {
        return None;
    }
    if from.len() > 60 || to.len() > 60 {
        return None;
    }
    Some((from.to_string(), to.to_string()))
}

/// Prefer phrase pairs like "use cloud" → "use Claude" when `from` is an ordinary English word.
fn candidate_pair_with_context(
    before_words: &[String],
    after_words: &[String],
    idx: usize,
) -> Option<(String, String)> {
    let from = &before_words[idx];
    let to = &after_words[idx];
    if let Some(pair) = candidate_pair(from, to) {
        return Some(pair);
    }
    if !is_ambiguous_common_word(from) {
        return None;
    }

    if idx > 0 && before_words[idx - 1].eq_ignore_ascii_case(&after_words[idx - 1]) {
        let from_phrase = join_words(&before_words[idx - 1..=idx]);
        let to_phrase = join_words(&after_words[idx - 1..=idx]);
        if let Some(pair) = candidate_phrase(&from_phrase, &to_phrase) {
            return Some(pair);
        }
    }

    if idx + 1 < before_words.len()
        && before_words[idx + 1].eq_ignore_ascii_case(&after_words[idx + 1])
    {
        let from_phrase = join_words(&before_words[idx..=idx + 1]);
        let to_phrase = join_words(&after_words[idx..=idx + 1]);
        if let Some(pair) = candidate_phrase(&from_phrase, &to_phrase) {
            return Some(pair);
        }
    }

    None
}

fn is_ambiguous_common_word(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        // Homophones / ordinary words Whisper often confuses with brands or names.
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
            | "he"
            | "she"
            | "her"
            | "him"
            | "his"
            | "hers"
            | "them"
            | "who"
            | "whom"
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
    fn learns_cloud_to_claude_as_phrase_not_single_word() {
        let pairs = edits_in_pasted_context(
            "Let's use cloud for this",
            "Earlier text. Let's use Claude for this and more.",
        )
        .unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "use cloud");
        assert_eq!(pairs[0].1, "use Claude");
    }

    #[test]
    fn skips_bare_cloud_to_claude() {
        assert!(edits_to_learn("cloud", "Claude").is_empty());
    }

    #[test]
    fn learns_open_cloud_as_phrase() {
        let pairs = edits_to_learn("Open cloud", "Open Claude");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "Open cloud");
        assert_eq!(pairs[0].1, "Open Claude");
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
    fn learns_merged_words() {
        let pairs = edits_to_learn("whiz pop", "WizPop");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "whiz pop");
        assert_eq!(pairs[0].1, "WizPop");
    }

    #[test]
    fn learns_phrase_in_sentence() {
        let pairs = edits_to_learn("Hello whiz pop", "Hello WizPop");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "whiz pop");
        assert_eq!(pairs[0].1, "WizPop");
    }

    #[test]
    fn ignores_length_mismatch_without_shared_affixes() {
        assert!(edits_to_learn("one two", "one two three").is_empty());
    }

    #[test]
    fn ignores_pronoun_changes() {
        assert!(edits_to_learn("tell her later", "tell him later").is_empty());
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
