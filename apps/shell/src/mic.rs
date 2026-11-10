//! Microphone capture with cpal for dictation.
//!
//! The webview's `getUserMedia` behaves differently in every WebView (and is
//! missing on some), so the shells capture audio natively. The recording is
//! downmixed to mono, resampled to 16 kHz and returned as a 16-bit PCM WAV,
//! which every server speech engine accepts and which keeps a long dictation
//! well under the server's audio size limit.
//!
//! Long dictation is streamed in chunks: [`Mic::peek`] returns the audio
//! since the last cut (for live interim transcripts) and [`Mic::split`] cuts
//! the buffer at a pause and hands the finished part over, so a recording
//! can run far longer than one request may carry.
//!
//! Noise suppression (on unless the caller turns it off): the audio callback
//! only queues the downmixed samples; the capture thread denoises them every
//! [`PUMP_INTERVAL`] with RNNoise (see [`crate::noise`]) and keeps the result
//! at 48 kHz, so peek, split and the pause search all see the cleaned
//! signal. The callback's peak counter stays on the raw input: it answers
//! "does the device deliver sound at all", which suppression must not hide.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};

use crate::noise::{SUPPRESSOR_RATE, Suppressor};

/// Output sample rate (what speech engines expect).
pub const OUTPUT_RATE: u32 = 16_000;
/// Most audio kept between two splits (about 7.7 MiB of WAV once
/// resampled); a caller that never splits gets this much.
pub const MAX_BUFFER_SECONDS: u32 = 240;
/// A recording stops by itself after this long.
pub const MAX_RECORDING_SECONDS: u32 = 30 * 60;
/// How far back from the end [`Mic::split`] looks for a pause.
const SPLIT_SEARCH_SECONDS: f32 = 5.0;
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the capture thread denoises what the callback queued.
const PUMP_INTERVAL: Duration = Duration::from_millis(20);
/// Most raw audio queued for the denoiser; more is dropped (only when the
/// capture thread stalls for this long).
const PENDING_LIMIT_SECONDS: u32 = 10;

/// How a capture is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicOptions {
    /// Denoise the signal with RNNoise (default on).
    pub noise_suppression: bool,
}

impl Default for MicOptions {
    fn default() -> Self {
        Self {
            noise_suppression: true,
        }
    }
}

/// What the audio callback has seen so far (diagnostics only: counters, a
/// level and the kind of device, never audio or the device's name).
#[derive(Default)]
struct Stats {
    callbacks: AtomicU64,
    samples: AtomicU64,
    /// Loudest raw input sample so far, in millionths of full scale.
    peak_ppm: AtomicU32,
    /// Loudest sample after noise suppression (0 while it is off).
    out_peak_ppm: AtomicU32,
    noise_suppression: AtomicBool,
    /// Time spent denoising, in microseconds (the CPU cost on phones).
    suppress_us: AtomicU64,
    stream_errors: AtomicU64,
    last_error: Mutex<String>,
    /// Device configuration, set once the stream is built.
    config: Mutex<String>,
    /// Kind of input device (see [`device_kind`]); never its name, which
    /// may carry a person's name.
    device: Mutex<String>,
}

struct Capture {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<Result<Recording, String>>,
    /// Samples so far (mono, at `rate`) for interim transcripts.
    buffer: Arc<Mutex<Vec<f32>>>,
    /// Rate of `buffer`: 48 kHz with noise suppression, else the device's.
    rate: u32,
}

struct Recording {
    samples: Vec<f32>,
    rate: u32,
}

/// One microphone shared by all windows; at most one capture at a time.
#[derive(Default)]
pub struct Mic {
    active: Mutex<Option<Capture>>,
    /// Statistics of the latest capture, kept after it ends.
    last: Mutex<Option<(Arc<Stats>, Instant)>>,
    /// Free-form notes of the latest start (for example the audio session).
    note: Mutex<String>,
}

impl Mic {
    /// Opens the default input device and starts recording.
    pub fn start(&self, options: MicOptions) -> Result<(), String> {
        let mut active = self.active.lock().expect("mic lock");
        if let Some(old) = active.take() {
            // A capture left running (for example after a webview reload)
            // is discarded rather than reported as busy.
            let _ = old.stop.send(());
            let _ = old.thread.join();
        }
        let (stop_tx, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let shared = buffer.clone();
        let stats = Arc::new(Stats::default());
        let thread_stats = stats.clone();
        let thread = std::thread::Builder::new()
            .name("vca-mic".into())
            .spawn(move || capture(stop_rx, ready_tx, shared, thread_stats, options))
            .map_err(|e| format!("cannot start the audio thread: {e}"))?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(rate)) => {
                *self.last.lock().expect("mic last") = Some((stats, Instant::now()));
                *active = Some(Capture {
                    stop: stop_tx,
                    thread,
                    buffer,
                    rate,
                });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = stop_tx.send(());
                Err("the microphone did not start".into())
            }
        }
    }

    /// Records a note about the latest start, shown by [`Mic::diagnostics`].
    pub fn set_note(&self, note: String) {
        *self.note.lock().expect("mic note") = note;
    }

    /// A JSON snapshot of the latest capture for the diagnostics log:
    /// counters, device kind and configuration and the buffer length.
    /// Contains no audio.
    pub fn diagnostics(&self) -> String {
        let active = self.active.lock().expect("mic lock");
        let buffered = active
            .as_ref()
            .map(|c| c.buffer.lock().expect("mic buffer").len());
        let buffer_rate = active.as_ref().map(|c| c.rate);
        let last = self.last.lock().expect("mic last");
        let capture = match last.as_ref() {
            Some((stats, started)) => serde_json::json!({
                "recording": active.is_some(),
                "device": stats.device.lock().expect("mic device").clone(),
                "config": stats.config.lock().expect("mic config").clone(),
                "noiseSuppression": stats.noise_suppression.load(Ordering::Relaxed),
                "callbacks": stats.callbacks.load(Ordering::Relaxed),
                "samples": stats.samples.load(Ordering::Relaxed),
                "peakPpm": stats.peak_ppm.load(Ordering::Relaxed),
                "outPeakPpm": stats.out_peak_ppm.load(Ordering::Relaxed),
                "suppressMs": stats.suppress_us.load(Ordering::Relaxed) / 1000,
                "streamErrors": stats.stream_errors.load(Ordering::Relaxed),
                "lastError": stats.last_error.lock().expect("mic error").clone(),
                "ageMs": started.elapsed().as_millis() as u64,
                "bufferedSamples": buffered,
                "bufferRate": buffer_rate,
            }),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "capture": capture,
            "note": self.note.lock().expect("mic note").clone(),
        })
        .to_string()
    }

    /// Stops recording and returns the audio as a WAV file.
    pub fn stop(&self) -> Result<Vec<u8>, String> {
        let capture = self
            .active
            .lock()
            .expect("mic lock")
            .take()
            .ok_or_else(|| "the microphone is not recording".to_owned())?;
        let _ = capture.stop.send(());
        let recording = capture
            .thread
            .join()
            .map_err(|_| "the audio thread panicked".to_owned())??;
        let samples = resample(&recording.samples, recording.rate, OUTPUT_RATE);
        Ok(wav_pcm16(&samples, OUTPUT_RATE))
    }

    /// Returns the audio recorded so far as a WAV file, without stopping
    /// (for interim transcripts while the user is still talking).
    pub fn peek(&self) -> Result<Vec<u8>, String> {
        let active = self.active.lock().expect("mic lock");
        let capture = active
            .as_ref()
            .ok_or_else(|| "the microphone is not recording".to_owned())?;
        let samples = capture.buffer.lock().expect("mic buffer").clone();
        let samples = resample(&samples, capture.rate, OUTPUT_RATE);
        Ok(wav_pcm16(&samples, OUTPUT_RATE))
    }

    /// Cuts the buffer at the quietest moment of its last few seconds and
    /// returns the part before the cut as a WAV file; recording continues.
    /// Returns an empty vector while less than `min_seconds` is buffered.
    pub fn split(&self, min_seconds: f32) -> Result<Vec<u8>, String> {
        let active = self.active.lock().expect("mic lock");
        let capture = active
            .as_ref()
            .ok_or_else(|| "the microphone is not recording".to_owned())?;
        let mut buffer = capture.buffer.lock().expect("mic buffer");
        if (buffer.len() as f32) < min_seconds.max(0.5) * capture.rate as f32 {
            return Ok(Vec::new());
        }
        let cut = quietest_cut(&buffer, capture.rate, SPLIT_SEARCH_SECONDS);
        let head: Vec<f32> = buffer.drain(..cut).collect();
        drop(buffer);
        Ok(wav_pcm16(
            &resample(&head, capture.rate, OUTPUT_RATE),
            OUTPUT_RATE,
        ))
    }

    /// Stops recording and drops the audio.
    pub fn cancel(&self) {
        if let Some(capture) = self.active.lock().expect("mic lock").take() {
            let _ = capture.stop.send(());
            let _ = capture.thread.join();
        }
    }
}

/// Runs on the audio thread: cpal streams are not `Send` on every platform,
/// so the stream lives and dies here. With noise suppression this thread
/// also denoises what the callback queued, so the callback stays cheap.
fn capture(
    stop: mpsc::Receiver<()>,
    ready: mpsc::Sender<Result<u32, String>>,
    buffer: Arc<Mutex<Vec<f32>>>,
    stats: Arc<Stats>,
    options: MicOptions,
) -> Result<Recording, String> {
    let fail = |ready: &mpsc::Sender<Result<u32, String>>, msg: String| {
        let _ = ready.send(Err(msg.clone()));
        Err(msg)
    };
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return fail(&ready, "no microphone found".into());
    };
    *stats.device.lock().expect("mic device") = device_kind(&device.name().unwrap_or_default());
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => return fail(&ready, format!("cannot use the microphone: {e}")),
    };
    let rate = config.sample_rate().0;
    let channels = usize::from(config.channels().max(1));
    let suppress = options.noise_suppression;
    stats.noise_suppression.store(suppress, Ordering::Relaxed);
    let buffer_rate = if suppress { SUPPRESSOR_RATE } else { rate };
    let limit = (MAX_BUFFER_SECONDS * buffer_rate) as usize;
    *stats.config.lock().expect("mic config") =
        format!("{rate} Hz, {channels} ch, {:?}", config.sample_format());
    buffer
        .lock()
        .expect("mic buffer")
        .reserve(buffer_rate as usize * 10);
    // The callback writes straight into the buffer, or into a queue that
    // this thread denoises into the buffer.
    let pending = Arc::new(Mutex::new(Vec::<f32>::with_capacity(rate as usize)));
    let (target, target_limit) = if suppress {
        (pending.clone(), (PENDING_LIMIT_SECONDS * rate) as usize)
    } else {
        (buffer.clone(), limit)
    };
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let stream = match format {
        SampleFormat::F32 => {
            build::<f32>(&device, &config, channels, target_limit, &target, &stats)
        }
        SampleFormat::I16 => {
            build::<i16>(&device, &config, channels, target_limit, &target, &stats)
        }
        SampleFormat::U16 => {
            build::<u16>(&device, &config, channels, target_limit, &target, &stats)
        }
        SampleFormat::I32 => {
            build::<i32>(&device, &config, channels, target_limit, &target, &stats)
        }
        other => Err(format!("unsupported sample format {other}")),
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => return fail(&ready, e),
    };
    if let Err(e) = stream.play() {
        return fail(&ready, format!("cannot start the microphone: {e}"));
    }
    let _ = ready.send(Ok(buffer_rate));
    let max = Duration::from_secs(u64::from(MAX_RECORDING_SECONDS));
    if suppress {
        let mut denoise = Denoise::new(rate, pending, buffer.clone(), limit, stats.clone());
        let deadline = Instant::now() + max;
        loop {
            match stop.recv_timeout(PUMP_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => break,
                Err(RecvTimeoutError::Timeout) => denoise.pump(),
            }
        }
        drop(stream);
        denoise.finish();
    } else {
        match stop.recv_timeout(max) {
            Ok(()) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
        }
        drop(stream);
    }
    let samples = std::mem::take(&mut *buffer.lock().expect("mic buffer"));
    Ok(Recording {
        samples,
        rate: buffer_rate,
    })
}

/// Moves queued raw samples through the [`Suppressor`] into the buffer
/// (on the capture thread, never in the audio callback).
struct Denoise {
    suppressor: Suppressor,
    pending: Arc<Mutex<Vec<f32>>>,
    buffer: Arc<Mutex<Vec<f32>>>,
    limit: usize,
    stats: Arc<Stats>,
    /// Swapped with `pending`, so neither side allocates in steady state.
    work: Vec<f32>,
    out: Vec<f32>,
}

impl Denoise {
    fn new(
        rate: u32,
        pending: Arc<Mutex<Vec<f32>>>,
        buffer: Arc<Mutex<Vec<f32>>>,
        limit: usize,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            suppressor: Suppressor::new(rate),
            pending,
            buffer,
            limit,
            stats,
            work: Vec::with_capacity(rate as usize),
            out: Vec::with_capacity(SUPPRESSOR_RATE as usize / 10),
        }
    }

    /// Denoises everything queued so far.
    fn pump(&mut self) {
        std::mem::swap(
            &mut *self.pending.lock().expect("mic pending"),
            &mut self.work,
        );
        if self.work.is_empty() {
            return;
        }
        let started = Instant::now();
        self.out.clear();
        self.suppressor.process(&self.work, &mut self.out);
        self.work.clear();
        self.note_time(started);
        self.deliver();
    }

    /// Denoises the rest and flushes the model (after the stream stopped).
    fn finish(&mut self) {
        self.pump();
        let started = Instant::now();
        self.out.clear();
        self.suppressor.finish(&mut self.out);
        self.note_time(started);
        self.deliver();
    }

    fn note_time(&self, started: Instant) {
        self.stats
            .suppress_us
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    fn deliver(&mut self) {
        if self.out.is_empty() {
            return;
        }
        let peak = self.out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        self.stats
            .out_peak_ppm
            .fetch_max((peak.min(1.0) * 1_000_000.0) as u32, Ordering::Relaxed);
        let mut buf = self.buffer.lock().expect("mic buffer");
        let room = self.limit.saturating_sub(buf.len());
        buf.extend_from_slice(&self.out[..self.out.len().min(room)]);
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    limit: usize,
    buffer: &Arc<Mutex<Vec<f32>>>,
    stats: &Arc<Stats>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let buffer = buffer.clone();
    let data_stats = stats.clone();
    let error_stats = stats.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut buf = buffer.lock().expect("mic buffer");
                let mut peak = 0.0f32;
                for frame in data.chunks(channels) {
                    if buf.len() >= limit {
                        return;
                    }
                    let sum: f32 = frame
                        .iter()
                        .map(|s| <f32 as cpal::FromSample<T>>::from_sample_(*s))
                        .sum();
                    let mono = sum / frame.len() as f32;
                    peak = peak.max(mono.abs());
                    buf.push(mono);
                }
                data_stats.callbacks.fetch_add(1, Ordering::Relaxed);
                data_stats
                    .samples
                    .fetch_add((data.len() / channels) as u64, Ordering::Relaxed);
                data_stats
                    .peak_ppm
                    .fetch_max((peak.min(1.0) * 1_000_000.0) as u32, Ordering::Relaxed);
            },
            move |e| {
                tracing::warn!("microphone stream error: {e}");
                error_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
                *error_stats.last_error.lock().expect("mic error") = e.to_string();
            },
            None,
        )
        .map_err(|e| format!("cannot open the microphone: {e}"))
}

/// A label for the input device that is safe to log: its kind when the name
/// tells it (`default`, `built-in`, `USB`, `Bluetooth`, `headset`), else
/// `named device (N chars)`. The name itself is never kept: it may contain a
/// person's name ("Alex's AirPods").
pub fn device_kind(name: &str) -> String {
    let lower = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let kind = if lower.trim().is_empty() {
        "unnamed"
    } else if has(&[
        "bluetooth",
        "airpods",
        "hands-free",
        "handsfree",
        "buds",
        "bt ",
    ]) {
        "Bluetooth"
    } else if has(&["usb"]) {
        "USB"
    } else if has(&["headset", "headphone", "earphone"]) {
        "headset"
    } else if has(&[
        "built-in",
        "builtin",
        "internal",
        "microphone array",
        "mic array",
        "macbook",
        "imac",
        "iphone",
        "ipad",
    ]) {
        "built-in"
    } else if has(&["default", "communications"]) {
        "default"
    } else {
        return format!("named device ({} chars)", name.chars().count());
    };
    kind.to_owned()
}

/// Index at which to cut `samples`: the middle of the quietest 100 ms window
/// within the last `search_seconds` (but never in the first half), so a cut
/// lands between words rather than inside one.
pub fn quietest_cut(samples: &[f32], rate: u32, search_seconds: f32) -> usize {
    let window = (rate as usize / 10).max(1);
    if samples.len() < window * 2 {
        return samples.len();
    }
    let search = (search_seconds * rate as f32) as usize;
    let start = samples.len().saturating_sub(search).max(samples.len() / 2);
    let mut best = (f32::INFINITY, samples.len());
    let mut pos = start;
    while pos + window <= samples.len() {
        let energy: f32 = samples[pos..pos + window].iter().map(|s| s * s).sum();
        if energy < best.0 {
            best = (energy, pos + window / 2);
        }
        pos += window / 2;
    }
    best.1
}

/// Linear-interpolation resampler (speech only needs a few kHz of bandwidth,
/// and the averaging of the downmix already softens aliasing).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 {
        return input.to_vec();
    }
    let out_len = (input.len() as u64 * u64::from(to) / u64::from(from)) as usize;
    let step = f64::from(from) / f64::from(to);
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * step;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input[idx.min(input.len() - 1)];
            let b = input[(idx + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Encodes mono samples in [-1, 1] as a 16-bit PCM WAV file.
pub fn wav_pcm16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_the_duration() {
        let one_second = vec![0.25f32; 48_000];
        let out = resample(&one_second, 48_000, OUTPUT_RATE);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|s| (s - 0.25).abs() < 1e-6));
        assert_eq!(resample(&one_second, 16_000, 16_000).len(), 48_000);
        assert!(resample(&[], 44_100, OUTPUT_RATE).is_empty());
    }

    #[test]
    fn resampling_interpolates() {
        let out = resample(&[0.0, 1.0], 1, 2);
        assert_eq!(out, vec![0.0, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn cuts_land_in_the_quietest_recent_window() {
        let rate = 1000;
        // 10 s of "speech" with a silent gap at 8.0-8.3 s and another at 1 s
        // (too early to be considered).
        let mut s = vec![0.5f32; 10 * rate as usize];
        s[8000..8300].iter_mut().for_each(|x| *x = 0.0);
        s[1000..1300].iter_mut().for_each(|x| *x = 0.0);
        let cut = quietest_cut(&s, rate, 5.0);
        assert!((8000..8300).contains(&cut), "{cut}");
        // Tiny buffers are taken whole.
        assert_eq!(quietest_cut(&[0.1; 10], rate, 5.0), 10);
    }

    #[test]
    fn wav_header_is_valid_pcm16_mono() {
        let wav = wav_pcm16(&[0.0, 1.0, -1.0, 2.0], OUTPUT_RATE);
        assert_eq!(wav.len(), 44 + 8);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(&wav[36..40], b"data");
        let samples: Vec<i16> = wav[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples, vec![0, i16::MAX, -i16::MAX, i16::MAX]);
    }

    #[test]
    fn device_kinds_never_carry_the_name() {
        assert_eq!(device_kind("Alex's AirPods Pro"), "Bluetooth");
        assert_eq!(device_kind("Microphone (USB Audio Device)"), "USB");
        assert_eq!(device_kind("MacBook Pro Microphone"), "built-in");
        assert_eq!(
            device_kind("Microphone Array (Realtek(R) Audio)"),
            "built-in"
        );
        assert_eq!(device_kind("Headset Microphone (Jabra)"), "headset");
        assert_eq!(device_kind("default"), "default");
        assert_eq!(device_kind(""), "unnamed");
        assert_eq!(device_kind("Kim Studio"), "named device (10 chars)");
    }

    #[test]
    fn suppression_is_on_by_default() {
        assert!(MicOptions::default().noise_suppression);
    }

    /// The capture thread's path without a device: queued 44.1 kHz audio is
    /// pumped in uneven pieces, denoised at 48 kHz and still comes out as a
    /// 16 kHz WAV of the recorded length.
    #[test]
    fn denoised_capture_keeps_the_16k_wav_contract() {
        let rate = 44_100;
        let pending = Arc::new(Mutex::new(Vec::new()));
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(Stats::default());
        let limit = (MAX_BUFFER_SECONDS * SUPPRESSOR_RATE) as usize;
        let mut denoise = Denoise::new(rate, pending.clone(), buffer.clone(), limit, stats.clone());
        let tone: Vec<f32> = (0..rate as usize * 2)
            .map(|i| 0.3 * (i as f32 * 0.02).sin())
            .collect();
        for piece in tone.chunks(rate as usize / 37) {
            pending.lock().unwrap().extend_from_slice(piece);
            denoise.pump();
        }
        denoise.finish();
        let samples = buffer.lock().unwrap().clone();
        assert!(
            (samples.len() as i64 - 96_000).abs() <= 2,
            "{}",
            samples.len()
        );
        assert!(stats.out_peak_ppm.load(Ordering::Relaxed) > 0);
        let wav = wav_pcm16(
            &resample(&samples, SUPPRESSOR_RATE, OUTPUT_RATE),
            OUTPUT_RATE,
        );
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().unwrap()),
            OUTPUT_RATE
        );
        let seconds = (wav.len() - 44) as f32 / 2.0 / OUTPUT_RATE as f32;
        assert!((seconds - 2.0).abs() < 0.001, "{seconds}");
    }
}
