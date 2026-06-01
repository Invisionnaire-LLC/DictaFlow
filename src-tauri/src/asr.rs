//! Real whisper-rs backend. Drop in as `asr.rs` to enable native transcription.
//! Requires: `whisper-rs = { version = "0.14", default-features = false }` in Cargo.toml,
//! CMake, LLVM (libclang), and MSVC or equivalent C++ toolchain.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::settings::LanguageMode;

pub const MODEL_FILENAME: &str = "ggml-large-v3-turbo.bin";
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin";
pub const EXPECTED_BYTES: u64 = 1_624_555_275;

pub const VOCAB_FILENAME: &str = "vocabulary.txt";

pub struct Transcriber {
    ctx: Mutex<WhisperContext>,
}

impl Transcriber {
    pub fn new(model_path: &Path) -> Result<Self> {
        let mut params = WhisperContextParameters::default();
        params.use_gpu(true).flash_attn(true);
        tracing::info!(
            "whisper context: use_gpu={}, gpu_device={}, flash_attn={}",
            params.use_gpu,
            params.gpu_device,
            params.flash_attn
        );
        let ctx = WhisperContext::new_with_params(
            model_path
                .to_str()
                .ok_or_else(|| anyhow!("model path not utf-8"))?,
            params,
        )
        .context("whisper context init")?;
        Ok(Transcriber {
            ctx: Mutex::new(ctx),
        })
    }

    pub fn prewarm(&self) -> Result<()> {
        let silent = vec![0.0_f32; 16_000];
        let _ = self.transcribe(&silent, LanguageMode::Auto)?;
        Ok(())
    }

    pub fn transcribe(&self, samples: &[f32], lang: LanguageMode) -> Result<String> {
        self.transcribe_inner(samples, true, false, lang)?
            .ok_or_else(|| anyhow!("empty transcription"))
    }

    /// Live preview pass — only the most recent audio tail to keep latency bounded.
    pub fn transcribe_partial(
        &self,
        samples: &[f32],
        lang: LanguageMode,
    ) -> Result<Option<String>> {
        const MAX_PARTIAL_SAMPLES: usize = 16_000 * 2;
        const MIN_PARTIAL_SAMPLES: usize = 16_000 * 3 / 10;
        let tail = if samples.len() > MAX_PARTIAL_SAMPLES {
            &samples[samples.len() - MAX_PARTIAL_SAMPLES..]
        } else {
            samples
        };
        if tail.len() < MIN_PARTIAL_SAMPLES {
            return Ok(None);
        }
        if !tail_has_speech(tail) {
            return Ok(None);
        }
        self.transcribe_inner(tail, false, true, lang)
    }

    fn transcribe_inner(
        &self,
        samples: &[f32],
        log_result: bool,
        live_preview: bool,
        lang: LanguageMode,
    ) -> Result<Option<String>> {
        let start = std::time::Instant::now();
        let audio_secs = samples.len() as f32 / 16_000.0;
        let ctx = self.ctx.lock();
        let mut state = ctx.create_state().context("whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(num_cpus());
        params.set_translate(lang.translate_to_english());
        params.set_language(lang.whisper_language());
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_context(true);
        params.set_temperature(0.0);
        if live_preview {
            params.set_single_segment(true);
            params.set_suppress_nst(true);
            params.set_entropy_thold(2.2);
            params.set_logprob_thold(-1.0);
            // Vocabulary prompt helps final transcription but can leak into live preview.
        } else {
            params.set_single_segment(false);
            // `prompt` must outlive `state.full` below; whisper-rs copies it internally.
            let prompt = initial_prompt();
            if !prompt.is_empty() {
                params.set_initial_prompt(&prompt);
            }
        }

        state.full(params, samples).context("whisper full")?;

        let n_segments = state.full_n_segments();
        let mut out = String::new();
        for i in 0..n_segments {
            let seg = state
                .get_segment(i)
                .ok_or_else(|| anyhow!("segment {} missing", i))?;
            if live_preview && !segment_is_reliable(&seg) {
                tracing::debug!(
                    "partial segment rejected: no_speech={:.2}, avg_prob={:.2}",
                    seg.no_speech_probability(),
                    segment_avg_token_probability(&seg)
                );
                continue;
            }
            let text = seg.to_str().context("segment text")?;
            out.push_str(text);
        }
        let elapsed = start.elapsed();
        let rtf = if elapsed.as_secs_f32() > 0.0 {
            audio_secs / elapsed.as_secs_f32()
        } else {
            0.0
        };
        let trimmed = out.trim().to_string();
        if live_preview && (trimmed.is_empty() || looks_like_partial_hallucination(&trimmed)) {
            return Ok(None);
        }
        if log_result {
            tracing::info!(
                "transcribed {:.2}s audio in {}ms (rtf={:.2}x)",
                audio_secs,
                elapsed.as_millis(),
                rtf
            );
        } else {
            tracing::debug!(
                "partial transcribed {:.2}s audio in {}ms",
                audio_secs,
                elapsed.as_millis()
            );
        }
        Ok(if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        })
    }
}

const PARTIAL_MIN_RMS: f32 = 0.008;
const PARTIAL_TAIL_RMS_SECS: f32 = 0.35;
const PARTIAL_MAX_NO_SPEECH: f32 = 0.72;
const PARTIAL_MIN_AVG_TOKEN_PROB: f32 = 0.28;

pub fn tail_has_speech(samples: &[f32]) -> bool {
    let tail_samples = (16_000.0 * PARTIAL_TAIL_RMS_SECS) as usize;
    let start = samples.len().saturating_sub(tail_samples);
    crate::audio::rms(&samples[start..]) >= PARTIAL_MIN_RMS
}

fn segment_avg_token_probability(seg: &whisper_rs::WhisperSegment<'_>) -> f32 {
    let n = seg.n_tokens();
    if n == 0 {
        return 0.0;
    }
    let sum: f32 = (0..n)
        .filter_map(|i| seg.get_token(i).map(|token| token.token_probability()))
        .sum();
    sum / n as f32
}

fn segment_is_reliable(seg: &whisper_rs::WhisperSegment<'_>) -> bool {
    seg.no_speech_probability() <= PARTIAL_MAX_NO_SPEECH
        && segment_avg_token_probability(seg) >= PARTIAL_MIN_AVG_TOKEN_PROB
}

fn looks_like_partial_hallucination(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const PHRASES: &[&str] = &[
        "thank you for watching",
        "thanks for watching",
        "please subscribe",
        "subtitles by",
        "subtitle by",
        "amara.org",
        "audible.com",
        "copyright",
        "all rights reserved",
        "[music]",
        "(music)",
        "♪",
        "www.",
        ".com",
        "mbc news",
        "breaking news",
    ];
    PHRASES.iter().any(|phrase| lower.contains(phrase))
}

fn num_cpus() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .min(8)
}

pub fn model_path() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow!("no data_local_dir"))?
        .join("DictaFlow")
        .join("models");
    fs::create_dir_all(&dir).ok();
    Ok(dir.join(MODEL_FILENAME))
}

pub fn vocabulary_path() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("DictaFlow");
    fs::create_dir_all(&dir).ok();
    Some(dir.join(VOCAB_FILENAME))
}

fn load_vocabulary() -> String {
    match vocabulary_path() {
        Some(path) => fs::read_to_string(&path)
            .unwrap_or_default()
            .trim()
            .to_string(),
        None => String::new(),
    }
}

/// User-editable custom words/names (comma-separated) that bias transcription.
pub fn read_vocabulary() -> String {
    load_vocabulary()
}

/// Persist the custom vocabulary. Applied fresh on the next transcription.
pub fn write_vocabulary(text: &str) -> Result<()> {
    let path = vocabulary_path().ok_or_else(|| anyhow!("no vocabulary path"))?;
    fs::write(&path, text.trim()).context("write vocabulary")?;
    Ok(())
}

/// The Whisper initial prompt: custom vocabulary plus dictionary terms.
/// Read fresh each call so edits apply without restarting the app.
pub fn initial_prompt() -> String {
    let mut parts: Vec<String> = Vec::new();
    let vocab = load_vocabulary();
    if !vocab.is_empty() {
        parts.push(vocab);
    }
    let dict = crate::dictionary::whisper_prompt();
    if !dict.is_empty() {
        parts.push(dict);
    }
    parts.join(" ")
}

pub fn is_model_downloaded() -> bool {
    model_path()
        .ok()
        .filter(|path| path.exists())
        .is_some_and(|path| validate_model_file(&path).is_ok())
}

pub fn validate_model_file(path: &Path) -> Result<()> {
    let len = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if len != EXPECTED_BYTES {
        return Err(anyhow!(
            "model size mismatch at {}: got {} bytes, expected {}",
            path.display(),
            len,
            EXPECTED_BYTES
        ));
    }
    Ok(())
}

pub fn download_model<F: FnMut(u64, u64)>(mut progress: F) -> Result<PathBuf> {
    let dest = model_path()?;
    if is_model_downloaded() {
        return Ok(dest);
    }
    if dest.exists() {
        tracing::warn!(
            "removing invalid whisper model at {} before re-download",
            dest.display()
        );
        let _ = fs::remove_file(&dest);
    }
    let tmp = dest.with_extension("bin.tmp");
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }

    tracing::info!("downloading whisper model to {}", dest.display());
    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    let mut resp = client.get(MODEL_URL).send().context("whisper model GET")?;
    if !resp.status().is_success() {
        return Err(anyhow!("download http status {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(EXPECTED_BYTES);

    let mut file = fs::File::create(&tmp)?;
    let mut downloaded: u64 = 0;
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = std::io::Read::read(&mut resp, &mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        progress(downloaded, total);
    }
    file.flush()?;
    drop(file);

    if downloaded != EXPECTED_BYTES {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow!(
            "download incomplete: got {} bytes, expected {}",
            downloaded,
            EXPECTED_BYTES
        ));
    }
    if total != EXPECTED_BYTES {
        tracing::warn!(
            "content-length was {} but expected {}; downloaded bytes matched expected size",
            total,
            EXPECTED_BYTES
        );
    }

    validate_model_file(&tmp)?;
    fs::rename(&tmp, &dest)?;
    tracing::info!("whisper model ready: {} bytes", downloaded);
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_known_youtube_hallucinations() {
        assert!(looks_like_partial_hallucination("Thanks for watching!"));
        assert!(looks_like_partial_hallucination("Please subscribe"));
        assert!(looks_like_partial_hallucination(
            "Subtitles by the community"
        ));
        assert!(looks_like_partial_hallucination("visit www.example"));
        assert!(looks_like_partial_hallucination("[Music]"));
    }

    #[test]
    fn allows_normal_speech() {
        assert!(!looks_like_partial_hallucination(
            "Let's ship the audit fixes today."
        ));
        assert!(!looks_like_partial_hallucination("Bayamón y Guaynabo"));
        assert!(!looks_like_partial_hallucination(""));
    }

    #[test]
    fn tail_has_speech_detects_energy_above_silence() {
        let silence = vec![0.0_f32; 16_000];
        assert!(!tail_has_speech(&silence));
        let loud = vec![0.5_f32; 16_000];
        assert!(tail_has_speech(&loud));
    }
}
