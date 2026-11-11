//! Client for the DNSPilot Agent local API
//! (<https://dns.dnspilot.kr/docs/en/agent-local-api>).
//!
//! Transport: plain HTTP/1.1 over the Unix socket `/run/dnspilot/local.sock`
//! (Linux, macOS; root, the service account and members of the `dnspilot`
//! group) or the named pipe `\\.\pipe\dnspilot-local` (Windows). The API is
//! never reachable from the network.
//!
//! On Windows the client checks after connecting that the pipe server is
//! the `DNSPilotAgent` service (as the docs require) and refuses any other
//! server.
//!
//! Authentication: every request except `GET /v1/status` sends the per-user
//! local token `Authorization: Bearer dpl_...`. On Linux and macOS it comes
//! from `GET /v1/token`; on Windows from
//! `%ProgramData%\DNSPilot\local-api\<SID>.token`. On a 401 the token is
//! fetched again once. The token is a per-user API token, not the device's
//! credentials (which VCA never reads).

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use super::contract::{self, paths};
use super::sse::{SseEvent, SseParser};
use crate::error::{ProviderError, Result};
use crate::secret::Secret;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Where the Agent's local API listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEndpoint {
    UnixSocket(PathBuf),
    NamedPipe(String),
}

impl Default for LocalEndpoint {
    fn default() -> Self {
        if cfg!(windows) {
            Self::NamedPipe(contract::WINDOWS_PIPE_NAME.into())
        } else {
            Self::UnixSocket(contract::UNIX_SOCKET_PATH.into())
        }
    }
}

impl LocalEndpoint {
    /// Parses a configured location: a `\\.\pipe\...` name or a socket path.
    pub fn parse(s: &str) -> Self {
        if s.starts_with(r"\\.\pipe\") {
            Self::NamedPipe(s.to_owned())
        } else {
            Self::UnixSocket(PathBuf::from(s))
        }
    }
}

/// Where the per-user local token comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// `GET /v1/token` (Linux, macOS).
    Endpoint,
    /// A token file (Windows: `%ProgramData%\DNSPilot\local-api\<SID>.token`).
    File(PathBuf),
}

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// Agent local API client.
pub struct LocalApiClient {
    endpoint: LocalEndpoint,
    token_source: Option<TokenSource>,
    token: Mutex<Option<Secret>>,
    timeout: Duration,
    verify_pipe_server: bool,
}

impl std::fmt::Debug for LocalApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalApiClient")
            .field("endpoint", &self.endpoint)
            .field("token_source", &self.token_source)
            .finish_non_exhaustive()
    }
}

impl Default for LocalApiClient {
    fn default() -> Self {
        Self::new(LocalEndpoint::default())
    }
}

impl LocalApiClient {
    pub fn new(endpoint: LocalEndpoint) -> Self {
        Self {
            endpoint,
            token_source: None,
            token: Mutex::new(None),
            timeout: DEFAULT_TIMEOUT,
            verify_pipe_server: true,
        }
    }

    /// Overrides where the local token comes from (default: the documented
    /// source of the platform).
    pub fn with_token_source(mut self, source: TokenSource) -> Self {
        self.token_source = Some(source);
        self
    }

    /// Skips the check that the named pipe belongs to the `DNSPilotAgent`
    /// service. **Tests only**: a fake Agent is not that service.
    pub fn insecure_skip_pipe_server_check_for_tests(mut self) -> Self {
        self.verify_pipe_server = false;
        self
    }

    pub fn endpoint(&self) -> &LocalEndpoint {
        &self.endpoint
    }

    async fn connect(&self) -> Result<Box<dyn Io>> {
        match &self.endpoint {
            #[cfg(unix)]
            LocalEndpoint::UnixSocket(path) => {
                let s = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(map_connect_err)?;
                Ok(Box::new(s))
            }
            #[cfg(windows)]
            LocalEndpoint::NamedPipe(name) => {
                use tokio::net::windows::named_pipe::ClientOptions;
                const ERROR_PIPE_BUSY: i32 = 231;
                let mut attempts = 0;
                let client = loop {
                    match ClientOptions::new().open(name) {
                        Ok(c) => break c,
                        Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 20 => {
                            attempts += 1;
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        Err(e) => return Err(map_connect_err(e)),
                    }
                };
                if self.verify_pipe_server {
                    let server = super::win::pipe_server_pid(&client)?;
                    match super::win::agent_service_pid() {
                        Some(agent) if agent == server => {}
                        _ => {
                            return Err(ProviderError::Unauthorized(format!(
                                "the local API pipe is not served by the {} service; refusing it",
                                contract::WINDOWS_SERVICE_NAME
                            )));
                        }
                    }
                }
                Ok(Box::new(client))
            }
            #[allow(unreachable_patterns)]
            _ => Err(ProviderError::UnsupportedPlatform(
                "this local API transport is not available on this platform",
            )),
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        token: Option<&Secret>,
        resume: Option<(u64, &str)>,
    ) -> Result<Response<Incoming>> {
        let io = self.connect().await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(http_err)?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("DNSPilot local connection ended: {e}");
            }
        });
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, "localhost");
        if let Some(t) = token {
            req = req.header(AUTHORIZATION, format!("Bearer {}", t.expose()));
        }
        if let Some((id, boot)) = resume {
            req = req
                .header("Last-Event-ID", id.to_string())
                .header("Last-Boot-ID", boot);
        }
        let body = match body {
            Some(b) => {
                req = req.header(CONTENT_TYPE, "application/json");
                Full::new(Bytes::from(b))
            }
            None => Full::new(Bytes::new()),
        };
        let req = req
            .body(body)
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        tokio::time::timeout(self.timeout, sender.send_request(req))
            .await
            .map_err(|_| ProviderError::Timeout("the DNSPilot Agent did not answer".into()))?
            .map_err(http_err)
    }

    fn token_source(&self) -> Result<TokenSource> {
        if let Some(s) = &self.token_source {
            return Ok(s.clone());
        }
        #[cfg(windows)]
        {
            Ok(TokenSource::File(super::win::token_file()?))
        }
        #[cfg(not(windows))]
        {
            Ok(TokenSource::Endpoint)
        }
    }

    /// Obtains the per-user local token again.
    pub async fn fetch_token(&self) -> Result<Secret> {
        let token = match self.token_source()? {
            TokenSource::Endpoint => {
                let resp = self
                    .send(Method::GET, paths::TOKEN, None, None, None)
                    .await?;
                let r: contract::LocalToken = read_json(resp).await?;
                r.token
            }
            TokenSource::File(path) => match tokio::fs::read_to_string(&path).await {
                Ok(t) => t,
                // The Agent creates the token on the first request.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let _ = self.status().await;
                    tokio::fs::read_to_string(&path).await.map_err(|e| {
                        ProviderError::NotSignedIn(format!(
                            "no DNSPilot local API token for this user yet: {e}"
                        ))
                    })?
                }
                Err(e) => return Err(ProviderError::Io(e)),
            },
        };
        let token = token.trim().to_owned();
        if token.is_empty() {
            return Err(ProviderError::Decode("empty local API token".into()));
        }
        let token = Secret::new(token);
        *self.token.lock().await = Some(token.clone());
        Ok(token)
    }

    async fn current_token(&self) -> Result<Secret> {
        if let Some(t) = self.token.lock().await.clone() {
            return Ok(t);
        }
        self.fetch_token().await
    }

    /// Sends an authenticated request, fetching the token again once on 401.
    async fn authed(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        resume: Option<(u64, &str)>,
    ) -> Result<Response<Incoming>> {
        let token = self.current_token().await?;
        let resp = self
            .send(method.clone(), path, body.clone(), Some(&token), resume)
            .await?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        let token = self.fetch_token().await?;
        self.send(method, path, body, Some(&token), resume).await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let body = body.map(serde_json::to_vec).transpose()?;
        read_json(self.authed(method, path, body, None).await?).await
    }

    async fn call_empty(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<()> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let resp = self.authed(method, path, body, None).await?;
        check(resp).await.map(|_| ())
    }

    /// `GET /v1/status` (no token). Use it to detect the Agent.
    pub async fn status(&self) -> Result<contract::AgentStatus> {
        let resp = self
            .send(Method::GET, paths::STATUS, None, None, None)
            .await?;
        read_json(resp).await
    }

    /// `POST /v1/activation`: starts device sign-in; the person approves in
    /// the browser. 409 when the Agent is already signed in.
    pub async fn start_activation(&self) -> Result<contract::Activation> {
        self.call(
            Method::POST,
            paths::ACTIVATION,
            Some(&serde_json::json!({})),
        )
        .await
    }

    /// `GET /v1/tunnels`: tunnels served by this PC, with their state.
    pub async fn list_tunnels(&self) -> Result<Vec<contract::LocalTunnel>> {
        let list: contract::LocalTunnelList =
            self.call(Method::GET, paths::TUNNELS, None::<&()>).await?;
        Ok(list.tunnels)
    }

    /// `PATCH /v1/tunnels/{id}` `{enabled}`: share or stop sharing a tunnel
    /// from this PC.
    pub async fn set_tunnel_enabled(
        &self,
        id: i64,
        enabled: bool,
    ) -> Result<contract::LocalPatchResponse> {
        self.call(
            Method::PATCH,
            &paths::tunnel(id),
            Some(&contract::PatchTunnel { enabled }),
        )
        .await
    }

    /// `POST /v1/tunnels/{id}/app-clients`: registers an app client key.
    pub async fn register_app_client(
        &self,
        tunnel_id: i64,
        req: &contract::RegisterAppClient,
    ) -> Result<contract::LocalAppClient> {
        self.call(Method::POST, &paths::app_clients(tunnel_id), Some(req))
            .await
    }

    /// `DELETE /v1/tunnels/{id}/app-clients/{cid}`.
    pub async fn remove_app_client(&self, tunnel_id: i64, client_id: i64) -> Result<()> {
        self.call_empty(
            Method::DELETE,
            &paths::app_client(tunnel_id, client_id),
            None::<&()>,
        )
        .await
    }

    /// `POST /v1/kill`: closes every tunnel and P2P session until resumed.
    /// VCA never calls this on its own; it exists for an explicit user
    /// action.
    pub async fn kill(&self) -> Result<()> {
        self.call_empty(Method::POST, paths::KILL, Some(&serde_json::json!({})))
            .await
    }

    /// `POST /v1/resume`: lifts the kill switch (only its owner or an
    /// administrator may). VCA never calls this on its own.
    pub async fn resume(&self) -> Result<()> {
        self.call_empty(Method::POST, paths::RESUME, Some(&serde_json::json!({})))
            .await
    }

    /// `GET /v1/events` (Server-Sent Events). `resume` is the last event id
    /// seen and its boot id (`Last-Event-ID` / `Last-Boot-ID`). The first
    /// event is the control event `hello` or `reset`.
    pub async fn events(&self, resume: Option<(u64, &str)>) -> Result<EventStream> {
        let resp = self
            .authed(Method::GET, paths::EVENTS, None, resume)
            .await?;
        let resp = check(resp).await?;
        Ok(EventStream {
            body: resp.into_body(),
            parser: SseParser::new(),
            pending: Default::default(),
        })
    }
}

/// A live SSE stream from the Agent.
pub struct EventStream {
    body: Incoming,
    parser: SseParser,
    pending: std::collections::VecDeque<SseEvent>,
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventStream { .. }")
    }
}

impl EventStream {
    /// Next event; `None` when the Agent closed the stream (reconnect with
    /// the tracker's resume point).
    pub async fn next(&mut self) -> Option<Result<SseEvent>> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Some(Ok(ev));
            }
            match self.body.frame().await? {
                Ok(frame) => {
                    if let Ok(data) = frame.into_data() {
                        self.pending.extend(self.parser.feed(&data));
                    }
                }
                Err(e) => return Some(Err(http_err(e))),
            }
        }
    }
}

fn map_connect_err(e: std::io::Error) -> ProviderError {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            ProviderError::NotInstalled("the DNSPilot Agent (local API)")
        }
        std::io::ErrorKind::PermissionDenied => ProviderError::Unauthorized(
            "no access to the DNSPilot Agent local API (on Linux, join the dnspilot group)".into(),
        ),
        _ => ProviderError::Io(e),
    }
}

fn http_err(e: hyper::Error) -> ProviderError {
    ProviderError::Io(std::io::Error::other(e.to_string()))
}

async fn check(resp: Response<Incoming>) -> Result<Response<Incoming>> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = resp
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let (code, message) = agent_error_message(status, &body);
    if code.as_deref() == Some("not_logged_in") {
        return Err(ProviderError::NotSignedIn(message));
    }
    Err(match status.as_u16() {
        401 => ProviderError::Unauthorized(message),
        403 => ProviderError::Forbidden(message),
        404 => ProviderError::NotFound(message),
        409 => ProviderError::Conflict(message),
        429 => ProviderError::RateLimited { retry_after },
        s => ProviderError::Api { status: s, message },
    })
}

/// Reads `{"error": "<code>", "detail": "<text>"}` (console errors are
/// passed through by the Agent) into a clear message.
fn agent_error_message(status: StatusCode, body: &[u8]) -> (Option<String>, String) {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_control())
            .take(300)
            .collect::<String>()
            .trim()
            .to_owned()
    };
    let parsed = serde_json::from_slice::<contract::AgentErrorResponse>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|e| e.error.clone())
        .filter(|c| !c.is_empty());
    let detail = parsed
        .as_ref()
        .and_then(|e| e.detail.as_ref())
        .map(|d| match d {
            serde_json::Value::String(s) => clean(s),
            other => clean(&other.to_string()),
        })
        .filter(|d| !d.is_empty());
    let message = match (&code, &detail) {
        (Some(c), d) => {
            let text = contract::agent_error_text(c)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("DNSPilot Agent error {c}"));
            match d {
                Some(d) => format!("{text} ({c}): {d}"),
                None => format!("{text} ({c})"),
            }
        }
        (None, Some(d)) => d.clone(),
        (None, None) => {
            let raw = clean(&String::from_utf8_lossy(body));
            if raw.is_empty() {
                status.canonical_reason().unwrap_or("error").to_owned()
            } else {
                raw
            }
        }
    };
    (code, message)
}

async fn read_json<T: DeserializeOwned>(resp: Response<Incoming>) -> Result<T> {
    let resp = check(resp).await?;
    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(http_err)?
        .to_bytes();
    if body.is_empty() {
        return Ok(serde_json::from_slice(b"{}")?);
    }
    Ok(serde_json::from_slice(&body)?)
}
