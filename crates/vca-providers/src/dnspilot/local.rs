//! Client for the DNS Pilot Agent's per-user local REST API.
//!
//! Transport: HTTP/1.1 over the Unix socket `/run/dnspilot/local.sock`
//! (Linux, macOS) or the named pipe `\\.\pipe\dnspilot-local` (Windows).
//! `POST /v1/token` returns a per-user bearer token (on Unix the Agent
//! identifies the caller by socket peer credentials); every other call
//! sends it as `Authorization: Bearer`. On a 401 the token is fetched again
//! once.
//!
//! The Windows named-pipe transport is unverified on Windows (it compiles,
//! but could not be run here).

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

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// Agent local API client.
pub struct LocalApiClient {
    endpoint: LocalEndpoint,
    token: Mutex<Option<Secret>>,
    timeout: Duration,
}

impl std::fmt::Debug for LocalApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalApiClient")
            .field("endpoint", &self.endpoint)
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
            token: Mutex::new(None),
            timeout: DEFAULT_TIMEOUT,
        }
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
                loop {
                    match ClientOptions::new().open(name) {
                        Ok(c) => return Ok(Box::new(c)),
                        Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 20 => {
                            attempts += 1;
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        Err(e) => return Err(map_connect_err(e)),
                    }
                }
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
        last_event_id: Option<&str>,
    ) -> Result<Response<Incoming>> {
        let io = self.connect().await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(http_err)?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("dnspilot local connection ended: {e}");
            }
        });
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, "localhost");
        if let Some(t) = token {
            req = req.header(AUTHORIZATION, format!("Bearer {}", t.expose()));
        }
        if let Some(id) = last_event_id {
            req = req.header("Last-Event-ID", id);
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
            .map_err(|_| ProviderError::Timeout("DNS Pilot Agent did not answer".into()))?
            .map_err(http_err)
    }

    /// Obtains a fresh per-user token (`POST /v1/token`).
    pub async fn fetch_token(&self) -> Result<Secret> {
        let resp = self
            .send(Method::POST, paths::TOKEN, None, None, None)
            .await?;
        let r: contract::TokenResponse = read_json(resp).await?;
        if r.token.is_empty() {
            return Err(ProviderError::Decode("empty token".into()));
        }
        let token = Secret::new(r.token);
        *self.token.lock().await = Some(token.clone());
        Ok(token)
    }

    async fn current_token(&self) -> Result<Secret> {
        if let Some(t) = self.token.lock().await.clone() {
            return Ok(t);
        }
        self.fetch_token().await
    }

    /// Sends an authenticated request, refreshing the token once on 401.
    async fn authed(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Response<Incoming>> {
        let token = self.current_token().await?;
        let resp = self
            .send(method.clone(), path, body.clone(), Some(&token), None)
            .await?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        let token = self.fetch_token().await?;
        self.send(method, path, body, Some(&token), None).await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let body = body.map(serde_json::to_vec).transpose()?;
        read_json(self.authed(method, path, body).await?).await
    }

    async fn call_empty(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<()> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let resp = self.authed(method, path, body).await?;
        check(resp).await.map(|_| ())
    }

    pub async fn status(&self) -> Result<contract::AgentStatus> {
        self.call(Method::GET, paths::STATUS, None::<&()>).await
    }

    pub async fn activation(&self) -> Result<contract::Activation> {
        self.call(Method::GET, paths::ACTIVATION, None::<&()>).await
    }

    /// Starts device activation; the user approves in the browser at the
    /// returned `verification_url` (unattended activation is impossible by design).
    pub async fn start_activation(&self) -> Result<contract::Activation> {
        self.call(
            Method::POST,
            paths::ACTIVATION,
            Some(&serde_json::json!({})),
        )
        .await
    }

    pub async fn list_tunnels(&self) -> Result<Vec<contract::Tunnel>> {
        let l: contract::TunnelList = self.call(Method::GET, paths::TUNNELS, None::<&()>).await?;
        Ok(l.tunnels)
    }

    pub async fn create_tunnel(&self, req: &contract::CreateTunnel) -> Result<contract::Tunnel> {
        self.call(Method::POST, paths::TUNNELS, Some(req)).await
    }

    pub async fn patch_tunnel(
        &self,
        id: &str,
        req: &contract::PatchTunnel,
    ) -> Result<contract::Tunnel> {
        self.call(Method::PATCH, &paths::tunnel(id), Some(req))
            .await
    }

    pub async fn register_app_client(
        &self,
        tunnel_id: &str,
        req: &contract::RegisterAppClient,
    ) -> Result<contract::AppClient> {
        self.call(Method::POST, &paths::app_clients(tunnel_id), Some(req))
            .await
    }

    pub async fn remove_app_client(&self, tunnel_id: &str, client_id: &str) -> Result<()> {
        self.call_empty(
            Method::DELETE,
            &paths::app_client(tunnel_id, client_id),
            None::<&()>,
        )
        .await
    }

    /// Engages the kill switch (all tunnels stop).
    pub async fn kill(&self) -> Result<()> {
        self.call_empty(Method::POST, paths::KILL, Some(&serde_json::json!({})))
            .await
    }

    pub async fn resume(&self) -> Result<()> {
        self.call_empty(Method::POST, paths::RESUME, Some(&serde_json::json!({})))
            .await
    }

    /// Opens the event stream, resuming after `last_event_id` when given.
    /// The first event is `hello` or `reset` (see [`super::events`]).
    pub async fn events(&self, last_event_id: Option<&str>) -> Result<EventStream> {
        let mut token = self.current_token().await?;
        let mut resp = self
            .send(
                Method::GET,
                paths::EVENTS,
                None,
                Some(&token),
                last_event_id,
            )
            .await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            token = self.fetch_token().await?;
            resp = self
                .send(
                    Method::GET,
                    paths::EVENTS,
                    None,
                    Some(&token),
                    last_event_id,
                )
                .await?;
        }
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
    /// the tracker's last event ID).
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
            ProviderError::NotInstalled("DNS Pilot Agent (local API)")
        }
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
    let message = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_owned());
    Err(match status.as_u16() {
        401 | 403 => ProviderError::Unauthorized(message),
        404 => ProviderError::NotFound(message),
        429 => ProviderError::RateLimited { retry_after },
        s => ProviderError::Api { status: s, message },
    })
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
