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

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};

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

struct Capture {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<Result<Recording, String>>,
    /// Samples so far (mono, device rate) for interim transcripts.
    buffer: Arc<Mutex<Vec<f32>>>,
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
}

impl Mic {
    /// Opens the default input device and starts recording.
    pub fn start(&self) -> Result<(), String> {
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
        let thread = std::thread::Builder::new()
            .name("vca-mic".into())
            .spawn(move || capture(stop_rx, ready_tx, shared))
            .map_err(|e| format!("cannot start the audio thread: {e}"))?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(rate)) => {
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
/// so the stream lives and dies here.
fn capture(
    stop: mpsc::Receiver<()>,
    ready: mpsc::Sender<Result<u32, String>>,
    buffer: Arc<Mutex<Vec<f32>>>,
) -> Result<Recording, String> {
    let fail = |ready: &mpsc::Sender<Result<u32, String>>, msg: String| {
        let _ = ready.send(Err(msg.clone()));
        Err(msg)
    };
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return fail(&ready, "no microphone found".into());
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => return fail(&ready, format!("cannot use the microphone: {e}")),
    };
    let rate = config.sample_rate().0;
    let channels = usize::from(config.channels().max(1));
    let limit = (MAX_BUFFER_SECONDS * rate) as usize;
    buffer
        .lock()
        .expect("mic buffer")
        .reserve(rate as usize * 10);
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config.into(), channels, limit, &buffer),
        SampleFormat::I16 => build::<i16>(&device, &config.into(), channels, limit, &buffer),
        SampleFormat::U16 => build::<u16>(&device, &config.into(), channels, limit, &buffer),
        SampleFormat::I32 => build::<i32>(&device, &config.into(), channels, limit, &buffer),
        other => Err(format!("unsupported sample format {other}")),
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => return fail(&ready, e),
    };
    if let Err(e) = stream.play() {
        return fail(&ready, format!("cannot start the microphone: {e}"));
    }
    let _ = ready.send(Ok(rate));
    match stop.recv_timeout(Duration::from_secs(u64::from(MAX_RECORDING_SECONDS))) {
        Ok(()) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
    }
    drop(stream);
    let samples = std::mem::take(&mut *buffer.lock().expect("mic buffer"));
    Ok(Recording { samples, rate })
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    limit: usize,
    buffer: &Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let buffer = buffer.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut buf = buffer.lock().expect("mic buffer");
                for frame in data.chunks(channels) {
                    if buf.len() >= limit {
                        return;
                    }
                    let sum: f32 = frame
                        .iter()
                        .map(|s| <f32 as cpal::FromSample<T>>::from_sample_(*s))
                        .sum();
                    buf.push(sum / frame.len() as f32);
                }
            },
            |e| tracing::warn!("microphone stream error: {e}"),
            None,
        )
        .map_err(|e| format!("cannot open the microphone: {e}"))
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
}
