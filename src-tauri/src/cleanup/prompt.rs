//! Backend-independent text logic shared by every cleanup backend (Ollama,
//! LM Studio). Both backends build the same prompt and strip the same model
//! preambles, so cleanup behavior is identical regardless of which LLM runs.

const COLON_TAIL_PREAMBLES: &[&str] = &["Here is the", "Here's the", "Here is", "Here's", "Sure,"];
const FULL_PREAMBLES: &[&str] = &["Output:", "Cleaned:"];

const PROMPT_HEADER: &str = "You are a literal text editor. Fix only obvious typos, missing \
punctuation, missing capitalization, and short transcription fragments where a partial word \
appears immediately before its full form (e.g., \"dict dictating\" → \"dictating\", \
\"s sentence\" → \"sentence\"). Do NOT change real words. Do NOT rephrase. Leave legitimate \
adjacent words alone (e.g., \"in inside\", \"my myself\", \"a apple\"). Output ONLY the fixed text.

Input: i went to the store and bought some apples
Output: I went to the store and bought some apples.

Input: she said hi and then walked away
Output: She said hi and then walked away.

Input: i am dict dictating a s sentence
Output: I am dictating a sentence.

Input: we went in inside the house
Output: We went in inside the house.

";

pub fn build_prompt(input: &str) -> String {
    format!("{}Input: {}\nOutput:", PROMPT_HEADER, input)
}

pub fn num_predict_cap(input: &str) -> i32 {
    let words = input.split_whitespace().count() as i32;
    60.max(words * 2)
}

pub fn strip_preamble(input: &str) -> String {
    let mut text = input.trim().to_string();

    for preamble in COLON_TAIL_PREAMBLES {
        if !text.to_lowercase().starts_with(&preamble.to_lowercase()) {
            continue;
        }
        let after = &text[preamble.len()..];
        if let Some(colon_idx) = after.find(':') {
            let head = &after[..colon_idx];
            if head.chars().count() <= 20 {
                text = after[colon_idx + 1..].trim().to_string();
                break;
            }
        }
    }

    for preamble in FULL_PREAMBLES {
        if text.to_lowercase().starts_with(&preamble.to_lowercase()) {
            text = text[preamble.len()..].trim().to_string();
            break;
        }
    }

    if text.starts_with('"') {
        text.remove(0);
        if text.ends_with('"') {
            text.pop();
        }
    }

    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_heres_the_cleaned_text() {
        assert_eq!(
            strip_preamble("Here's the cleaned text: Hello world."),
            "Hello world."
        );
    }

    #[test]
    fn strip_output_prefix() {
        assert_eq!(strip_preamble("Output: Hello."), "Hello.");
    }

    #[test]
    fn strip_quotes() {
        assert_eq!(strip_preamble("\"Hello world.\""), "Hello world.");
    }

    #[test]
    fn preserves_heres_without_colon_tail() {
        // No colon after "Here's" → not a preamble, real content.
        assert_eq!(
            strip_preamble("Here's how I did it."),
            "Here's how I did it."
        );
    }

    #[test]
    fn num_predict_floor_60() {
        assert_eq!(num_predict_cap("hi"), 60);
    }

    #[test]
    fn num_predict_scales_with_words() {
        let s = "a b c d e f g h i j k l m n o p q r s t u v w x y z ".repeat(5);
        let cap = num_predict_cap(&s);
        assert!(cap > 60);
    }
}
