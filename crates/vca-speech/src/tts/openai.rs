//! Client for OpenAI-compatible speech APIs (`POST {base_url}/audio/speech`
//! with a JSON body `{model, input, voice, response_format, speed,
//! language}`).
//!
//! `language` is the base language of the text (`ko`, `en`): multilingual
//! engines such as Supertonic otherwise guess it from the text and can read
//! Korean mixed with Latin words in another language's sounds. OpenAI's own
//! API has no such field; a server that refuses a request with it (400 or
//! 422) is asked again without it, and this client stops sending it.
//!
//! Most self-hosted TTS servers offer this endpoint, often through a small
//! wrapper. The answer is the raw audio, read up to a size cap.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use super::{SynthesisRequest, Synthesized, TtsEngine, TtsFormat};
use crate::auth::EngineAuth;
use crate::error::{Result, SpeechError};

/// Longest error text kept from a failed response.
const ERROR_CHARS: usize = 200;

/// Configuration of [`OpenAiTtsEngine`].
#[derive(Clone, PartialEq)]
pub struct OpenAiTtsConfig {
    /// API root, e.g. `http://127.0.0.1:8880/v1`.
    pub base_url: String,
    /// Bearer token. Optional for self-hosted servers.
    pub api_key: Option<String>,
    pub model: String,
    /// Voice used when a request names none.
    pub voice: String,
    pub format: TtsFormat,
    /// Speed used when a request names none.
    pub speed: Option<f32>,
    pub timeout: Duration,
    /// Largest accepted audio answer.
    pub max_audio_bytes: usize,
    /// Key header and gateway token.
    pub auth: EngineAuth,
}

// The key is never printed.
impl std::fmt::Debug for OpenAiTtsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiTtsConfig")
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("format", &self.format)
            .field("speed", &self.speed)
            .field("timeout", &self.timeout)
            .field("max_audio_bytes", &self.max_audio_bytes)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl Default for OpenAiTtsConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8880/v1".into(),
            api_key: None,
            model: "tts-1".into(),
            voice: "alloy".into(),
            format: TtsFormat::Mp3,
            speed: None,
            timeout: Duration::from_secs(30),
            max_audio_bytes: 2 * 1024 * 1024,
            auth: EngineAuth::default(),
        }
    }
}

/// Text-to-speech over an OpenAI-compatible HTTP API.
#[derive(Debug, Clone)]
pub struct OpenAiTtsEngine {
    config: OpenAiTtsConfig,
    client: reqwest::Client,
    /// The server refused a request with `language`; it is not sent again.
    no_language: Arc<AtomicBool>,
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
}

/// The base language of a BCP-47 tag, lower case (`ko-KR` -> `ko`); `None`
/// for a blank or malformed tag.
fn base_language(tag: &str) -> Option<String> {
    let base = tag.trim().split(['-', '_']).next().unwrap_or_default();
    let ok = (2..=3).contains(&base.len()) && base.chars().all(|c| c.is_ascii_alphabetic());
    ok.then(|| base.to_ascii_lowercase())
}

impl OpenAiTtsEngine {
    /// Fails on an invalid key header or a gateway token without one.
    pub fn new(config: OpenAiTtsConfig) -> Result<Self> {
        config.auth.validate()?;
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(crate::auth::redirect_policy())
            .build()?;
        Ok(Self {
            config,
            client,
            no_language: Arc::new(AtomicBool::new(false)),
        })
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/audio/speech",
            self.config.base_url.trim_end_matches('/')
        )
    }

    async fn post(&self, body: &Body<'_>) -> Result<reqwest::Response> {
        let req = self.client.post(self.endpoint()).json(body);
        let req = self.config.auth.apply(req, self.config.api_key.as_deref());
        req.send().await.map_err(http)
    }

    fn too_large(&self) -> SpeechError {
        SpeechError::Decode(format!(
            "the TTS answer is larger than {} bytes",
            self.config.max_audio_bytes
        ))
    }
}

/// Transport errors without the URL (it may name a private host).
fn http(err: reqwest::Error) -> SpeechError {
    SpeechError::Http(err.without_url())
}

fn error_message(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("detail"))
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| body.to_owned());
    message
        .chars()
        .filter(|c| !c.is_control())
        .take(ERROR_CHARS)
        .collect()
}

#[async_trait]
impl TtsEngine for OpenAiTtsEngine {
    fn name(&self) -> &str {
        "openai_compatible"
    }

    async fn synthesize(&self, request: &SynthesisRequest) -> Result<Synthesized> {
        let voice = request
            .voice
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(&self.config.voice);
        let language = request
            .language
            .as_deref()
            .and_then(base_language)
            .filter(|_| !self.no_language.load(Ordering::Relaxed));
        let mut body = Body {
            model: &self.config.model,
            input: &request.text,
            voice,
            response_format: self.config.format.as_str(),
            speed: request.speed.or(self.config.speed),
            language: language.as_deref(),
        };
        let mut resp = self.post(&body).await?;
        if body.language.is_some() && matches!(resp.status().as_u16(), 400 | 422) {
            // Probably a strict server that does not know `language`.
            self.no_language.store(true, Ordering::Relaxed);
            body.language = None;
            resp = self.post(&body).await?;
        }
        let status = resp.status();
        if !status.is_success() {
            let mut text = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(http)? {
                text.extend_from_slice(&chunk);
                if text.len() > 16 * 1024 {
                    break;
                }
            }
            return Err(SpeechError::Api {
                status: status.as_u16(),
                message: error_message(&String::from_utf8_lossy(&text)),
            });
        }
        let max = self.config.max_audio_bytes;
        if resp.content_length().is_some_and(|n| n > max as u64) {
            return Err(self.too_large());
        }
        let mut audio = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(http)? {
            if audio.len() + chunk.len() > max {
                return Err(self.too_large());
            }
            audio.extend_from_slice(&chunk);
        }
        if audio.is_empty() {
            return Err(SpeechError::Decode("the TTS answer is empty".into()));
        }
        Ok(Synthesized {
            format: self.config.format,
            audio,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_key_and_the_url() {
        let cfg = OpenAiTtsConfig {
            api_key: Some("sk-hidden".into()),
            base_url: "http://private-host:1/v1".into(),
            ..OpenAiTtsConfig::default()
        };
        let text = format!("{cfg:?}");
        assert!(!text.contains("sk-hidden"));
        assert!(!text.contains("private-host"));
    }

    #[test]
    fn languages_are_sent_as_base_tags() {
        assert_eq!(base_language("ko-KR").as_deref(), Some("ko"));
        assert_eq!(base_language(" EN_us ").as_deref(), Some("en"));
        assert_eq!(base_language("ja").as_deref(), Some("ja"));
        assert_eq!(base_language(""), None);
        assert_eq!(base_language("k"), None);
        assert_eq!(base_language("ko1-KR"), None);
    }

    #[test]
    fn error_messages_are_short_and_single_line() {
        assert_eq!(
            error_message(r#"{"error":{"message":"bad voice"}}"#),
            "bad voice"
        );
        assert_eq!(error_message(r#"{"detail":"no model"}"#), "no model");
        let long = "x\n".repeat(500);
        let msg = error_message(&long);
        assert_eq!(msg.chars().count(), ERROR_CHARS);
        assert!(!msg.contains('\n'));
    }
}
