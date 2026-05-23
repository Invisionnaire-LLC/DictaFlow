//! Real whisper-rs backend. Drop in as `asr.rs` to enable native transcription.
//! Requires: `whisper-rs = { version = "0.14", default-features = false }` in Cargo.toml,
//! CMake, LLVM (libclang), and MSVC or equivalent C++ toolchain.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub const MODEL_FILENAME: &str = "ggml-large-v3-turbo.bin";
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin";
pub const EXPECTED_BYTES: u64 = 1_624_555_275;

pub const VOCAB_FILENAME: &str = "vocabulary.txt";
const DEFAULT_VOCAB: &str = "DictaFlow, Invisionnaire.\n";

pub struct Transcriber {
    ctx: Mutex<WhisperContext>,
    vocabulary: String,
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
        seed_vocabulary_if_missing();
        Ok(Transcriber {
            ctx: Mutex::new(ctx),
            vocabulary: load_vocabulary(),
        })
    }

    pub fn prewarm(&self) -> Result<()> {
        let silent = vec![0.0_f32; 16_000];
        let _ = self.transcribe(&silent)?;
        Ok(())
    }

    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        self.transcribe_inner(samples, true, false)?
            .ok_or_else(|| anyhow!("empty transcription"))
    }

    /// Live preview pass — only the most recent audio tail to keep latency bounded.
    pub fn transcribe_partial(&self, samples: &[f32]) -> Result<Option<String>> {
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
        self.transcribe_inner(tail, false, true)
    }

    fn transcribe_inner(
        &self,
        samples: &[f32],
        log_result: bool,
        live_preview: bool,
    ) -> Result<Option<String>> {
        let start = std::time::Instant::now();
        let audio_secs = samples.len() as f32 / 16_000.0;
        let ctx = self.ctx.lock();
        let mut state = ctx.create_state().context("whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(num_cpus());
        params.set_translate(false);
        params.set_language(Some("en"));
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
            let vocab = crate::dictionary::whisper_prompt();
            if vocab.is_empty() {
                if !self.vocabulary.is_empty() {
                    params.set_initial_prompt(&self.vocabulary);
                }
            } else {
                params.set_initial_prompt(&vocab);
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
        if live_preview {
            if trimmed.is_empty() || looks_like_partial_hallucination(&trimmed) {
                return Ok(None);
            }
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

fn seed_vocabulary_if_missing() {
    if let Some(path) = vocabulary_path() {
        if !path.exists() {
            if let Err(e) = fs::write(&path, DEFAULT_VOCAB) {
                tracing::warn!("could not seed vocabulary at {}: {:?}", path.display(), e);
            } else {
                tracing::info!("seeded vocabulary at {}", path.display());
            }
        }
    }
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

pub fn is_model_downloaded() -> bool {
    match model_path() {
        Ok(p) => {
            p.exists()
                && p.metadata()
                    .map(|m| m.len() > 1_000_000_000)
                    .unwrap_or(false)
        }
        Err(_) => false,
    }
}

pub fn download_model<F: FnMut(u64, u64)>(mut progress: F) -> Result<PathBuf> {
    let dest = model_path()?;
    if is_model_downloaded() {
        return Ok(dest);
    }
    let tmp = dest.with_extension("bin.tmp");

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
    fs::rename(&tmp, &dest)?;
    tracing::info!("whisper model ready: {} bytes", downloaded);
    Ok(dest)
}
