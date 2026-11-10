//! Noise suppression for the native microphone (RNNoise via `nnnoiseless`).
//!
//! RNNoise works on 10 ms frames of 48 kHz audio (480 samples), so the
//! microphone's mono signal is first brought to 48 kHz (most devices already
//! deliver 48 kHz, then this is a copy), denoised frame by frame and handed
//! on at 48 kHz; the existing 16 kHz resampling for the WAV happens later.
//!
//! The model delays its output by exactly one frame. [`Suppressor`] drops
//! that first frame (it also holds the model's fade-in) and flushes the tail
//! on [`Suppressor::finish`], so the output lines up with the input sample
//! for sample and has the same length.

use nnnoiseless::DenoiseState;

/// The rate RNNoise works at.
pub const SUPPRESSOR_RATE: u32 = 48_000;
/// Samples per RNNoise frame (10 ms at 48 kHz).
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// RNNoise takes samples in 16-bit range rather than [-1, 1].
const SCALE: f32 = 32_768.0;

/// Streaming linear-interpolation resampler (one input chunk at a time,
/// continuous across chunks).
struct StreamResampler {
    /// Input samples per output sample.
    step: f64,
    /// Position of the next output sample, relative to `prev` (or to the
    /// first sample of the next chunk while there is no `prev`).
    pos: f64,
    prev: Option<f32>,
}

impl StreamResampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            step: f64::from(from) / f64::from(to),
            pos: 0.0,
            prev: None,
        }
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let offset = usize::from(self.prev.is_some());
        let len = input.len() + offset;
        let at = |i: usize| match (i, self.prev) {
            (0, Some(p)) => p,
            _ => input[i - offset],
        };
        while self.pos + 1.0 < len as f64 {
            let idx = self.pos as usize;
            let frac = (self.pos - idx as f64) as f32;
            let (a, b) = (at(idx), at(idx + 1));
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= (len - 1) as f64;
        self.prev = input.last().copied();
    }
}

/// Denoises a mono stream. Not real-time safe (allocates while growing its
/// buffers, runs a small neural network): call it from a worker thread,
/// never from the audio callback.
pub struct Suppressor {
    state: Box<DenoiseState<'static>>,
    /// `None` when the device already delivers 48 kHz.
    resampler: Option<StreamResampler>,
    /// Scratch for the resampled chunk.
    scratch: Vec<f32>,
    /// 48 kHz samples waiting for a full frame (already scaled).
    frame: Vec<f32>,
    out_frame: Vec<f32>,
    /// Output samples still to drop (the model's one-frame delay).
    skip: usize,
    /// 48 kHz samples fed in and handed out.
    fed: u64,
    emitted: u64,
}

impl Suppressor {
    /// A suppressor for mono input at `input_rate`; output is always
    /// [`SUPPRESSOR_RATE`].
    pub fn new(input_rate: u32) -> Self {
        Self {
            state: DenoiseState::new(),
            resampler: (input_rate != SUPPRESSOR_RATE && input_rate > 0)
                .then(|| StreamResampler::new(input_rate, SUPPRESSOR_RATE)),
            scratch: Vec::new(),
            frame: Vec::with_capacity(FRAME),
            out_frame: vec![0.0; FRAME],
            skip: FRAME,
            fed: 0,
            emitted: 0,
        }
    }

    /// Denoises `input` (mono, in [-1, 1]) and appends what is ready to
    /// `out`. Output trails input by less than one frame until [`finish`].
    ///
    /// [`finish`]: Suppressor::finish
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        let samples: &[f32] = match self.resampler.as_mut() {
            Some(r) => {
                r.push(input, &mut scratch);
                &scratch
            }
            None => input,
        };
        self.fed += samples.len() as u64;
        for &s in samples {
            self.frame.push(s * SCALE);
            if self.frame.len() == FRAME {
                self.run_frame(out);
            }
        }
        self.scratch = scratch;
    }

    /// Flushes the samples still inside the model, so the whole output is as
    /// long as the (48 kHz) input.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        while self.emitted < self.fed {
            self.frame.resize(FRAME, 0.0);
            self.run_frame(out);
        }
        let extra = (self.emitted - self.fed) as usize;
        out.truncate(out.len().saturating_sub(extra));
        self.emitted = self.fed;
    }

    fn run_frame(&mut self, out: &mut Vec<f32>) {
        self.state.process_frame(&mut self.out_frame, &self.frame);
        self.frame.clear();
        let start = self.skip.min(FRAME);
        self.skip -= start;
        let ready = &self.out_frame[start..];
        self.emitted += ready.len() as u64;
        out.extend(ready.iter().map(|s| (s / SCALE).clamp(-1.0, 1.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic white-noise source (xorshift), in [-amp, amp].
    fn noise(len: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut x = seed.max(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    /// A voiced, speech-like signal: a 140 Hz pulse train with formant-ish
    /// harmonics, switched on and off like syllables.
    fn speech_like(len: usize, rate: u32) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let syllable = ((t * 4.0).fract() < 0.6) as u8 as f32;
                let f0 = 140.0 + 20.0 * (t * 3.0).sin();
                let mut v = 0.0;
                for (k, a) in [
                    (1.0, 0.5),
                    (2.0, 0.35),
                    (4.0, 0.25),
                    (7.0, 0.15),
                    (12.0, 0.08),
                ] {
                    v += a * (std::f32::consts::TAU * f0 * k * t).sin();
                }
                v * 0.3 * syllable
            })
            .collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    fn run(input: &[f32], rate: u32, chunk: usize) -> Vec<f32> {
        let mut s = Suppressor::new(rate);
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            s.process(c, &mut out);
        }
        s.finish(&mut out);
        out
    }

    #[test]
    fn keeps_the_length_at_48k() {
        for len in [0, 1, 479, 480, 481, 48_000, 48_123] {
            let out = run(&noise(len, 0.1, 7), SUPPRESSOR_RATE, 441);
            assert_eq!(out.len(), len);
        }
    }

    #[test]
    fn resamples_other_rates_to_48k() {
        let input = vec![0.0f32; 44_100];
        let out = run(&input, 44_100, 512);
        assert!((out.len() as i64 - 48_000).abs() <= 2, "{}", out.len());
        let out = run(&vec![0.0f32; 16_000], 16_000, 160);
        assert!((out.len() as i64 - 48_000).abs() <= 3, "{}", out.len());
    }

    #[test]
    fn stream_resampler_matches_whole_buffer_resampling() {
        let input: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut whole = Vec::new();
        StreamResampler::new(44_100, 48_000).push(&input, &mut whole);
        let mut parts = Vec::new();
        let mut r = StreamResampler::new(44_100, 48_000);
        for c in input.chunks(37) {
            r.push(c, &mut parts);
        }
        assert_eq!(whole.len(), parts.len());
        for (a, b) in whole.iter().zip(&parts) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn attenuates_steady_noise() {
        let input = noise(4 * 48_000, 0.03, 42);
        let out = run(&input, SUPPRESSOR_RATE, 480);
        // Once the model has settled (the last two seconds).
        let (a, b) = (rms(&input[96_000..]), rms(&out[96_000..]));
        assert!(b < a * 0.4, "noise rms {a} -> {b}");
    }

    #[test]
    fn keeps_speech_like_signal() {
        let rate = SUPPRESSOR_RATE;
        let speech = speech_like(3 * rate as usize, rate);
        let out = run(&speech, rate, 960);
        let (a, b) = (rms(&speech[24_000..]), rms(&out[24_000..]));
        assert!(b > a * 0.5, "speech rms {a} -> {b}");
    }

    #[test]
    fn improves_speech_in_noise() {
        let rate = SUPPRESSOR_RATE;
        let speech = speech_like(4 * rate as usize, rate);
        let hiss = noise(speech.len(), 0.03, 9);
        let mixed: Vec<f32> = speech.iter().zip(&hiss).map(|(s, n)| s + n).collect();
        let out = run(&mixed, rate, 480);
        // Level while a "syllable" sounds against the level between them
        // (only noise there), from the second second on.
        let ratio = |s: &[f32]| {
            let (mut on, mut off) = (Vec::new(), Vec::new());
            for (i, v) in s.iter().enumerate().skip(rate as usize) {
                let phase = (i as f32 / rate as f32 * 4.0).fract();
                if (0.05..0.55).contains(&phase) {
                    on.push(*v);
                } else if (0.65..0.95).contains(&phase) {
                    off.push(*v);
                }
            }
            rms(&on) / rms(&off)
        };
        let (before, after) = (ratio(&mixed), ratio(&out));
        assert!(after > before * 3.0, "speech/noise {before} -> {after}");
        // The speech itself stays at about its level.
        let on = rms(&out[rate as usize..]) / rms(&mixed[rate as usize..]);
        assert!(on > 0.6, "{on}");
    }
}
