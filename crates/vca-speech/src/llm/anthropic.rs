//! Anthropic Messages API backend (`POST {base_url}/v1/messages`) over raw
//! HTTP (there is no official Rust SDK).
//!
//! - No `temperature`, `top_p` or `top_k` (current models reject them), no
//!   prefilled assistant turn and no forced `tool_choice`: the system prompt
//!   asks for JSON only.
//! - `stop_reason` `refusal` or `max_tokens` is an error, so callers keep the
//!   vocabulary-corrected text.
//! - Authentication is an API key (`x-api-key`) or a short-lived OAuth access
//!   token from a [`TokenProvider`] (`Authorization: Bearer` plus the
//!   `anthropic-beta: oauth-2025-04-20` header). [`AntCliTokenProvider`]
//!   gets the token of the user's Anthropic (Console) account from the `ant`
//!   CLI; on a 401 the token is refreshed once.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::ChatBackend;
use crate::error::{Result, SpeechError};
use crate::stt::openai::extract_error_message;

/// Default API root.
pub const ANTHROPIC_API: &str = "https://api.anthropic.com";
/// Default model: small and fast, which suits low-latency correction.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta header required with OAuth access tokens.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Default `max_tokens`.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Supplies short-lived access tokens. `refresh` asks for a new token even
/// when a cached one looks valid (after a 401).
#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn token(&self, refresh: bool) -> Result<String>;
}

/// How requests authenticate.
#[derive(Clone)]
pub enum AnthropicAuth {
    /// `x-api-key`.
    ApiKey(String),
    /// `Authorization: Bearer <token>` with the OAuth beta header.
    Token(Arc<dyn TokenProvider>),
}

impl std::fmt::Debug for AnthropicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key.
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey(..)"),
            Self::Token(_) => f.write_str("Token(..)"),
        }
    }
}

/// Configuration of [`AnthropicClient`].
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// API root; requests go to `{base_url}/v1/messages`.
    pub base_url: String,
    pub model: String,
    pub auth: AnthropicAuth,
    pub timeout: Duration,
    pub max_tokens: u32,
}

impl AnthropicConfig {
    /// Defaults (API root, model, `max_tokens`) with the given auth.
    pub fn new(auth: AnthropicAuth, timeout: Duration) -> Self {
        Self {
            base_url: ANTHROPIC_API.into(),
            model: DEFAULT_MODEL.into(),
            auth,
            timeout,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

/// Anthropic Messages API client.
#[derive(Debug, Clone)]
pub struct AnthropicClient {
    config: AnthropicConfig,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

impl AnthropicClient {
    pub fn new(config: AnthropicConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(crate::auth::redirect_policy())
            .build()?;
        Ok(Self { config, client })
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }

    async fn send(&self, body: &serde_json::Value, refresh: bool) -> Result<reqwest::Response> {
        let mut req = self
            .client
            .post(self.endpoint())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(body);
        req = match &self.config.auth {
            AnthropicAuth::ApiKey(key) => req.header("x-api-key", key),
            AnthropicAuth::Token(provider) => req
                .bearer_auth(provider.token(refresh).await?)
                .header("anthropic-beta", OAUTH_BETA),
        };
        Ok(req.send().await?)
    }
}

#[async_trait]
impl ChatBackend for AnthropicClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let body = json!({
            "model": self.config.model,
            "max_tokens": self.config.max_tokens,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        });
        let mut resp = self.send(&body, false).await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED
            && matches!(self.config.auth, AnthropicAuth::Token(_))
        {
            // The access token may have expired: refresh once.
            resp = self.send(&body, true).await?;
        }
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(SpeechError::Api {
                status: status.as_u16(),
                message: extract_error_message(&text),
            });
        }
        let parsed: MessagesResponse =
            serde_json::from_str(&text).map_err(|e| SpeechError::Decode(e.to_string()))?;
        match parsed.stop_reason.as_deref() {
            Some("refusal") => return Err(SpeechError::Decode("the model refused".into())),
            Some("max_tokens") => {
                return Err(SpeechError::Decode(
                    "the reply was cut off (max_tokens)".into(),
                ));
            }
            _ => {}
        }
        let out: String = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text.as_deref())
            .collect();
        if out.trim().is_empty() {
            return Err(SpeechError::Decode("no text in the reply".into()));
        }
        Ok(out)
    }
}

/// Access tokens of the user's Anthropic account from the `ant` CLI
/// (`ant auth print-credentials --access-token`, after `ant auth login`).
/// Tokens are cached for [`AntCliTokenProvider::TTL`].
///
/// This bills the user's Anthropic API (Console) account. claude.ai
/// subscription logins cannot be used by third-party apps.
pub struct AntCliTokenProvider {
    /// `ANTHROPIC_PROFILE` for the CLI; `None` or `default` uses its default.
    profile: Option<String>,
    program: PathBuf,
    cache: tokio::sync::Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for AntCliTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntCliTokenProvider")
            .field("profile", &self.profile)
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

impl AntCliTokenProvider {
    /// How long a token is reused.
    pub const TTL: Duration = Duration::from_secs(300);
    /// Longest wait for the CLI.
    const RUN_TIMEOUT: Duration = Duration::from_secs(15);

    pub fn new(profile: Option<String>) -> Self {
        Self {
            profile: profile.filter(|p| !p.trim().is_empty()),
            program: PathBuf::from("ant"),
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Uses another executable (absolute path, or a wrapper on Windows).
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    async fn run(&self) -> Result<String> {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(["auth", "print-credentials", "--access-token"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(p) = self.profile.as_deref().filter(|p| *p != "default") {
            cmd.env("ANTHROPIC_PROFILE", p);
        }
        let out = tokio::time::timeout(Self::RUN_TIMEOUT, cmd.output())
            .await
            .map_err(|_| SpeechError::Config("`ant auth print-credentials` timed out".into()))?
            .map_err(|e| {
                SpeechError::Config(format!(
                    "cannot run `{}` (install the ant CLI and run `ant auth login`): {e}",
                    self.program.display()
                ))
            })?;
        let token = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || token.is_empty() || token.contains(char::is_whitespace) {
            let reason: String = String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(200)
                .collect();
            return Err(SpeechError::Config(format!(
                "`ant auth print-credentials` failed (run `ant auth login`): {reason}"
            )));
        }
        Ok(token)
    }
}

#[async_trait]
impl TokenProvider for AntCliTokenProvider {
    async fn token(&self, refresh: bool) -> Result<String> {
        let mut cache = self.cache.lock().await;
        if !refresh
            && let Some((token, at)) = cache.as_ref()
            && at.elapsed() < Self::TTL
        {
            return Ok(token.clone());
        }
        let token = self.run().await?;
        *cache = Some((token.clone(), Instant::now()));
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_cli_is_a_configuration_error() {
        let p = AntCliTokenProvider::new(Some("work".into()))
            .with_program("vca-test-no-such-ant-binary-7d1c");
        let err = p.token(false).await.unwrap_err();
        assert!(err.to_string().contains("ant auth login"), "{err}");
    }

    #[test]
    fn auth_debug_hides_the_key() {
        let s = format!("{:?}", AnthropicAuth::ApiKey("sk-ant-secret".into()));
        assert!(!s.contains("secret"));
    }
}
