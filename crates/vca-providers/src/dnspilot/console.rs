//! DNSPilot console API for apps, with an OAuth access token
//! (<https://dns.dnspilot.kr/docs/en/tunnel-app-mode>, "API for apps", and
//! the public OpenAPI spec).
//!
//! Rules DNSPilot enforces for OAuth apps:
//!
//! - an app can create `app`-mode tunnels only, and only on the Agent
//!   devices the person picked when approving it;
//! - an app can change only the tunnels it created (others are read-only);
//! - when the person disconnects the app, the tunnels it created are
//!   disabled (not deleted) and its app clients and tokens are revoked.
//!
//! Tunnel and device objects follow the production shapes supplied by the
//! DNSPilot team (see [`super::contract`]).

use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::contract::{self, console_paths, invite_errors};
use super::invite::redact_invite_codes;
use super::keeper::{MemoryTokenStore, TokenKeeper};
use super::oauth::{DeviceFlowClient, OAuthError, TokenSet};
use crate::error::{ProviderError, Result};
use crate::secret::Secret;

/// Called with every new token set (refresh tokens rotate: persist the
/// newest one, replacing the previous one).
pub type TokenSaver = Arc<dyn Fn(&TokenSet) + Send + Sync>;

/// Deadline of the public enrollment call.
const ENROLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Console client acting for the person who approved VCA. Every client of
/// one grant must share the grant's [`TokenKeeper`]: it is the only place
/// that refreshes, and refresh tokens must never be sent twice.
pub struct ConsoleClient {
    oauth: DeviceFlowClient,
    keeper: Arc<TokenKeeper>,
}

impl std::fmt::Debug for ConsoleClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleClient")
            .field("console_url", &self.oauth.config().console_url)
            .finish_non_exhaustive()
    }
}

fn oauth_to_provider(e: OAuthError) -> ProviderError {
    match e {
        OAuthError::Provider(p) => p,
        other => ProviderError::Unauthorized(other.to_string()),
    }
}

impl ConsoleClient {
    /// A client with its own in-memory tokens. Only for a single client of
    /// a grant (tests, one-off tools); apps share one keeper per grant
    /// through [`with_keeper`](Self::with_keeper).
    pub fn new(oauth: DeviceFlowClient, tokens: TokenSet) -> Self {
        Self::in_memory(oauth, tokens, None)
    }

    fn in_memory(oauth: DeviceFlowClient, tokens: TokenSet, saver: Option<TokenSaver>) -> Self {
        let store = Arc::new(MemoryTokenStore::new(Some(tokens), saver));
        let keeper = TokenKeeper::new(oauth.clone(), store);
        Self {
            keeper: Arc::new(keeper),
            oauth,
        }
    }

    /// A client that uses (and refreshes through) the grant's shared keeper.
    pub fn with_keeper(keeper: Arc<TokenKeeper>) -> Self {
        Self {
            oauth: keeper.oauth().clone(),
            keeper,
        }
    }

    /// Persists rotated tokens through `saver` (for a client made with
    /// [`new`](Self::new), before its first call).
    pub fn with_token_saver(self, saver: TokenSaver) -> Self {
        let current = self.keeper.stored_tokens();
        match current {
            Some(tokens) => Self::in_memory(self.oauth, tokens, Some(saver)),
            None => self,
        }
    }

    /// The current token set (`None` once it was deleted).
    pub async fn tokens(&self) -> Option<TokenSet> {
        self.keeper.tokens().await.ok().flatten()
    }

    pub(super) async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response> {
        let url = self.oauth.url(path);
        let mut rejected: Option<Secret> = None;
        loop {
            let token = self.keeper.access_token(rejected.as_ref()).await?;
            let mut req = self
                .oauth
                .http()
                .request(method.clone(), &url)
                .bearer_auth(token.expose())
                .header(reqwest::header::ACCEPT_LANGUAGE, "en");
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            if resp.status().as_u16() == 401 && rejected.is_none() {
                // The access token may have been revoked or expired early:
                // refresh once, unless another caller already did.
                rejected = Some(token);
                continue;
            }
            return check(resp).await;
        }
    }

    pub(super) async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let body = body.map(serde_json::to_value).transpose()?;
        let resp = self.send(method, path, body.as_ref()).await?;
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            return Ok(serde_json::from_slice(b"{}")?);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// `GET /api/auth/me`: the person and organization the token acts for.
    pub async fn me(&self) -> Result<serde_json::Value> {
        self.call(Method::GET, console_paths::AUTH_ME, None::<&()>)
            .await
    }

    /// `GET /api/agent-devices` (`devices:read`).
    pub async fn agent_devices(&self) -> Result<Vec<contract::AgentDevice>> {
        let list: contract::AgentDeviceList = self
            .call(Method::GET, console_paths::AGENT_DEVICES, None::<&()>)
            .await?;
        Ok(list.items)
    }

    /// `GET /api/tunnels` (`tunnels:read`).
    pub async fn tunnels(&self) -> Result<contract::TunnelList> {
        self.call(Method::GET, console_paths::TUNNELS, None::<&()>)
            .await
    }

    /// `GET /api/tunnels/{id}` (`tunnels:read`).
    pub async fn tunnel(&self, id: i64) -> Result<contract::Tunnel> {
        self.call(Method::GET, &console_paths::tunnel(id), None::<&()>)
            .await
    }

    /// `GET /api/tunnels/zones` (`tunnels:read`): the zones tunnels can be
    /// created in. `None` while the console does not offer it yet (404).
    pub async fn zones(&self) -> Result<Option<Vec<contract::Zone>>> {
        match self
            .call::<contract::ZoneList>(Method::GET, console_paths::TUNNEL_ZONES, None::<&()>)
            .await
        {
            Ok(z) => Ok(Some(z.into_vec())),
            Err(ProviderError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `POST /api/tunnels` with `access_mode: "app"` (`tunnels:write`,
    /// paid plan); returns the new tunnel. Opening a tunnel sends the
    /// organization a "tunnel opened" email with a kill link. 403 when the
    /// device was not picked when the app was approved.
    pub async fn create_app_tunnel(
        &self,
        req: &contract::CreateTunnel,
    ) -> Result<contract::Tunnel> {
        if req.access_mode != contract::ACCESS_MODE_APP {
            return Err(ProviderError::InvalidConfig(
                "OAuth apps can only create app-mode tunnels".into(),
            ));
        }
        self.call(Method::POST, console_paths::TUNNELS, Some(req))
            .await
    }

    /// `GET /api/tunnels/{id}/app-clients` (`tunnels:read`).
    pub async fn app_clients(&self, tunnel_id: i64) -> Result<Vec<contract::AppClient>> {
        let list: contract::AppClientList = self
            .call(
                Method::GET,
                &console_paths::app_clients(tunnel_id),
                None::<&()>,
            )
            .await?;
        Ok(list.items)
    }

    /// `POST /api/tunnels/{id}/app-clients` (`tunnels:write`; only tunnels
    /// this grant created, otherwise 403).
    pub async fn register_app_client(
        &self,
        tunnel_id: i64,
        req: &contract::RegisterAppClient,
    ) -> Result<contract::AppClient> {
        self.call(
            Method::POST,
            &console_paths::app_clients(tunnel_id),
            Some(req),
        )
        .await
    }

    /// `DELETE /api/tunnels/{id}/app-clients/{cid}`: the Agent drops the
    /// client's open connections within about a minute.
    pub async fn revoke_app_client(&self, tunnel_id: i64, client_id: i64) -> Result<()> {
        self.send(
            Method::DELETE,
            &console_paths::app_client(tunnel_id, client_id),
            None,
        )
        .await
        .map(|_| ())
    }

    /// `POST /api/tunnels/{id}/app-invites` (`tunnels:write`): a single-use
    /// code for another device. App-mode tunnels only; for OAuth apps, only
    /// tunnels this grant created on a device picked at approval. At most
    /// 10 active invites per tunnel. The result holds the only copy of the
    /// code.
    pub async fn create_app_invite(
        &self,
        tunnel_id: i64,
        req: &contract::CreateAppInvite,
    ) -> Result<contract::AppInviteCreated> {
        let body = serde_json::to_value(req)?;
        let resp = self
            .send(
                Method::POST,
                &console_paths::app_invites(tunnel_id),
                Some(&body),
            )
            .await?;
        decode_quietly(&resp.bytes().await?)
    }

    /// `GET /api/tunnels/{id}/app-invites` (`tunnels:read`): codes are never
    /// listed.
    pub async fn app_invites(&self, tunnel_id: i64) -> Result<contract::AppInviteList> {
        self.call(
            Method::GET,
            &console_paths::app_invites(tunnel_id),
            None::<&()>,
        )
        .await
    }

    /// `DELETE /api/tunnels/{id}/app-invites/{invite_id}` (`tunnels:write`):
    /// an unused code stops working immediately.
    pub async fn revoke_app_invite(&self, tunnel_id: i64, invite_id: i64) -> Result<()> {
        self.send(
            Method::DELETE,
            &console_paths::app_invite(tunnel_id, invite_id),
            None,
        )
        .await
        .map(|_| ())
    }

    /// `PUT /api/tunnels/{id}/app-token-allowed`; returns `token_allowed`.
    pub async fn set_app_token_allowed(&self, tunnel_id: i64, allowed: bool) -> Result<bool> {
        let r: contract::AppTokenAllowedResponse = self
            .call(
                Method::PUT,
                &console_paths::app_token_allowed(tunnel_id),
                Some(&contract::AppTokenAllowed { allowed }),
            )
            .await?;
        Ok(r.token_allowed)
    }

    /// `GET /api/tunnels/{id}/app-tokens`.
    pub async fn app_tokens(&self, tunnel_id: i64) -> Result<Vec<contract::AppToken>> {
        let list: contract::AppTokenList = self
            .call(
                Method::GET,
                &console_paths::app_tokens(tunnel_id),
                None::<&()>,
            )
            .await?;
        Ok(list.items)
    }

    /// `POST /api/tunnels/{id}/app-tokens`: the `dpt_app_...` token is in
    /// the response once.
    pub async fn issue_app_token(
        &self,
        tunnel_id: i64,
        req: &contract::IssueAppToken,
    ) -> Result<contract::AppToken> {
        self.call(
            Method::POST,
            &console_paths::app_tokens(tunnel_id),
            Some(req),
        )
        .await
    }

    /// `DELETE /api/tunnels/{id}/app-tokens/{tid}`.
    pub async fn revoke_app_token(&self, tunnel_id: i64, token_id: i64) -> Result<()> {
        self.send(
            Method::DELETE,
            &console_paths::app_token(tunnel_id, token_id),
            None,
        )
        .await
        .map(|_| ())
    }

    /// `POST /api/tunnels/{id}/enable` (tunnels this grant created).
    pub async fn enable_tunnel(&self, tunnel_id: i64) -> Result<()> {
        self.send(Method::POST, &console_paths::enable(tunnel_id), None)
            .await
            .map(|_| ())
    }

    /// `POST /api/tunnels/{id}/disable` (tunnels this grant created).
    pub async fn disable_tunnel(&self, tunnel_id: i64) -> Result<()> {
        self.send(Method::POST, &console_paths::disable(tunnel_id), None)
            .await
            .map(|_| ())
    }

    /// `DELETE /api/tunnels/{id}` (tunnels this grant created).
    pub async fn delete_tunnel(&self, tunnel_id: i64) -> Result<()> {
        self.send(Method::DELETE, &console_paths::tunnel(tunnel_id), None)
            .await
            .map(|_| ())
    }

    /// Disconnects VCA from the DNSPilot account (`POST /oauth/revoke` with
    /// the refresh token).
    pub async fn disconnect(&self) -> Result<()> {
        let refresh = self
            .keeper
            .tokens()
            .await?
            .ok_or_else(|| ProviderError::NotSignedIn("connect DNSPilot first".into()))?
            .refresh_token;
        self.oauth.revoke(&refresh).await.map_err(oauth_to_provider)
    }
}

/// `GET /api/public/status` (no authentication; cached for 60 seconds, so
/// do not poll it more often).
pub async fn public_status(
    http: &reqwest::Client,
    console_url: &str,
) -> Result<contract::PublicStatus> {
    let url = format!(
        "{}{}",
        console_url.trim_end_matches('/'),
        console_paths::PUBLIC_STATUS
    );
    let resp = check(http.get(url).send().await?).await?;
    Ok(resp.json().await?)
}

/// `POST /api/public/tunnel-app-enroll`: registers the certificate of
/// `req` on the tunnel the code belongs to. No authentication. Single use,
/// except that a failed attempt does not consume the code. Rate limit: 20
/// requests per 10 minutes per IP.
///
/// `console_url` always comes from the app's configuration, never from a
/// link, and redirects are not followed, so a code only ever goes to the
/// console.
pub async fn public_enroll(
    console_url: &str,
    req: &contract::AppEnrollRequest,
) -> Result<contract::AppEnrollResult> {
    let base = console_url.trim_end_matches('/');
    if !is_console_origin(base) {
        return Err(ProviderError::InvalidConfig(
            "invite codes are only sent to an https DNSPilot console".into(),
        ));
    }
    let http = reqwest::Client::builder()
        .timeout(ENROLL_TIMEOUT)
        .user_agent(concat!("vca-providers/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| ProviderError::InvalidConfig(format!("cannot build an HTTP client: {e}")))?;
    let resp = http
        .post(format!("{base}{}", console_paths::PUBLIC_ENROLL))
        .header(reqwest::header::ACCEPT_LANGUAGE, "en")
        .json(req)
        .send()
        .await?;
    let resp = check(resp).await?;
    decode_quietly(&resp.bytes().await?)
}

/// `https://`, or a loopback address (only the tests' mock console).
fn is_console_origin(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:")
}

/// Decodes a response that may hold a secret: decoder errors can quote the
/// offending value, so the error says nothing about the body.
fn decode_quietly<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|_| ProviderError::Decode("unexpected answer from DNSPilot".into()))
}

/// The readable parts of an error body.
#[derive(Debug, Default, PartialEq, Eq)]
struct Failure {
    code: Option<String>,
    message: Option<String>,
}

fn string_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Understands `{"detail": "..."}`, the invite endpoints'
/// `{"detail": {"message", "code"}}`, and fields at the top level
/// (`{"code", "message"}`, `{"error", "error_description"}`).
fn parse_failure(body: &[u8]) -> Failure {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Failure::default();
    };
    let detail = value.get("detail");
    let code = detail
        .and_then(|d| string_field(d, "code"))
        .or_else(|| string_field(&value, "code"))
        .or_else(|| string_field(&value, "error"));
    let message = match detail {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(d @ serde_json::Value::Object(_)) => string_field(d, "message"),
        Some(serde_json::Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
    .or_else(|| string_field(&value, "message"))
    .or_else(|| string_field(&value, "error_description"));
    Failure { code, message }
}

/// The text for an error: without control characters or invite codes
/// (a server may echo the request), and bounded.
fn clean_message(text: &str) -> String {
    redact_invite_codes(text)
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect()
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = resp.bytes().await.unwrap_or_default();
    let failure = parse_failure(&body);
    let mut message = clean_message(
        failure
            .message
            .as_deref()
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error")),
    );
    let code = failure.code.map(|c| clean_message(&c));
    if status.as_u16() == 429 {
        return Err(ProviderError::RateLimited { retry_after });
    }
    if let Some(code) = code {
        if invite_errors::ALL.contains(&code.as_str()) {
            return Err(ProviderError::Coded {
                status: status.as_u16(),
                code,
                message,
            });
        }
        // Other codes (OAuth limits such as "device not allowed") keep the
        // HTTP mapping; the code stays readable in the text.
        if !message.contains(&code) {
            message = format!("{code}: {message}");
        }
    }
    Err(match status.as_u16() {
        401 => ProviderError::Unauthorized(message),
        403 => ProviderError::Forbidden(message),
        404 => ProviderError::NotFound(message),
        409 => ProviderError::Conflict(message),
        s => ProviderError::Api { status: s, message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_bodies_are_understood_in_every_documented_shape() {
        let f = parse_failure(br#"{"detail": "Device not allowed for this app."}"#);
        assert_eq!(f.code, None);
        assert_eq!(
            f.message.as_deref(),
            Some("Device not allowed for this app.")
        );
        let f = parse_failure(
            br#"{"detail": {"message": "Unknown code", "code": "invite_invalid", "message_src": "x"}}"#,
        );
        assert_eq!(f.code.as_deref(), Some("invite_invalid"));
        assert_eq!(f.message.as_deref(), Some("Unknown code"));
        let f = parse_failure(br#"{"code": "device_not_allowed", "message": "No"}"#);
        assert_eq!(f.code.as_deref(), Some("device_not_allowed"));
        assert_eq!(f.message.as_deref(), Some("No"));
        let f = parse_failure(br#"{"error": "access_denied", "error_description": "No"}"#);
        assert_eq!(f.code.as_deref(), Some("access_denied"));
        assert_eq!(f.message.as_deref(), Some("No"));
        assert_eq!(parse_failure(b"<html>"), Failure::default());
    }

    #[test]
    fn messages_lose_control_characters_and_codes() {
        let code = format!("dpt_inv_{}", "a".repeat(43));
        let clean = clean_message(&format!("bad\n{code}"));
        assert_eq!(clean, "baddpt_inv_<redacted>");
        assert_eq!(clean_message(&"x".repeat(500)).len(), 300);
    }

    #[test]
    fn enrollment_only_goes_to_a_console_origin() {
        assert!(is_console_origin("https://dns.dnspilot.kr"));
        assert!(is_console_origin("http://127.0.0.1:1234"));
        assert!(!is_console_origin("http://dns.dnspilot.kr"));
        assert!(!is_console_origin("ftp://dns.dnspilot.kr"));
    }
}
