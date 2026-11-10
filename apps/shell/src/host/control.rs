//! A minimal client for vcad's owner-only local control channel (a named
//! pipe with an owner-only DACL on Windows, a 0600 Unix socket elsewhere).
//!
//! The wire format is vca-server's (`vca_server::control`): one JSON object
//! per line, tagged by `op`, with a client-chosen `id` echoed in the answer
//! and `id = 0` for pushes. Only the requests the desktop app needs are
//! modelled here, so the app does not link the whole server (PTYs, broker,
//! speech, tunnels) just to talk to it. The JSON shapes are pinned by the
//! tests below.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interprocess::local_socket::tokio::{Stream, prelude::*};
use interprocess::local_socket::{GenericFilePath, ToFsName};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use vca_core::ClientMsg;

/// Environment variable overriding the control endpoint (as in vcad).
pub const CONTROL_ENV: &str = "VCA_CONTROL";

/// Longest accepted line, in bytes (vcad's limit).
pub const MAX_LINE: usize = 4 * 1024 * 1024;

/// How long connecting may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long one request may take (pairing may wait for a tunnel to start).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

fn user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// The per-user default control endpoint, honouring [`CONTROL_ENV`]; the
/// same rules as `vca_server::control::default_endpoint` (and so the `vca`
/// CLI).
pub fn default_endpoint() -> PathBuf {
    if let Some(p) = std::env::var(CONTROL_ENV).ok().filter(|p| !p.is_empty()) {
        return parse_endpoint(&p);
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

/// On Windows a bare name means the named pipe `\\.\pipe\<name>`.
fn parse_endpoint(value: &str) -> PathBuf {
    if cfg!(windows) {
        if !value.contains(['/', '\\']) {
            return PathBuf::from(format!(r"\\.\pipe\{value}"));
        }
        let rest = value.trim_start_matches(['/', '\\']);
        let prefix = rest.get(..7).map(str::to_ascii_lowercase);
        if matches!(prefix.as_deref(), Some(r".\pipe\") | Some("./pipe/")) {
            let name = &rest[7..];
            if !name.is_empty() && !name.contains(['/', '\\']) {
                return PathBuf::from(format!(r"\\.\pipe\{name}"));
            }
        }
    }
    PathBuf::from(value)
}

/// A request on the control channel (the subset the app sends).
#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request<'a> {
    Api {
        id: u64,
        body: &'a ClientMsg,
    },
    /// An API message that needs local verification on this channel.
    GatedApi {
        id: u64,
        body: &'a ClientMsg,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_up: Option<SecretStr<'a>>,
    },
    /// An API message this build has no `ClientMsg` variant for.
    #[serde(rename = "api")]
    ApiValue {
        id: u64,
        body: &'a serde_json::Value,
    },
    /// `step_up` (here and below): the token of a local verification.
    Pair {
        id: u64,
        scope: &'a str,
        endpoints: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_up: Option<SecretStr<'a>>,
    },
    TotpBegin {
        id: u64,
        account: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_up: Option<SecretStr<'a>>,
    },
    TotpConfirm {
        id: u64,
        code: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_code: Option<SecretStr<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_up: Option<SecretStr<'a>>,
    },
    TotpBackupCodes {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_code: Option<SecretStr<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_up: Option<SecretStr<'a>>,
    },
    /// The app's route for `ClientMsg::SetDeviceScope`: only this one can
    /// widen a permission.
    DeviceScope {
        id: u64,
        device_id: &'a str,
        scope: vca_core::DeviceScope,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_up: Option<SecretStr<'a>>,
    },
    RemoteStatus {
        id: u64,
    },
    Shutdown {
        id: u64,
    },
    RestoreLocal {
        id: u64,
        action: RestoreLocalAction<'a>,
    },
    // Speech engine setup (see `super::engines`); profiles stay JSON.
    EnginesGet {
        id: u64,
    },
    EngineSave {
        id: u64,
        kind: &'a str,
        original_id: Option<&'a str>,
        profile: &'a serde_json::Value,
        secret: Option<SecretStr<'a>>,
        make_default: bool,
        /// Stores or replaces the gateway token.
        gateway_token: Option<SecretStr<'a>>,
        /// Deletes the stored gateway token.
        clear_gateway_token: bool,
        /// Needed when existing credentials would go to a new URL.
        #[serde(skip_serializing_if = "Option::is_none")]
        step_up: Option<SecretStr<'a>>,
    },
    EngineRemove {
        id: u64,
        kind: &'a str,
        profile_id: &'a str,
        delete_key: bool,
    },
    EngineSetDefault {
        id: u64,
        kind: &'a str,
        profile_id: Option<&'a str>,
    },
    EngineTest {
        id: u64,
        kind: &'a str,
        profile: Option<&'a serde_json::Value>,
        profile_id: Option<&'a str>,
        secret: Option<SecretStr<'a>>,
        gateway_token: Option<SecretStr<'a>>,
    },
}

/// A key on its way to vcad: serialized as the plain string, never shown
/// by `Debug`.
#[derive(Clone, Copy, Serialize)]
#[serde(transparent)]
pub struct SecretStr<'a>(pub &'a str);

impl std::fmt::Debug for SecretStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretStr(<redacted>)")
    }
}

/// The owner-only parts of session restore.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RestoreLocalAction<'a> {
    Status,
    CreateShortcut { name: &'a str },
    SetSignIn { enabled: bool, delay_secs: u32 },
}

impl Request<'_> {
    pub fn id(&self) -> u64 {
        match self {
            Self::Api { id, .. }
            | Self::GatedApi { id, .. }
            | Self::ApiValue { id, .. }
            | Self::Pair { id, .. }
            | Self::TotpBegin { id, .. }
            | Self::TotpConfirm { id, .. }
            | Self::TotpBackupCodes { id, .. }
            | Self::DeviceScope { id, .. }
            | Self::RemoteStatus { id }
            | Self::Shutdown { id }
            | Self::RestoreLocal { id, .. }
            | Self::EnginesGet { id }
            | Self::EngineSave { id, .. }
            | Self::EngineRemove { id, .. }
            | Self::EngineSetDefault { id, .. }
            | Self::EngineTest { id, .. } => *id,
        }
    }
}

/// The server's challenge for a gated request. The nonce stays in this
/// process (see `super::security`).
#[derive(Clone, PartialEq, Deserialize)]
pub struct StepUpChallenge {
    pub nonce: String,
    pub action: String,
}

impl std::fmt::Debug for StepUpChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepUpChallenge")
            .field("nonce", &"<redacted>")
            .field("action", &self.action)
            .finish()
    }
}

/// A response on the control channel. API bodies stay JSON: callers decode
/// what they need, and unknown ops are skipped.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Response {
    Api {
        id: u64,
        body: serde_json::Value,
    },
    /// A gated request needs a local verification first.
    StepUpRequired {
        id: u64,
        challenge: StepUpChallenge,
    },
    Pairing {
        id: u64,
        uri: String,
        expires_at: u64,
        #[serde(default)]
        server_fingerprint: String,
    },
    TotpPending {
        id: u64,
        otpauth_uri: String,
        secret_base32: String,
    },
    TotpEnrolled {
        id: u64,
        backup_codes: Vec<String>,
    },
    RemoteStatus {
        id: u64,
        enabled: bool,
        #[serde(default)]
        listen: Option<String>,
        #[serde(default)]
        server_fingerprint: Option<String>,
        #[serde(default)]
        totp_enrolled: bool,
        #[serde(default)]
        totp_enrolled_at: Option<u64>,
        #[serde(default)]
        totp_issuer: Option<String>,
        #[serde(default)]
        totp_account: Option<String>,
        #[serde(default)]
        backup_codes_remaining: usize,
        #[serde(default)]
        backup_codes_total: usize,
        #[serde(default)]
        require_totp: bool,
        #[serde(default)]
        kill_switch_tripped: bool,
        #[serde(default)]
        paired_devices: usize,
        #[serde(default)]
        connections: usize,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        provider_state: Option<String>,
        #[serde(default)]
        provider_diagnostics: Vec<String>,
    },
    Done {
        id: u64,
    },
    RestoreLocal {
        id: u64,
        #[serde(default)]
        supported: bool,
        #[serde(default)]
        sign_in: bool,
        #[serde(default)]
        sign_in_delay_secs: u32,
    },
    /// The engine setup (vcad's `EngineSetup`, no secrets).
    Engines {
        id: u64,
        setup: serde_json::Value,
    },
    /// vcad's `EngineTestResult`.
    EngineTest {
        id: u64,
        result: serde_json::Value,
    },
    Error {
        id: u64,
        code: serde_json::Value,
        message: String,
    },
    /// Responses this client does not model (hooks, runs, ...).
    #[serde(other)]
    Other,
}

impl Response {
    /// `None` for [`Response::Other`].
    pub fn id(&self) -> Option<u64> {
        match self {
            Self::Api { id, .. }
            | Self::StepUpRequired { id, .. }
            | Self::Pairing { id, .. }
            | Self::TotpPending { id, .. }
            | Self::TotpEnrolled { id, .. }
            | Self::RemoteStatus { id, .. }
            | Self::Done { id }
            | Self::RestoreLocal { id, .. }
            | Self::Engines { id, .. }
            | Self::EngineTest { id, .. }
            | Self::Error { id, .. } => Some(*id),
            Self::Other => None,
        }
    }
}

/// Why a control request failed.
#[derive(Debug)]
pub enum ControlError {
    /// vcad is not running (or not reachable at the endpoint).
    Unreachable(io::Error),
    /// The connection failed or timed out mid-request.
    Io(io::Error),
    /// vcad answered with an error (`code` is vca-core's `ErrorCode`, e.g.
    /// `unsupported`, `invalid_request`).
    Server { code: String, message: String },
    /// vcad answered with something else than expected.
    Unexpected(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "the VCA server on this PC is not running ({e})"),
            Self::Io(e) => write!(f, "control channel: {e}"),
            Self::Server { message, .. } => f.write_str(message),
            Self::Unexpected(what) => write!(f, "unexpected answer from vcad: {what}"),
        }
    }
}

impl std::error::Error for ControlError {}

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

/// Reads one non-empty line (without the newline); `None` at end of
/// stream. Not cancel safe.
pub async fn read_raw_line<R>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<Option<()>>
where
    R: tokio::io::AsyncBufRead + Unpin,
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
        let trimmed = buf.trim_ascii().len();
        if trimmed == 0 {
            continue;
        }
        return Ok(Some(()));
    }
}

/// Connects to the local socket or named pipe at `endpoint`.
pub async fn connect_stream(endpoint: &Path, timeout: Duration) -> io::Result<Stream> {
    let name = endpoint.to_fs_name::<GenericFilePath>()?;
    tokio::time::timeout(timeout, Stream::connect(name))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connecting to vcad timed out"))?
}

/// One connection to vcad, for request/response use.
pub struct ControlConn {
    recv: BufReader<ReadHalf<Stream>>,
    send: WriteHalf<Stream>,
    buf: Vec<u8>,
    next_id: u64,
}

impl ControlConn {
    pub async fn connect(endpoint: &Path) -> Result<Self, ControlError> {
        let stream = connect_stream(endpoint, CONNECT_TIMEOUT)
            .await
            .map_err(ControlError::Unreachable)?;
        let (recv, send) = tokio::io::split(stream);
        Ok(Self {
            recv: BufReader::new(recv),
            send,
            buf: Vec::new(),
            next_id: 1,
        })
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Sends `req` and returns the response with the same id (pushes and
    /// unrelated responses are skipped). A [`Response::Error`] becomes
    /// [`ControlError::Server`].
    pub async fn call(&mut self, req: &Request<'_>) -> Result<Response, ControlError> {
        self.call_timeout(req, CALL_TIMEOUT).await
    }

    /// [`call`](Self::call) with its own time limit (engine tests wait for
    /// the owner's engine).
    pub async fn call_timeout(
        &mut self,
        req: &Request<'_>,
        timeout: Duration,
    ) -> Result<Response, ControlError> {
        tokio::time::timeout(timeout, self.call_inner(req))
            .await
            .map_err(|_| {
                ControlError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "vcad did not answer in time",
                ))
            })?
    }

    async fn call_inner(&mut self, req: &Request<'_>) -> Result<Response, ControlError> {
        let id = req.id();
        write_line(&mut self.send, req)
            .await
            .map_err(ControlError::Io)?;
        loop {
            let line = read_raw_line(&mut self.recv, &mut self.buf)
                .await
                .map_err(ControlError::Io)?;
            if line.is_none() {
                return Err(ControlError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "vcad closed the connection",
                )));
            }
            let Ok(resp) = serde_json::from_slice::<Response>(self.buf.trim_ascii()) else {
                continue;
            };
            if resp.id() != Some(id) {
                continue;
            }
            return match resp {
                Response::Error { code, message, .. } => Err(ControlError::Server {
                    code: code.as_str().unwrap_or("internal").to_owned(),
                    message,
                }),
                Response::StepUpRequired { challenge, .. } => {
                    super::security::note_challenge(challenge);
                    Err(ControlError::Server {
                        code: "step_up_required".to_owned(),
                        message: "local verification needed".to_owned(),
                    })
                }
                other => Ok(other),
            };
        }
    }

    /// Sends an API message and returns the direct reply's JSON body. A
    /// `ServerMsg::Error` becomes [`ControlError::Server`].
    pub async fn api(&mut self, body: &ClientMsg) -> Result<serde_json::Value, ControlError> {
        let id = self.next_id();
        let response = self.call(&Request::Api { id, body }).await?;
        Self::api_body(response)
    }

    /// [`api`](Self::api) for a message given as JSON.
    pub async fn api_value(
        &mut self,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ControlError> {
        let id = self.next_id();
        let response = self.call(&Request::ApiValue { id, body }).await?;
        Self::api_body(response)
    }

    fn api_body(response: Response) -> Result<serde_json::Value, ControlError> {
        match response {
            Response::Api { body, .. } => {
                if body.get("type").and_then(|t| t.as_str()) == Some("error") {
                    return Err(ControlError::Server {
                        code: body["code"].as_str().unwrap_or("internal").to_owned(),
                        message: body["message"].as_str().unwrap_or_default().to_owned(),
                    });
                }
                Ok(body)
            }
            other => Err(ControlError::Unexpected(format!("{other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_shapes_match_vcad() {
        let hello = ClientMsg::Hello {
            protocol_version: 1,
            app_version: "0.1.0".into(),
        };
        let v = serde_json::to_value(Request::Api {
            id: 3,
            body: &hello,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "op": "api", "id": 3, "body": { "type": "hello", "protocol_version": 1, "app_version": "0.1.0" } })
        );
        let v = serde_json::to_value(Request::Pair {
            id: 4,
            scope: "command",
            endpoints: vec![],
            step_up: None,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "op": "pair", "id": 4, "scope": "command", "endpoints": [] })
        );
        assert_eq!(
            serde_json::to_value(Request::RemoteStatus { id: 5 }).unwrap(),
            json!({ "op": "remote_status", "id": 5 })
        );
        assert_eq!(
            serde_json::to_value(Request::Shutdown { id: 6 }).unwrap(),
            json!({ "op": "shutdown", "id": 6 })
        );
        assert_eq!(
            serde_json::to_value(Request::TotpBegin {
                id: 7,
                account: "me",
                step_up: Some(SecretStr("t")),
            })
            .unwrap(),
            json!({ "op": "totp_begin", "id": 7, "account": "me", "step_up": "t" })
        );
        assert_eq!(
            serde_json::to_value(Request::TotpConfirm {
                id: 8,
                code: "123456",
                current_code: None,
                step_up: None,
            })
            .unwrap(),
            json!({ "op": "totp_confirm", "id": 8, "code": "123456" })
        );
    }

    #[test]
    fn response_shapes_from_vcad() {
        let r: Response = serde_json::from_value(json!({
            "op": "remote_status", "id": 2, "enabled": true, "listen": "127.0.0.1:47800",
            "server_fingerprint": "ab", "totp_enrolled": false, "require_totp": true,
            "kill_switch_tripped": false, "paired_devices": 1, "connections": 0,
            "provider": "lan", "provider_state": "ready", "provider_diagnostics": []
        }))
        .unwrap();
        assert_eq!(r.id(), Some(2));
        assert!(matches!(
            r,
            Response::RemoteStatus {
                require_totp: true,
                ..
            }
        ));

        let r: Response = serde_json::from_value(json!({
            "op": "pairing", "id": 3, "uri": "vca://pair?x", "expires_at": 10, "server_fingerprint": "ab"
        }))
        .unwrap();
        assert!(matches!(r, Response::Pairing { expires_at: 10, .. }));

        let r: Response = serde_json::from_value(json!({
            "op": "error", "id": 4, "code": "unsupported", "message": "the remote transport is off"
        }))
        .unwrap();
        assert!(matches!(r, Response::Error { .. }));

        let r: Response =
            serde_json::from_value(json!({ "op": "hook", "id": 1, "decision": {} })).unwrap();
        assert_eq!(r, Response::Other);
        assert_eq!(r.id(), None);
    }

    #[test]
    fn endpoints() {
        if cfg!(windows) {
            assert_eq!(parse_endpoint("vca-x"), PathBuf::from(r"\\.\pipe\vca-x"));
            assert_eq!(
                parse_endpoint("//./pipe/vca-x"),
                PathBuf::from(r"\\.\pipe\vca-x")
            );
        } else {
            assert_eq!(parse_endpoint("/run/x.sock"), PathBuf::from("/run/x.sock"));
        }
    }

    #[tokio::test]
    async fn long_lines_are_rejected() {
        let long = vec![b'x'; MAX_LINE + 10];
        let mut reader = BufReader::new(long.as_slice());
        let mut buf = Vec::new();
        let err = read_raw_line(&mut reader, &mut buf).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let mut reader = BufReader::new(&b"\n\n{\"a\":1}\n"[..]);
        assert!(
            read_raw_line(&mut reader, &mut buf)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(buf.trim_ascii(), b"{\"a\":1}");
        assert!(
            read_raw_line(&mut reader, &mut buf)
                .await
                .unwrap()
                .is_none()
        );
    }
}
