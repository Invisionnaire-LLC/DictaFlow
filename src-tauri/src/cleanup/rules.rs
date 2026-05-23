use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};

const SPOKEN_COMMANDS: &[(&str, &str)] = &[
    ("new paragraph", "\n\n"),
    ("new line", "\n"),
    ("full stop", ". "),
    ("period", ". "),
    ("comma", ", "),
    ("question mark", "? "),
    ("exclamation point", "! "),
    ("exclamation mark", "! "),
    ("semicolon", "; "),
    ("colon", ": "),
];

const FILLERS: &[&str] = &["um", "umm", "uh", "uhh", "erm", "ah", "ahh", "mhm", "hmm"];

static SPOKEN_COMMAND_REGEXES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    SPOKEN_COMMANDS
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
static REPEATED_WORDS: Lazy<fancy_regex::Regex> =
    Lazy::new(|| fancy_regex::Regex::new(r"(?i)\b(\w+)( +\1\b)+").unwrap());
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
    let mut result = text.to_string();
    for (re, rep) in SPOKEN_COMMAND_REGEXES.iter() {
        result = re.replace_all(&result, *rep).into_owned();
    }
    result
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

pub fn collapse_repeated_words(text: &str) -> String {
    REPEATED_WORDS.replace_all(text, "$1").into_owned()
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
        assert_eq!(apply("hello period how are you"), "Hello. How are you.");
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
}
