//! LLM passes over transcripts through any OpenAI-compatible chat
//! completions endpoint (`POST {base_url}/chat/completions`) or the
//! Anthropic Messages API ([`anthropic`]):
//!
//! - contextual correction: the model proposes minimal `{from, to}` fixes
//!   for misheard words, which [`validate::apply_fixes`] checks strictly
//!   before anything changes;
//! - topic detection: a short topic label and domain terms that feed the
//!   recognizer's prompt hints and the correction prompt.
//!
//! Prompts live in [`prompts`]. Prompts ask for JSON only; OpenAI-compatible
//! requests use temperature 0 (Anthropic models reject sampling parameters,
//! so none are sent there). Callers treat every error as "keep the text
//! unchanged".

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::EngineAuth;
use crate::error::{Result, SpeechError};
use crate::stt::openai::extract_error_message;

pub mod anthropic;
pub mod prompts;
pub mod tools;
pub mod validate;

pub use anthropic::{
    AntCliTokenProvider, AnthropicAuth, AnthropicClient, AnthropicConfig, TokenProvider,
};
pub use validate::{Applied, Fix, FixLimits, Rejection, apply_fixes};

/// Longest topic label kept, in characters.
pub const MAX_TOPIC_CHARS: usize = 60;
/// Domain terms kept per topic.
pub const MAX_TOPIC_TERMS: usize = 20;
/// Longest domain term kept, in characters.
const MAX_TERM_CHARS: usize = 48;
/// Vocabulary and topic terms sent with a correction request.
const MAX_PROMPT_TERMS: usize = 60;
/// Characters of preceding text sent with a correction request.
const MAX_PRECEDING_CHARS: usize = 600;
/// Characters of dictation sent with a topic request.
const MAX_TOPIC_TEXT_CHARS: usize = 3000;

/// Configuration of [`ChatClient`].
#[derive(Clone, PartialEq)]
pub struct ChatConfig {
    /// API root, e.g. `https://api.openai.com/v1` or `http://localhost:8000/v1`.
    pub base_url: String,
    /// Bearer token. Optional for self-hosted servers.
    pub api_key: Option<String>,
    pub model: String,
    /// Request timeout.
    pub timeout: Duration,
    /// Key header and gateway token.
    pub auth: EngineAuth,
}

// Neither the key nor the gateway token is printed.
impl std::fmt::Debug for ChatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("auth", &self.auth)
            .finish()
    }
}

/// A minimal OpenAI-compatible chat completions client.
#[derive(Debug, Clone)]
pub struct ChatClient {
    config: ChatConfig,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

impl ChatClient {
    /// Fails on an invalid key header or a gateway token without one.
    pub fn new(config: ChatConfig) -> Result<Self> {
        config.auth.validate()?;
        let client = crate::retry::client_builder(config.timeout).build()?;
        Ok(Self { config, client })
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Adds the API key and the gateway token.
    pub(crate) fn authorized(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        self.config.auth.apply(req, self.config.api_key.as_deref())
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// Sends one system and one user message and returns the reply text.
    pub async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let body = json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "temperature": 0,
            "stream": false,
        });
        // Resent only when no connection was made (never billed twice).
        let resp = crate::retry::send_unconnected_retry(|| {
            self.authorized(self.client.post(self.endpoint()).json(&body))
                .send()
        })
        .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(SpeechError::Api {
                status: status.as_u16(),
                message: extract_error_message(&text),
            });
        }
        let parsed: ChatResponse =
            serde_json::from_str(&text).map_err(|e| SpeechError::Decode(e.to_string()))?;
        parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| SpeechError::Decode("no message content".into()))
    }
}

/// What the correction pass sees.
#[derive(Debug, Clone, Copy, Default)]
pub struct CorrectionInput<'a> {
    /// The chunk after vocabulary correction.
    pub text: &'a str,
    /// Text dictated just before the chunk.
    pub preceding: Option<&'a str>,
    pub topic: Option<&'a str>,
    /// Topic terms and vocabulary hint terms.
    pub terms: &'a [String],
    pub language: Option<&'a str>,
}

/// A detected topic.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Topic {
    pub topic: String,
    #[serde(default)]
    pub terms: Vec<String>,
}

/// Drops fixes that would replace a known term (`domain_terms`, compared
/// case-insensitively): models sometimes "correct" a valid term into a more
/// common one, such as pnpm into npm.
fn protect_terms(fixes: Vec<Fix>, terms: &[String]) -> Vec<Fix> {
    fixes
        .into_iter()
        .filter(|f| {
            let from = f.from.trim();
            !terms
                .iter()
                .any(|t| t.trim().to_lowercase() == from.to_lowercase())
        })
        .collect()
}

/// The JSON user message of a correction request.
pub fn correction_user_message(input: &CorrectionInput<'_>) -> String {
    let preceding = input.preceding.map(|p| tail_chars(p, MAX_PRECEDING_CHARS));
    let terms: Vec<&String> = input.terms.iter().take(MAX_PROMPT_TERMS).collect();
    json!({
        "text": input.text,
        "preceding_text": preceding,
        "topic": input.topic,
        "domain_terms": terms,
        "language": input.language,
    })
    .to_string()
}

/// The JSON user message of a topic request.
pub fn topic_user_message(previous: Option<&str>, text: &str) -> String {
    json!({
        "text": tail_chars(text, MAX_TOPIC_TEXT_CHARS),
        "previous_topic": previous,
    })
    .to_string()
}

fn tail_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    let skip = count - n;
    let start = s.char_indices().nth(skip).map_or(s.len(), |(i, _)| i);
    &s[start..]
}

/// Pulls the JSON object out of a model reply, tolerating code fences and
/// stray prose around it.
fn json_object(reply: &str) -> Result<&str> {
    let start = reply.find('{');
    let end = reply.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if s < e => Ok(&reply[s..=e]),
        _ => Err(SpeechError::Decode("the reply holds no JSON object".into())),
    }
}

#[derive(Debug, Deserialize)]
struct FixesReply {
    fixes: Vec<Fix>,
}

/// Parses a correction reply (`{"fixes": [...]}`).
pub fn parse_fixes(reply: &str) -> Result<Vec<Fix>> {
    let parsed: FixesReply = serde_json::from_str(json_object(reply)?)
        .map_err(|e| SpeechError::Decode(format!("correction reply: {e}")))?;
    Ok(parsed.fixes)
}

/// Parses and sanitizes a topic reply (`{"topic": "...", "terms": [...]}`).
pub fn parse_topic(reply: &str) -> Result<Topic> {
    let parsed: Topic = serde_json::from_str(json_object(reply)?)
        .map_err(|e| SpeechError::Decode(format!("topic reply: {e}")))?;
    let topic = clean_label(&parsed.topic, MAX_TOPIC_CHARS);
    if topic.is_empty() {
        return Err(SpeechError::Decode("empty topic".into()));
    }
    let mut terms: Vec<String> = Vec::new();
    for t in &parsed.terms {
        let t = clean_label(t, MAX_TERM_CHARS);
        if !t.is_empty() && !terms.iter().any(|o| o.eq_ignore_ascii_case(&t)) {
            terms.push(t);
        }
        if terms.len() == MAX_TOPIC_TERMS {
            break;
        }
    }
    Ok(Topic { topic, terms })
}

/// Single-line, trimmed, at most `max` characters.
fn clean_label(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(max).collect::<String>().trim().to_owned()
}

/// A chat model behind one HTTP API: OpenAI-compatible chat completions
/// ([`ChatClient`]) or the Anthropic Messages API
/// ([`anthropic::AnthropicClient`]).
#[async_trait]
pub trait ChatBackend: Send + Sync {
    /// Sends one system and one user message and returns the reply text.
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}

#[async_trait]
impl ChatBackend for ChatClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        ChatClient::complete(self, system, user).await
    }
}

/// Contextual correction and topic detection over a [`ChatBackend`].
#[derive(Clone)]
pub struct ContextCorrector {
    chat: Arc<dyn ChatBackend>,
    limits: FixLimits,
}

impl std::fmt::Debug for ContextCorrector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextCorrector")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl ContextCorrector {
    pub fn new(chat: Arc<dyn ChatBackend>) -> Self {
        Self {
            chat,
            limits: FixLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: FixLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The chat model behind this corrector, for other short passes over
    /// the same profile (spoken result summaries).
    pub fn chat(&self) -> Arc<dyn ChatBackend> {
        Arc::clone(&self.chat)
    }

    /// Asks the model for fixes and applies them when they pass validation.
    /// Any transport, parse or validation problem is an error; the caller
    /// then keeps `input.text`.
    pub async fn correct(&self, input: &CorrectionInput<'_>) -> Result<Applied> {
        if input.text.trim().is_empty() {
            return Ok(Applied {
                text: input.text.to_owned(),
                fixes: Vec::new(),
            });
        }
        let reply = self
            .chat
            .complete(prompts::CORRECTION_SYSTEM, &correction_user_message(input))
            .await?;
        let fixes = protect_terms(parse_fixes(&reply)?, input.terms);
        apply_fixes(input.text, &fixes, self.limits)
            .map_err(|e| SpeechError::Decode(format!("rejected correction: {e}")))
    }

    /// Asks the model for the topic of `text`.
    pub async fn detect_topic(&self, previous: Option<&str>, text: &str) -> Result<Topic> {
        let reply = self
            .chat
            .complete(prompts::TOPIC_SYSTEM, &topic_user_message(previous, text))
            .await?;
        parse_topic(&reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_and_bare_replies() {
        let fixes =
            parse_fixes("```json\n{\"fixes\":[{\"from\":\"a\",\"to\":\"b\"}]}\n```").unwrap();
        assert_eq!(fixes, vec![Fix::new("a", "b")]);
        assert!(parse_fixes("{\"fixes\":[]}").unwrap().is_empty());
        assert!(parse_fixes("Sure! Nothing to fix.").is_err());
        assert!(parse_fixes("{\"fixes\": \"none\"}").is_err());
    }

    #[test]
    fn sanitizes_topics() {
        let mut terms = vec!["React".to_owned(), "react".to_owned(), " ".to_owned()];
        terms.extend((0..30).map(|i| format!("term{i}")));
        let reply = json!({
            "topic": "  frontend/React\n hooks  ",
            "terms": terms,
        })
        .to_string();
        let t = parse_topic(&reply).unwrap();
        assert_eq!(t.topic, "frontend/React hooks");
        assert_eq!(t.terms.len(), MAX_TOPIC_TERMS);
        assert_eq!(t.terms[0], "React");
        assert_eq!(t.terms[1], "term0");
        assert!(parse_topic("{\"topic\":\"  \",\"terms\":[]}").is_err());
    }

    #[test]
    fn user_messages_are_json_with_bounded_context() {
        let long = "가".repeat(MAX_PRECEDING_CHARS + 10);
        let terms = vec!["useEffect".to_owned()];
        let msg = correction_user_message(&CorrectionInput {
            text: "유즈 이펙트 고쳐 줘",
            preceding: Some(&long),
            topic: Some("frontend/React"),
            terms: &terms,
            language: Some("ko"),
        });
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["text"], "유즈 이펙트 고쳐 줘");
        assert_eq!(
            v["preceding_text"].as_str().unwrap().chars().count(),
            MAX_PRECEDING_CHARS
        );
        assert_eq!(v["domain_terms"][0], "useEffect");
        let v: serde_json::Value =
            serde_json::from_str(&topic_user_message(None, "kubectl rollout")).unwrap();
        assert!(v["previous_topic"].is_null());
    }

    #[test]
    fn prompts_demand_json_only() {
        assert!(prompts::CORRECTION_SYSTEM.contains("Output JSON only"));
        assert!(prompts::TOPIC_SYSTEM.contains("Output JSON only"));
    }
}
