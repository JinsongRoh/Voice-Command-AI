//! OAuth 2.0 Device Authorization Grant (RFC 8628) client for the DNS
//! Pilot console, with refresh-token rotation (RFC 6749 §6) and token
//! revocation (RFC 7009).
//!
//! Flow: [`DeviceFlowClient::start`] returns a user code and verification
//! URL to show in the setup wizard; the user approves in a browser (and
//! picks the target PC there); [`DeviceFlowClient::poll`] waits for the
//! token, honoring `interval` and `slow_down`.
//!
//! Grants are limited by DNS Pilot: OAuth tokens may only create `app`-mode
//! tunnels and may only modify or delete tunnels created by the same grant.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::contract::{self, paths};
use crate::error::{ProviderError, Result};
use crate::secret::Secret;

/// RFC 8628 §3.5: default polling interval when the server sends none.
const DEFAULT_INTERVAL_SECS: u64 = 5;
/// RFC 8628 §3.5: `slow_down` increases the interval by 5 seconds.
const SLOW_DOWN_STEP_SECS: u64 = 5;
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Console OAuth settings.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Console base URL (configurable; default `https://dns.dnspilot.kr`).
    pub console_url: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    /// Length of one interval "second". Real clients use one second; tests
    /// shorten it.
    pub interval_unit: Duration,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            console_url: contract::DEFAULT_CONSOLE_URL.into(),
            client_id: contract::OAUTH_CLIENT_ID.into(),
            scopes: contract::OAUTH_SCOPES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            interval_unit: Duration::from_secs(1),
        }
    }
}

/// What the wizard shows the user.
#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    device_code: Secret,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_at: Instant,
    pub interval_secs: u64,
}

/// Tokens from the console. `Debug` output is redacted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub token_type: String,
    /// Seconds since the Unix epoch.
    pub expires_at: Option<u64>,
    pub scope: Option<String>,
}

impl TokenSet {
    /// True when the access token expires within `margin`.
    pub fn expires_within(&self, margin: Duration) -> bool {
        match self.expires_at {
            Some(t) => now_secs() + margin.as_secs() >= t,
            None => false,
        }
    }
}

/// OAuth-specific failures.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("the user denied the request")]
    AccessDenied,
    #[error("the device code expired; start again")]
    Expired,
    #[error("the grant is invalid or was revoked")]
    InvalidGrant,
    #[error("OAuth error {code}: {description}")]
    Other { code: String, description: String },
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

impl From<reqwest::Error> for OAuthError {
    fn from(e: reqwest::Error) -> Self {
        Self::Provider(e.into())
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Device flow client.
#[derive(Debug, Clone)]
pub struct DeviceFlowClient {
    http: reqwest::Client,
    config: OAuthConfig,
}

enum TokenReply {
    Tokens(contract::TokenEndpointResponse),
    Error(contract::OAuthErrorResponse),
}

impl DeviceFlowClient {
    pub fn new(config: OAuthConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("vca-providers/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http, config }
    }

    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.console_url.trim_end_matches('/'), path)
    }

    /// Requests a device code (`POST /oauth/device_authorization`).
    pub async fn start(&self) -> Result<DeviceAuthorization, OAuthError> {
        let scope = self.config.scopes.join(" ");
        let resp = self
            .http
            .post(self.url(paths::OAUTH_DEVICE_AUTHORIZATION))
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", scope.as_str()),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let err: contract::OAuthErrorResponse = resp.json().await.unwrap_or_default();
            return Err(oauth_error(err, status));
        }
        let r: contract::DeviceAuthorizationResponse = resp.json().await?;
        Ok(DeviceAuthorization {
            device_code: Secret::new(r.device_code),
            user_code: r.user_code,
            verification_uri: r.verification_uri,
            verification_uri_complete: r.verification_uri_complete,
            expires_at: Instant::now()
                + self.config.interval_unit * u32::try_from(r.expires_in).unwrap_or(u32::MAX),
            interval_secs: r.interval.unwrap_or(DEFAULT_INTERVAL_SECS).max(1),
        })
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<TokenReply, OAuthError> {
        let resp = self
            .http
            .post(self.url(paths::OAUTH_TOKEN))
            .form(form)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if status.is_success() {
            let t: contract::TokenEndpointResponse =
                serde_json::from_slice(&body).map_err(ProviderError::from)?;
            return Ok(TokenReply::Tokens(t));
        }
        match serde_json::from_slice::<contract::OAuthErrorResponse>(&body) {
            Ok(e) if !e.error.is_empty() => Ok(TokenReply::Error(e)),
            _ => Err(OAuthError::Provider(ProviderError::Api {
                status: status.as_u16(),
                message: String::from_utf8_lossy(&body).chars().take(300).collect(),
            })),
        }
    }

    /// Polls the token endpoint until the user approves, denies, or the
    /// code expires. Honors `interval` and `slow_down` (+5 s each time).
    pub async fn poll(&self, auth: &DeviceAuthorization) -> Result<TokenSet, OAuthError> {
        let mut interval = auth.interval_secs;
        loop {
            tokio::time::sleep(
                self.config.interval_unit * u32::try_from(interval).unwrap_or(u32::MAX),
            )
            .await;
            if Instant::now() >= auth.expires_at {
                return Err(OAuthError::Expired);
            }
            let reply = self
                .token_request(&[
                    ("grant_type", DEVICE_CODE_GRANT),
                    ("device_code", auth.device_code.expose()),
                    ("client_id", &self.config.client_id),
                ])
                .await?;
            match reply {
                TokenReply::Tokens(t) => return Ok(to_token_set(t, None)),
                TokenReply::Error(e) => match e.error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => interval += SLOW_DOWN_STEP_SECS,
                    _ => return Err(oauth_error(e, 400)),
                },
            }
        }
    }

    /// Refreshes the access token. DNS Pilot rotates refresh tokens: the
    /// returned set carries the new refresh token, and the old one must be
    /// discarded. If the server omits a new one, the old one is kept.
    pub async fn refresh(&self, tokens: &TokenSet) -> Result<TokenSet, OAuthError> {
        let refresh = tokens
            .refresh_token
            .as_ref()
            .ok_or(OAuthError::InvalidGrant)?;
        let reply = self
            .token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.expose()),
                ("client_id", &self.config.client_id),
            ])
            .await?;
        match reply {
            TokenReply::Tokens(t) => Ok(to_token_set(t, Some(refresh.clone()))),
            TokenReply::Error(e) => Err(oauth_error(e, 400)),
        }
    }

    /// Revokes a token (RFC 7009). Revoking the refresh token ends the grant.
    pub async fn revoke(&self, token: &Secret, hint: Option<&str>) -> Result<(), OAuthError> {
        let mut form = vec![
            ("token", token.expose()),
            ("client_id", self.config.client_id.as_str()),
        ];
        if let Some(h) = hint {
            form.push(("token_type_hint", h));
        }
        let resp = self
            .http
            .post(self.url(paths::OAUTH_REVOKE))
            .form(&form)
            .send()
            .await?;
        // RFC 7009 §2.2: 200 even for unknown tokens.
        if resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status().as_u16();
        let err: contract::OAuthErrorResponse = resp.json().await.unwrap_or_default();
        Err(oauth_error(err, status))
    }
}

fn to_token_set(t: contract::TokenEndpointResponse, previous_refresh: Option<Secret>) -> TokenSet {
    TokenSet {
        access_token: Secret::new(t.access_token),
        refresh_token: t.refresh_token.map(Secret::new).or(previous_refresh),
        token_type: t.token_type.unwrap_or_else(|| "Bearer".into()),
        expires_at: t.expires_in.map(|s| now_secs() + s),
        scope: t.scope,
    }
}

fn oauth_error(e: contract::OAuthErrorResponse, status: u16) -> OAuthError {
    match e.error.as_str() {
        "access_denied" => OAuthError::AccessDenied,
        "expired_token" => OAuthError::Expired,
        "invalid_grant" => OAuthError::InvalidGrant,
        "" => OAuthError::Provider(ProviderError::Api {
            status,
            message: "OAuth request failed".into(),
        }),
        code => OAuthError::Other {
            code: code.to_owned(),
            description: e.error_description.unwrap_or_default(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scopes_exclude_p2p_write() {
        let c = OAuthConfig::default();
        assert_eq!(c.client_id, "vca");
        assert!(c.scopes.iter().any(|s| s == "devices:read"));
        assert!(!c.scopes.iter().any(|s| s == "p2p:write"));
        assert_eq!(c.console_url, "https://dns.dnspilot.kr");
    }

    #[test]
    fn token_set_debug_is_redacted() {
        let t = to_token_set(
            contract::TokenEndpointResponse {
                access_token: "at-secret".into(),
                token_type: None,
                expires_in: Some(3600),
                refresh_token: Some("rt-secret".into()),
                scope: None,
            },
            None,
        );
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        assert!(!t.expires_within(Duration::from_secs(60)));
        assert!(t.expires_within(Duration::from_secs(7200)));
    }
}
