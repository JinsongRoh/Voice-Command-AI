//! Audio containers and small DSP helpers (WAV decoding, PCM conversion,
//! down-mixing and resampling).

use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SpeechError};

/// Encoding of an [`AudioInput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    /// RIFF/WAVE container (any PCM layout `hound` can read).
    Wav,
    /// Raw little-endian signed 16-bit PCM, interleaved when multi-channel.
    Pcm16,
    /// WebM (usually Opus), as recorded by browsers and Android.
    Webm,
    /// Ogg (usually Opus).
    Ogg,
    /// MPEG-4 audio (usually AAC), as recorded by iOS.
    M4a,
}

impl AudioFormat {
    /// The MIME type used when uploading this format.
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Pcm16 => "audio/L16",
            Self::Webm => "audio/webm",
            Self::Ogg => "audio/ogg",
            Self::M4a => "audio/mp4",
        }
    }

    /// The conventional file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Pcm16 => "pcm",
            Self::Webm => "webm",
            Self::Ogg => "ogg",
            Self::M4a => "m4a",
        }
    }
}

/// One utterance of recorded audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioInput {
    /// Encoded audio bytes.
    pub bytes: Vec<u8>,
    pub format: AudioFormat,
    /// Sample rate in Hz. Required for [`AudioFormat::Pcm16`]; informative
    /// for container formats (which carry their own rate).
    pub sample_rate: u32,
    /// Channel count for [`AudioFormat::Pcm16`] (ignored for containers).
    #[serde(default = "one")]
    pub channels: u16,
}

fn one() -> u16 {
    1
}

impl AudioInput {
    /// Wraps raw mono PCM16 samples.
    pub fn from_pcm16(samples: &[i16], sample_rate: u32) -> Self {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        Self {
            bytes,
            format: AudioFormat::Pcm16,
            sample_rate,
            channels: 1,
        }
    }

    /// Wraps an encoded file.
    pub fn from_encoded(bytes: Vec<u8>, format: AudioFormat, sample_rate: u32) -> Self {
        Self {
            bytes,
            format,
            sample_rate,
            channels: 1,
        }
    }

    /// Returns a version of this input that remote APIs accept: raw PCM16 is
    /// wrapped into a WAV container, everything else is returned unchanged.
    pub fn to_uploadable(&self) -> Result<AudioInput> {
        if self.format != AudioFormat::Pcm16 {
            return Ok(self.clone());
        }
        let samples = pcm16_from_bytes(&self.bytes)?;
        let bytes = encode_wav_pcm16(&samples, self.sample_rate, self.channels.max(1))?;
        Ok(AudioInput {
            bytes,
            format: AudioFormat::Wav,
            sample_rate: self.sample_rate,
            channels: self.channels,
        })
    }

    /// Decodes WAV or PCM16 input into mono `f32` samples in `[-1, 1]` and
    /// returns them with their sample rate. Compressed formats (WebM, Ogg,
    /// M4A) need a codec and are rejected with [`SpeechError::Unsupported`].
    pub fn decode_mono_f32(&self) -> Result<(Vec<f32>, u32)> {
        match self.format {
            AudioFormat::Pcm16 => {
                if self.sample_rate == 0 {
                    return Err(SpeechError::Audio("PCM16 input needs a sample rate".into()));
                }
                let samples = pcm16_from_bytes(&self.bytes)?;
                let f: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
                Ok((downmix(&f, self.channels.max(1)), self.sample_rate))
            }
            AudioFormat::Wav => decode_wav(&self.bytes),
            other => Err(SpeechError::Unsupported(format!(
                "decoding {other:?} audio locally is not supported; send WAV or PCM16"
            ))),
        }
    }
}

/// Parses little-endian PCM16 bytes.
pub fn pcm16_from_bytes(bytes: &[u8]) -> Result<Vec<i16>> {
    if bytes.len() % 2 != 0 {
        return Err(SpeechError::Audio(
            "PCM16 data has an odd byte length".into(),
        ));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}

/// Encodes PCM16 samples into an in-memory WAV file.
pub fn encode_wav_pcm16(samples: &[i16], sample_rate: u32, channels: u16) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| SpeechError::Audio(e.to_string()))?;
        for &s in samples {
            writer
                .write_sample(s)
                .map_err(|e| SpeechError::Audio(e.to_string()))?;
        }
        writer
            .finalize()
            .map_err(|e| SpeechError::Audio(e.to_string()))?;
    }
    Ok(cursor.into_inner())
}

/// Decodes a WAV file into mono `f32` samples and its sample rate.
pub fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let reader =
        hound::WavReader::new(Cursor::new(bytes)).map_err(|e| SpeechError::Audio(e.to_string()))?;
    let spec = reader.spec();
    let audio_err = |e: hound::Error| SpeechError::Audio(e.to_string());
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(audio_err)?,
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            if bits == 0 || bits > 32 {
                return Err(SpeechError::Audio(format!("unsupported bit depth {bits}")));
            }
            let scale = (1u64 << (bits - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()
                .map_err(audio_err)?
        }
    };
    Ok((
        downmix(&interleaved, spec.channels.max(1)),
        spec.sample_rate,
    ))
}

/// Averages interleaved channels into mono.
pub fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels as usize;
    if ch <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

/// Resamples mono audio with linear interpolation. When down-sampling, a
/// moving-average low-pass filter is applied first to limit aliasing. This is
/// adequate for speech recognition input; it is not a mastering-grade
/// resampler.
pub fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() || from_rate == 0 || to_rate == 0 {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let filtered;
    let src: &[f32] = if ratio > 1.0 {
        let width = ratio.ceil() as usize;
        filtered = moving_average(samples, width);
        &filtered
    } else {
        samples
    };
    let out_len = ((src.len() as f64) / ratio).floor().max(1.0) as usize;
    let last = src.len() - 1;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let a = src[idx.min(last)];
            let b = src[(idx + 1).min(last)];
            a + (b - a) * frac
        })
        .collect()
}

fn moving_average(samples: &[f32], width: usize) -> Vec<f32> {
    if width <= 1 {
        return samples.to_vec();
    }
    let half = width / 2;
    let mut prefix = Vec::with_capacity(samples.len() + 1);
    prefix.push(0.0f64);
    for &s in samples {
        let last = *prefix.last().unwrap_or(&0.0);
        prefix.push(last + s as f64);
    }
    (0..samples.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + width - half).min(samples.len());
            ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32
        })
        .collect()
}

/// Converts any WAV/PCM16 input to 16 kHz mono `f32`, the layout Whisper
/// expects.
pub fn to_whisper_input(audio: &AudioInput) -> Result<Vec<f32>> {
    let (mono, rate) = audio.decode_mono_f32()?;
    Ok(resample(&mono, rate, 16_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn wav_round_trip() {
        let samples: Vec<i16> = (0..1600).map(|i| ((i % 100) * 100) as i16).collect();
        let input = AudioInput::from_pcm16(&samples, 16_000);
        let wav = input.to_uploadable().unwrap();
        assert_eq!(wav.format, AudioFormat::Wav);
        assert_eq!(&wav.bytes[..4], b"RIFF");
        let (decoded, rate) = wav.decode_mono_f32().unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(decoded.len(), samples.len());
        assert!((decoded[5] - samples[5] as f32 / 32768.0).abs() < 1e-6);
    }

    #[test]
    fn stereo_is_downmixed() {
        let bytes = encode_wav_pcm16(&[1000, 3000, -1000, -3000], 8000, 2).unwrap();
        let (mono, _) = decode_wav(&bytes).unwrap();
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 2000.0 / 32768.0).abs() < 1e-6);
    }

    #[test]
    fn resample_changes_length_and_keeps_signal() {
        let s = sine(440.0, 48_000, 1.0);
        let r = resample(&s, 48_000, 16_000);
        assert_eq!(r.len(), 16_000);
        let rms = (r.iter().map(|x| x * x).sum::<f32>() / r.len() as f32).sqrt();
        // A 440 Hz tone at amplitude 0.5 has RMS ~0.354; the low-pass must not kill it.
        assert!((rms - 0.354).abs() < 0.03, "rms {rms}");
        let up = resample(&s[..8000], 8_000, 16_000);
        assert_eq!(up.len(), 16_000);
    }

    #[test]
    fn compressed_formats_are_not_decoded_locally() {
        let input = AudioInput::from_encoded(vec![1, 2, 3], AudioFormat::Webm, 48_000);
        assert!(matches!(
            input.decode_mono_f32(),
            Err(SpeechError::Unsupported(_))
        ));
    }
}
