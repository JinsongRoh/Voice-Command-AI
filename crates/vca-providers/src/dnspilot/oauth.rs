//! OAuth 2.0 Device Authorization Grant (RFC 8628) client for the DNSPilot
//! console (<https://dns.dnspilot.kr/docs/en/oauth-device-flow>).
//!
//! VCA is the registered public client `vca` (no secret). The flow:
//! [`DeviceFlowClient::start`] returns a user code and the activation URL
//! (`/oauth/activate`) to show; the person signs in there, confirms the
//! code, picks the organization and the Agent devices VCA may create tunnels
//! on, and approves; [`DeviceFlowClient::poll`] waits for the tokens,
//! honoring `interval`, `slow_down` (+5 s) and `429 Retry-After`.
//!
//! Access tokens (`dpo_at_...`) last one hour. Refresh tokens (`dpo_rt_...`)
//! last 30 days and rotate: every refresh returns a new one and the old one
//! stops working; reusing an old one revokes the whole grant. Store only the
//! newest, in the operating system's secure storage.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::contract::{self, console_paths};
use crate::error::{ProviderError, Result};
use crate::secret::Secret;

/// RFC 8628 §3.5: `slow_down` increases the interval by 5 seconds.
const SLOW_DOWN_STEP_SECS: u64 = 5;
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Console OAuth settings.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Console base URL (default `https://dns.dnspilot.kr`).
    pub console_url: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    /// Length of one interval "second". Real clients use one second; tests
    /// shorten it.
    pub interval_unit: Duration,
    /// Deadline of each console request (30 s; tests shorten it).
    pub request_timeout: Duration,
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
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// A started device authorization: what the app shows the person.
#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    device_code: Secret,
    /// For example `BCDF-GHJK`.
    pub user_code: String,
    /// `https://dns.dnspilot.kr/oauth/activate`.
    pub verification_uri: String,
    /// The activation URL with the code pre-filled (the page still asks the
    /// person to confirm it matches the app's screen).
    pub verification_uri_complete: Option<String>,
    pub expires_in_secs: u64,
    pub expires_at: Instant,
    pub interval_secs: u64,
}

/// Tokens from the console. `Debug` output is redacted; `Serialize` writes
/// the real values (for the secure store only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub token_type: String,
    /// Seconds since the Unix epoch.
    pub expires_at: u64,
    /// Space-separated granted scopes.
    pub scope: String,
    /// Write-ahead marker: set (Unix seconds) and persisted just before
    /// `refresh_token` is sent to the token endpoint, cleared when the
    /// outcome is known. A stored set that still carries it may have been
    /// rotated already, so its refresh token is never sent again (reuse
    /// revokes the whole grant); the person connects DNSPilot again instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_in_flight: Option<u64>,
}

impl TokenSet {
    /// True when the access token expires within `margin`.
    pub fn expires_within(&self, margin: Duration) -> bool {
        now_secs() + margin.as_secs() >= self.expires_at
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scope.split_whitespace().any(|s| s == scope)
    }
}

/// OAuth-specific failures.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("the request was denied in DNSPilot")]
    AccessDenied,
    #[error("the approval code expired; start again")]
    Expired,
    #[error("the DNSPilot authorization is no longer valid; connect DNSPilot again")]
    InvalidGrant,
    #[error("OAuth is not enabled on this DNSPilot installation")]
    Disabled,
    #[error("DNSPilot OAuth error {code}: {description}")]
    Other { code: String, description: String },
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Outcome of a failed [`DeviceFlowClient::refresh_once`].
#[derive(Debug)]
pub enum RefreshError {
    /// No connection was made: the refresh token was not sent and is still
    /// valid.
    NotSent(OAuthError),
    /// The console answered without rotating (rate limit, OAuth disabled,
    /// another OAuth error): the refresh token is still valid.
    Rejected(OAuthError),
    /// The console refused the refresh token (`invalid_grant`): the grant is
    /// gone and the person must connect DNSPilot again.
    InvalidGrant,
    /// The request may have reached the console but the answer was lost
    /// (timeout, dropped connection, server error, unreadable success). The
    /// refresh token may have been rotated: never send it again.
    Ambiguous(OAuthError),
}

impl RefreshError {
    /// True when the refresh token is known to be unused and still valid.
    pub fn token_still_valid(&self) -> bool {
        matches!(self, Self::NotSent(_) | Self::Rejected(_))
    }

    pub fn into_oauth(self) -> OAuthError {
        match self {
            Self::NotSent(e) | Self::Rejected(e) | Self::Ambiguous(e) => e,
            Self::InvalidGrant => OAuthError::InvalidGrant,
        }
    }
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent(e) => write!(f, "not sent: {e}"),
            Self::Rejected(e) => write!(f, "rejected: {e}"),
            Self::InvalidGrant => f.write_str("invalid_grant"),
            Self::Ambiguous(e) => write!(f, "outcome unknown: {e}"),
        }
    }
}

impl From<reqwest::Error> for OAuthError {
    fn from(e: reqwest::Error) -> Self {
        Self::Provider(e.into())
    }
}

pub(crate) fn now_secs() -> u64 {
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
    RateLimited(Option<Duration>),
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

impl DeviceFlowClient {
    pub fn new(config: OAuthConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .user_agent(concat!("vca-providers/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http, config }
    }

    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.console_url.trim_end_matches('/'), path)
    }

    /// `POST /oauth/device_authorization`.
    pub async fn start(&self) -> Result<DeviceAuthorization, OAuthError> {
        let scope = self.config.scopes.join(" ");
        let resp = self
            .http
            .post(self.url(console_paths::OAUTH_DEVICE_AUTHORIZATION))
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", scope.as_str()),
            ])
            .send()
            .await?;
        let status = resp.status().as_u16();
        match status {
            200 => {}
            404 => return Err(OAuthError::Disabled),
            429 => {
                return Err(ProviderError::RateLimited {
                    retry_after: retry_after(&resp),
                }
                .into());
            }
            _ => {
                let err: contract::OAuthErrorResponse = resp.json().await.unwrap_or_default();
                return Err(oauth_error(err, status));
            }
        }
        let r: contract::DeviceAuthorizationResponse = resp.json().await?;
        Ok(DeviceAuthorization {
            device_code: Secret::new(r.device_code),
            user_code: r.user_code,
            verification_uri: r.verification_uri,
            verification_uri_complete: r.verification_uri_complete,
            expires_in_secs: r.expires_in,
            expires_at: Instant::now()
                + self.config.interval_unit * u32::try_from(r.expires_in).unwrap_or(u32::MAX),
            interval_secs: r.interval.max(1),
        })
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<TokenReply, OAuthError> {
        let resp = self
            .http
            .post(self.url(console_paths::OAUTH_TOKEN))
            .form(form)
            .send()
            .await?;
        let status = resp.status();
        if status.as_u16() == 429 {
            return Ok(TokenReply::RateLimited(retry_after(&resp)));
        }
        if status.as_u16() == 404 {
            return Err(OAuthError::Disabled);
        }
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

    /// Polls `POST /oauth/token` until the person approves, denies, or the
    /// code expires.
    pub async fn poll(&self, auth: &DeviceAuthorization) -> Result<TokenSet, OAuthError> {
        let mut interval = auth.interval_secs;
        let mut wait = self.config.interval_unit * u32::try_from(interval).unwrap_or(u32::MAX);
        loop {
            tokio::time::sleep(wait).await;
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
            wait = self.config.interval_unit * u32::try_from(interval).unwrap_or(u32::MAX);
            match reply {
                TokenReply::Tokens(t) => return Ok(to_token_set(t)),
                TokenReply::RateLimited(after) => {
                    let unit = self.config.interval_unit * u32::try_from(interval).unwrap_or(1);
                    wait = after
                        .map(|d| {
                            // `Retry-After` is in real seconds; scale it like
                            // the interval for tests.
                            self.config.interval_unit
                                * u32::try_from(d.as_secs()).unwrap_or(u32::MAX)
                        })
                        .unwrap_or(unit)
                        .max(unit);
                }
                TokenReply::Error(e) => match e.error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => {
                        interval += SLOW_DOWN_STEP_SECS;
                        wait =
                            self.config.interval_unit * u32::try_from(interval).unwrap_or(u32::MAX);
                    }
                    _ => return Err(oauth_error(e, 400)),
                },
            }
        }
    }

    /// Refreshes the access token. The returned set carries the new refresh
    /// token; the old one no longer works and must be discarded.
    ///
    /// Sends the request once and never retries. Prefer
    /// [`refresh_once`](Self::refresh_once), which says whether the refresh
    /// token may have been consumed.
    pub async fn refresh(&self, tokens: &TokenSet) -> Result<TokenSet, OAuthError> {
        self.refresh_once(tokens)
            .await
            .map_err(RefreshError::into_oauth)
    }

    /// One `POST /oauth/token` with `grant_type=refresh_token`, never
    /// retried. The error tells whether the console may have consumed (and
    /// rotated) the refresh token: after [`RefreshError::Ambiguous`] the old
    /// token must not be sent again, because reusing a rotated refresh token
    /// revokes the whole grant.
    pub async fn refresh_once(&self, tokens: &TokenSet) -> Result<TokenSet, RefreshError> {
        let sent = self
            .http
            .post(self.url(console_paths::OAUTH_TOKEN))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", tokens.refresh_token.expose()),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await;
        let resp = match sent {
            Ok(resp) => resp,
            // No connection was made, so the request never left this PC.
            Err(e) if e.is_connect() => return Err(RefreshError::NotSent(e.into())),
            // Timeout or a connection lost after the request was written:
            // the console may have rotated the token.
            Err(e) => return Err(RefreshError::Ambiguous(e.into())),
        };
        let status = resp.status().as_u16();
        if status == 429 {
            return Err(RefreshError::Rejected(
                ProviderError::RateLimited {
                    retry_after: retry_after(&resp),
                }
                .into(),
            ));
        }
        if status == 404 {
            return Err(RefreshError::Rejected(OAuthError::Disabled));
        }
        let body = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return Err(RefreshError::Ambiguous(e.into())),
        };
        if (200..300).contains(&status) {
            // A success whose body cannot be read is the worst case: the
            // console rotated, but the new token is lost.
            return serde_json::from_slice::<contract::TokenEndpointResponse>(&body)
                .map(to_token_set)
                .map_err(|e| RefreshError::Ambiguous(ProviderError::from(e).into()));
        }
        if status >= 500 {
            // The console may have failed after committing the rotation.
            return Err(RefreshError::Ambiguous(
                ProviderError::Api {
                    status,
                    message: "the DNSPilot console failed while renewing the sign-in".into(),
                }
                .into(),
            ));
        }
        let err: contract::OAuthErrorResponse = serde_json::from_slice(&body).unwrap_or_default();
        match oauth_error(err, status) {
            OAuthError::InvalidGrant => Err(RefreshError::InvalidGrant),
            other => Err(RefreshError::Rejected(other)),
        }
    }

    /// `POST /oauth/revoke` (RFC 7009). Revoking the refresh token ends the
    /// grant: tunnels VCA created are disabled (not deleted) and the app
    /// clients and tokens it registered are revoked.
    pub async fn revoke(&self, token: &Secret) -> Result<(), OAuthError> {
        let resp = self
            .http
            .post(self.url(console_paths::OAUTH_REVOKE))
            .form(&[
                ("token", token.expose()),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status().as_u16();
        let err: contract::OAuthErrorResponse = resp.json().await.unwrap_or_default();
        Err(oauth_error(err, status))
    }
}

fn to_token_set(t: contract::TokenEndpointResponse) -> TokenSet {
    TokenSet {
        access_token: Secret::new(t.access_token),
        refresh_token: Secret::new(t.refresh_token),
        token_type: t.token_type,
        expires_at: now_secs() + t.expires_in,
        scope: t.scope,
        refresh_in_flight: None,
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
    fn defaults_request_minimal_scopes() {
        let c = OAuthConfig::default();
        assert_eq!(c.client_id, "vca");
        assert_eq!(c.scopes, ["tunnels:read", "tunnels:write", "devices:read"]);
        assert_eq!(c.console_url, "https://dns.dnspilot.kr");
    }

    #[test]
    fn token_set_debug_is_redacted() {
        let t = to_token_set(contract::TokenEndpointResponse {
            access_token: "dpo_at_secret".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
            refresh_token: "dpo_rt_secret".into(),
            scope: "tunnels:read tunnels:write".into(),
        });
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        assert!(!t.expires_within(Duration::from_secs(60)));
        assert!(t.expires_within(Duration::from_secs(7200)));
        assert!(t.has_scope("tunnels:write"));
        assert!(!t.has_scope("devices:read"));
    }
}
