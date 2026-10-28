//! Local control channel between vcad and the `vca` CLI (including agent
//! hooks).
//!
//! The channel is a local socket that only the owner can open (a 0600 Unix
//! socket in a 0700 directory, or a named pipe with an owner-only DACL on
//! Windows). Each message is one JSON object per line. Requests carry a
//! client-chosen `id` that the matching response echoes; server-initiated
//! API messages use `id = 0`.
//!
//! Besides the transport-agnostic API ([`LocalRequest::Api`]), the local
//! channel carries requests that only make sense on this machine: agent
//! hook events, running an arbitrary command under management (`vca run`)
//! and looking up a session's attach endpoint.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use interprocess::local_socket::tokio::{Stream, prelude::*};
use interprocess::local_socket::{GenericFilePath, ToFsName};
use serde::{Deserialize, Serialize};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use vca_adapters::{AgentHookEvent, HookDecision};
use vca_core::error::ErrorCode;
use vca_core::{AgentKind, ClientMsg, ServerMsg, SessionId, SessionInfo};

/// Environment variable overriding the control endpoint. vcad sets it in
/// managed sessions so that hooks reach the daemon that launched them.
pub const CONTROL_ENV: &str = "VCA_CONTROL";
/// Environment variable carrying the VCA session id inside managed sessions.
pub const SESSION_ENV: &str = "VCA_SESSION_ID";

/// Longest accepted line, in bytes.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

/// Terminal size of the `vca run` caller, in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

/// Runs a command typed in a local terminal under VCA management
/// (`vca run -- <cmd>`, shell integration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The caller's working directory.
    pub cwd: PathBuf,
    /// The caller's environment; applied on top of vcad's environment so the
    /// agent sees the same `PATH`, virtualenv and so on as in the terminal.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub size: Option<TermSize>,
    /// Detected from `program` when `None`.
    pub agent: Option<AgentKind>,
    pub display_name: Option<String>,
}

/// A request on the local channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalRequest {
    /// A message of the transport-agnostic API.
    Api { id: u64, body: ClientMsg },
    /// An agent hook event from `vca hook`. Answered with
    /// [`LocalResponse::Hook`]. `session_id` comes from [`SESSION_ENV`]
    /// when the agent runs in a managed session.
    Hook {
        id: u64,
        session_id: Option<SessionId>,
        event: Box<AgentHookEvent>,
    },
    /// Answered with [`LocalResponse::Run`].
    Run { id: u64, request: RunRequest },
    /// Answered with [`LocalResponse::Attach`].
    Attach { id: u64, session_id: SessionId },
}

impl LocalRequest {
    pub fn id(&self) -> u64 {
        match self {
            Self::Api { id, .. }
            | Self::Hook { id, .. }
            | Self::Run { id, .. }
            | Self::Attach { id, .. } => *id,
        }
    }
}

/// A response or push on the local channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalResponse {
    Api {
        id: u64,
        body: ServerMsg,
    },
    Hook {
        id: u64,
        decision: HookDecision,
    },
    /// The session was launched; attach to `endpoint` to see it.
    Run {
        id: u64,
        session: SessionInfo,
        endpoint: PathBuf,
    },
    Attach {
        id: u64,
        session_id: SessionId,
        endpoint: PathBuf,
    },
    /// A local request failed (API errors use `Api` with
    /// [`ServerMsg::Error`]).
    Error {
        id: u64,
        code: ErrorCode,
        message: String,
    },
}

impl LocalResponse {
    pub fn id(&self) -> u64 {
        match self {
            Self::Api { id, .. }
            | Self::Hook { id, .. }
            | Self::Run { id, .. }
            | Self::Attach { id, .. }
            | Self::Error { id, .. } => *id,
        }
    }

    pub fn error(id: u64, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            id,
            code,
            message: message.into(),
        }
    }
}

fn user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// The per-user default control endpoint, honouring [`CONTROL_ENV`].
///
/// * Unix: `$XDG_RUNTIME_DIR/vca/control.sock`, or
///   `/tmp/vca-<user>/control.sock` when `XDG_RUNTIME_DIR` is unset.
/// * Windows: `\\.\pipe\vca-control-<user>`.
pub fn default_endpoint() -> PathBuf {
    if let Some(p) = std::env::var_os(CONTROL_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let user = user_name();
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\vca-control-{user}"))
    } else {
        let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) if !d.is_empty() => PathBuf::from(d).join("vca"),
            _ => PathBuf::from(format!("/tmp/vca-{user}")),
        };
        dir.join("control.sock")
    }
}

/// Writes one message as a JSON line.
pub async fn write_line<W, T>(w: &mut W, msg: &T) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: Serialize,
{
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await
}

/// Reads one JSON line; `None` at end of stream. Not cancel safe.
pub async fn read_line<R, T>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<Option<T>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    loop {
        buf.clear();
        let n = (&mut *r)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', buf)
            .await?;
        if n == 0 {
            return Ok(None);
        }
        if buf.len() > MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        let text = buf.trim_ascii();
        if text.is_empty() {
            continue;
        }
        return serde_json::from_slice(text)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
    }
}

/// One direction of a local socket stream shared through an `Arc`
/// (`&Stream` implements the Tokio I/O traits). The connection closes when
/// both halves are dropped.
pub struct StreamHalf(Arc<Stream>);

/// Splits a local socket stream into receiving and sending halves.
pub fn split_stream(stream: Stream) -> (StreamHalf, StreamHalf) {
    let stream = Arc::new(stream);
    (StreamHalf(Arc::clone(&stream)), StreamHalf(stream))
}

impl AsyncRead for StreamHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for StreamHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &*self.0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_shutdown(cx)
    }
}

/// Client side of the local control channel.
pub struct ControlClient {
    recv: BufReader<StreamHalf>,
    send: StreamHalf,
    buf: Vec<u8>,
    next_id: u64,
}

impl ControlClient {
    /// Connects to vcad at `endpoint`.
    pub async fn connect(endpoint: &Path) -> io::Result<Self> {
        let name = endpoint.to_fs_name::<GenericFilePath>()?;
        let stream = Stream::connect(name).await?;
        let (recv, send) = split_stream(stream);
        Ok(Self {
            recv: BufReader::new(recv),
            send,
            buf: Vec::new(),
            next_id: 1,
        })
    }

    /// Connects with a timeout.
    pub async fn connect_timeout(endpoint: &Path, timeout: Duration) -> io::Result<Self> {
        tokio::time::timeout(timeout, Self::connect(endpoint))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connecting to vcad timed out"))?
    }

    /// A fresh request id.
    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub async fn send(&mut self, req: &LocalRequest) -> io::Result<()> {
        write_line(&mut self.send, req).await
    }

    /// Next response or push; `None` when vcad closed the connection.
    pub async fn recv(&mut self) -> io::Result<Option<LocalResponse>> {
        read_line(&mut self.recv, &mut self.buf).await
    }

    /// Sends `req` and waits for the response with the same id, skipping
    /// pushes (`id = 0`) and other responses.
    pub async fn call(&mut self, req: &LocalRequest) -> io::Result<LocalResponse> {
        let id = req.id();
        self.send(req).await?;
        loop {
            match self.recv().await? {
                Some(resp) if resp.id() == id => return Ok(resp),
                Some(_) => continue,
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "vcad closed the connection",
                    ));
                }
            }
        }
    }

    /// Sends an API message and returns the server's direct reply.
    pub async fn api(&mut self, body: ClientMsg) -> io::Result<ServerMsg> {
        let id = self.next_id();
        match self.call(&LocalRequest::Api { id, body }).await? {
            LocalResponse::Api { body, .. } => Ok(body),
            LocalResponse::Error { code, message, .. } => Ok(ServerMsg::Error { code, message }),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response: {other:?}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_core::{Input, PROTOCOL_VERSION};

    #[test]
    fn request_json_shape() {
        let req = LocalRequest::Api {
            id: 3,
            body: ClientMsg::SendInput {
                session_id: "s".into(),
                input: Input::Text {
                    text: "hi".into(),
                    submit: true,
                },
            },
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["op"], "api");
        assert_eq!(v["id"], 3);
        assert_eq!(v["body"]["type"], "send_input");
        let back: LocalRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn response_ids() {
        let r = LocalResponse::Api {
            id: 0,
            body: ServerMsg::Welcome {
                protocol_version: PROTOCOL_VERSION,
                server_version: "0".into(),
                server_name: "x".into(),
            },
        };
        assert_eq!(r.id(), 0);
        assert_eq!(LocalResponse::error(5, ErrorCode::NotFound, "x").id(), 5);
    }

    #[tokio::test]
    async fn line_roundtrip_and_limits() {
        let mut out = Vec::new();
        write_line(
            &mut out,
            &LocalResponse::error(1, ErrorCode::Internal, "boom"),
        )
        .await
        .unwrap();
        out.extend_from_slice(b"\n\n");
        let mut reader = BufReader::new(out.as_slice());
        let mut buf = Vec::new();
        let first: Option<LocalResponse> = read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(first.unwrap().id(), 1);
        let end: Option<LocalResponse> = read_line(&mut reader, &mut buf).await.unwrap();
        assert!(end.is_none());

        let long = vec![b'x'; MAX_LINE + 10];
        let mut reader = BufReader::new(long.as_slice());
        let err = read_line::<_, LocalResponse>(&mut reader, &mut buf)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
