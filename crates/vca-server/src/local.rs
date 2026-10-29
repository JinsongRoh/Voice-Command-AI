//! Server side of the local control channel (see [`crate::control`]).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName};
use tokio::io::BufReader;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use vca_core::error::ErrorCode;

use crate::api::Api;
use crate::control::{
    ControlClient, LocalRequest, LocalResponse, read_line, split_stream, write_line,
};
use crate::remote::Remote;
use crate::util::error_text;

/// The remote transport, once started (it starts after the local channel).
pub type RemoteSlot = Arc<OnceLock<Arc<Remote>>>;

/// Owner-only administration of the remote transport.
async fn admin(remote: &RemoteSlot, req: LocalRequest) -> LocalResponse {
    let id = req.id();
    let Some(remote) = remote.get() else {
        if let LocalRequest::RemoteStatus { id } = req {
            return LocalResponse::RemoteStatus {
                id,
                enabled: false,
                listen: None,
                server_fingerprint: None,
                totp_enrolled: false,
                require_totp: false,
                kill_switch_tripped: false,
                paired_devices: 0,
                connections: 0,
                provider: None,
                provider_state: None,
                provider_diagnostics: Vec::new(),
            };
        }
        return LocalResponse::error(id, ErrorCode::Unsupported, "the remote transport is off");
    };
    let fail = |err: vca_core::Error| LocalResponse::error(id, err.code(), error_text(&err));
    match req {
        LocalRequest::Pair {
            scope, endpoints, ..
        } => match remote.pair(&scope, endpoints).await {
            Ok(p) => LocalResponse::Pairing {
                id,
                uri: p.uri,
                expires_at: p.expires_at,
                server_fingerprint: p.server_fingerprint,
            },
            Err(err) => fail(err),
        },
        LocalRequest::TotpBegin { account, .. } => {
            let (otpauth_uri, secret_base32) = remote.totp_begin(&account);
            LocalResponse::TotpPending {
                id,
                otpauth_uri,
                secret_base32,
            }
        }
        LocalRequest::TotpConfirm { code, .. } => match remote.totp_confirm(&code) {
            Ok(backup_codes) => LocalResponse::TotpEnrolled { id, backup_codes },
            Err(err) => fail(err),
        },
        LocalRequest::KillSwitchReset { .. } => match remote.reset_kill_switch("local") {
            Ok(()) => LocalResponse::Done { id },
            Err(err) => fail(err),
        },
        LocalRequest::RemoteStatus { .. } => {
            let s = remote.status().await;
            let c = s.connectivity;
            LocalResponse::RemoteStatus {
                id,
                enabled: true,
                listen: Some(s.listen.to_string()),
                server_fingerprint: Some(s.server_fingerprint),
                totp_enrolled: s.totp_enrolled,
                require_totp: s.require_totp,
                kill_switch_tripped: s.kill_switch_tripped,
                paired_devices: s.paired_devices,
                connections: s.connections,
                provider: c.as_ref().map(|c| c.provider.clone()),
                provider_state: c.as_ref().map(|c| c.state.clone()),
                provider_diagnostics: c.map(|c| c.diagnostics).unwrap_or_default(),
            }
        }
        _ => LocalResponse::error(id, ErrorCode::InvalidRequest, "unexpected request"),
    }
}

/// Listens on the control endpoint until dropped.
pub struct LocalServer {
    #[cfg_attr(not(unix), allow(dead_code))]
    endpoint: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

impl LocalServer {
    /// Binds `endpoint` and starts serving. Fails if another vcad already
    /// listens there.
    pub async fn bind(endpoint: &Path, api: Api, remote: RemoteSlot) -> io::Result<Self> {
        if ControlClient::connect_timeout(endpoint, Duration::from_millis(500))
            .await
            .is_ok()
        {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "another vcad is already listening on {}",
                    endpoint.display()
                ),
            ));
        }
        let listener = listener(endpoint)?;
        let task = tokio::spawn(accept_loop(listener, api, remote));
        Ok(Self {
            endpoint: endpoint.to_owned(),
            task,
        })
    }
}

#[cfg(unix)]
fn listener(endpoint: &Path) -> io::Result<Listener> {
    #[cfg(target_os = "linux")]
    use interprocess::os::unix::local_socket::ListenerOptionsExt;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    if let Some(dir) = endpoint.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
    }
    // A stale socket from a crashed vcad (the connect above failed).
    if std::fs::symlink_metadata(endpoint).is_ok() {
        std::fs::remove_file(endpoint)?;
    }
    let options = ListenerOptions::new().name(endpoint.to_fs_name::<GenericFilePath>()?);
    #[cfg(target_os = "linux")]
    let options = options.mode(0o600);
    let listener = options.create_tokio()?;
    // Other Unix systems cannot set the mode before binding; the parent
    // directory is 0700, so the window is not reachable by other users.
    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(windows)]
fn listener(endpoint: &Path) -> io::Result<Listener> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;

    // Protected DACL: full access for the owner and SYSTEM only.
    let sddl = U16CString::from_str("D:P(A;;GA;;;OW)(A;;GA;;;SY)")
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let sd = SecurityDescriptor::deserialize(&sddl)?;
    ListenerOptions::new()
        .name(endpoint.to_fs_name::<GenericFilePath>()?)
        .security_descriptor(sd)
        .create_tokio()
}

#[cfg(not(any(unix, windows)))]
fn listener(endpoint: &Path) -> io::Result<Listener> {
    ListenerOptions::new()
        .name(endpoint.to_fs_name::<GenericFilePath>()?)
        .create_tokio()
}

async fn accept_loop(listener: Listener, api: Api, remote: RemoteSlot) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            conn = listener.accept() => match conn {
                Ok(stream) => {
                    conns.spawn(serve(stream, api.clone(), Arc::clone(&remote)));
                }
                Err(e) => {
                    tracing::warn!("control accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

async fn serve(stream: Stream, api: Api, remote: RemoteSlot) {
    let (recv, mut send) = split_stream(stream);
    let mut recv = BufReader::new(recv);
    let (client, mut outbound) = api.connect("local");
    let (local_tx, mut local_rx) = mpsc::channel::<LocalResponse>(64);

    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                Some(env) = outbound.recv() => LocalResponse::Api { id: env.id, body: env.body },
                Some(resp) = local_rx.recv() => resp,
                else => return,
            };
            if write_line(&mut send, &msg).await.is_err() {
                return;
            }
        }
    });

    let mut buf = Vec::new();
    let mut hooks = JoinSet::new();
    loop {
        let req: LocalRequest = match read_line(&mut recv, &mut buf).await {
            Ok(Some(req)) => req,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("bad control request: {e}");
                let _ = local_tx
                    .send(LocalResponse::error(
                        0,
                        ErrorCode::InvalidRequest,
                        e.to_string(),
                    ))
                    .await;
                break;
            }
        };
        match req {
            LocalRequest::Api { id, body } => {
                api.handle(&client, vca_core::Envelope { id, body }).await;
            }
            LocalRequest::Hook {
                id,
                session_id,
                event,
            } => {
                let mgr = api.sessions().clone();
                let tx = local_tx.clone();
                hooks.spawn(async move {
                    let decision = mgr.handle_hook(session_id, *event).await;
                    let _ = tx.send(LocalResponse::Hook { id, decision }).await;
                });
            }
            LocalRequest::Run { id, request } => {
                let resp = match api.sessions().run(&request) {
                    Ok(session) => match api.sessions().get(&session.id) {
                        Some(s) => LocalResponse::Run {
                            id,
                            endpoint: s.attach_endpoint().map(Path::to_owned).unwrap_or_default(),
                            session,
                        },
                        None => LocalResponse::error(id, ErrorCode::Internal, "session vanished"),
                    },
                    Err(err) => LocalResponse::error(id, err.code(), error_text(&err)),
                };
                let _ = local_tx.send(resp).await;
            }
            LocalRequest::Attach { id, session_id } => {
                let resp = match api.sessions().get(&session_id) {
                    Some(s) => match s.attach_endpoint() {
                        Some(ep) => LocalResponse::Attach {
                            id,
                            session_id,
                            endpoint: ep.to_owned(),
                        },
                        None => LocalResponse::error(
                            id,
                            ErrorCode::Unsupported,
                            "only managed sessions can be attached",
                        ),
                    },
                    None => LocalResponse::error(
                        id,
                        ErrorCode::NotFound,
                        format!("no session {session_id}"),
                    ),
                };
                let _ = local_tx.send(resp).await;
            }
            other => {
                let resp = admin(&remote, other).await;
                let _ = local_tx.send(resp).await;
            }
        }
    }
    // Let in-flight hook replies go out before closing.
    while hooks.join_next().await.is_some() {}
    drop(local_tx);
    drop(client);
    let _ = writer.await;
}
