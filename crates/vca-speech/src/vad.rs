//! Energy-based voice activity detection over PCM16 audio.
//!
//! The detector splits incoming samples into fixed frames, measures each
//! frame's RMS level in dBFS and tracks an adaptive noise floor. Speech
//! starts after `min_speech_ms` of frames above the start threshold and ends
//! after `hangover_ms` of frames below the stop threshold. A pre-roll buffer
//! keeps the audio just before the detected start so the first syllable is
//! not clipped.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Tuning parameters of [`VoiceActivityDetector`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VadConfig {
    pub sample_rate: u32,
    /// Frame length in milliseconds (10–30 ms is typical).
    pub frame_ms: u32,
    /// Absolute level (dBFS) a frame must exceed to count as speech.
    pub start_threshold_db: f32,
    /// Level (dBFS) below which a frame counts as silence while in speech.
    /// Lower than the start threshold to give hysteresis.
    pub stop_threshold_db: f32,
    /// Speech must also exceed the adaptive noise floor by this margin (dB).
    pub noise_margin_db: f32,
    /// Consecutive speech needed before an utterance starts.
    pub min_speech_ms: u32,
    /// Silence needed before an utterance ends.
    pub hangover_ms: u32,
    /// Audio kept from before the detected start.
    pub pre_roll_ms: u32,
    /// Utterances are force-ended at this length (0 = unlimited).
    pub max_utterance_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            frame_ms: 20,
            start_threshold_db: -40.0,
            stop_threshold_db: -45.0,
            noise_margin_db: 10.0,
            min_speech_ms: 100,
            hangover_ms: 800,
            pre_roll_ms: 300,
            max_utterance_ms: 60_000,
        }
    }
}

/// Why an utterance ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// Silence for the hangover period.
    Silence,
    /// The utterance reached `max_utterance_ms`.
    MaxLength,
    /// [`VoiceActivityDetector::flush`] was called during speech.
    Flushed,
}

/// Events emitted by [`VoiceActivityDetector::push`].
#[derive(Debug, Clone, PartialEq)]
pub enum VadEvent {
    /// Speech started; `at_ms` is the stream time of the first speech frame.
    SpeechStart { at_ms: u64 },
    /// Speech ended; `samples` holds the utterance including pre-roll and the
    /// trailing hangover.
    SpeechEnd {
        start_ms: u64,
        end_ms: u64,
        reason: EndReason,
        samples: Vec<i16>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Silence,
    /// Candidate speech: number of consecutive loud frames.
    Rising(u32),
    /// In speech: number of consecutive quiet frames.
    Speech(u32),
}

/// Streaming energy-based VAD.
#[derive(Debug, Clone)]
pub struct VoiceActivityDetector {
    config: VadConfig,
    frame_len: usize,
    partial: Vec<i16>,
    pre_roll: VecDeque<i16>,
    utterance: Vec<i16>,
    state: State,
    noise_floor_db: f32,
    frames_seen: u64,
    utterance_start_ms: u64,
}

/// RMS level of PCM16 samples in dBFS (−100 for digital silence).
pub fn level_dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return -100.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum / samples.len() as f64).sqrt() / 32768.0;
    if rms <= 1e-5 {
        -100.0
    } else {
        (20.0 * rms.log10()) as f32
    }
}

impl VoiceActivityDetector {
    pub fn new(config: VadConfig) -> Self {
        let frame_len =
            ((config.sample_rate as u64 * config.frame_ms.max(1) as u64) / 1000).max(1) as usize;
        Self {
            frame_len,
            partial: Vec::with_capacity(frame_len),
            pre_roll: VecDeque::new(),
            utterance: Vec::new(),
            state: State::Silence,
            noise_floor_db: config.start_threshold_db - config.noise_margin_db,
            frames_seen: 0,
            utterance_start_ms: 0,
            config,
        }
    }

    pub fn config(&self) -> &VadConfig {
        &self.config
    }

    /// True while an utterance is in progress.
    pub fn in_speech(&self) -> bool {
        matches!(self.state, State::Speech(_))
    }

    /// Current estimate of the background noise level (dBFS).
    pub fn noise_floor_db(&self) -> f32 {
        self.noise_floor_db
    }

    /// Feeds mono PCM16 samples of any length and returns the events they
    /// produced.
    pub fn push(&mut self, samples: &[i16]) -> Vec<VadEvent> {
        let mut events = Vec::new();
        let mut rest = samples;
        while !rest.is_empty() {
            let need = self.frame_len - self.partial.len();
            let take = need.min(rest.len());
            self.partial.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.partial.len() == self.frame_len {
                let frame = std::mem::take(&mut self.partial);
                if let Some(e) = self.process_frame(&frame) {
                    events.push(e);
                }
                self.partial = frame;
                self.partial.clear();
            }
        }
        events
    }

    /// Ends the current utterance, if any (for example when push-to-talk is
    /// released or the app leaves the foreground).
    pub fn flush(&mut self) -> Option<VadEvent> {
        let remainder = std::mem::take(&mut self.partial);
        if self.in_speech() {
            self.utterance.extend_from_slice(&remainder);
            return Some(self.end_utterance(EndReason::Flushed));
        }
        self.state = State::Silence;
        None
    }

    fn ms_per_frame(&self) -> u64 {
        self.config.frame_ms.max(1) as u64
    }

    fn frames_for(&self, ms: u32) -> u32 {
        (ms as u64).div_ceil(self.ms_per_frame()).max(1) as u32
    }

    fn now_ms(&self) -> u64 {
        self.frames_seen * self.ms_per_frame()
    }

    fn process_frame(&mut self, frame: &[i16]) -> Option<VadEvent> {
        let level = level_dbfs(frame);
        let start_threshold = self
            .config
            .start_threshold_db
            .max(self.noise_floor_db + self.config.noise_margin_db);
        let stop_threshold = self
            .config
            .stop_threshold_db
            .max(self.noise_floor_db + self.config.noise_margin_db / 2.0);
        let loud = level > start_threshold;
        let frame_start_ms = self.now_ms();
        self.frames_seen += 1;

        match self.state {
            State::Silence | State::Rising(_) => {
                let count = match self.state {
                    State::Rising(n) if loud => n + 1,
                    _ if loud => 1,
                    _ => 0,
                };
                self.push_pre_roll(frame);
                if !loud {
                    self.adapt_noise_floor(level);
                }
                if count == 0 {
                    self.state = State::Silence;
                    None
                } else if count >= self.frames_for(self.config.min_speech_ms) {
                    let speech_start =
                        frame_start_ms.saturating_sub((count as u64 - 1) * self.ms_per_frame());
                    self.utterance = self.pre_roll.drain(..).collect();
                    let pre_roll_ms =
                        (self.utterance.len() as u64 * 1000) / self.config.sample_rate as u64;
                    self.utterance_start_ms =
                        (frame_start_ms + self.ms_per_frame()).saturating_sub(pre_roll_ms);
                    self.state = State::Speech(0);
                    Some(VadEvent::SpeechStart {
                        at_ms: speech_start,
                    })
                } else {
                    self.state = State::Rising(count);
                    None
                }
            }
            State::Speech(quiet) => {
                self.utterance.extend_from_slice(frame);
                let quiet = if level < stop_threshold { quiet + 1 } else { 0 };
                let length_ms =
                    (self.utterance.len() as u64 * 1000) / self.config.sample_rate as u64;
                if quiet >= self.frames_for(self.config.hangover_ms) {
                    Some(self.end_utterance(EndReason::Silence))
                } else if self.config.max_utterance_ms > 0
                    && length_ms >= self.config.max_utterance_ms as u64
                {
                    Some(self.end_utterance(EndReason::MaxLength))
                } else {
                    self.state = State::Speech(quiet);
                    None
                }
            }
        }
    }

    fn push_pre_roll(&mut self, frame: &[i16]) {
        let cap = (self.config.sample_rate as u64 * self.config.pre_roll_ms as u64 / 1000) as usize
            + self.frame_len * self.frames_for(self.config.min_speech_ms) as usize;
        self.pre_roll.extend(frame.iter().copied());
        while self.pre_roll.len() > cap {
            self.pre_roll.pop_front();
        }
    }

    fn adapt_noise_floor(&mut self, level: f32) {
        // Fast attack downwards, slow release upwards so speech does not
        // drag the floor up.
        let alpha = if level < self.noise_floor_db {
            0.3
        } else {
            0.02
        };
        self.noise_floor_db += alpha * (level - self.noise_floor_db);
    }

    fn end_utterance(&mut self, reason: EndReason) -> VadEvent {
        self.state = State::Silence;
        self.pre_roll.clear();
        let samples = std::mem::take(&mut self.utterance);
        let start_ms = self.utterance_start_ms;
        let end_ms = start_ms + (samples.len() as u64 * 1000) / self.config.sample_rate as u64;
        VadEvent::SpeechEnd {
            start_ms,
            end_ms,
            reason,
            samples,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn silence(ms: u32) -> Vec<i16> {
        vec![0; (RATE * ms / 1000) as usize]
    }

    /// Deterministic low-level noise (about -60 dBFS).
    fn noise(ms: u32, seed: u32) -> Vec<i16> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        (0..(RATE * ms / 1000))
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                ((x % 64) as i16) - 32
            })
            .collect()
    }

    fn tone(ms: u32, amplitude: f32) -> Vec<i16> {
        (0..(RATE * ms / 1000))
            .map(|i| {
                let t = i as f32 / RATE as f32;
                ((2.0 * std::f32::consts::PI * 220.0 * t).sin() * amplitude * 32767.0) as i16
            })
            .collect()
    }

    fn run(vad: &mut VoiceActivityDetector, chunks: &[Vec<i16>]) -> Vec<VadEvent> {
        let mut events = Vec::new();
        for c in chunks {
            // Feed in odd-sized pieces to exercise frame reassembly.
            for piece in c.chunks(333) {
                events.extend(vad.push(piece));
            }
        }
        events
    }

    #[test]
    fn level_of_full_scale_and_silence() {
        assert_eq!(level_dbfs(&[0; 100]), -100.0);
        let l = level_dbfs(&tone(100, 1.0));
        assert!((l + 3.0).abs() < 0.2, "{l}");
    }

    #[test]
    fn detects_single_utterance() {
        let mut vad = VoiceActivityDetector::new(VadConfig::default());
        let events = run(&mut vad, &[noise(500, 1), tone(1000, 0.3), noise(1500, 2)]);
        assert_eq!(events.len(), 2, "{events:?}");
        let VadEvent::SpeechStart { at_ms } = events[0] else {
            panic!("expected start");
        };
        assert!((480..=540).contains(&at_ms), "start {at_ms}");
        let VadEvent::SpeechEnd {
            reason,
            samples,
            start_ms,
            end_ms,
        } = &events[1]
        else {
            panic!("expected end");
        };
        assert_eq!(*reason, EndReason::Silence);
        // Utterance = pre-roll + 1 s of speech + hangover.
        let ms = samples.len() as u32 * 1000 / RATE;
        assert!((1700..=2300).contains(&ms), "len {ms}");
        assert!(*start_ms < 500 && *end_ms > 1500);
        assert!(!vad.in_speech());
    }

    #[test]
    fn short_clicks_are_ignored() {
        let mut vad = VoiceActivityDetector::new(VadConfig::default());
        let events = run(&mut vad, &[silence(300), tone(40, 0.5), silence(1000)]);
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn short_pause_does_not_split_utterance() {
        let mut vad = VoiceActivityDetector::new(VadConfig::default());
        let events = run(
            &mut vad,
            &[
                silence(200),
                tone(500, 0.3),
                silence(400),
                tone(500, 0.3),
                silence(1200),
            ],
        );
        let ends = events
            .iter()
            .filter(|e| matches!(e, VadEvent::SpeechEnd { .. }))
            .count();
        assert_eq!(ends, 1, "{events:?}");
    }

    #[test]
    fn two_utterances_and_flush() {
        let mut vad = VoiceActivityDetector::new(VadConfig::default());
        let events = run(&mut vad, &[tone(300, 0.3), silence(1000), tone(300, 0.3)]);
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(vad.in_speech());
        let Some(VadEvent::SpeechEnd { reason, .. }) = vad.flush() else {
            panic!("flush should end the utterance");
        };
        assert_eq!(reason, EndReason::Flushed);
        assert!(vad.flush().is_none());
    }

    #[test]
    fn max_length_forces_end() {
        let config = VadConfig {
            max_utterance_ms: 1000,
            ..VadConfig::default()
        };
        let mut vad = VoiceActivityDetector::new(config);
        let events = run(&mut vad, &[tone(2500, 0.3)]);
        assert!(events.iter().any(|e| matches!(
            e,
            VadEvent::SpeechEnd {
                reason: EndReason::MaxLength,
                ..
            }
        )));
    }

    #[test]
    fn noise_floor_tracks_background_and_raises_threshold() {
        let mut vad = VoiceActivityDetector::new(VadConfig::default());
        // A steady hum at about -49 dBFS stays below the start threshold and
        // pulls the noise floor towards its level.
        let events = run(&mut vad, &[tone(2000, 0.005)]);
        assert!(events.is_empty());
        let floor = vad.noise_floor_db();
        assert!(floor < -45.0 && floor > -55.0, "{floor}");
        // A voice only slightly above the absolute threshold but less than
        // `noise_margin_db` above the floor is not speech.
        let events = run(&mut vad, &[tone(1000, 0.012)]); // about -41 dBFS
        assert!(events.is_empty(), "{events:?}");
        // A clear voice is.
        let events = run(&mut vad, &[tone(500, 0.3)]);
        assert!(matches!(events[0], VadEvent::SpeechStart { .. }));
    }
}
