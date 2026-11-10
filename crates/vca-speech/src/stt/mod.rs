//! Speech-to-text engines behind a common [`SttEngine`] trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::audio::AudioInput;
use crate::error::Result;

pub mod google;
pub mod mock;
pub mod openai;
#[cfg(feature = "whisper-local")]
pub mod whisper;

pub use google::{GoogleAuth, GoogleSttConfig, GoogleSttEngine};
pub use mock::MockSttEngine;
pub use openai::{OpenAiSttConfig, OpenAiSttEngine};
#[cfg(feature = "whisper-local")]
pub use whisper::{WhisperLocalConfig, WhisperLocalEngine};

/// Per-request recognition options.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TranscribeOptions {
    /// Primary language as a BCP-47 tag (`en`, `en-US`, `ja-JP`, ...).
    /// `None` lets the engine detect the language.
    pub primary_language: Option<String>,
    /// Additional languages the speaker mixes in. When non-empty, engines
    /// avoid forcing recognition to the primary language so code-switching
    /// works (engines that support several language codes receive them all).
    #[serde(default)]
    pub secondary_languages: Vec<String>,
    /// Free-text vocabulary hint (for example the output of
    /// [`crate::lexicon::VocabularyEngine::hint_prompt`]).
    pub prompt_hint: Option<String>,
    /// Individual phrases to boost, for engines with phrase-level adaptation
    /// (Google). When empty, engines derive phrases by splitting
    /// `prompt_hint` on commas.
    #[serde(default)]
    pub phrase_hints: Vec<String>,
}

impl TranscribeOptions {
    /// All configured languages, primary first, without duplicates.
    pub fn languages(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in self
            .primary_language
            .iter()
            .chain(self.secondary_languages.iter())
        {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(l)) {
                out.push(l.clone());
            }
        }
        out
    }

    /// The phrases to boost: `phrase_hints`, or `prompt_hint` split on commas.
    pub fn phrases(&self) -> Vec<String> {
        if !self.phrase_hints.is_empty() {
            return self.phrase_hints.clone();
        }
        self.prompt_hint
            .as_deref()
            .unwrap_or_default()
            .split([',', '\n', '、', '，'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

/// A piece of the transcript with optional timing and confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    /// Confidence in `[0, 1]` when the engine reports one.
    pub confidence: Option<f32>,
    /// Language of this segment, when the engine reports one per segment.
    pub language: Option<String>,
}

/// The result of recognizing one utterance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    /// Detected (or forced) language as a lowercase BCP-47 tag when known.
    pub language: Option<String>,
    #[serde(default)]
    pub segments: Vec<Segment>,
}

impl Transcript {
    /// A transcript with a single text and no segments.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            language: None,
            segments: Vec::new(),
        }
    }

    /// Mean segment confidence, when any segment reports one.
    pub fn confidence(&self) -> Option<f32> {
        let values: Vec<f32> = self.segments.iter().filter_map(|s| s.confidence).collect();
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum::<f32>() / values.len() as f32)
        }
    }
}

/// A speech-to-text engine. Implementations must be cheap to share across
/// tasks (`Arc<dyn SttEngine>`).
#[async_trait]
pub trait SttEngine: Send + Sync {
    /// Short identifier used in logs and settings (`openai`, `google`, ...).
    fn name(&self) -> &str;

    /// Recognizes one utterance.
    async fn transcribe(
        &self,
        audio: &AudioInput,
        options: &TranscribeOptions,
    ) -> Result<Transcript>;
}

/// Returns the primary language subtag: `en-US` → `en`, `zh_Hans` → `zh`.
pub fn base_language(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase()
}

/// Maps Whisper's English language names ("japanese") to ISO 639-1 codes.
/// Codes are passed through unchanged.
pub fn language_code_from_name(name: &str) -> String {
    const NAMES: &[(&str, &str)] = &[
        ("english", "en"),
        ("japanese", "ja"),
        ("korean", "ko"),
        ("chinese", "zh"),
        ("spanish", "es"),
        ("german", "de"),
        ("french", "fr"),
        ("portuguese", "pt"),
        ("italian", "it"),
        ("russian", "ru"),
        ("dutch", "nl"),
        ("polish", "pl"),
        ("turkish", "tr"),
        ("vietnamese", "vi"),
        ("thai", "th"),
        ("indonesian", "id"),
        ("hindi", "hi"),
        ("arabic", "ar"),
        ("ukrainian", "uk"),
        ("swedish", "sv"),
    ];
    let lower = name.trim().to_ascii_lowercase();
    NAMES
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, c)| (*c).to_owned())
        .unwrap_or(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_and_phrases() {
        let o = TranscribeOptions {
            primary_language: Some("en-US".into()),
            secondary_languages: vec!["ja-JP".into(), "EN-us".into()],
            prompt_hint: Some("kubectl, useEffect，Claude Code".into()),
            phrase_hints: vec![],
        };
        assert_eq!(o.languages(), vec!["en-US", "ja-JP"]);
        assert_eq!(o.phrases(), vec!["kubectl", "useEffect", "Claude Code"]);
        assert_eq!(base_language("zh_Hans"), "zh");
        assert_eq!(language_code_from_name("Japanese"), "ja");
    }
}
