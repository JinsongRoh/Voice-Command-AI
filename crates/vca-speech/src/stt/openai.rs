//! Client for OpenAI-compatible transcription APIs
//! (`POST {base_url}/audio/transcriptions`, multipart form).
//!
//! Works with the OpenAI API (`whisper-1`, `gpt-4o-transcribe`,
//! `gpt-4o-mini-transcribe`) and with self-hosted servers that implement the
//! same endpoint (for example faster-whisper or whisper.cpp servers).

use std::time::Duration;

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};

use super::{SttEngine, TranscribeOptions, Transcript, base_language, language_code_from_name};
use crate::audio::AudioInput;
use crate::error::{Result, SpeechError};
use crate::stt::Segment;

/// Configuration of [`OpenAiSttEngine`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiSttConfig {
    /// API root, e.g. `https://api.openai.com/v1` or `http://localhost:8000/v1`.
    pub base_url: String,
    /// Bearer token. Optional for self-hosted servers.
    pub api_key: Option<String>,
    /// Model name (`whisper-1`, `gpt-4o-transcribe`, ...).
    pub model: String,
    /// `response_format` form field. `None` picks `verbose_json` for
    /// Whisper-family models (segments and language) and `json` otherwise.
    pub response_format: Option<String>,
    /// Sampling temperature, if set.
    pub temperature: Option<f32>,
    /// Request timeout.
    #[serde(with = "secs")]
    pub timeout: Duration,
}

mod secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

impl Default for OpenAiSttConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            api_key: None,
            model: "whisper-1".into(),
            response_format: None,
            temperature: None,
            timeout: Duration::from_secs(60),
        }
    }
}

/// Speech-to-text over an OpenAI-compatible HTTP API.
#[derive(Debug, Clone)]
pub struct OpenAiSttEngine {
    config: OpenAiSttConfig,
    client: reqwest::Client,
}

impl OpenAiSttEngine {
    /// Creates the engine with its own HTTP client.
    pub fn new(config: OpenAiSttConfig) -> Result<Self> {
        let client = reqwest::Client::builder().timeout(config.timeout).build()?;
        Ok(Self::with_client(config, client))
    }

    /// Creates the engine with a shared HTTP client.
    pub fn with_client(config: OpenAiSttConfig, client: reqwest::Client) -> Self {
        Self { config, client }
    }

    fn response_format(&self) -> String {
        self.config.response_format.clone().unwrap_or_else(|| {
            if self.config.model.contains("whisper") {
                "verbose_json".into()
            } else {
                "json".into()
            }
        })
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/audio/transcriptions",
            self.config.base_url.trim_end_matches('/')
        )
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    segments: Vec<ApiSegment>,
}

#[derive(Debug, Deserialize)]
struct ApiSegment {
    text: String,
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    #[serde(default)]
    avg_logprob: Option<f64>,
}

#[async_trait]
impl SttEngine for OpenAiSttEngine {
    fn name(&self) -> &str {
        "openai"
    }

    async fn transcribe(
        &self,
        audio: &AudioInput,
        options: &TranscribeOptions,
    ) -> Result<Transcript> {
        let upload = audio.to_uploadable()?;
        let file = Part::bytes(upload.bytes)
            .file_name(format!("audio.{}", upload.format.extension()))
            .mime_str(upload.format.mime_type())?;
        let format = self.response_format();
        let mut form = Form::new()
            .part("file", file)
            .text("model", self.config.model.clone())
            .text("response_format", format.clone());
        // Forcing a language disables code-switching, so only send it when
        // the user speaks a single language.
        if let Some(lang) = &options.primary_language
            && options.secondary_languages.is_empty()
        {
            form = form.text("language", base_language(lang));
        }
        if let Some(prompt) = options.prompt_hint.as_deref().filter(|p| !p.is_empty()) {
            form = form.text("prompt", prompt.to_owned());
        }
        if let Some(t) = self.config.temperature {
            form = form.text("temperature", t.to_string());
        }

        let mut req = self.client.post(self.endpoint()).multipart(form);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(SpeechError::Api {
                status: status.as_u16(),
                message: extract_error_message(&body),
            });
        }
        if format == "text" {
            return Ok(Transcript::plain(body.trim()));
        }
        let parsed: ApiResponse =
            serde_json::from_str(&body).map_err(|e| SpeechError::Decode(e.to_string()))?;
        let language = parsed
            .language
            .map(|l| language_code_from_name(&l))
            .or_else(|| {
                (options.secondary_languages.is_empty())
                    .then(|| options.primary_language.as_deref().map(base_language))
                    .flatten()
            });
        let segments = parsed
            .segments
            .into_iter()
            .map(|s| Segment {
                text: s.text.trim().to_owned(),
                start_ms: s.start.map(|v| (v * 1000.0).round() as u64),
                end_ms: s.end.map(|v| (v * 1000.0).round() as u64),
                confidence: s.avg_logprob.map(|lp| lp.exp().clamp(0.0, 1.0) as f32),
                language: None,
            })
            .collect();
        Ok(Transcript {
            text: parsed.text.trim().to_owned(),
            language,
            segments,
        })
    }
}

/// Pulls `error.message` out of an OpenAI-style error body, falling back to
/// the raw body (truncated).
pub(crate) fn extract_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| body.chars().take(500).collect())
}
