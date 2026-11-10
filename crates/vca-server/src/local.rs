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
use vca_core::ClientMsg;
use vca_core::error::ErrorCode;

use crate::api::Api;
use crate::control::{
    ControlClient, LocalRequest, LocalResponse, read_line, split_stream, write_line,
};
use crate::remote::Remote;
use crate::security::{
    Caller, FailReason, GateError, GateTarget, GatedAction, Origin, Refusal, SecurityCall,
    SecurityReply, Sensitive,
};
use crate::util::error_text;

/// The remote transport, once started (it starts after the local channel).
pub type RemoteSlot = Arc<OnceLock<Arc<Remote>>>;

/// What a failed security request becomes on the older requests, which
/// answer with [`LocalResponse::Error`].
fn security_error(id: u64, reply: SecurityReply) -> LocalResponse {
    let (reason, message) = match reply {
        SecurityReply::Failed { reason, message } => (reason, message),
        SecurityReply::StepUpRequired { challenge } => {
            return LocalResponse::StepUpRequired { id, challenge };
        }
        _ => return LocalResponse::error(id, ErrorCode::Internal, "unexpected security answer"),
    };
    let code = match reason {
        FailReason::StepUpRequired => ErrorCode::StepUpRequired,
        FailReason::CurrentCodeRequired => ErrorCode::CurrentCodeRequired,
        FailReason::RemoteForbidden => ErrorCode::Forbidden,
        FailReason::TotpInvalid | FailReason::TotpReplayed => ErrorCode::Unauthorized,
        FailReason::RemoteShellOpen => ErrorCode::Conflict,
        FailReason::Conflict => ErrorCode::Conflict,
        FailReason::RateLimited | FailReason::Unavailable => ErrorCode::Unavailable,
        FailReason::InvalidRequest => ErrorCode::InvalidRequest,
        _ => ErrorCode::Internal,
    };
    LocalResponse::error(id, code, message)
}

/// The API messages that need local verification on this channel. Other
/// clients are refused them outright (conductor and task code), or use their
/// own scope (`ConductorDecide` from a paired phone). Only an approval is
/// gated: denying never needs a proof.
pub fn gated_api_target(body: &ClientMsg) -> Option<GateTarget> {
    let action = match body {
        ClientMsg::ConductorConfigure { .. }
        | ClientMsg::ConductorStart
        | ClientMsg::ConductorAllowSession { .. } => GatedAction::ConductorSetup,
        ClientMsg::ConductorDecide { allow: true, .. } => GatedAction::ConductorDecide,
        ClientMsg::ConductorStop { kill: true, .. } => GatedAction::ConductorKill,
        ClientMsg::TaskConfigure { .. } => GatedAction::TaskSettings,
        _ => return None,
    };
    Some(GateTarget::new(action, body))
}

/// The gate for a request on this channel. Without the security service
/// the answer is no: a gated action never runs unchecked.
pub(crate) async fn gate(
    api: &Api,
    id: u64,
    target: &GateTarget,
    step_up: Option<&Sensitive>,
    caller: &Caller,
) -> Result<(), LocalResponse> {
    let Some(security) = api.security() else {
        return Err(LocalResponse::error(
            id,
            ErrorCode::Unavailable,
            "security services are off",
        ));
    };
    let token = step_up.map(|t| t.0.as_str());
    match security.gate(Origin::Local, target, token, caller).await {
        Ok(()) => Ok(()),
        Err(Refusal::Remote) => Err(LocalResponse::error(
            id,
            ErrorCode::Forbidden,
            GateError::RemoteForbidden.to_string(),
        )),
        Err(Refusal::Required(challenge)) => Err(LocalResponse::StepUpRequired { id, challenge }),
    }
}

/// One security call on behalf of an older request.
async fn security_call(
    api: &Api,
    id: u64,
    call: SecurityCall,
    caller: &Caller,
) -> Result<SecurityReply, LocalResponse> {
    let Some(security) = api.security() else {
        return Err(LocalResponse::error(
            id,
            ErrorCode::Unavailable,
            "security services are off",
        ));
    };
    match security.handle_from(call, caller).await {
        reply @ (SecurityReply::Failed { .. } | SecurityReply::StepUpRequired { .. }) => {
            Err(security_error(id, reply))
        }
        reply => Ok(reply),
    }
}

/// Changes a device's permission; widening it needs local verification.
async fn device_scope(
    api: &Api,
    id: u64,
    device_id: String,
    scope: vca_core::DeviceScope,
    step_up: Option<Sensitive>,
    caller: &Caller,
) -> LocalResponse {
    let verified = api.scope_raises(&device_id, scope);
    if verified {
        let target = GateTarget::new(GatedAction::DeviceApprove, &(&device_id, scope));
        if let Err(refused) = gate(api, id, &target, step_up.as_ref(), caller).await {
            return refused;
        }
    }
    let body = api.apply_device_scope(&device_id, scope, verified).await;
    LocalResponse::Api { id, body }
}

/// Owner-only administration of the remote transport. Whatever opens this
/// channel is a process of the owner, which may be an agent or a remote
/// shell: every action that widens access checks [`gate`] here, in the
/// server, whichever client asks.
async fn admin(
    api: &Api,
    remote: &RemoteSlot,
    req: LocalRequest,
    caller: &Caller,
) -> LocalResponse {
    let id = req.id();
    let Some(remote) = remote.get() else {
        if let LocalRequest::RemoteStatus { id } = req {
            return LocalResponse::RemoteStatus {
                id,
                enabled: false,
                listen: None,
                server_fingerprint: None,
                totp_enrolled: false,
                totp_enrolled_at: None,
                totp_issuer: None,
                totp_account: None,
                backup_codes_remaining: 0,
                backup_codes_total: 0,
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
            scope,
            endpoints,
            step_up,
            ..
        } => {
            // The scope goes into the prompt: it must be one of the words.
            if !matches!(scope.as_str(), "view" | "command" | "admin") {
                return LocalResponse::error(
                    id,
                    ErrorCode::InvalidRequest,
                    "unknown scope (view, command or admin)",
                );
            }
            let target = GateTarget::new(GatedAction::PairingCreate, &(&scope, &endpoints))
                .with_detail(scope.clone());
            if let Err(refused) = gate(api, id, &target, step_up.as_ref(), caller).await {
                return refused;
            }
            match remote.pair(&scope, endpoints).await {
                Ok(p) => LocalResponse::Pairing {
                    id,
                    uri: p.uri,
                    expires_at: p.expires_at,
                    server_fingerprint: p.server_fingerprint,
                },
                Err(err) => fail(err),
            }
        }
        LocalRequest::TotpBegin {
            account, step_up, ..
        } => {
            let call = SecurityCall::TotpBegin {
                account,
                // These clients check "already enrolled" themselves.
                replace: true,
                step_up,
            };
            match security_call(api, id, call, caller).await {
                Ok(SecurityReply::TotpPending {
                    otpauth_uri,
                    secret_base32,
                }) => LocalResponse::TotpPending {
                    id,
                    otpauth_uri: otpauth_uri.0,
                    secret_base32: secret_base32.0,
                },
                Ok(reply) => security_error(id, reply),
                Err(resp) => resp,
            }
        }
        LocalRequest::TotpConfirm {
            code,
            current_code,
            step_up,
            ..
        } => {
            let call = SecurityCall::TotpConfirm {
                code: Sensitive(code),
                current_code,
                step_up,
            };
            enrolled(id, security_call(api, id, call, caller).await)
        }
        LocalRequest::TotpBackupCodes {
            current_code,
            step_up,
            ..
        } => {
            let call = SecurityCall::BackupCodes {
                current_code,
                step_up,
            };
            enrolled(id, security_call(api, id, call, caller).await)
        }
        LocalRequest::KillSwitchReset { step_up, .. } => {
            let target = GateTarget::new(GatedAction::SecuritySettings, &"kill_switch_reset");
            if let Err(refused) = gate(api, id, &target, step_up.as_ref(), caller).await {
                return refused;
            }
            match remote.reset_kill_switch("local") {
                Ok(()) => LocalResponse::Done { id },
                Err(err) => fail(err),
            }
        }
        LocalRequest::RemoteStatus { .. } => {
            let s = remote.status().await;
            let c = s.connectivity;
            let t = s.totp;
            LocalResponse::RemoteStatus {
                id,
                enabled: true,
                listen: Some(s.listen.to_string()),
                server_fingerprint: Some(s.server_fingerprint),
                totp_enrolled: s.totp_enrolled,
                totp_enrolled_at: t.as_ref().map(|t| t.enrolled_at),
                totp_issuer: t.as_ref().and_then(|t| t.issuer.clone()),
                totp_account: t.as_ref().and_then(|t| t.account.clone()),
                backup_codes_remaining: t.as_ref().map_or(0, |t| t.backup_codes_remaining),
                backup_codes_total: t.as_ref().map_or(0, |t| t.backup_codes_total),
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

fn enrolled(id: u64, reply: Result<SecurityReply, LocalResponse>) -> LocalResponse {
    match reply {
        Ok(SecurityReply::TotpEnrolled { backup_codes }) => LocalResponse::TotpEnrolled {
            id,
            backup_codes: backup_codes.into_iter().map(|c| c.0).collect(),
        },
        Ok(reply) => security_error(id, reply),
        Err(resp) => resp,
    }
}

/// The desktop shortcut and restore at sign-in (owner only).
async fn restore_local(api: &Api, req: LocalRequest) -> LocalResponse {
    use crate::control::RestoreLocalAction;
    use crate::restore::shortcut;

    let id = req.id();
    let LocalRequest::RestoreLocal { action, .. } = req else {
        return LocalResponse::error(id, ErrorCode::InvalidRequest, "unexpected request");
    };
    let Some(restore) = api.restore() else {
        return LocalResponse::error(id, ErrorCode::Unsupported, "session restore is off");
    };
    let result = tokio::task::spawn_blocking({
        let restore = restore.clone();
        move || -> Result<(), vca_core::Error> {
            match action {
                RestoreLocalAction::Status => Ok(()),
                RestoreLocalAction::CreateShortcut { name } => {
                    shortcut::create(&shortcut::desktop_shortcut(&desktop_app()?, &name))
                }
                RestoreLocalAction::SetSignIn {
                    enabled: true,
                    delay_secs,
                } => {
                    let delay = delay_secs.min(shortcut::MAX_SIGN_IN_DELAY_SECS);
                    shortcut::create(&shortcut::sign_in_shortcut(&desktop_app()?, delay))?;
                    restore.set_sign_in_delay(delay)
                }
                RestoreLocalAction::SetSignIn { enabled: false, .. } => shortcut::remove_sign_in(),
            }
        }
    })
    .await;
    match result {
        Ok(Ok(())) => LocalResponse::RestoreLocal {
            id,
            supported: cfg!(windows),
            sign_in: shortcut::sign_in_enabled(),
            sign_in_delay_secs: restore.sign_in_delay(),
        },
        Ok(Err(err)) => LocalResponse::error(id, err.code(), error_text(&err)),
        Err(err) => LocalResponse::error(id, ErrorCode::Internal, err.to_string()),
    }
}

/// The desktop app next to this program, which the shortcuts start with
/// `--restore` (vcad never writes a shortcut that starts itself).
fn desktop_app() -> Result<PathBuf, vca_core::Error> {
    let vcad = std::env::current_exe().map_err(|e| {
        vca_core::Error::api(ErrorCode::Internal, format!("cannot locate vcad: {e}"))
    })?;
    crate::restore::shortcut::desktop_app_next_to(&vcad).ok_or_else(|| {
        vca_core::Error::api(
            ErrorCode::Unsupported,
            "restore shortcuts open the Voice Command AI desktop app, which is not              installed next to this server",
        )
    })
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

/// An API message that may need local verification. `None`: it passed, and
/// the API answers on its own.
async fn gated_api(
    api: &Api,
    client: &Arc<crate::api::Client>,
    id: u64,
    body: ClientMsg,
    step_up: Option<&Sensitive>,
    caller: &Caller,
) -> Option<LocalResponse> {
    if let Some(target) = gated_api_target(&body)
        && let Err(refused) = gate(api, id, &target, step_up, caller).await
    {
        return Some(refused);
    }
    api.handle(client, vca_core::Envelope { id, body }).await;
    None
}

/// Before a `ConductorAsk`: whether its process is the owner's own, outside
/// every session and remote shell. Only then may the instruction
/// pre-approve the conductor's sends (`conductor::grant`); text an agent
/// sends from inside a session never does.
async fn mark_instruction_caller(
    api: &Api,
    client: &crate::api::Client,
    body: &ClientMsg,
    peer_pid: Option<u32>,
) {
    if matches!(body, ClientMsg::ConductorAsk { .. }) {
        let caller = crate::peer::caller(api, peer_pid).await;
        client.set_untrusted_caller(
            caller.pid.is_none() || caller.in_session || caller.in_remote_shell,
        );
    }
}

async fn serve(stream: Stream, api: Api, remote: RemoteSlot) {
    // The process on the other end, where the operating system can name it.
    // The id is signed on Unix.
    #[allow(clippy::useless_conversion)]
    let peer_pid = stream
        .peer_creds()
        .ok()
        .and_then(|creds| creds.pid())
        .and_then(|pid| u32::try_from(pid).ok());
    let (recv, mut send) = split_stream(stream);
    let mut recv = BufReader::new(recv);
    let (client, mut outbound) = api.connect("local");
    // Unproven until a `ConductorAsk` names its process.
    client.set_untrusted_caller(true);
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
                mark_instruction_caller(&api, &client, &body, peer_pid).await;
                // These need a token, which this request cannot carry.
                if gated_api_target(&body).is_some() {
                    let caller = crate::peer::caller(&api, peer_pid).await;
                    let resp = gated_api(&api, &client, id, body, None, &caller).await;
                    if let Some(resp) = resp {
                        let _ = local_tx.send(resp).await;
                    }
                    continue;
                }
                api.handle(&client, vca_core::Envelope { id, body }).await;
            }
            LocalRequest::GatedApi { id, body, step_up } => {
                mark_instruction_caller(&api, &client, &body, peer_pid).await;
                let caller = crate::peer::caller(&api, peer_pid).await;
                let resp = gated_api(&api, &client, id, body, step_up.as_ref(), &caller).await;
                if let Some(resp) = resp {
                    let _ = local_tx.send(resp).await;
                }
            }
            LocalRequest::Hook {
                id,
                session_id,
                hook_token,
                event,
            } => {
                let mgr = api.sessions().clone();
                let tx = local_tx.clone();
                hooks.spawn(async move {
                    let token = hook_token.as_ref().map(|t| t.0.as_str());
                    let decision = mgr.handle_hook(session_id, token, *event).await;
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
            req @ (LocalRequest::CloudCredentialSet { .. }
            | LocalRequest::CloudCredentialDelete { .. }) => {
                let resp = crate::cloud::local_request(&api, req).await;
                let _ = local_tx.send(resp).await;
            }
            req @ (LocalRequest::SpeechCredentialSet { .. }
            | LocalRequest::SpeechCredentialDelete { .. }) => {
                let resp = api.speech_local_request(req);
                let _ = local_tx.send(resp).await;
            }
            req @ (LocalRequest::TtsCredentialSet { .. }
            | LocalRequest::TtsCredentialDelete { .. }) => {
                let resp = api.tts_local_request(req);
                let _ = local_tx.send(resp).await;
            }
            req @ LocalRequest::RestoreLocal { .. } => {
                let resp = restore_local(&api, req).await;
                let _ = local_tx.send(resp).await;
            }
            req @ (LocalRequest::EnginesGet { .. }
            | LocalRequest::EngineSave { .. }
            | LocalRequest::EngineGatewayToken { .. }
            | LocalRequest::EngineRemove { .. }
            | LocalRequest::EngineSetDefault { .. }
            | LocalRequest::EngineTest { .. }) => {
                let caller = crate::peer::caller(&api, peer_pid).await;
                let resp = crate::engines::handle_local(&api, &client, req, &caller).await;
                let _ = local_tx.send(resp).await;
            }
            // A send may wait minutes for the person: answer from a task.
            LocalRequest::Conductor { id, token, call } => {
                let (api, tx) = (api.clone(), local_tx.clone());
                tokio::spawn(async move {
                    let token = token.as_ref().map(|t| t.0.as_str());
                    let resp = crate::conductor::local_call(&api, id, token, call).await;
                    let _ = tx.send(resp).await;
                });
            }
            // An OS prompt waits for the person: answer from a task.
            LocalRequest::Security { id, call } => {
                let (api, tx) = (api.clone(), local_tx.clone());
                tokio::spawn(async move {
                    let caller = crate::peer::caller(&api, peer_pid).await;
                    let resp = match api.security() {
                        Some(security) => LocalResponse::Security {
                            id,
                            reply: Box::new(security.handle_from(*call, &caller).await),
                        },
                        None => LocalResponse::error(
                            id,
                            ErrorCode::Unsupported,
                            "security services are off",
                        ),
                    };
                    let _ = tx.send(resp).await;
                });
            }
            LocalRequest::Shutdown { id } => {
                tracing::info!("shutdown requested over the control channel");
                let _ = local_tx.send(LocalResponse::Done { id }).await;
                crate::daemon::request_shutdown();
            }
            LocalRequest::DeviceScope {
                id,
                device_id,
                scope,
                step_up,
            } => {
                let caller = crate::peer::caller(&api, peer_pid).await;
                let resp = device_scope(&api, id, device_id, scope, step_up, &caller).await;
                let _ = local_tx.send(resp).await;
            }
            // Polled often, and gated by nothing.
            status @ LocalRequest::RemoteStatus { .. } => {
                let resp = admin(&api, &remote, status, &Caller::default()).await;
                let _ = local_tx.send(resp).await;
            }
            other => {
                let caller = crate::peer::caller(&api, peer_pid).await;
                let resp = admin(&api, &remote, other, &caller).await;
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
