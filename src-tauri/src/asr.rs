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
}

impl Transcriber {
    pub fn new(model_path: &Path) -> Result<Self> {
        let mut params = WhisperContextParameters::default();
        params.use_gpu(true).flash_attn(true);
        tracing::info!(
            "whisper context: use_gpu={}, gpu_device={}, flash_attn={}",
            params.use_gpu, params.gpu_device, params.flash_attn
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
        })
    }

    pub fn prewarm(&self) -> Result<()> {
        let silent = vec![0.0_f32; 16_000];
        let _ = self.transcribe(&silent)?;
        Ok(())
    }

    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
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
        params.set_single_segment(false);
        params.set_temperature(0.0);
        let vocab = load_vocabulary();
        if !vocab.is_empty() {
            params.set_initial_prompt(&vocab);
        }

        state.full(params, samples).context("whisper full")?;

        let n_segments = state.full_n_segments();
        let mut out = String::new();
        for i in 0..n_segments {
            let seg = state.get_segment(i).ok_or_else(|| anyhow!("segment {} missing", i))?;
            let text = seg.to_str().context("segment text")?;
            out.push_str(text);
        }
        let elapsed = start.elapsed();
        let rtf = if elapsed.as_secs_f32() > 0.0 {
            audio_secs / elapsed.as_secs_f32()
        } else {
            0.0
        };
        tracing::info!(
            "transcribed {:.2}s audio in {}ms (rtf={:.2}x)",
            audio_secs,
            elapsed.as_millis(),
            rtf
        );
        Ok(out.trim().to_string())
    }
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
        Some(path) => fs::read_to_string(&path).unwrap_or_default().trim().to_string(),
        None => String::new(),
    }
}

pub fn is_model_downloaded() -> bool {
    match model_path() {
        Ok(p) => p.exists() && p.metadata().map(|m| m.len() > 1_000_000_000).unwrap_or(false),
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
