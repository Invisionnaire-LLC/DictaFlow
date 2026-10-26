use std::io::{Seek, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use parking_lot::Mutex;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
pub const MIN_DURATION_SECS: f32 = 0.3;
pub const MAX_DURATION_SECS: f32 = 60.0;

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

/// Live-preview view of an in-progress recording.
pub struct Snapshot {
    /// The most recent audio only (16 kHz mono, filtered), not the whole recording.
    pub tail: PcmBuffer,
    /// Total audio captured so far, in seconds.
    pub captured_secs: f32,
}

pub enum AudioCmd {
    Start(Option<String>),
    Stop(mpsc::Sender<Result<PcmBuffer>>),
    Snapshot {
        tail_secs: f32,
        reply: mpsc::Sender<Result<Snapshot>>,
    },
}

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

    /// Copy only the last `tail_secs` of the recording, so preview cost stays
    /// constant no matter how long the user has been talking.
    pub fn snapshot(&self, tail_secs: f32) -> Result<Snapshot> {
        let (resp_tx, resp_rx) = mpsc::channel();
        self.tx
            .send(AudioCmd::Snapshot {
                tail_secs,
                reply: resp_tx,
            })
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
    stream: cpal::Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
    max_samples: usize,
}

impl Drop for ActiveStream {
    /// Always stop the CoreAudio unit before the stream handle is released, on
    /// every exit path (stop, thread shutdown, error). Dropping without an
    /// explicit pause has left the mic assertion held for hours on macOS.
    fn drop(&mut self) {
        if let Err(e) = self.stream.pause() {
            tracing::warn!("audio stream pause on drop failed: {}", e);
        }
    }
}

/// Raw capture released from a stopped stream, ready for post-processing.
struct Captured {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
}

/// Stop the input stream, take the captured samples, and release the device
/// *before* any DSP runs, so the microphone is freed immediately.
fn release_stream(active: ActiveStream) -> Captured {
    if let Err(e) = active.stream.pause() {
        tracing::warn!("audio stream pause failed: {}", e);
    }
    let samples = {
        let mut guard = active.buffer.lock();
        std::mem::take(&mut *guard)
    };
    if samples.len() >= active.max_samples {
        tracing::warn!(
            "recording reached the {}s cap; audio after that was not captured",
            MAX_DURATION_SECS
        );
    }
    let sample_rate = active.sample_rate;
    let channels = active.channels;
    drop(active);
    tracing::debug!("audio input stream released");
    Captured {
        samples,
        sample_rate,
        channels,
    }
}

/// Drain any queued level readings, returning the peak of `level` and the
/// backlog. Used by the UI forwarder to coalesce per-callback readings (~100/s)
/// into a throttled stream of peak values.
pub fn drain_peak(rx: &mpsc::Receiver<f32>, mut level: f32) -> f32 {
    while let Ok(l) = rx.try_recv() {
        if l > level {
            level = l;
        }
    }
    level
}

fn max_capture_samples(sample_rate: u32, channels: u16) -> usize {
    (sample_rate as usize) * (channels as usize) * MAX_DURATION_SECS as usize
}

/// Audio-callback body: convert to f32, append up to the capture cap, and report
/// the block's RMS level. Past the cap we stop appending (O(1)) rather than
/// shifting the whole buffer on every callback.
fn capture_block<T>(buffer: &Mutex<Vec<f32>>, data: &[T], max_samples: usize) -> f32
where
    T: Sample,
    f32: FromSample<T>,
{
    let mut sum_sq = 0.0f32;
    let mut guard = buffer.lock();
    let room = max_samples.saturating_sub(guard.len());
    for (i, &s) in data.iter().enumerate() {
        let v: f32 = s.to_sample();
        sum_sq += v * v;
        if i < room {
            guard.push(v);
        }
    }
    drop(guard);
    if data.is_empty() {
        0.0
    } else {
        (sum_sq / data.len() as f32).sqrt().min(1.0)
    }
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
                    Some(s) => finalize(release_stream(s)),
                    None => Err(anyhow!("stop received but not recording")),
                };
                let _ = reply.send(result);
            }
            AudioCmd::Snapshot { tail_secs, reply } => {
                let result = match active.as_ref() {
                    Some(s) => snapshot_tail(s, tail_secs),
                    None => Err(anyhow!("snapshot received but not recording")),
                };
                let _ = reply.send(result);
            }
        }
    }

    // Controller dropped: make sure a stream left open does not outlive us.
    if let Some(s) = active.take() {
        tracing::warn!("audio thread exiting with an active stream; releasing");
        drop(release_stream(s));
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

    let max_samples = max_capture_samples(sample_rate, channels);
    // Reserve the whole cap up front (pages are only committed as they fill), so
    // the real-time callback never reallocates and copies the buffer.
    let buffer = Arc::new(Mutex::new(Vec::<f32>::with_capacity(max_samples)));
    let stream_config = config.config();

    let stream = match sample_format {
        SampleFormat::F32 => {
            build_input::<f32>(&device, &stream_config, &buffer, &level_tx, max_samples)?
        }
        SampleFormat::I16 => {
            build_input::<i16>(&device, &stream_config, &buffer, &level_tx, max_samples)?
        }
        SampleFormat::U16 => {
            build_input::<u16>(&device, &stream_config, &buffer, &level_tx, max_samples)?
        }
        other => return Err(anyhow!("unsupported sample format: {:?}", other)),
    };

    stream.play()?;

    Ok(ActiveStream {
        stream,
        buffer,
        sample_rate,
        channels,
        max_samples,
    })
}

fn build_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: &Arc<Mutex<Vec<f32>>>,
    level_tx: &mpsc::Sender<f32>,
    max_samples: usize,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let buffer = buffer.clone();
    let level_tx = level_tx.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let level = capture_block(&buffer, data, max_samples);
            let _ = level_tx.send(level);
        },
        |err| tracing::error!("audio stream error: {}", err),
        None,
    )?;
    Ok(stream)
}

/// Downmix → resample to 16 kHz → remove DC → 80 Hz high-pass.
fn condition(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<f32> {
    let mono = downmix_mono(samples, channels);
    let resampled = if sample_rate == TARGET_SAMPLE_RATE {
        mono
    } else {
        resample_linear(&mono, sample_rate, TARGET_SAMPLE_RATE)
    };
    high_pass_filter(&remove_dc_offset(&resampled), TARGET_SAMPLE_RATE, 80.0)
}

fn finalize(captured: Captured) -> Result<PcmBuffer> {
    let Captured {
        samples,
        sample_rate,
        channels,
    } = captured;

    if samples.is_empty() {
        return Err(anyhow!("no audio captured"));
    }

    // Capture is already capped at MAX_DURATION_SECS, so no truncation is needed here.
    let conditioned = condition(&samples, sample_rate, channels);
    let final_samples = trim_silence(&conditioned, TARGET_SAMPLE_RATE, 0.01, 0.10, 0.20);

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

/// Non-destructive read of the last `tail_secs` of the in-progress recording.
fn snapshot_tail(active: &ActiveStream, tail_secs: f32) -> Result<Snapshot> {
    let ch = active.channels.max(1) as usize;
    let (samples, captured_frames) = {
        let guard = active.buffer.lock();
        if guard.is_empty() {
            return Err(anyhow!("no audio captured yet"));
        }
        let tail_len = (tail_secs * active.sample_rate as f32).ceil() as usize * ch;
        let start = guard.len().saturating_sub(tail_len);
        (guard[start..].to_vec(), guard.len() / ch)
    };
    Ok(Snapshot {
        tail: PcmBuffer {
            samples: condition(&samples, active.sample_rate, active.channels),
            sample_rate: TARGET_SAMPLE_RATE,
        },
        captured_secs: captured_frames as f32 / active.sample_rate as f32,
    })
}

/// Write mono f32 PCM as 16-bit WAV.
pub fn write_wav<W: Write + Seek>(writer: W, samples: &[f32], sample_rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::new(writer, spec).context("wav writer")?;
    for s in samples {
        wav.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    wav.finalize().context("wav finalize")?;
    Ok(())
}

/// Save a dictation to `<app dir>/recordings`, linked from its history entry
/// (deleted with it).
pub fn save_wav(buffer: &PcmBuffer) -> Result<PathBuf> {
    let dir = crate::fs_util::app_dir()
        .ok_or_else(|| anyhow!("no app data dir"))?
        .join("recordings");
    std::fs::create_dir_all(&dir)?;

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    let path = dir.join(format!("rec-{}.wav", stamp));
    let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    write_wav(file, &buffer.samples, buffer.sample_rate)?;
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
    fn capture_block_stops_at_cap_but_still_reports_level() {
        let buf = Mutex::new(Vec::new());
        capture_block(&buf, &[0.5f32; 6], 8);
        let level = capture_block(&buf, &[0.5f32; 6], 8);
        // Keeps the first 8 samples; never shifts or grows past the cap.
        assert_eq!(buf.lock().len(), 8);
        assert!((level - 0.5).abs() < 1e-6);
    }

    #[test]
    fn capture_block_converts_integer_samples() {
        let buf = Mutex::new(Vec::new());
        capture_block(&buf, &[i16::MAX, 0, i16::MIN], 16);
        let got = buf.lock().clone();
        assert!((got[0] - 1.0).abs() < 1e-3 && got[1] == 0.0 && (got[2] + 1.0).abs() < 1e-3);
    }

    #[test]
    fn drain_peak_folds_backlog_into_max() {
        let (tx, rx) = mpsc::channel::<f32>();
        for l in [0.1, 0.7, 0.3] {
            tx.send(l).unwrap();
        }
        assert_eq!(drain_peak(&rx, 0.2), 0.7);
        // Backlog consumed; a later call returns the seed level unchanged.
        assert_eq!(drain_peak(&rx, 0.05), 0.05);
    }

    #[test]
    fn drain_peak_keeps_seed_when_seed_is_highest() {
        let (tx, rx) = mpsc::channel::<f32>();
        tx.send(0.1).unwrap();
        assert_eq!(drain_peak(&rx, 0.9), 0.9);
    }

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
