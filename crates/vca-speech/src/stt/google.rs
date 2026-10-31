//! Google Cloud Speech-to-Text v2 `recognize` client (synchronous REST
//! recognition for short utterances, up to about one minute).
//!
//! Code-switching uses multiple `languageCodes` (supported by the `long`,
//! `short` and Chirp models for many languages); vocabulary uses an inline
//! phrase set for model adaptation.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{SttEngine, TranscribeOptions, Transcript, base_language};
use crate::audio::{AudioFormat, AudioInput};
use crate::error::{Result, SpeechError};
use crate::stt::Segment;
use crate::stt::openai::extract_error_message;

/// How requests are authenticated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GoogleAuth {
    /// API key, sent as the `key` query parameter.
    ApiKey { key: String },
    /// OAuth 2.0 access token (for example from a service account or
    /// `gcloud auth print-access-token`), sent as a bearer token.
    Bearer { token: String },
}

/// Configuration of [`GoogleSttEngine`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoogleSttConfig {
    /// API root. Defaults to `https://speech.googleapis.com`; regional
    /// recognizers use `https://{location}-speech.googleapis.com`.
    pub base_url: String,
    pub project_id: String,
    /// Recognizer location, `global` by default.
    pub location: String,
    /// Recognizer id; `_` uses the implicit default recognizer configured by
    /// the request.
    pub recognizer: String,
    /// Model (`long`, `short`, `chirp_2`, ...).
    pub model: String,
    pub auth: GoogleAuth,
    /// Boost applied to every phrase hint (0 disables boosting).
    pub phrase_boost: f32,
    /// Maximum number of phrase hints sent per request.
    pub max_phrases: usize,
    /// Optional `x-goog-user-project` header for quota attribution.
    pub quota_project: Option<String>,
    pub timeout_secs: u64,
}

impl GoogleSttConfig {
    /// A configuration with sensible defaults for the given project and auth.
    pub fn new(project_id: impl Into<String>, auth: GoogleAuth) -> Self {
        Self {
            base_url: "https://speech.googleapis.com".into(),
            project_id: project_id.into(),
            location: "global".into(),
            recognizer: "_".into(),
            model: "long".into(),
            auth,
            phrase_boost: 10.0,
            max_phrases: 500,
            quota_project: None,
            timeout_secs: 60,
        }
    }
}

/// Google Cloud Speech-to-Text v2 engine.
#[derive(Debug, Clone)]
pub struct GoogleSttEngine {
    config: GoogleSttConfig,
    client: reqwest::Client,
}

impl GoogleSttEngine {
    pub fn new(config: GoogleSttConfig) -> Result<Self> {
        if config.project_id.is_empty() {
            return Err(SpeechError::Config("Google project id is empty".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()?;
        Ok(Self { config, client })
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/v2/projects/{}/locations/{}/recognizers/{}:recognize",
            self.config.base_url.trim_end_matches('/'),
            self.config.project_id,
            self.config.location,
            self.config.recognizer
        )
    }

    /// Builds the JSON request body (exposed for tests and debugging).
    pub fn request_body(
        &self,
        audio: &AudioInput,
        options: &TranscribeOptions,
    ) -> serde_json::Value {
        let mut languages: Vec<String> = options
            .languages()
            .iter()
            .map(|l| google_language_code(l))
            .collect();
        if languages.is_empty() {
            languages.push("auto".into());
        }
        let mut config = json!({
            "model": self.config.model,
            "languageCodes": languages,
            "features": { "enableAutomaticPunctuation": true },
        });
        if audio.format == AudioFormat::Pcm16 {
            config["explicitDecodingConfig"] = json!({
                "encoding": "LINEAR16",
                "sampleRateHertz": audio.sample_rate,
                "audioChannelCount": audio.channels.max(1),
            });
        } else {
            config["autoDecodingConfig"] = json!({});
        }
        let phrases: Vec<serde_json::Value> = options
            .phrases()
            .into_iter()
            .take(self.config.max_phrases)
            .map(|p| {
                if self.config.phrase_boost > 0.0 {
                    json!({ "value": p, "boost": self.config.phrase_boost })
                } else {
                    json!({ "value": p })
                }
            })
            .collect();
        if !phrases.is_empty() {
            config["adaptation"] = json!({
                "phraseSets": [{ "inlinePhraseSet": { "phrases": phrases } }]
            });
        }
        json!({
            "config": config,
            "content": base64::engine::general_purpose::STANDARD.encode(&audio.bytes),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecognizeResponse {
    #[serde(default)]
    results: Vec<RecognitionResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecognitionResult {
    #[serde(default)]
    alternatives: Vec<Alternative>,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    result_end_offset: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Alternative {
    #[serde(default)]
    transcript: String,
    #[serde(default)]
    confidence: Option<f32>,
}

/// Parses a protobuf JSON duration such as `"3.500s"` into milliseconds.
fn parse_duration_ms(s: &str) -> Option<u64> {
    let secs: f64 = s.strip_suffix('s')?.parse().ok()?;
    Some((secs * 1000.0).round() as u64)
}

#[async_trait]
impl SttEngine for GoogleSttEngine {
    fn name(&self) -> &str {
        "google"
    }

    async fn transcribe(
        &self,
        audio: &AudioInput,
        options: &TranscribeOptions,
    ) -> Result<Transcript> {
        let body = self.request_body(audio, options);
        let mut req = self.client.post(self.endpoint()).json(&body);
        req = match &self.config.auth {
            GoogleAuth::ApiKey { key } => req.query(&[("key", key)]),
            GoogleAuth::Bearer { token } => req.bearer_auth(token),
        };
        if let Some(p) = &self.config.quota_project {
            req = req.header("x-goog-user-project", p);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(SpeechError::Api {
                status: status.as_u16(),
                message: extract_error_message(&text),
            });
        }
        let parsed: RecognizeResponse =
            serde_json::from_str(&text).map_err(|e| SpeechError::Decode(e.to_string()))?;

        let mut segments = Vec::new();
        let mut prev_end = 0;
        for r in parsed.results {
            let Some(best) = r.alternatives.into_iter().next() else {
                continue;
            };
            if best.transcript.trim().is_empty() {
                continue;
            }
            let end = r.result_end_offset.as_deref().and_then(parse_duration_ms);
            segments.push(Segment {
                text: best.transcript.trim().to_owned(),
                start_ms: end.map(|_| prev_end),
                end_ms: end,
                confidence: best.confidence.filter(|c| *c > 0.0),
                language: r.language_code.map(|l| l.to_ascii_lowercase()),
            });
            if let Some(e) = end {
                prev_end = e;
            }
        }
        let text = join_segments(segments.iter().map(|s| s.text.as_str()));
        // The language of the longest segment is the utterance language.
        let language = segments
            .iter()
            .filter(|s| s.language.is_some())
            .max_by_key(|s| s.text.len())
            .and_then(|s| s.language.clone());
        Ok(Transcript {
            text,
            language,
            segments,
        })
    }
}

/// Joins segment texts, without inserting spaces between CJK segments.
pub(crate) fn join_segments<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for p in parts {
        let needs_space = match (out.chars().last(), p.chars().next()) {
            (Some(a), Some(b)) => {
                !(crate::text::CharClass::of(a).is_boundary_free()
                    && crate::text::CharClass::of(b).is_boundary_free())
            }
            _ => false,
        };
        if needs_space {
            out.push(' ');
        }
        out.push_str(p);
    }
    out
}

/// Expands bare language codes to the regional codes Google expects
/// (`en` → `en-US`, `zh` → `cmn-Hans-CN`). Full tags pass through.
pub fn google_language_code(tag: &str) -> String {
    if tag.contains(['-', '_']) {
        return tag.replace('_', "-");
    }
    let code = match base_language(tag).as_str() {
        "en" => "en-US",
        "ja" => "ja-JP",
        "ko" => "ko-KR",
        "zh" => "cmn-Hans-CN",
        "es" => "es-ES",
        "de" => "de-DE",
        "fr" => "fr-FR",
        "pt" => "pt-BR",
        "it" => "it-IT",
        "ru" => "ru-RU",
        "nl" => "nl-NL",
        "hi" => "hi-IN",
        "vi" => "vi-VN",
        "id" => "id-ID",
        "tr" => "tr-TR",
        "pl" => "pl-PL",
        other => return other.to_owned(),
    };
    code.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_codes() {
        assert_eq!(parse_duration_ms("3.500s"), Some(3500));
        assert_eq!(parse_duration_ms("2s"), Some(2000));
        assert_eq!(parse_duration_ms("x"), None);
        assert_eq!(google_language_code("ja"), "ja-JP");
        assert_eq!(google_language_code("en_GB"), "en-GB");
    }

    #[test]
    fn segments_join_without_spaces_in_cjk() {
        assert_eq!(
            join_segments(["こんにちは", "世界"].into_iter()),
            "こんにちは世界"
        );
        assert_eq!(join_segments(["hello", "world"].into_iter()), "hello world");
    }

    #[test]
    fn request_body_has_languages_and_phrases() {
        let engine = GoogleSttEngine::new(GoogleSttConfig::new(
            "p",
            GoogleAuth::ApiKey { key: "k".into() },
        ))
        .unwrap();
        let audio = AudioInput::from_pcm16(&[0, 1, 2], 16_000);
        let opts = TranscribeOptions {
            primary_language: Some("en".into()),
            secondary_languages: vec!["ko".into()],
            prompt_hint: Some("kubectl, useEffect".into()),
            phrase_hints: vec![],
        };
        let body = engine.request_body(&audio, &opts);
        assert_eq!(body["config"]["languageCodes"], json!(["en-US", "ko-KR"]));
        assert_eq!(
            body["config"]["explicitDecodingConfig"]["sampleRateHertz"],
            16_000
        );
        assert_eq!(
            body["config"]["adaptation"]["phraseSets"][0]["inlinePhraseSet"]["phrases"][1]["value"],
            "useEffect"
        );
    }
}
