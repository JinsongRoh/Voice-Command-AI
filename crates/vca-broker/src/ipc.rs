//! Local IPC between agent hooks and the broker.
//!
//! # Transport
//!
//! Newline-delimited JSON over a local socket: a Unix domain socket on
//! Linux/macOS (default `$XDG_RUNTIME_DIR/vca/broker.sock`, or
//! `<tmp>/vca-<uid>/broker.sock`; the directory is created with mode 0700),
//! and the named pipe `\\.\pipe\vca-broker` on Windows (**unverified on
//! Windows**). `VCA_BROKER_SOCKET` overrides the path. Each line is one
//! [`IpcRequest`] or [`IpcResponse`]; requests on one connection are handled
//! in order.
//!
//! # Holding a slot while the job runs
//!
//! Hook processes are short-lived, and in agents such as Claude Code the
//! `PreToolUse` hook must exit **before** the command runs, so the hook
//! cannot simply keep the slot open. Supported flows:
//!
//! 1. **Hook pair (recommended for agents).** The `PreToolUse` hook sends
//!    `acquire` with `key` = the tool-use id, `holder_pid` = the agent's pid
//!    (the hook's parent) and a `wait_ms`. The broker answers `queue`
//!    (informational, `done = false`) and then a final `grant`/`deny`. The hook
//!    allows or blocks the tool call accordingly and exits. The
//!    `PostToolUse` hook sends `release` with the same `key`.
//! 2. **Wrapper (`vca run -- cargo test`).** The wrapper sends `acquire` with
//!    `hold = true` and its own pid, runs the job as its child, and exits.
//!    The broker releases the slot when the connection closes (the wrapper
//!    exited or crashed) and keeps the lease alive while it is open.
//!
//! Safety nets in every flow: the slot is freed when `holder_pid` exits, and
//! when its lease (`slot_lease_secs`, renewed by `heartbeat`) runs out, so a
//! missed `PostToolUse` cannot leak a slot forever. If the client disconnects
//! while queued, its job is cancelled.
//!
//! Example exchange:
//!
//! ```text
//! → {"type":"acquire","command":"cargo test","session_id":"s1","key":"toolu_1","holder_pid":4242,"wait_ms":600000}
//! ← {"type":"verdict","verdict":{"type":"queue","job_id":"…","position":0},"done":false}
//! ← {"type":"verdict","verdict":{"type":"grant","slot_id":"…"},"done":true}
//! → {"type":"release","key":"toolu_1"}
//! ← {"type":"released","count":1}
//! ```

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::{Listener, Stream as LocalStream, prelude::*};
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ListenerOptions, Name};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task::JoinHandle;
use tokio_util::codec::{Framed, LinesCodec};
use vca_core::{AdmissionVerdict, JobClass, QueueEntry, SessionId};

use crate::admission::{AdmissionController, AdmissionRequest, SlotInfo};

/// Maximum length of one JSON line.
const MAX_LINE: usize = 256 * 1024;

/// A request from a hook or wrapper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    /// Ask for a slot. Answered by one or more `verdict` responses; the last
    /// one has `done = true`.
    Acquire {
        command: String,
        #[serde(default)]
        session_id: Option<SessionId>,
        #[serde(default)]
        class: Option<JobClass>,
        #[serde(default)]
        holder_pid: Option<u32>,
        #[serde(default)]
        key: Option<String>,
        /// How long to wait while queued; `0` returns the `queue` verdict
        /// immediately; absent uses the policy's `queue_timeout_secs`.
        #[serde(default)]
        wait_ms: Option<u64>,
        /// Release the slot when this connection closes.
        #[serde(default)]
        hold: bool,
    },
    /// Release by slot id or by key.
    Release {
        #[serde(default)]
        slot_id: Option<String>,
        #[serde(default)]
        key: Option<String>,
    },
    Heartbeat {
        slot_id: String,
    },
    Cancel {
        job_id: String,
    },
    Classify {
        command: String,
    },
    Status,
    Ping,
}

/// A response from the broker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    Verdict {
        verdict: AdmissionVerdict,
        /// `false` for intermediate `queue` updates.
        done: bool,
    },
    Released {
        count: u32,
    },
    /// Result of `heartbeat` or `cancel`: whether the slot/job existed.
    Ack {
        found: bool,
    },
    Classified {
        class: JobClass,
        matched: Option<String>,
    },
    Status {
        max_heavy_jobs: u32,
        slots: Vec<SlotInfo>,
        queue: Vec<QueueEntry>,
    },
    Pong,
    Error {
        message: String,
    },
}

/// Where the broker listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcEndpoint {
    /// A filesystem socket path (Unix).
    Path(PathBuf),
    /// A namespaced name (Windows named pipe `\\.\pipe\<name>`).
    Named(String),
}

impl IpcEndpoint {
    /// The per-user default endpoint, honouring `VCA_BROKER_SOCKET`.
    pub fn default_for_user() -> Self {
        if let Some(p) = std::env::var_os("VCA_BROKER_SOCKET") {
            let s = p.to_string_lossy().into_owned();
            return if cfg!(windows) && !s.contains(['/', '\\']) {
                Self::Named(s)
            } else {
                Self::Path(PathBuf::from(p))
            };
        }
        #[cfg(unix)]
        {
            let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
                Some(d) if !d.is_empty() => PathBuf::from(d).join("vca"),
                _ => std::env::temp_dir().join(format!("vca-{}", nix::unistd::getuid())),
            };
            Self::Path(dir.join("broker.sock"))
        }
        #[cfg(not(unix))]
        {
            Self::Named("vca-broker".into())
        }
    }

    fn name(&self) -> io::Result<Name<'_>> {
        match self {
            Self::Path(p) => p.as_path().to_fs_name::<GenericFilePath>(),
            Self::Named(n) => n.as_str().to_ns_name::<GenericNamespaced>(),
        }
    }
}

impl std::fmt::Display for IpcEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(p) => write!(f, "{}", p.display()),
            Self::Named(n) => write!(f, r"\\.\pipe\{n}"),
        }
    }
}

// ---------------------------------------------------------------- server

/// A bound IPC server. Call [`IpcServer::spawn`] to start accepting.
pub struct IpcServer {
    listener: Listener,
    endpoint: IpcEndpoint,
    admission: Arc<AdmissionController>,
    options: ConnectionOptions,
}

impl IpcServer {
    /// Binds the endpoint. On Unix the parent directory is created with mode
    /// 0700 and a stale socket file is replaced.
    pub fn bind(endpoint: IpcEndpoint, admission: Arc<AdmissionController>) -> io::Result<Self> {
        #[cfg(unix)]
        if let IpcEndpoint::Path(p) = &endpoint {
            if let Some(dir) = p.parent() {
                use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
                if !dir.exists() {
                    std::fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(dir)?;
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            if p.exists() && std::os::unix::net::UnixStream::connect(p).is_err() {
                // A socket left behind by a crashed broker.
                std::fs::remove_file(p)?;
            }
        }
        let listener = ListenerOptions::new()
            .name(endpoint.name()?)
            .create_tokio()?;
        Ok(Self {
            listener,
            endpoint,
            admission,
            options: ConnectionOptions::default(),
        })
    }

    /// Sets per-connection options (default queue wait, heartbeat interval).
    pub fn with_options(mut self, options: ConnectionOptions) -> Self {
        self.options = options;
        self
    }

    pub fn endpoint(&self) -> &IpcEndpoint {
        &self.endpoint
    }

    /// Accepts connections until the returned task is aborted or dropped
    /// with the runtime.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match self.listener.accept().await {
                    Ok(conn) => {
                        let admission = self.admission.clone();
                        let opts = self.options;
                        tokio::spawn(async move {
                            if let Err(e) = serve_connection(conn, admission, opts).await {
                                tracing::debug!("ipc connection ended: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!("ipc accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        })
    }
}

/// Per-connection settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionOptions {
    /// Queue wait when a request does not set `wait_ms`.
    pub default_wait: Duration,
    /// How often slots held by the connection are renewed.
    pub heartbeat_every: Duration,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            default_wait: Duration::from_secs(1800),
            heartbeat_every: Duration::from_secs(10),
        }
    }
}

type Lines<S> = Framed<S, LinesCodec>;

async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    lines: &mut Lines<S>,
    resp: &IpcResponse,
) -> io::Result<()> {
    let text = serde_json::to_string(resp).map_err(io::Error::other)?;
    lines.send(text).await.map_err(io::Error::other)
}

/// Serves one connection: any duplex byte stream (a local socket in
/// production, an in-memory pipe in tests).
pub async fn serve_connection<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    admission: Arc<AdmissionController>,
    opts: ConnectionOptions,
) -> io::Result<()> {
    let mut lines = Framed::new(stream, LinesCodec::new_with_max_length(MAX_LINE));
    let mut held: Vec<String> = Vec::new();
    let mut heartbeat = tokio::time::interval(opts.heartbeat_every);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let result = async {
        loop {
            let line = tokio::select! {
                line = lines.next() => match line {
                    Some(Ok(line)) => line,
                    Some(Err(e)) => return Err(io::Error::other(e)),
                    None => return Ok(()),
                },
                _ = heartbeat.tick(), if !held.is_empty() => {
                    for slot in &held {
                        admission.heartbeat(slot);
                    }
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let request: IpcRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    send(
                        &mut lines,
                        &IpcResponse::Error {
                            message: format!("invalid request: {e}"),
                        },
                    )
                    .await?;
                    continue;
                }
            };
            match request {
                IpcRequest::Acquire {
                    command,
                    session_id,
                    class,
                    holder_pid,
                    key,
                    wait_ms,
                    hold,
                } => {
                    let req = AdmissionRequest {
                        command,
                        session_id,
                        class,
                        holder_pid,
                        key,
                    };
                    let wait = wait_ms.map_or(opts.default_wait, Duration::from_millis);
                    let submitted = admission.submit(req);
                    let verdict = match submitted.ticket {
                        Some(ticket) if !wait.is_zero() => {
                            send(
                                &mut lines,
                                &IpcResponse::Verdict {
                                    verdict: submitted.verdict,
                                    done: false,
                                },
                            )
                            .await?;
                            let job_id = ticket.job_id.clone();
                            let waiting = admission.wait(ticket, wait);
                            tokio::pin!(waiting);
                            // Watch for disconnects while waiting. Clients must
                            // not send further requests while queued.
                            let mut ignored_input = false;
                            loop {
                                tokio::select! {
                                    v = &mut waiting => break v,
                                    next = lines.next(), if !ignored_input => match next {
                                        Some(Ok(_)) => {
                                            tracing::debug!("ignoring request sent while queued");
                                            ignored_input = true;
                                        }
                                        _ => {
                                            admission.cancel(&job_id);
                                            return Ok(());
                                        }
                                    },
                                }
                            }
                        }
                        _ => submitted.verdict,
                    };
                    if hold {
                        if let AdmissionVerdict::Grant { slot_id } = &verdict {
                            if slot_id != crate::admission::LIGHT_SLOT_ID {
                                held.push(slot_id.clone());
                            }
                        }
                    }
                    let done = !matches!(verdict, AdmissionVerdict::Queue { .. }) || wait.is_zero();
                    send(&mut lines, &IpcResponse::Verdict { verdict, done }).await?;
                }
                IpcRequest::Release { slot_id, key } => {
                    let mut count = 0;
                    if let Some(id) = slot_id {
                        held.retain(|h| h != &id);
                        count += u32::from(admission.release(&id));
                    }
                    if let Some(key) = key {
                        count += admission.release_by_key(&key) as u32;
                    }
                    send(&mut lines, &IpcResponse::Released { count }).await?;
                }
                IpcRequest::Heartbeat { slot_id } => {
                    let found = admission.heartbeat(&slot_id);
                    send(&mut lines, &IpcResponse::Ack { found }).await?;
                }
                IpcRequest::Cancel { job_id } => {
                    let found = admission.cancel(&job_id);
                    send(&mut lines, &IpcResponse::Ack { found }).await?;
                }
                IpcRequest::Classify { command } => {
                    let c = admission.classify_explained(&command);
                    send(
                        &mut lines,
                        &IpcResponse::Classified {
                            class: c.class,
                            matched: c.matched,
                        },
                    )
                    .await?;
                }
                IpcRequest::Status => {
                    send(
                        &mut lines,
                        &IpcResponse::Status {
                            max_heavy_jobs: admission.max_heavy_jobs(),
                            slots: admission.slots(),
                            queue: admission.queue(),
                        },
                    )
                    .await?;
                }
                IpcRequest::Ping => send(&mut lines, &IpcResponse::Pong).await?,
            }
        }
    }
    .await;

    // Connection closed: free slots bound to it.
    for slot in held {
        admission.release(&slot);
    }
    result
}

// ---------------------------------------------------------------- client

/// Errors from the IPC client.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("broker connection failed: {0}")]
    Io(#[from] io::Error),
    #[error("broker closed the connection")]
    Closed,
    #[error("broker error: {0}")]
    Broker(String),
    #[error("unexpected response from broker: {0:?}")]
    Unexpected(Box<IpcResponse>),
}

/// A slot request as sent by a client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotRequest {
    pub command: String,
    pub session_id: Option<SessionId>,
    pub class: Option<JobClass>,
    pub holder_pid: Option<u32>,
    pub key: Option<String>,
    /// Release the slot when this client disconnects.
    pub hold: bool,
}

/// A client connection to the broker.
pub struct IpcClient<S = LocalStream> {
    lines: Lines<S>,
}

impl IpcClient<LocalStream> {
    /// Connects to the broker.
    pub async fn connect(endpoint: &IpcEndpoint) -> Result<Self, IpcError> {
        let stream = LocalStream::connect(endpoint.name()?).await?;
        Ok(Self::from_stream(stream))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> IpcClient<S> {
    /// Wraps an already connected stream.
    pub fn from_stream(stream: S) -> Self {
        Self {
            lines: Framed::new(stream, LinesCodec::new_with_max_length(MAX_LINE)),
        }
    }

    async fn send(&mut self, req: &IpcRequest) -> Result<(), IpcError> {
        let text = serde_json::to_string(req).map_err(io::Error::other)?;
        self.lines.send(text).await.map_err(io::Error::other)?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<IpcResponse, IpcError> {
        match self.lines.next().await {
            Some(Ok(line)) => {
                let resp: IpcResponse = serde_json::from_str(&line).map_err(io::Error::other)?;
                match resp {
                    IpcResponse::Error { message } => Err(IpcError::Broker(message)),
                    other => Ok(other),
                }
            }
            Some(Err(e)) => Err(io::Error::other(e).into()),
            None => Err(IpcError::Closed),
        }
    }

    /// Sends one request and returns its (single) response. Use
    /// [`IpcClient::request_slot`] for `acquire`.
    pub async fn call(&mut self, req: &IpcRequest) -> Result<IpcResponse, IpcError> {
        self.send(req).await?;
        self.recv().await
    }

    /// Requests a slot and waits up to `timeout` while queued. Returns
    /// `Grant` or `Deny` (a timeout is reported as `Deny`). `on_queued` is
    /// called with each intermediate queue verdict.
    pub async fn request_slot_with_progress(
        &mut self,
        request: SlotRequest,
        timeout: Duration,
        mut on_queued: impl FnMut(&AdmissionVerdict),
    ) -> Result<AdmissionVerdict, IpcError> {
        self.send(&IpcRequest::Acquire {
            command: request.command,
            session_id: request.session_id,
            class: request.class,
            holder_pid: request.holder_pid,
            key: request.key,
            wait_ms: Some(timeout.as_millis().min(u64::MAX as u128) as u64),
            hold: request.hold,
        })
        .await?;
        loop {
            match self.recv().await? {
                IpcResponse::Verdict {
                    verdict,
                    done: true,
                } => return Ok(verdict),
                IpcResponse::Verdict { verdict, .. } => on_queued(&verdict),
                other => return Err(IpcError::Unexpected(Box::new(other))),
            }
        }
    }

    /// Requests a slot, blocking while queued (see
    /// [`IpcClient::request_slot_with_progress`]).
    pub async fn request_slot(
        &mut self,
        request: SlotRequest,
        timeout: Duration,
    ) -> Result<AdmissionVerdict, IpcError> {
        self.request_slot_with_progress(request, timeout, |_| {})
            .await
    }

    /// Releases a slot by id. Returns whether it existed.
    pub async fn release(&mut self, slot_id: &str) -> Result<bool, IpcError> {
        self.release_inner(Some(slot_id.to_owned()), None)
            .await
            .map(|n| n > 0)
    }

    /// Releases all slots acquired with `key`. Returns how many.
    pub async fn release_key(&mut self, key: &str) -> Result<u32, IpcError> {
        self.release_inner(None, Some(key.to_owned())).await
    }

    async fn release_inner(
        &mut self,
        slot_id: Option<String>,
        key: Option<String>,
    ) -> Result<u32, IpcError> {
        match self.call(&IpcRequest::Release { slot_id, key }).await? {
            IpcResponse::Released { count } => Ok(count),
            other => Err(IpcError::Unexpected(Box::new(other))),
        }
    }

    /// Renews a slot's lease.
    pub async fn heartbeat(&mut self, slot_id: &str) -> Result<bool, IpcError> {
        match self
            .call(&IpcRequest::Heartbeat {
                slot_id: slot_id.to_owned(),
            })
            .await?
        {
            IpcResponse::Ack { found } => Ok(found),
            other => Err(IpcError::Unexpected(Box::new(other))),
        }
    }

    /// Checks that the broker is reachable.
    pub async fn ping(&mut self) -> Result<(), IpcError> {
        match self.call(&IpcRequest::Ping).await? {
            IpcResponse::Pong => Ok(()),
            other => Err(IpcError::Unexpected(Box::new(other))),
        }
    }
}

/// One-shot helper for hooks: connect, request a slot for `command`, and
/// wait up to `timeout` while queued.
pub async fn request_slot(
    endpoint: &IpcEndpoint,
    command: &str,
    session_id: Option<SessionId>,
    timeout: Duration,
) -> Result<AdmissionVerdict, IpcError> {
    let mut client = IpcClient::connect(endpoint).await?;
    client
        .request_slot(
            SlotRequest {
                command: command.to_owned(),
                session_id,
                ..SlotRequest::default()
            },
            timeout,
        )
        .await
}

/// One-shot helper for hooks: release a slot by id.
pub async fn release(endpoint: &IpcEndpoint, slot_id: &str) -> Result<bool, IpcError> {
    IpcClient::connect(endpoint).await?.release(slot_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_tagged_snake_case() {
        let req: IpcRequest = serde_json::from_str(
            r#"{"type":"acquire","command":"cargo test","session_id":"s1","wait_ms":0}"#,
        )
        .unwrap();
        assert!(matches!(
            req,
            IpcRequest::Acquire {
                hold: false,
                wait_ms: Some(0),
                ..
            }
        ));
        let resp = IpcResponse::Verdict {
            verdict: AdmissionVerdict::Grant {
                slot_id: "x".into(),
            },
            done: true,
        };
        assert_eq!(
            serde_json::to_string(&resp).unwrap(),
            r#"{"type":"verdict","verdict":{"type":"grant","slot_id":"x"},"done":true}"#
        );
        let req: IpcRequest = serde_json::from_str(r#"{"type":"release","key":"k"}"#).unwrap();
        assert_eq!(
            req,
            IpcRequest::Release {
                slot_id: None,
                key: Some("k".into())
            }
        );
    }

    #[test]
    fn default_endpoint_is_per_user() {
        let ep = IpcEndpoint::default_for_user();
        #[cfg(unix)]
        assert!(matches!(ep, IpcEndpoint::Path(ref p) if p.ends_with("broker.sock")));
        #[cfg(windows)]
        assert_eq!(ep, IpcEndpoint::Named("vca-broker".into()));
    }
}
