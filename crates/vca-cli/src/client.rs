//! Connection to vcad over the local control channel.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vca_core::{ClientMsg, DeliveryStatus, Input, ServerMsg, SessionId, SessionInfo};
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};

use crate::error::{CliError, CliResult};

/// How long to wait for vcad to accept a connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long `send` and `key` wait for the `Delivered` acknowledgement.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// A connected control client.
pub struct Client {
    inner: ControlClient,
    endpoint: PathBuf,
}

/// Outcome of an input request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// vcad confirmed delivery to the session.
    Delivered,
    /// vcad queued the input but did not confirm delivery in time.
    Queued,
}

impl Client {
    pub async fn connect(endpoint: &Path) -> CliResult<Self> {
        let inner = ControlClient::connect_timeout(endpoint, CONNECT_TIMEOUT)
            .await
            .map_err(|err| unreachable_error(endpoint, &err))?;
        Ok(Self {
            inner,
            endpoint: endpoint.to_owned(),
        })
    }

    fn io_error(&self, err: io::Error) -> CliError {
        CliError::failure(format!(
            "control channel {}: {err}",
            self.endpoint.display()
        ))
    }

    /// Sends a local request and returns the matching response. A
    /// [`LocalResponse::Error`] becomes a [`CliError`].
    pub async fn local(&mut self, req: LocalRequest) -> CliResult<LocalResponse> {
        match self.inner.call(&req).await.map_err(|e| self.io_error(e))? {
            LocalResponse::Error { message, .. } => Err(CliError::failure(message)),
            other => Ok(other),
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.inner.next_id()
    }

    /// Sends an API message and returns the direct reply. A
    /// [`ServerMsg::Error`] becomes a [`CliError`].
    pub async fn api(&mut self, body: ClientMsg) -> CliResult<ServerMsg> {
        match self.inner.api(body).await.map_err(|e| self.io_error(e))? {
            ServerMsg::Error { message, .. } => Err(CliError::failure(message)),
            other => Ok(other),
        }
    }

    pub async fn ping(&mut self) -> CliResult<Duration> {
        let start = Instant::now();
        match self
            .api(ClientMsg::Ping {
                at: vca_core::now_millis(),
            })
            .await?
        {
            ServerMsg::Pong { .. } => Ok(start.elapsed()),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn sessions(&mut self) -> CliResult<Vec<SessionInfo>> {
        match self.api(ClientMsg::ListSessions).await? {
            ServerMsg::Sessions { sessions } => Ok(sessions),
            other => Err(unexpected(&other)),
        }
    }

    /// Resolves a full session id or a unique prefix.
    pub async fn resolve(&mut self, arg: &str) -> CliResult<SessionId> {
        let sessions = self.sessions().await?;
        resolve_session(&sessions, arg)
    }

    /// Sends input to a session and waits up to [`DELIVERY_TIMEOUT`] for the
    /// `Delivered` push that follows the direct `Queued` reply.
    pub async fn send_input(&mut self, session_id: SessionId, input: Input) -> CliResult<Delivery> {
        let id = self.next_id();
        let req = LocalRequest::Api {
            id,
            body: ClientMsg::SendInput {
                session_id: session_id.clone(),
                input,
            },
        };
        self.inner.send(&req).await.map_err(|e| self.io_error(e))?;

        let deadline = tokio::time::Instant::now() + DELIVERY_TIMEOUT;
        let mut replied = false;
        let mut delivered = false;
        // A push may in principle overtake the reply, so track both.
        while !(replied && delivered) {
            let next = match tokio::time::timeout_at(deadline, self.inner.recv()).await {
                Ok(next) => next.map_err(|e| self.io_error(e))?,
                Err(_) if replied => return Ok(Delivery::Queued),
                Err(_) => return Err(CliError::failure("timed out waiting for vcad")),
            };
            match next {
                None => {
                    if replied {
                        return Ok(Delivery::Queued);
                    }
                    return Err(CliError::failure("vcad closed the connection"));
                }
                Some(LocalResponse::Api { id: rid, body }) if rid == id => match body {
                    ServerMsg::InputAck { status, .. } => {
                        replied = true;
                        delivered |= matches!(
                            status,
                            DeliveryStatus::Delivered | DeliveryStatus::Acknowledged
                        );
                    }
                    ServerMsg::Ok => replied = true,
                    ServerMsg::Error { message, .. } => return Err(CliError::failure(message)),
                    other => return Err(unexpected(&other)),
                },
                Some(LocalResponse::Error {
                    id: rid, message, ..
                }) if rid == id => {
                    return Err(CliError::failure(message));
                }
                Some(LocalResponse::Api {
                    id: 0,
                    body:
                        ServerMsg::InputAck {
                            session_id: sid,
                            status: DeliveryStatus::Delivered | DeliveryStatus::Acknowledged,
                        },
                }) if sid == session_id => delivered = true,
                Some(_) => {}
            }
        }
        Ok(Delivery::Delivered)
    }
}

/// The error shown when vcad cannot be reached.
pub fn unreachable_error(endpoint: &Path, err: &io::Error) -> CliError {
    CliError::failure(format!(
        "cannot reach vcad at {}: {err} (start it with `vca daemon`)",
        endpoint.display()
    ))
}

pub fn unexpected(msg: &ServerMsg) -> CliError {
    CliError::failure(format!("unexpected reply from vcad: {msg:?}"))
}

/// Connects and pings once, returning the round-trip time.
pub async fn ping(endpoint: &Path) -> CliResult<Duration> {
    Client::connect(endpoint).await?.ping().await
}

/// Finds the session whose id equals `arg`, or the only one starting with it.
pub fn resolve_session(sessions: &[SessionInfo], arg: &str) -> CliResult<SessionId> {
    if arg.is_empty() {
        return Err(CliError::usage("empty session id"));
    }
    if let Some(s) = sessions.iter().find(|s| s.id.0 == arg) {
        return Ok(s.id.clone());
    }
    let matches: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|s| s.id.0.starts_with(arg))
        .collect();
    match matches.as_slice() {
        [] => Err(CliError::usage(format!("no session matches {arg:?}"))),
        [one] => Ok(one.id.clone()),
        many => {
            let ids: Vec<&str> = many.iter().map(|s| s.id.0.as_str()).collect();
            Err(CliError::usage(format!(
                "session prefix {arg:?} is ambiguous: {}",
                ids.join(", ")
            )))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use vca_core::{AgentKind, ControlTier, SessionOrigin, SessionState};

    use super::*;
    use crate::error::EXIT_USAGE;

    pub(crate) fn session(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            origin: SessionOrigin::Local {
                agent: AgentKind::ClaudeCode,
                project_dir: PathBuf::from("/work/app"),
                pid: Some(42),
            },
            display_name: "app".into(),
            state: SessionState::Running,
            control: ControlTier::ManagedPty,
            profile: None,
            created_at: 0,
            last_activity: 0,
            unread: 0,
            preview: None,
        }
    }

    #[test]
    fn resolves_exact_and_prefix() {
        let all = [session("abc123"), session("abd456"), session("abc")];
        assert_eq!(resolve_session(&all, "abc").unwrap().0, "abc");
        assert_eq!(resolve_session(&all, "abc1").unwrap().0, "abc123");
        assert_eq!(resolve_session(&all, "abd").unwrap().0, "abd456");
    }

    #[test]
    fn rejects_unknown_and_ambiguous() {
        let all = [session("abc123"), session("abd456")];
        let err = resolve_session(&all, "ab").unwrap_err();
        assert_eq!(err.exit_code, EXIT_USAGE);
        assert!(err.message.contains("ambiguous"), "{}", err.message);
        assert!(err.message.contains("abc123") && err.message.contains("abd456"));

        let err = resolve_session(&all, "zz").unwrap_err();
        assert_eq!(err.exit_code, EXIT_USAGE);
        assert!(err.message.contains("no session"));

        assert_eq!(resolve_session(&all, "").unwrap_err().exit_code, EXIT_USAGE);
        assert_eq!(resolve_session(&[], "a").unwrap_err().exit_code, EXIT_USAGE);
    }
}
