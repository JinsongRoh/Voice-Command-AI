//! Desktop only: the VCA server (vcad) on this PC.
//!
//! * Runs the bundled vcad ([`sidecar`]) at startup when it is not running
//!   yet and the "Run the VCA server on this PC" setting is on (default),
//!   and starts or stops it on request. A vcad started elsewhere (`vca
//!   daemon`) is used as is and never stopped.
//! * Creates pairing codes for phones (`vca_host_pair`) and enrolls TOTP,
//!   over the owner-only local control channel, exactly like `vca pair` and
//!   `vca totp enroll`. The webview never gets the control channel itself:
//!   only the specific commands below, with typed arguments.
//! * When phones reach this PC through a DNSPilot tunnel, adds a DNSPilot
//!   invite to the pairing code, so the phone registers its key with the
//!   same scan. Only this app holds the OAuth token, not vcad.
//! * Shows this PC's sessions without pairing: [`local_link`] exposes the
//!   local vcad to the webview as the machine `"local"`.
//!
//! Commands: `vca_host_status`, `vca_host_pair`, `vca_host_start`,
//! `vca_host_stop`, `vca_host_set_run_server`, `vca_host_totp_begin`,
//! `vca_host_totp_confirm`, `vca_host_totp_backup_codes`,
//! `vca_host_restore_status`,
//! `vca_host_restore_shortcut` and `vca_host_restore_sign_in`. The `vca-host-changed` event fires when the
//! local server comes up or goes away. Speech engine setup:
//! `vca_host_engine*` (see [`engines`]). Agent hooks for exact approval
//! alerts: `vca_host_hooks_*` (see [`hooks`]).

mod control;
#[cfg(test)]
mod e2e;
pub mod engines;
pub mod hooks;
mod local_link;
mod qr;
pub mod security;
mod sidecar;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::watch;
use vca_client::dnspilot::{DnsPilotManager, PairingInvite};
use vca_client::{ClientHub, LinkSink, LinkStatus, MachineInfo};
use vca_core::{ClientMsg, PROTOCOL_VERSION};

use crate::{CommandError, CommandResult};
use control::{ControlConn, ControlError, Request, Response, RestoreLocalAction, SecretStr};
pub use local_link::{LOCAL_MACHINE_ID, SendError};
use sidecar::Sidecar;

/// Event emitted with `true` / `false` when the local server's
/// reachability changes.
pub const HOST_CHANGED_EVENT: &str = "vca-host-changed";

const SETTINGS_FILE: &str = "host-settings.json";
/// How long a freshly started vcad may take to answer.
const START_TIMEOUT: Duration = Duration::from_secs(12);
/// How long vcad may take to stop after a shutdown request (its own grace
/// period for managed sessions is 5 s by default).
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(200);
const MONITOR_INTERVAL: Duration = Duration::from_secs(4);
/// How long `vca_machines` waits for the startup check.
const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// Smallest side of the QR codes, in pixels.
const QR_PX: u32 = 320;
/// TOTP account label (as `vca totp enroll`).
const TOTP_ACCOUNT: &str = "owner";
/// `remote.provider` of a vcad that serves through DNSPilot. vcad reports the
/// provider's display name ("DNSPilot"), which is not the config kind.
const PROVIDER_DNSPILOT: &str = "dnspilot";

fn serves_through_dnspilot(provider: Option<&str>) -> bool {
    provider.is_some_and(|name| name.eq_ignore_ascii_case(PROVIDER_DNSPILOT))
}

/// Persistent desktop settings for the local server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSettings {
    /// Run the VCA server on this PC (start the bundled vcad at launch).
    pub run_server: bool,
}

impl Default for HostSettings {
    fn default() -> Self {
        Self { run_server: true }
    }
}

/// The local-only parts of session restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreLocalStatus {
    pub supported: bool,
    /// Restore at sign-in is on (off by default).
    pub sign_in: bool,
    pub sign_in_delay_secs: u32,
}

/// State of the remote transport (how phones reach this PC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteInfo {
    pub enabled: bool,
    pub listen: Option<String>,
    pub provider: Option<String>,
    pub provider_state: Option<String>,
    pub provider_diagnostics: Vec<String>,
    pub totp_enrolled: bool,
    /// When the authenticator was enrolled (Unix ms).
    pub totp_enrolled_at: Option<u64>,
    /// The authenticator app entry (absent for older enrollments).
    pub totp_issuer: Option<String>,
    pub totp_account: Option<String>,
    pub backup_codes_remaining: usize,
    pub backup_codes_total: usize,
    pub require_totp: bool,
    pub kill_switch_tripped: bool,
    pub paired_devices: usize,
    pub connections: usize,
}

/// `vca_host_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostStatus {
    /// vcad answers on the control channel.
    pub running: bool,
    /// The control endpoint (named pipe or socket path).
    pub endpoint: String,
    pub server_name: Option<String>,
    /// vcad's version.
    pub version: Option<String>,
    pub remote: Option<RemoteInfo>,
    /// The running vcad was started by this app (so it may stop it).
    pub managed: bool,
    /// The bundled vcad is present (so it can be started).
    pub can_start: bool,
    /// The "Run the VCA server on this PC" setting.
    pub run_server: bool,
    /// Where the bundled vcad logs.
    pub log_path: Option<String>,
}

/// `vca_host_pair`. `Debug` hides the link and its QR code.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct HostPairing {
    /// `vca://pair?...`; contains the one-time secret, and with
    /// `invite_included` a DNSPilot invite code.
    pub uri: String,
    /// Unix seconds.
    pub expires_at: u64,
    /// Where phones will connect (from the code itself).
    pub endpoints: Vec<String>,
    /// Every endpoint is a loopback address: phones cannot reach it.
    pub loopback_only: bool,
    /// The QR code of `uri`, as an SVG document.
    pub qr_svg: String,
    pub scope: PairScope,
    pub server_fingerprint: String,
    /// Shown next to the QR code; the phone shows the same one.
    pub confirm_code: String,
    /// The code also registers the phone on the DNSPilot tunnel.
    pub invite_included: bool,
    /// Why a code that goes through DNSPilot has no invite: `not_connected`,
    /// `not_owned`, `rate_limited`, `invite_limit` or `failed`.
    pub invite_skipped: Option<String>,
}

impl std::fmt::Debug for HostPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostPairing")
            .field("expires_at", &self.expires_at)
            .field("endpoints", &self.endpoints)
            .field("scope", &self.scope)
            .field("invite_included", &self.invite_included)
            .field("invite_skipped", &self.invite_skipped)
            .finish_non_exhaustive()
    }
}

/// `vca_host_totp_begin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TotpEnrollment {
    pub otpauth_uri: String,
    pub secret: String,
    pub qr_svg: String,
}

/// `vca_host_totp_confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TotpEnrolled {
    pub backup_codes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairScope {
    View,
    Command,
    Admin,
}

impl PairScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Command => "command",
            Self::Admin => "admin",
        }
    }
}

impl From<ControlError> for CommandError {
    fn from(e: ControlError) -> Self {
        match &e {
            ControlError::Unreachable(_) => CommandError::new("not_running", e.to_string()),
            ControlError::Io(_) => CommandError::new("unreachable", e.to_string()),
            ControlError::Server { code, message } => {
                // The remote transport is off in the configuration.
                let code = if code == "unsupported" && message.contains("remote transport") {
                    "remote_off"
                } else {
                    code.as_str()
                };
                CommandError::new(code, message.clone())
            }
            ControlError::Unexpected(_) => CommandError::new("protocol", e.to_string()),
        }
    }
}

impl From<SendError> for CommandError {
    fn from(e: SendError) -> Self {
        match e {
            SendError::Protocol => {
                CommandError::new("protocol", "not a valid MessagePack client envelope")
            }
            SendError::NotConnected => CommandError::new(
                "not_connected",
                "not connected to the VCA server on this PC",
            ),
        }
    }
}

/// Maps the control channel's `remote_status` answer.
fn remote_info(resp: Response) -> Option<RemoteInfo> {
    match resp {
        Response::RemoteStatus {
            enabled,
            listen,
            totp_enrolled,
            totp_enrolled_at,
            totp_issuer,
            totp_account,
            backup_codes_remaining,
            backup_codes_total,
            require_totp,
            kill_switch_tripped,
            paired_devices,
            connections,
            provider,
            provider_state,
            provider_diagnostics,
            ..
        } => Some(RemoteInfo {
            enabled,
            listen,
            provider,
            provider_state,
            provider_diagnostics,
            totp_enrolled,
            totp_enrolled_at,
            totp_issuer,
            totp_account,
            backup_codes_remaining,
            backup_codes_total,
            require_totp,
            kill_switch_tripped,
            paired_devices,
            connections,
        }),
        _ => None,
    }
}

/// `(server_name, version)` from a `welcome` body.
fn welcome(body: &serde_json::Value) -> Option<(String, String)> {
    if body.get("type")?.as_str()? != "welcome" {
        return None;
    }
    Some((
        body.get("server_name")?.as_str()?.to_owned(),
        body.get("server_version")?.as_str()?.to_owned(),
    ))
}

/// True for `ws://127.0.0.1:47800/v1/ws`, `ws://localhost/...`,
/// `ws://[::1]:1/...` and the like.
fn is_loopback_url(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn loopback_only(endpoints: &[String]) -> bool {
    !endpoints.is_empty() && endpoints.iter().all(|e| is_loopback_url(e))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The local server, managed as Tauri state on desktop.
pub struct Host {
    endpoint: PathBuf,
    dir: PathBuf,
    sidecar: Sidecar,
    settings: Mutex<HostSettings>,
    /// The server name, once vcad was reachable during this run. The
    /// machine "local" is listed from then on.
    seen: Mutex<Option<String>>,
    reachable: Mutex<bool>,
    ready: watch::Sender<bool>,
    link: local_link::LocalLink,
    app: OnceLock<AppHandle>,
    /// Serializes start and stop.
    lifecycle: tokio::sync::Mutex<()>,
}

impl Host {
    /// `dir`: the app's data directory for host state (settings, ownership
    /// record, vcad log).
    pub fn new(dir: PathBuf) -> Self {
        Self::with(dir, control::default_endpoint(), sidecar::bundled_vcad())
    }

    fn with(dir: PathBuf, endpoint: PathBuf, vcad: Option<PathBuf>) -> Self {
        let settings = std::fs::read(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            sidecar: Sidecar::open(dir.clone(), vcad),
            link: local_link::LocalLink::new(endpoint.clone()),
            endpoint,
            dir,
            settings: Mutex::new(settings),
            seen: Mutex::new(None),
            reachable: Mutex::new(false),
            ready: watch::channel(false).0,
            app: OnceLock::new(),
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }

    /// Checks for vcad, starts the bundled one if configured, then keeps
    /// watching it. Called once from the app's setup.
    pub fn start_background(self: &Arc<Self>, app: AppHandle) {
        let _ = self.app.set(app);
        let host = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            if host.probe().await.is_none() && host.settings().run_server {
                if host.sidecar.exe().is_some() {
                    if let Err(e) = host.start().await {
                        tracing::warn!("cannot start the VCA server: {}", e.message);
                    }
                } else {
                    tracing::info!("no bundled vcad; not starting a VCA server");
                }
            }
            host.ready.send_replace(true);
            loop {
                tokio::time::sleep(MONITOR_INTERVAL).await;
                host.probe().await;
            }
        });
    }

    pub fn settings(&self) -> HostSettings {
        lock(&self.settings).clone()
    }

    fn set_run_server(&self, run_server: bool) -> std::io::Result<()> {
        let settings = HostSettings { run_server };
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(
            self.dir.join(SETTINGS_FILE),
            serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?,
        )?;
        *lock(&self.settings) = settings;
        Ok(())
    }

    /// Pings vcad (`hello`); records reachability and emits the change event.
    async fn probe(&self) -> Option<(String, String)> {
        let found = match ControlConn::connect(&self.endpoint).await {
            Ok(mut conn) => conn
                .api(&ClientMsg::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    app_version: env!("CARGO_PKG_VERSION").to_owned(),
                })
                .await
                .ok()
                .and_then(|body| welcome(&body)),
            Err(_) => None,
        };
        if let Some((name, _)) = &found {
            *lock(&self.seen) = Some(name.clone());
        }
        let now = found.is_some();
        let changed = std::mem::replace(&mut *lock(&self.reachable), now) != now;
        if changed {
            if let Some(app) = self.app.get() {
                let _ = app.emit(HOST_CHANGED_EVENT, now);
            }
        }
        found
    }

    /// What the approval alert watcher needs from vcad: the `sessions` body
    /// and, from a vcad that knows the message, the `push_phones` body.
    /// `None` while vcad does not answer.
    pub(crate) async fn push_snapshot(
        &self,
    ) -> Option<(serde_json::Value, Option<serde_json::Value>)> {
        let mut conn = ControlConn::connect(&self.endpoint).await.ok()?;
        let sessions = conn.api(&ClientMsg::ListSessions).await.ok()?;
        let phones = conn
            .api_value(&serde_json::json!({ "type": "list_push_phones" }))
            .await
            .ok();
        Some((sessions, phones))
    }

    /// The machine entry for this PC, once vcad was seen (waits for the
    /// startup check first).
    pub async fn local_machine(&self) -> Option<MachineInfo> {
        let mut ready = self.ready.subscribe();
        let _ = tokio::time::timeout(READY_TIMEOUT, ready.wait_for(|r| *r)).await;
        lock(&self.seen).as_deref().map(local_link::machine_info)
    }

    pub async fn status(&self) -> HostStatus {
        let mut status = HostStatus {
            running: false,
            endpoint: self.endpoint.display().to_string(),
            server_name: None,
            version: None,
            remote: None,
            managed: false,
            can_start: self.sidecar.exe().is_some(),
            run_server: self.settings().run_server,
            log_path: self
                .sidecar
                .exe()
                .map(|_| self.sidecar.log_path().display().to_string()),
        };
        if let Some((name, version)) = self.probe().await {
            status.running = true;
            status.server_name = Some(name);
            status.version = Some(version);
            status.managed = self.sidecar.owned_pid().is_some();
            if let Ok(mut conn) = ControlConn::connect(&self.endpoint).await {
                let id = conn.next_id();
                if let Ok(resp) = conn.call(&Request::RemoteStatus { id }).await {
                    status.remote = remote_info(resp);
                }
            }
        }
        status
    }

    async fn remote_status(conn: &mut ControlConn) -> CommandResult<RemoteInfo> {
        let id = conn.next_id();
        remote_info(conn.call(&Request::RemoteStatus { id }).await?)
            .ok_or_else(|| CommandError::new("protocol", "unexpected answer to remote_status"))
    }

    /// Creates a pairing code. With `dnspilot` (the app's own DNSPilot
    /// connection) and a vcad that serves through DNSPilot, the code also
    /// carries an invite when one can be made; otherwise it is a plain code.
    pub async fn pair(
        &self,
        scope: PairScope,
        dnspilot: Option<&DnsPilotManager>,
    ) -> CommandResult<HostPairing> {
        let mut conn = ControlConn::connect(&self.endpoint).await?;
        let remote = Self::remote_status(&mut conn).await?;
        check_pairable(&remote)?;
        let held = security::held_token();
        let id = conn.next_id();
        let resp = conn
            .call(&Request::Pair {
                id,
                scope: scope.as_str(),
                endpoints: Vec::new(),
                step_up: held.as_deref().map(SecretStr),
            })
            .await?;
        let Response::Pairing {
            uri,
            expires_at,
            server_fingerprint,
            ..
        } = resp
        else {
            return Err(CommandError::new("protocol", "unexpected answer to pair"));
        };
        let plain = pairing(uri, expires_at, server_fingerprint, scope)?;
        match dnspilot {
            Some(manager) if serves_through_dnspilot(remote.provider.as_deref()) => {
                let invite = manager.pairing_invite(&plain.endpoints).await;
                Ok(with_invite(plain, invite))
            }
            _ => Ok(plain),
        }
    }

    /// Starts an enrollment. With an authenticator already enrolled this
    /// needs `replace`; the old one keeps working until the new code is
    /// confirmed ([`Self::totp_confirm`]).
    pub async fn totp_begin(&self, replace: bool) -> CommandResult<TotpEnrollment> {
        let mut conn = ControlConn::connect(&self.endpoint).await?;
        let remote = Self::remote_status(&mut conn).await?;
        check_enrollable(&remote, replace)?;
        let held = security::held_token();
        let id = conn.next_id();
        let resp = conn
            .call(&Request::TotpBegin {
                id,
                account: TOTP_ACCOUNT,
                step_up: held.as_deref().map(SecretStr),
            })
            .await?;
        let Response::TotpPending {
            otpauth_uri,
            secret_base32,
            ..
        } = resp
        else {
            return Err(CommandError::new(
                "protocol",
                "unexpected answer to totp_begin",
            ));
        };
        let qr_svg = qr::svg(&otpauth_uri, QR_PX)
            .ok_or_else(|| CommandError::new("internal", "cannot draw the QR code"))?;
        Ok(TotpEnrollment {
            otpauth_uri,
            secret: secret_base32,
            qr_svg,
        })
    }

    pub async fn totp_confirm(
        &self,
        code: &str,
        current_code: Option<&str>,
    ) -> CommandResult<TotpEnrolled> {
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if code.is_empty() || code.len() > 10 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err(CommandError::new(
                "totp_invalid",
                "enter the 6-digit code from the authenticator app",
            ));
        }
        let mut conn = ControlConn::connect(&self.endpoint).await?;
        let id = conn.next_id();
        let held = security::held_token();
        let req = Request::TotpConfirm {
            id,
            code: &code,
            current_code: current_code.map(SecretStr),
            step_up: held.as_deref().map(SecretStr),
        };
        match conn.call(&req).await {
            Ok(Response::TotpEnrolled { backup_codes, .. }) => Ok(TotpEnrolled { backup_codes }),
            Ok(_) => Err(CommandError::new(
                "protocol",
                "unexpected answer to totp_confirm",
            )),
            Err(ControlError::Server { code, message }) if code == "unauthorized" => {
                Err(CommandError::new("totp_invalid", message))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Replaces the backup codes; returns the new ones (shown once).
    pub async fn totp_backup_codes(
        &self,
        current_code: Option<&str>,
    ) -> CommandResult<TotpEnrolled> {
        let mut conn = ControlConn::connect(&self.endpoint).await?;
        let id = conn.next_id();
        let held = security::held_token();
        let req = Request::TotpBackupCodes {
            id,
            current_code: current_code.map(SecretStr),
            step_up: held.as_deref().map(SecretStr),
        };
        match conn.call(&req).await? {
            Response::TotpEnrolled { backup_codes, .. } => Ok(TotpEnrolled { backup_codes }),
            _ => Err(CommandError::new(
                "protocol",
                "unexpected answer to totp_backup_codes",
            )),
        }
    }

    /// The desktop shortcut and restore at sign-in. Local only: these go
    /// over the owner-only channel, never through a paired connection.
    async fn restore_local(
        &self,
        action: RestoreLocalAction<'_>,
    ) -> CommandResult<RestoreLocalStatus> {
        let mut conn = ControlConn::connect(&self.endpoint).await?;
        let id = conn.next_id();
        match conn.call(&Request::RestoreLocal { id, action }).await? {
            Response::RestoreLocal {
                supported,
                sign_in,
                sign_in_delay_secs,
                ..
            } => Ok(RestoreLocalStatus {
                supported,
                sign_in,
                sign_in_delay_secs,
            }),
            _ => Err(CommandError::new(
                "protocol",
                "unexpected answer to restore_local",
            )),
        }
    }

    /// Starts the bundled vcad unless one is running already.
    pub async fn start(&self) -> CommandResult<HostStatus> {
        {
            let _guard = self.lifecycle.lock().await;
            if self.probe().await.is_none() {
                self.sidecar.spawn(&self.endpoint).map_err(|e| {
                    let code = if e.kind() == std::io::ErrorKind::NotFound {
                        "sidecar_missing"
                    } else {
                        "start_failed"
                    };
                    CommandError::new(code, e.to_string())
                })?;
                self.wait_started().await?;
            }
        }
        Ok(self.status().await)
    }

    async fn wait_started(&self) -> CommandResult<()> {
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        loop {
            if self.probe().await.is_some() {
                return Ok(());
            }
            if let Some(status) = self.sidecar.exited() {
                return Err(CommandError::new(
                    "start_failed",
                    format!(
                        "the VCA server exited during startup ({status}); see {}",
                        self.sidecar.log_path().display()
                    ),
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CommandError::new(
                    "start_failed",
                    format!(
                        "the VCA server did not answer within {} s; see {}",
                        START_TIMEOUT.as_secs(),
                        self.sidecar.log_path().display()
                    ),
                ));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Stops the vcad this app started: a graceful shutdown request first,
    /// then (if it does not exit in time) that process only.
    pub async fn stop(&self) -> CommandResult<HostStatus> {
        {
            let _guard = self.lifecycle.lock().await;
            if self.sidecar.owned_pid().is_none() {
                return Err(CommandError::new(
                    "not_owned",
                    "this VCA server was not started by the app; stop it where it was started",
                ));
            }
            if let Ok(mut conn) = ControlConn::connect(&self.endpoint).await {
                let id = conn.next_id();
                // vcad may close the connection before answering.
                let _ = conn.call(&Request::Shutdown { id }).await;
            }
            if !self.wait_exited(STOP_TIMEOUT).await {
                tracing::warn!("vcad did not stop in time; ending the process the app started");
                self.sidecar.kill();
                self.wait_exited(Duration::from_secs(5)).await;
            }
            self.probe().await;
        }
        Ok(self.status().await)
    }

    async fn wait_exited(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.sidecar.owned_pid().is_some() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
        true
    }

    pub fn connect_link(&self, sink: Arc<dyn LinkSink>) {
        self.link.connect(sink);
    }

    pub fn link_status(&self) -> LinkStatus {
        self.link.status()
    }

    pub async fn link_send(&self, msgpack: &[u8]) -> Result<(), SendError> {
        self.link.send_from(msgpack, self.sidecar.owned_pid()).await
    }

    pub fn link_disconnect(&self) {
        self.link.disconnect();
    }

    #[cfg(test)]
    fn endpoint(&self) -> &std::path::Path {
        &self.endpoint
    }
}

/// The desktop machine list: the paired machines (other computers, in
/// pairing order) followed by this PC's own server, once it was seen. This
/// PC is one more entry, never a replacement: remote machines stay listed
/// and usable whether or not the local server runs.
pub fn machine_list(paired: Vec<MachineInfo>, local: Option<MachineInfo>) -> Vec<MachineInfo> {
    let mut machines: Vec<MachineInfo> = paired
        .into_iter()
        .filter(|m| m.id != LOCAL_MACHINE_ID)
        .collect();
    machines.extend(local);
    machines
}

/// An enrollment needs the remote transport; replacing an enrolled
/// authenticator must be asked for explicitly.
fn check_enrollable(remote: &RemoteInfo, replace: bool) -> CommandResult<()> {
    if !remote.enabled {
        return Err(CommandError::new(
            "remote_off",
            "the remote transport is off in the server configuration",
        ));
    }
    if remote.totp_enrolled && !replace {
        return Err(CommandError::new(
            "totp_enrolled",
            "an authenticator is already enrolled; choose to set it up again to replace it",
        ));
    }
    Ok(())
}

/// Pairing would fail at the phone's first login without these.
fn check_pairable(remote: &RemoteInfo) -> CommandResult<()> {
    if !remote.enabled {
        return Err(CommandError::new(
            "remote_off",
            "the remote transport is off in the server configuration ([remote] enabled = false)",
        ));
    }
    if remote.require_totp && !remote.totp_enrolled {
        return Err(CommandError::new(
            "totp_not_enrolled",
            "the server requires an authenticator (TOTP); enroll one first",
        ));
    }
    if remote.kill_switch_tripped {
        return Err(CommandError::new(
            "kill_switch",
            "the kill switch is engaged; reset it with `vca kill-switch --reset`",
        ));
    }
    Ok(())
}

fn pairing(
    uri: String,
    expires_at: u64,
    server_fingerprint: String,
    scope: PairScope,
) -> CommandResult<HostPairing> {
    let (endpoints, confirm_code) = vca_net::PairingPayload::parse(&uri)
        .map(|p| (p.endpoints.clone(), p.confirm_code()))
        .map_err(|e| CommandError::new("protocol", format!("vcad sent a bad pairing link: {e}")))?;
    let qr_svg = qr::svg(&uri, QR_PX).ok_or_else(|| {
        CommandError::new("internal", "the pairing link is too long for a QR code")
    })?;
    Ok(HostPairing {
        loopback_only: loopback_only(&endpoints),
        uri,
        expires_at,
        endpoints,
        qr_svg,
        scope,
        server_fingerprint,
        confirm_code,
        invite_included: false,
        invite_skipped: None,
    })
}

/// Adds the outcome of the invite step. A failure there never costs the
/// plain pairing code.
fn with_invite(plain: HostPairing, invite: PairingInvite) -> HostPairing {
    match invite {
        PairingInvite::NotApplicable => plain,
        PairingInvite::Skipped { reason } => HostPairing {
            invite_skipped: Some(reason.to_owned()),
            ..plain
        },
        PairingInvite::Included(link) => {
            let uri = link.append_to_pairing_uri(&plain.uri);
            match qr::svg(&uri, QR_PX) {
                Some(qr_svg) => HostPairing {
                    uri,
                    qr_svg,
                    invite_included: true,
                    ..plain
                },
                None => HostPairing {
                    invite_skipped: Some("failed".to_owned()),
                    ..plain
                },
            }
        }
    }
}

type HostState<'a> = State<'a, Arc<Host>>;

#[tauri::command]
pub async fn vca_host_status(host: HostState<'_>) -> CommandResult<HostStatus> {
    Ok(host.status().await)
}

#[tauri::command]
pub async fn vca_host_pair(
    host: HostState<'_>,
    hub: State<'_, Arc<ClientHub>>,
    scope: PairScope,
) -> CommandResult<HostPairing> {
    host.pair(scope, Some(hub.dnspilot())).await
}

#[tauri::command]
pub async fn vca_host_start(host: HostState<'_>) -> CommandResult<HostStatus> {
    host.start().await
}

#[tauri::command]
pub async fn vca_host_stop(host: HostState<'_>) -> CommandResult<HostStatus> {
    host.stop().await
}

#[tauri::command]
pub fn vca_host_set_run_server(host: HostState<'_>, enabled: bool) -> CommandResult<()> {
    host.set_run_server(enabled)
        .map_err(|e| CommandError::new("storage", e.to_string()))
}

#[tauri::command]
pub async fn vca_host_totp_begin(
    host: HostState<'_>,
    replace: Option<bool>,
) -> CommandResult<TotpEnrollment> {
    host.totp_begin(replace.unwrap_or(false)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_totp_confirm(
    host: HostState<'_>,
    code: String,
    current_code: Option<String>,
) -> CommandResult<TotpEnrolled> {
    host.totp_confirm(&code, current_code.as_deref()).await
}

#[tauri::command]
pub async fn vca_host_totp_backup_codes(
    host: HostState<'_>,
    current_code: Option<String>,
) -> CommandResult<TotpEnrolled> {
    host.totp_backup_codes(current_code.as_deref()).await
}

#[tauri::command]
pub async fn vca_host_restore_status(host: HostState<'_>) -> CommandResult<RestoreLocalStatus> {
    host.restore_local(RestoreLocalAction::Status).await
}

#[tauri::command]
pub async fn vca_host_restore_shortcut(
    host: HostState<'_>,
    name: String,
) -> CommandResult<RestoreLocalStatus> {
    host.restore_local(RestoreLocalAction::CreateShortcut { name: &name })
        .await
}

#[tauri::command]
pub async fn vca_host_restore_sign_in(
    host: HostState<'_>,
    enabled: bool,
    delay_secs: u32,
) -> CommandResult<RestoreLocalStatus> {
    host.restore_local(RestoreLocalAction::SetSignIn {
        enabled,
        delay_secs,
    })
    .await
}

/// Arguments of the restore shortcuts: `--restore [--restore-delay <s>]`.
/// Returns the delay in seconds when `--restore` is present.
pub fn restore_flag(args: impl IntoIterator<Item = String>) -> Option<u64> {
    let mut restore = false;
    let mut delay = 0;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--restore" => restore = true,
            "--restore-delay" => {
                delay = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            _ => {}
        }
    }
    restore.then_some(delay.min(u64::from(MAX_RESTORE_DELAY_SECS)))
}

/// Longest wait accepted from a shortcut (as the server's own limit).
const MAX_RESTORE_DELAY_SECS: u32 = 600;
/// Lets discovery see the running sessions after a fresh start, so open
/// windows are skipped.
const DISCOVERY_SETTLE: Duration = Duration::from_secs(6);

/// The app's data directory (Tauri's `app_data_dir`) without a running app.
fn app_data_dir(identifier: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.map(|b| b.join(identifier))
}

/// What the desktop and sign-in shortcuts run (`vca-desktop --restore`):
/// without opening a window, asks the local server to reopen the saved AI
/// CLI windows, starting the bundled server first when it is not running
/// (it then belongs to the app, as if the app had started it). Returns the
/// process exit code.
pub fn run_restore(identifier: &str, delay_secs: u64) -> i32 {
    let Some(dir) = app_data_dir(identifier) else {
        return 2;
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return 1,
    };
    runtime.block_on(async move {
        tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        let host = Host::new(dir.join("host"));
        if host.probe().await.is_none() {
            if let Err(e) = host.start().await {
                tracing::warn!("restore: cannot start the VCA server: {}", e.message);
                return 1;
            }
            tokio::time::sleep(DISCOVERY_SETTLE).await;
        }
        let request = ClientMsg::RestoreRun {
            request: vca_core::RestoreRunRequest::default(),
        };
        match ControlConn::connect(&host.endpoint).await {
            Ok(mut conn) => match conn.api(&request).await {
                Ok(_) => 0,
                Err(e) => {
                    tracing::warn!("restore: {e}");
                    1
                }
            },
            Err(e) => {
                tracing::warn!("restore: {e}");
                1
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restore_flags_from_the_shortcut() {
        let args = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(restore_flag(args("vca-desktop")), None);
        assert_eq!(restore_flag(args("vca-desktop --restore")), Some(0));
        assert_eq!(
            restore_flag(args("vca-desktop --restore --restore-delay 60")),
            Some(60)
        );
        assert_eq!(
            restore_flag(args("vca-desktop --restore --restore-delay 9999")),
            Some(600)
        );
    }

    fn remote(enabled: bool, require_totp: bool, totp_enrolled: bool) -> RemoteInfo {
        RemoteInfo {
            enabled,
            listen: Some("127.0.0.1:47800".into()),
            provider: None,
            provider_state: None,
            provider_diagnostics: vec![],
            totp_enrolled,
            totp_enrolled_at: None,
            totp_issuer: None,
            totp_account: None,
            backup_codes_remaining: 0,
            backup_codes_total: 0,
            require_totp,
            kill_switch_tripped: false,
            paired_devices: 0,
            connections: 0,
        }
    }

    #[test]
    fn setting_up_again_must_be_asked_for() {
        assert!(check_enrollable(&remote(true, true, false), false).is_ok());
        assert_eq!(
            check_enrollable(&remote(true, true, true), false)
                .unwrap_err()
                .code,
            "totp_enrolled"
        );
        assert!(check_enrollable(&remote(true, true, true), true).is_ok());
        assert_eq!(
            check_enrollable(&remote(false, true, false), true)
                .unwrap_err()
                .code,
            "remote_off"
        );
    }

    #[test]
    fn maps_remote_status() {
        let resp: Response = serde_json::from_value(json!({
            "op": "remote_status", "id": 1, "enabled": true, "listen": "0.0.0.0:47800",
            "server_fingerprint": "ff", "totp_enrolled": true, "require_totp": true,
            "kill_switch_tripped": false, "paired_devices": 2, "connections": 1,
            "provider": "tailscale", "provider_state": "ready",
            "provider_diagnostics": ["serve on"], "totp_enrolled_at": 1_793_337_900_000_u64,
            "totp_issuer": "Voice Command AI", "totp_account": "owner",
            "backup_codes_remaining": 10, "backup_codes_total": 10
        }))
        .unwrap();
        let info = remote_info(resp).unwrap();
        assert_eq!(
            info,
            RemoteInfo {
                enabled: true,
                listen: Some("0.0.0.0:47800".into()),
                provider: Some("tailscale".into()),
                provider_state: Some("ready".into()),
                provider_diagnostics: vec!["serve on".into()],
                totp_enrolled: true,
                totp_enrolled_at: Some(1_793_337_900_000),
                totp_issuer: Some("Voice Command AI".into()),
                totp_account: Some("owner".into()),
                backup_codes_remaining: 10,
                backup_codes_total: 10,
                require_totp: true,
                kill_switch_tripped: false,
                paired_devices: 2,
                connections: 1,
            }
        );
        // An older vcad without provider fields, and the "remote off" answer.
        let resp: Response = serde_json::from_value(json!({
            "op": "remote_status", "id": 1, "enabled": false, "listen": null,
            "server_fingerprint": null, "totp_enrolled": false, "require_totp": false,
            "kill_switch_tripped": false, "paired_devices": 0, "connections": 0
        }))
        .unwrap();
        let info = remote_info(resp).unwrap();
        assert!(!info.enabled);
        assert_eq!(info.provider, None);
        assert!(remote_info(Response::Done { id: 1 }).is_none());
    }

    #[test]
    fn status_serializes_with_the_documented_fields() {
        let status = HostStatus {
            running: true,
            endpoint: "pipe".into(),
            server_name: Some("studio".into()),
            version: Some("0.1.0".into()),
            remote: Some(remote(true, true, false)),
            managed: true,
            can_start: true,
            run_server: true,
            log_path: None,
        };
        let v = serde_json::to_value(&status).unwrap();
        for key in [
            "running",
            "endpoint",
            "server_name",
            "version",
            "remote",
            "managed",
            "can_start",
            "run_server",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        for key in [
            "enabled",
            "listen",
            "provider",
            "provider_state",
            "require_totp",
        ] {
            assert!(v["remote"].get(key).is_some(), "remote.{key}");
        }
    }

    #[test]
    fn reads_welcome() {
        let body = json!({ "type": "welcome", "protocol_version": 1, "server_version": "0.1.0", "server_name": "studio" });
        assert_eq!(
            welcome(&body),
            Some(("studio".to_owned(), "0.1.0".to_owned()))
        );
        assert_eq!(welcome(&json!({ "type": "pong", "at": 1 })), None);
    }

    #[test]
    fn detects_loopback_endpoints() {
        for url in [
            "ws://127.0.0.1:47800/v1/ws",
            "ws://localhost:47800/v1/ws",
            "ws://[::1]:47800/v1/ws",
            "wss://LOCALHOST/v1/ws",
            "ws://127.8.0.1",
        ] {
            assert!(is_loopback_url(url), "{url}");
        }
        for url in [
            "ws://192.168.0.10:47800/v1/ws",
            "wss://pc.tail1234.ts.net/v1/ws",
            "wss://studio.dnspilot.kr/v1/ws",
            "ws://[fd00::1]:47800/v1/ws",
            "ws://localhost.example.com/v1/ws",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
        assert!(loopback_only(&["ws://127.0.0.1:47800/v1/ws".into()]));
        assert!(!loopback_only(&[
            "ws://127.0.0.1:47800/v1/ws".into(),
            "ws://192.168.0.10:47800/v1/ws".into()
        ]));
        assert!(!loopback_only(&[]));
    }

    #[test]
    fn pairing_needs_remote_and_totp() {
        assert!(check_pairable(&remote(true, true, true)).is_ok());
        assert!(check_pairable(&remote(true, false, false)).is_ok());
        assert_eq!(
            check_pairable(&remote(false, true, true)).unwrap_err().code,
            "remote_off"
        );
        assert_eq!(
            check_pairable(&remote(true, true, false)).unwrap_err().code,
            "totp_not_enrolled"
        );
        let mut killed = remote(true, false, false);
        killed.kill_switch_tripped = true;
        assert_eq!(check_pairable(&killed).unwrap_err().code, "kill_switch");
    }

    fn plain_pairing() -> HostPairing {
        let key = vca_net::StaticKeypair::generate();
        let uri = vca_net::PairingPayload {
            server_id: vca_net::pairing::server_id(key.public()),
            server_key: *key.public(),
            server_name: "pc".into(),
            secret: zeroize::Zeroizing::new(vec![9; 32]),
            expires_at: 1_700_000_300,
            endpoints: vec!["wss://vca-a.example.com/v1/ws".into()],
            totp_required: false,
        }
        .to_uri();
        pairing(uri, 1_700_000_300, "ff".into(), PairScope::Command).unwrap()
    }

    #[test]
    fn an_invite_is_added_to_the_pairing_link_and_its_qr_code() {
        let plain = plain_pairing();
        let code = format!("dpt_inv_{}", "a".repeat(43));
        let link =
            vca_client::dnspilot::EnrollLink::new("vca-a.example.com", 12, code.as_str().into())
                .unwrap();
        let with = with_invite(plain.clone(), PairingInvite::Included(link.clone()));
        assert!(with.invite_included && with.invite_skipped.is_none());
        assert_ne!(with.qr_svg, plain.qr_svg);
        assert_eq!(
            vca_client::dnspilot::EnrollLink::from_pairing_uri(&with.uri).unwrap(),
            Some(link)
        );
        // Parsers that know nothing about invites read the same pairing code.
        let old = vca_net::PairingPayload::parse(&plain.uri).unwrap();
        let new = vca_net::PairingPayload::parse(&with.uri).unwrap();
        assert_eq!(old.endpoints, new.endpoints);
        assert_eq!(old.expires_at, new.expires_at);
        assert_eq!(*old.secret, *new.secret);
        assert_eq!(
            with.expires_at, plain.expires_at,
            "the pairing code stays the same"
        );
        // Debug output (and so any log of it) shows neither secret.
        assert!(!format!("{with:?}").contains(&code));
        assert!(!format!("{with:?}").contains("vca://"));
    }

    #[test]
    fn recognizes_the_provider_name_vcad_reports() {
        assert!(serves_through_dnspilot(Some("DNSPilot")));
        assert!(serves_through_dnspilot(Some("dnspilot")));
        assert!(!serves_through_dnspilot(Some("Tailscale")));
        assert!(!serves_through_dnspilot(None));
    }

    #[test]
    fn a_failed_invite_keeps_the_plain_pairing_code() {
        let plain = plain_pairing();
        let skipped = with_invite(
            plain.clone(),
            PairingInvite::Skipped {
                reason: "not_owned",
            },
        );
        assert_eq!(skipped.invite_skipped.as_deref(), Some("not_owned"));
        assert_eq!(skipped.uri, plain.uri);
        assert!(!skipped.invite_included);
        assert_eq!(
            with_invite(plain.clone(), PairingInvite::NotApplicable),
            plain
        );
    }

    #[test]
    fn builds_the_pairing_answer_with_a_qr_code() {
        let key = vca_net::StaticKeypair::generate();
        let payload = vca_net::PairingPayload {
            server_id: vca_net::pairing::server_id(key.public()),
            server_key: *key.public(),
            server_name: "studio".into(),
            secret: zeroize_secret(),
            expires_at: 1_700_000_300,
            endpoints: vec!["ws://127.0.0.1:47800/v1/ws".into()],
            totp_required: false,
        };
        let uri = payload.to_uri();
        let p = pairing(uri.clone(), 1_700_000_300, "ff".into(), PairScope::Command).unwrap();
        assert_eq!(p.uri, uri);
        assert_eq!(p.endpoints, vec!["ws://127.0.0.1:47800/v1/ws".to_owned()]);
        assert!(p.loopback_only);
        assert!(p.qr_svg.contains("<svg"));
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["scope"], "command");
        assert_eq!(v["expires_at"], 1_700_000_300u64);
        assert!(v.get("qr_svg").is_some());

        let err = pairing("vca://pair?v=1".into(), 0, String::new(), PairScope::View).unwrap_err();
        assert_eq!(err.code, "protocol");
    }

    fn zeroize_secret() -> zeroize::Zeroizing<Vec<u8>> {
        zeroize::Zeroizing::new(vec![7u8; 32])
    }

    /// A stored (paired) machine as vca-client writes it.
    fn stored(name: &str) -> serde_json::Value {
        let key = vca_net::StaticKeypair::generate();
        json!({
            "id": vca_net::pairing::server_id(key.public()),
            "name": name,
            "endpoints": [format!("wss://{name}.example.ts.net/v1/ws")],
            "deviceId": format!("dev-{name}"),
            "scope": "command",
            "pairedAt": 1,
            "serverKey": key.public().to_base64url(),
        })
    }

    #[tokio::test]
    async fn this_pc_and_paired_computers_coexist_in_the_hub() {
        let dir = tempfile::tempdir().unwrap();
        let machines = dir.path().join("machines");
        std::fs::create_dir_all(&machines).unwrap();
        std::fs::write(
            machines.join(vca_client::MACHINES_FILE),
            serde_json::to_vec(&json!({
                "version": 1,
                "machines": [stored("mac-studio"), stored("linux-box")],
            }))
            .unwrap(),
        )
        .unwrap();
        let hub = vca_client::ClientHub::open(
            &machines,
            Arc::new(vca_net::MemoryKeyStore::default()) as Arc<dyn vca_net::KeyStore>,
        )
        .unwrap();
        let paired = hub.machines();
        assert_eq!(paired.len(), 2, "{paired:?}");

        // The local server was never reachable: only the paired computers.
        let host = Host::with(
            dir.path().join("host"),
            PathBuf::from("vca-test-coexist-none"),
            None,
        );
        assert_eq!(
            machine_list(hub.machines(), host.local_machine_now()),
            paired
        );

        // Once seen, this PC is added after them, not instead of them.
        *lock(&host.seen) = Some("WIN-DESK".into());
        let list = machine_list(hub.machines(), host.local_machine_now());
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["mac-studio", "linux-box", "WIN-DESK"]);
        assert_eq!(list[2].id, LOCAL_MACHINE_ID);
        assert_eq!(&list[..2], paired.as_slice());
        assert!(
            list[..2]
                .iter()
                .all(|m| vca_client::is_valid_server_id(&m.id))
        );

        // Turning the local server off keeps every entry: this PC just
        // reconnects, the remote computers are untouched.
        *lock(&host.reachable) = false;
        assert_eq!(
            machine_list(hub.machines(), host.local_machine_now()).len(),
            3
        );
        // The local entry is never forgotten through the hub.
        assert!(hub.forget(LOCAL_MACHINE_ID).await.is_err());
        assert_eq!(hub.machines().len(), 2);
    }

    #[test]
    fn scopes_deserialize_from_the_webview() {
        let s: PairScope = serde_json::from_value(json!("command")).unwrap();
        assert_eq!(s, PairScope::Command);
        assert!(serde_json::from_value::<PairScope>(json!("root")).is_err());
    }

    #[test]
    fn control_errors_map_to_codes() {
        let e: CommandError = ControlError::Unreachable(std::io::Error::other("x")).into();
        assert_eq!(e.code, "not_running");
        let e: CommandError = ControlError::Server {
            code: "unsupported".into(),
            message: "the remote transport is off".into(),
        }
        .into();
        assert_eq!(e.code, "remote_off");
        let e: CommandError = SendError::NotConnected.into();
        assert_eq!(e.code, "not_connected");
    }

    #[test]
    fn settings_default_on_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::with(
            dir.path().to_owned(),
            PathBuf::from("vca-test-no-such-endpoint"),
            None,
        );
        assert!(host.settings().run_server, "on by default on desktop");
        host.set_run_server(false).unwrap();
        let again = Host::with(dir.path().to_owned(), host.endpoint().to_owned(), None);
        assert!(!again.settings().run_server);
    }

    #[tokio::test]
    async fn status_when_no_server_runs() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = if cfg!(windows) {
            PathBuf::from(r"\\.\pipe\vca-test-host-status-none")
        } else {
            dir.path().join("none.sock")
        };
        let host = Host::with(dir.path().to_owned(), endpoint, None);
        let status = host.status().await;
        assert!(!status.running);
        assert!(!status.managed);
        assert!(!status.can_start);
        assert_eq!(status.remote, None);
        assert_eq!(host.local_machine_now(), None);
        let err = host.pair(PairScope::Command, None).await.unwrap_err();
        assert_eq!(err.code, "not_running");
        let err = host.stop().await.unwrap_err();
        assert_eq!(err.code, "not_owned");
        let err = host.start().await.unwrap_err();
        assert_eq!(err.code, "sidecar_missing");
    }

    impl Host {
        fn local_machine_now(&self) -> Option<MachineInfo> {
            lock(&self.seen).as_deref().map(local_link::machine_info)
        }
    }
}
