use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use parking_lot::Mutex;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
pub const MIN_DURATION_SECS: f32 = 0.3;
pub const MAX_DURATION_SECS: f32 = 60.0;

#[derive(Clone)]
pub struct PcmBuffer {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl PcmBuffer {
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples.len() as f32 / self.sample_rate as f32
        }
    }
}

pub enum AudioCmd {
    Start(Option<String>),
    Stop(mpsc::Sender<Result<PcmBuffer>>),
    Snapshot(mpsc::Sender<Result<PcmBuffer>>),
}

#[derive(Clone)]
pub struct AudioController {
    tx: mpsc::Sender<AudioCmd>,
    pub device: Arc<Mutex<Option<String>>>,
}

impl AudioController {
    pub fn spawn() -> (Self, mpsc::Receiver<f32>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AudioCmd>();
        let (level_tx, level_rx) = mpsc::channel::<f32>();
        thread::spawn(move || run(cmd_rx, level_tx));
        (
            AudioController {
                tx: cmd_tx,
                device: Arc::new(Mutex::new(None)),
            },
            level_rx,
        )
    }

    pub fn start(&self) -> Result<()> {
        let dev = self.device.lock().clone();
        self.tx
            .send(AudioCmd::Start(dev))
            .map_err(|_| anyhow!("audio thread dead"))
    }

    pub fn stop(&self) -> Result<PcmBuffer> {
        let (resp_tx, resp_rx) = mpsc::channel();
        self.tx
            .send(AudioCmd::Stop(resp_tx))
            .map_err(|_| anyhow!("audio thread dead"))?;
        resp_rx.recv().context("audio stop reply")?
    }

    pub fn snapshot(&self) -> Result<PcmBuffer> {
        let (resp_tx, resp_rx) = mpsc::channel();
        self.tx
            .send(AudioCmd::Snapshot(resp_tx))
            .map_err(|_| anyhow!("audio thread dead"))?;
        resp_rx.recv().context("audio snapshot reply")?
    }

    pub fn set_device(&self, name: Option<String>) {
        *self.device.lock() = name;
    }
}

pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|iter| iter.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

struct ActiveStream {
    _stream: cpal::Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
}

fn run(rx: mpsc::Receiver<AudioCmd>, level_tx: mpsc::Sender<f32>) {
    let mut active: Option<ActiveStream> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            AudioCmd::Start(device_name) => {
                if active.is_some() {
                    tracing::warn!("start received while already recording; ignoring");
                    continue;
                }
                match build_stream(level_tx.clone(), device_name.as_deref()) {
                    Ok(s) => active = Some(s),
                    Err(e) => tracing::error!("failed to start audio: {:?}", e),
                }
            }
            AudioCmd::Stop(reply) => {
                let result = match active.take() {
                    Some(s) => finalize(s),
                    None => Err(anyhow!("stop received but not recording")),
                };
                let _ = reply.send(result);
            }
            AudioCmd::Snapshot(reply) => {
                let result = match active.as_ref() {
                    Some(s) => snapshot_buffer(s),
                    None => Err(anyhow!("snapshot received but not recording")),
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn build_stream(level_tx: mpsc::Sender<f32>, device_name: Option<&str>) -> Result<ActiveStream> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host
            .input_devices()
            .ok()
            .and_then(|mut it| it.find(|d| d.name().as_deref().ok() == Some(name)))
            .or_else(|| {
                tracing::warn!("input device '{}' not found; using default", name);
                host.default_input_device()
            })
            .ok_or_else(|| anyhow!("no input device available"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device"))?,
    };
    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let sample_format = config.sample_format();

    tracing::info!(
        "audio device: {:?}  rate: {}  channels: {}  format: {:?}",
        device.name().unwrap_or_default(),
        sample_rate,
        channels,
        sample_format
    );

    let buffer = Arc::new(Mutex::new(Vec::<f32>::with_capacity(
        sample_rate as usize * 10,
    )));
    let err_fn = |err| tracing::error!("audio stream error: {}", err);
    let stream_config = config.config();
    let ch = channels as usize;

    let stream = match sample_format {
        SampleFormat::F32 => {
            let buf = buffer.clone();
            let ltx = level_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    let _ = ltx.send(rms(data));
                    buf.lock().extend_from_slice(data);
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let buf = buffer.clone();
            let ltx = level_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let mut guard = buf.lock();
                    let before = guard.len();
                    guard.extend(data.iter().map(|&s| s as f32 / i16::MAX as f32));
                    let _ = ltx.send(rms(&guard[before..]));
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let buf = buffer.clone();
            let ltx = level_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    let mut guard = buf.lock();
                    let before = guard.len();
                    guard
                        .extend(data.iter().map(|&s| {
                            (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0)
                        }));
                    let _ = ltx.send(rms(&guard[before..]));
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {:?}", other)),
    };

    stream.play()?;
    let _ = ch; // reserved for multi-channel bookkeeping if ever needed

    Ok(ActiveStream {
        _stream: stream,
        buffer,
        sample_rate,
        channels,
    })
}

fn finalize(active: ActiveStream) -> Result<PcmBuffer> {
    let samples = {
        let mut guard = active.buffer.lock();
        std::mem::take(&mut *guard)
    };

    if samples.is_empty() {
        return Err(anyhow!("no audio captured"));
    }

    // 1. Downmix to mono if needed
    let mono = downmix_mono(&samples, active.channels);

    // 2. Resample to 16 kHz if needed
    let resampled = if active.sample_rate == TARGET_SAMPLE_RATE {
        mono
    } else {
        resample_linear(&mono, active.sample_rate, TARGET_SAMPLE_RATE)
    };

    // 3. DSP: DC offset, high-pass at 80 Hz, silence trim
    let dc_removed = remove_dc_offset(&resampled);
    let hp = high_pass_filter(&dc_removed, TARGET_SAMPLE_RATE, 80.0);
    let trimmed = trim_silence(&hp, TARGET_SAMPLE_RATE, 0.01, 0.10, 0.20);

    // 4. Truncate to max duration
    let max_samples = (TARGET_SAMPLE_RATE as f32 * MAX_DURATION_SECS) as usize;
    let final_samples = if trimmed.len() > max_samples {
        trimmed[..max_samples].to_vec()
    } else {
        trimmed
    };

    tracing::info!(
        "audio finalized: {} samples ({:.2}s) at {} Hz",
        final_samples.len(),
        final_samples.len() as f32 / TARGET_SAMPLE_RATE as f32,
        TARGET_SAMPLE_RATE
    );

    Ok(PcmBuffer {
        samples: final_samples,
        sample_rate: TARGET_SAMPLE_RATE,
    })
}

/// Non-destructive read of the in-progress recording buffer for live previews.
fn snapshot_buffer(active: &ActiveStream) -> Result<PcmBuffer> {
    let samples = active.buffer.lock().clone();
    if samples.is_empty() {
        return Err(anyhow!("no audio captured yet"));
    }

    let mono = downmix_mono(&samples, active.channels);
    let resampled = if active.sample_rate == TARGET_SAMPLE_RATE {
        mono
    } else {
        resample_linear(&mono, active.sample_rate, TARGET_SAMPLE_RATE)
    };

    let dc_removed = remove_dc_offset(&resampled);
    let hp = high_pass_filter(&dc_removed, TARGET_SAMPLE_RATE, 80.0);

    Ok(PcmBuffer {
        samples: hp,
        sample_rate: TARGET_SAMPLE_RATE,
    })
}

/// Save a PcmBuffer to a WAV file for debugging.
pub fn save_wav(buffer: &PcmBuffer) -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow!("no data_local_dir"))?
        .join("DictaFlow")
        .join("recordings");
    std::fs::create_dir_all(&dir)?;

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("rec-{}.wav", stamp));

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: buffer.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(&path, spec)?;
    for s in &buffer.samples {
        let clamped = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(clamped)?;
    }
    writer.finalize()?;

    Ok(path)
}

// ---------- Pure DSP functions (testable) ----------

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt().min(1.0)
}

pub fn downmix_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let ch = channels as usize;
    let frames = interleaved.len() / ch;
    let mut out = Vec::with_capacity(frames);
    for frame in 0..frames {
        let sum: f32 = interleaved[frame * ch..frame * ch + ch].iter().sum();
        out.push(sum / ch as f32);
    }
    out
}

/// Simple linear resampler. Whisper applies its own preprocessing so the
/// slight aliasing this introduces is inaudible to the model.
pub fn resample_linear(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = dst_rate as f64 / src_rate as f64;
    let out_len = (samples.len() as f64 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = samples[idx];
        let b = if idx + 1 < samples.len() {
            samples[idx + 1]
        } else {
            a
        };
        out.push(a + (b - a) * frac);
    }
    out
}

pub fn remove_dc_offset(samples: &[f32]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let sum: f32 = samples.iter().sum();
    let mean = sum / samples.len() as f32;
    samples.iter().map(|s| s - mean).collect()
}

/// Single-pole IIR high-pass filter: y[n] = α * (y[n-1] + x[n] - x[n-1])
/// where α = RC / (RC + dt), RC = 1 / (2π * cutoffHz), dt = 1 / sampleRate.
pub fn high_pass_filter(samples: &[f32], sample_rate: u32, cutoff_hz: f64) -> Vec<f32> {
    if samples.len() < 2 || cutoff_hz <= 0.0 {
        return samples.to_vec();
    }
    let rc = 1.0 / (2.0 * std::f64::consts::PI * cutoff_hz);
    let dt = 1.0 / sample_rate as f64;
    let alpha = (rc / (rc + dt)) as f32;
    let mut out = vec![0.0; samples.len()];
    out[0] = samples[0];
    for i in 1..samples.len() {
        out[i] = alpha * (out[i - 1] + samples[i] - samples[i - 1]);
    }
    out
}

/// Energy-based head/tail silence trim with leading/trailing margin so
/// first and last syllables aren't clipped.
pub fn trim_silence(
    samples: &[f32],
    sample_rate: u32,
    threshold: f32,
    leading_margin_secs: f32,
    trailing_margin_secs: f32,
) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let window = (sample_rate / 40).max(1) as usize; // 25 ms
    let margin_lead = (sample_rate as f32 * leading_margin_secs) as usize;
    let margin_trail = (sample_rate as f32 * trailing_margin_secs) as usize;

    let mut first_active: Option<usize> = None;
    let mut i = 0;
    while i + window <= samples.len() {
        if rms(&samples[i..i + window]) > threshold {
            first_active = Some(i);
            break;
        }
        i += window;
    }
    let first_active = match first_active {
        Some(v) => v,
        None => return Vec::new(),
    };

    let mut last_active_end = samples.len();
    if samples.len() >= window {
        let mut j = samples.len() - window;
        while j > first_active {
            let end = (j + window).min(samples.len());
            if rms(&samples[j..end]) > threshold {
                last_active_end = end;
                break;
            }
            if j < window {
                break;
            }
            j -= window;
        }
    }

    let start = first_active.saturating_sub(margin_lead);
    let stop = (last_active_end + margin_trail).min(samples.len());
    if start >= stop {
        return Vec::new();
    }
    samples[start..stop].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&vec![0.0; 100]), 0.0);
    }

    #[test]
    fn rms_of_sine_is_root_half_amplitude() {
        let n = 1000;
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * i as f32 / 50.0).sin())
            .collect();
        let r = rms(&samples);
        assert!((r - (0.5f32).sqrt()).abs() < 0.05, "rms={}", r);
    }

    #[test]
    fn downmix_stereo_to_mono() {
        let stereo = vec![1.0, 0.0, 0.5, 0.5];
        let mono = downmix_mono(&stereo, 2);
        assert_eq!(mono, vec![0.5, 0.5]);
    }

    #[test]
    fn resample_48k_to_16k_matches_length() {
        let samples = vec![0.1_f32; 48_000];
        let out = resample_linear(&samples, 48_000, 16_000);
        assert!((out.len() as i32 - 16_000).abs() < 2);
    }

    #[test]
    fn dc_offset_removed() {
        let samples = vec![1.0, 1.0, 1.0, 1.0];
        let out = remove_dc_offset(&samples);
        assert!(out.iter().all(|&s| s.abs() < 1e-6));
    }
}
