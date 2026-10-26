use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};

const SAFE_SPOKEN_COMMANDS: &[(&str, &str)] = &[
    ("new paragraph", "\n\n"),
    ("new line", "\n"),
    ("full stop", ". "),
    ("question mark", "? "),
    ("exclamation point", "! "),
    ("exclamation mark", "! "),
];

/// Single-word punctuation aliases that collide with normal speech (e.g. "Cretaceous period").
const AMBIGUOUS_SPOKEN: &[(&str, &str)] = &[
    ("period", ". "),
    ("comma", ", "),
    ("colon", ": "),
    ("semicolon", "; "),
];

/// Words that often start a new clause after a spoken punctuation command.
const CLAUSE_START_WORDS: &[&str] = &[
    "how", "what", "when", "where", "why", "who", "which", "and", "but", "or", "so", "then",
    "also", "plus", "i", "we", "you", "they", "he", "she", "it", "can", "could", "would", "will",
    "should", "please", "is", "are", "was", "were", "do", "does", "did", "have", "has", "had",
    "am", "let", "now", "well", "okay", "ok",
];

const FILLERS: &[&str] = &["um", "umm", "uh", "uhh", "erm", "ah", "ahh", "mhm", "hmm"];

static SPOKEN_COMMAND_REGEXES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    SAFE_SPOKEN_COMMANDS
        .iter()
        .map(|(kw, rep)| {
            let pattern = format!(r"\s*\b{}\b\s*", regex::escape(kw));
            let re = RegexBuilder::new(&pattern)
                .case_insensitive(true)
                .build()
                .expect("valid spoken command pattern");
            (re, *rep)
        })
        .collect()
});

static AMBIGUOUS_LOOKUP: Lazy<std::collections::HashMap<&'static str, &'static str>> =
    Lazy::new(|| {
        AMBIGUOUS_SPOKEN
            .iter()
            .copied()
            .collect::<std::collections::HashMap<_, _>>()
    });

static CLAUSE_START_SET: Lazy<std::collections::HashSet<&'static str>> =
    Lazy::new(|| CLAUSE_START_WORDS.iter().copied().collect());

static FILLER_REGEX: Lazy<Regex> = Lazy::new(|| {
    let alternation = FILLERS
        .iter()
        .map(|f| regex::escape(f))
        .collect::<Vec<_>>()
        .join("|");
    let pattern = format!(r"\s*,?\s*\b(?:{})\b\s*,?\s*", alternation);
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .expect("valid filler pattern")
});

static WS_BEFORE_PUNCT: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+([.,?!;:])").unwrap());
static MULTIPLE_SPACES: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]{2,}").unwrap());
static LEADING_WS_ON_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(^|\n)[ \t]+").unwrap());
static WORD: Lazy<Regex> = Lazy::new(|| Regex::new(r"\w+").unwrap());
static STANDALONE_I: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bi\b").unwrap());

const TERMINAL_PUNCT: &[char] = &['.', '!', '?'];

pub fn apply(input: &str) -> String {
    let text = replace_spoken_commands(input);
    let text = remove_fillers(&text);
    let text = collapse_whitespace(&text);
    let text = collapse_repeated_words(&text);
    let text = capitalize_sentences(&text);
    let text = fix_standalone_i(&text);
    let text = ensure_terminal_punctuation(&text);
    text.trim().to_string()
}

pub fn replace_spoken_commands(text: &str) -> String {
    let mut result = replace_ambiguous_spoken(text);
    for (re, rep) in SPOKEN_COMMAND_REGEXES.iter() {
        result = re.replace_all(&result, *rep).into_owned();
    }
    result
}

fn replace_ambiguous_spoken(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let mut out = Vec::with_capacity(words.len());
    for (index, word) in words.iter().enumerate() {
        let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
        let lower = bare.to_ascii_lowercase();
        if let Some(rep) = AMBIGUOUS_LOOKUP.get(lower.as_str()) {
            let at_end = index + 1 == words.len();
            let next_starter = words.get(index + 1).is_some_and(|next| {
                let bare_next = next
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_ascii_lowercase();
                CLAUSE_START_SET.contains(bare_next.as_str())
            });
            if at_end || next_starter {
                out.push(*rep);
                continue;
            }
        }
        out.push(*word);
    }
    out.join(" ")
}

pub fn remove_fillers(text: &str) -> String {
    FILLER_REGEX.replace_all(text, " ").into_owned()
}

pub fn collapse_whitespace(text: &str) -> String {
    let a = WS_BEFORE_PUNCT.replace_all(text, "$1");
    let b = MULTIPLE_SPACES.replace_all(&a, " ");
    let c = LEADING_WS_ON_LINE.replace_all(&b, "$1");
    c.into_owned()
}

/// Drop a word that repeats the previous one with only spaces between them
/// ("the the cat" → "the cat"), case-insensitively, keeping the first.
pub fn collapse_repeated_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied_to = 0;
    // (text of the last kept word, end of the last word seen)
    let mut prev: Option<(&str, usize)> = None;
    for word in WORD.find_iter(text) {
        if let Some((kept, prev_end)) = prev {
            let gap = &text[prev_end..word.start()];
            if !gap.is_empty()
                && gap.bytes().all(|b| b == b' ')
                && kept.to_lowercase() == word.as_str().to_lowercase()
            {
                out.push_str(&text[copied_to..prev_end]);
                copied_to = word.end();
                prev = Some((kept, word.end()));
                continue;
            }
        }
        prev = Some((word.as_str(), word.end()));
    }
    out.push_str(&text[copied_to..]);
    out
}

pub fn capitalize_sentences(text: &str) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let mut out: Vec<char> = text.chars().collect();
    let mut capitalize_next = true;
    for ch in &mut out {
        if capitalize_next && ch.is_alphabetic() {
            if let Some(uc) = ch.to_uppercase().next() {
                *ch = uc;
            }
            capitalize_next = false;
        } else if TERMINAL_PUNCT.contains(ch) || *ch == '\n' {
            capitalize_next = true;
        } else if !ch.is_whitespace() {
            capitalize_next = false;
        }
    }
    out.into_iter().collect()
}

pub fn fix_standalone_i(text: &str) -> String {
    STANDALONE_I.replace_all(text, "I").into_owned()
}

pub fn ensure_terminal_punctuation(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let word_count = trimmed.split_whitespace().count();
    if word_count < 2 {
        return trimmed.to_string();
    }
    if let Some(last) = trimmed.chars().last() {
        if TERMINAL_PUNCT.contains(&last) || last == ',' || last == ':' || last == ';' {
            return trimmed.to_string();
        }
    }
    format!("{}.", trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_period() {
        assert_eq!(apply("hello full stop how are you"), "Hello. How are you.");
    }

    #[test]
    fn filler_um_removed() {
        assert_eq!(apply("um hello world"), "Hello world.");
    }

    #[test]
    fn repeated_words_collapsed() {
        assert_eq!(apply("the the quick brown fox"), "The quick brown fox.");
    }

    #[test]
    fn repeated_words_collapse_runs_and_ignore_case() {
        assert_eq!(collapse_repeated_words("The the the cat"), "The cat");
        assert_eq!(collapse_repeated_words("go go, go"), "go, go");
        assert_eq!(collapse_repeated_words("a b a b"), "a b a b");
        assert_eq!(
            collapse_repeated_words("no repeats here"),
            "no repeats here"
        );
    }

    #[test]
    fn standalone_i_capitalized() {
        assert_eq!(apply("i went home"), "I went home.");
    }

    #[test]
    fn adds_terminal_period() {
        assert_eq!(apply("this is a sentence"), "This is a sentence.");
    }

    #[test]
    fn preserves_existing_question_mark() {
        assert_eq!(apply("what is this"), "What is this.");
        assert_eq!(apply("what is this?"), "What is this?");
    }

    #[test]
    fn capitalize_after_period() {
        assert_eq!(apply("hello. how are you"), "Hello. How are you.");
    }

    #[test]
    fn empty_input() {
        assert_eq!(apply(""), "");
    }

    #[test]
    fn legitimate_adjacent_words_preserved() {
        // "in inside" and "my myself" share no space-separated duplicate,
        // so they should NOT collapse.
        let out = apply("we went in inside the house");
        assert!(out.contains("in inside"), "got: {}", out);
    }

    #[test]
    fn new_paragraph_command() {
        assert_eq!(
            apply("first line new paragraph second line"),
            "First line\n\nSecond line."
        );
    }

    #[test]
    fn period_in_noun_phrase_not_replaced() {
        assert_eq!(
            apply("the Cretaceous period ended abruptly"),
            "The Cretaceous period ended abruptly."
        );
    }

    #[test]
    fn period_as_command_before_new_clause() {
        assert_eq!(apply("hello period how are you"), "Hello. How are you.");
    }

    #[test]
    fn period_as_command_at_end() {
        assert_eq!(apply("that is all period"), "That is all.");
    }
}
