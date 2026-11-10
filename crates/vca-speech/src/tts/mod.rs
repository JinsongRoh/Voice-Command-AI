//! Text-to-speech engines behind a common [`TtsEngine`] trait.
//!
//! VCA ships no voices or model weights: the owner points it at their own
//! engine. Only the OpenAI-compatible speech endpoint is implemented
//! ([`OpenAiTtsEngine`]); other APIs can be added as further
//! implementations of the trait.
//!
//! Engines never log the text they synthesize or the audio they return.

use async_trait::async_trait;

use crate::error::Result;

pub mod openai;

pub use openai::{OpenAiTtsConfig, OpenAiTtsEngine};

/// Audio containers a client can play with a plain audio element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TtsFormat {
    Mp3,
    Opus,
    Aac,
    Flac,
    Wav,
}

impl TtsFormat {
    pub const ALL: [TtsFormat; 5] = [Self::Mp3, Self::Opus, Self::Aac, Self::Flac, Self::Wav];

    /// The `response_format` value and the wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Wav => "wav",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|f| f.as_str().eq_ignore_ascii_case(s))
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/ogg",
            Self::Aac => "audio/aac",
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
        }
    }
}

/// One synthesis request.
#[derive(Clone, PartialEq)]
pub struct SynthesisRequest {
    pub text: String,
    /// Engine-specific voice name; the engine's default when `None`.
    pub voice: Option<String>,
    /// Speaking speed (1.0 is normal); the engine's default when `None`.
    pub speed: Option<f32>,
    /// Language of the text (BCP-47, e.g. `ko-KR`). Multilingual engines
    /// need it: left to guess, they may read Korean mixed with Latin words
    /// ("npm run build 실패" (npm run build failed)) in another language's sounds.
    pub language: Option<String>,
}

// The text is never printed, not even in debug output.
impl std::fmt::Debug for SynthesisRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SynthesisRequest")
            .field("text_chars", &self.text.chars().count())
            .field("voice", &self.voice)
            .field("speed", &self.speed)
            .field("language", &self.language)
            .finish()
    }
}

/// Synthesized audio.
#[derive(Clone, PartialEq, Eq)]
pub struct Synthesized {
    pub format: TtsFormat,
    pub audio: Vec<u8>,
}

impl std::fmt::Debug for Synthesized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Synthesized")
            .field("format", &self.format)
            .field("bytes", &self.audio.len())
            .finish()
    }
}

/// A text-to-speech engine.
#[async_trait]
pub trait TtsEngine: Send + Sync {
    /// Short engine kind for diagnostics (never a URL).
    fn name(&self) -> &str;

    async fn synthesize(&self, request: &SynthesisRequest) -> Result<Synthesized>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_parse_case_insensitively() {
        assert_eq!(TtsFormat::parse(" MP3 "), Some(TtsFormat::Mp3));
        assert_eq!(TtsFormat::parse("wav"), Some(TtsFormat::Wav));
        assert_eq!(TtsFormat::parse("pcm"), None);
        for f in TtsFormat::ALL {
            assert_eq!(TtsFormat::parse(f.as_str()), Some(f));
            assert!(f.mime_type().starts_with("audio/"));
        }
    }

    #[test]
    fn debug_output_never_contains_the_text() {
        let req = SynthesisRequest {
            text: "secret words".into(),
            voice: None,
            speed: None,
            language: None,
        };
        assert!(!format!("{req:?}").contains("secret"));
    }
}
