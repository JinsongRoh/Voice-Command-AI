//! The transport-agnostic API: one [`Client`] per connection, requests
//! through [`Api::handle`], and everything the server sends (responses and
//! pushes) on the client's outbound channel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use vca_core::error::ErrorCode;
use vca_core::{
    ClientMsg, DeliveryStatus, Envelope, Error, PROTOCOL_VERSION, PairedDevice, ServerMsg,
    SessionAction, SessionId,
};
use vca_net::Scope;

use crate::attachments::Attachments;
use crate::cloud::Cloud;
use crate::events::{EventStream, StreamItem};
use crate::push::{PushRegistration, PushStore};
use crate::restore::{Restore, StopCheck};
use crate::sessions::{SessionManager, Update};
use crate::shell::audit::events as audit_events;
use crate::shell::{Actor, CloseReason, Shells};
use crate::speech::{Speech, TranscribeRequest};
use crate::tts::Tts;
use crate::util::{error_text, lock};

/// Outbound queue length per client. A client that falls this far behind
/// slows down its own event forwarding; other clients are unaffected.
const OUTBOUND_CAPACITY: usize = 1024;

/// How long a delivered prompt waits to be acknowledged ("seen by the
/// agent"): a busy agent may queue it until its current turn ends.
const ACK_WAIT: Duration = Duration::from_secs(10 * 60);

/// A paired device behind a remote connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub scope: Scope,
}

/// Device administration, provided by the remote transport.
pub trait DeviceAdmin: Send + Sync {
    /// Paired devices; `current` marks the asking device.
    fn list_devices(&self, current: Option<&str>) -> Result<Vec<PairedDevice>, Error>;
    /// Returns `false` when the device was unknown or already revoked.
    fn revoke_device(&self, device_id: &str) -> Result<bool, Error>;
    /// Drops every remote connection and refuses new ones (SEC-09).
    fn trip_kill_switch(&self, by: &str) -> Result<(), Error>;
    // --- Device management. `by` is the asking device id or `local`. ---
    /// Returns `false` when the device is unknown or revoked.
    fn rename_device(&self, device_id: &str, name: &str, by: &str) -> Result<bool, Error>;
    /// Returns `false` when the device is unknown or revoked.
    fn set_device_scope(&self, device_id: &str, scope: Scope, by: &str) -> Result<bool, Error>;
    /// The device's current scope (`None`: unknown or revoked). Checked on
    /// every request, so scope changes apply to live connections at once.
    fn device_scope(&self, device_id: &str) -> Result<Option<Scope>, Error>;
    /// Remembers the app version a device sent in `hello` (display only).
    fn note_app_version(&self, _device_id: &str, _version: &str) {}
}

/// Makes a device's permission check and the request it authorizes atomic
/// with respect to changes of that device's scope (one gate per device).
///
/// A remote request holds its device's read side from the scope check until
/// its reply is ready; a scope change or revocation holds the target's
/// write side while it applies. So once a change returns, every request the
/// old scope authorized has finished, and later ones see the new scope.
/// Tokio locks: requests hold their side across `.await`.
#[derive(Default)]
struct ScopeGates(Mutex<HashMap<String, Arc<tokio::sync::RwLock<()>>>>);

type RequestGuard = tokio::sync::OwnedRwLockReadGuard<()>;
type ChangeGuard = tokio::sync::OwnedRwLockWriteGuard<()>;

impl ScopeGates {
    fn gate(&self, device_id: &str) -> Arc<tokio::sync::RwLock<()>> {
        // Entries are never removed: a request still holding an old gate
        // must exclude a change that would otherwise get a fresh one.
        Arc::clone(lock(&self.0).entry(device_id.to_owned()).or_default())
    }

    async fn request(&self, device_id: &str) -> RequestGuard {
        self.gate(device_id).read_owned().await
    }

    async fn change(&self, device_id: &str) -> ChangeGuard {
        self.gate(device_id).write_owned().await
    }

    /// A remote `actor` changing `target`: the actor's read side (its own
    /// permission must hold while the change applies) and the target's
    /// write side, taken in device-id order so that two admins changing
    /// each other at once cannot deadlock. The caller must not hold the
    /// actor's request guard.
    async fn change_by(&self, actor: &str, target: &str) -> (Option<RequestGuard>, ChangeGuard) {
        if actor == target {
            return (None, self.change(target).await);
        }
        if actor < target {
            let r = self.request(actor).await;
            (Some(r), self.change(target).await)
        } else {
            let w = self.change(target).await;
            (Some(self.request(actor).await), w)
        }
    }
}

/// One connected client.
pub struct Client {
    pub id: u64,
    pub transport: &'static str,
    /// Set for remote devices; local clients (the owner's CLI) have none
    /// and are not restricted.
    pub device: Option<DeviceIdentity>,
    out: mpsc::Sender<Envelope<ServerMsg>>,
    subs: Mutex<HashMap<SessionId, JoinHandle<()>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    wants_resources: Arc<AtomicBool>,
    /// Counts this client as a viewer of the resource figures (from its
    /// first `GetResources` until it disconnects), which sets the broker's
    /// sampling pace.
    resources_viewer: Mutex<Option<vca_broker::monitor::ResourceViewer>>,
    /// Open attachment uploads; unfinished ones are deleted on disconnect.
    uploads: crate::attachments::Uploads,
    /// Local channel: the process behind this connection is inside a
    /// session or a remote shell, or could not be identified. Its
    /// instructions to the conductor never pre-approve sends
    /// (`conductor::grant`). Set by the local channel before each
    /// `ConductorAsk`.
    untrusted_caller: AtomicBool,
}

impl Drop for Client {
    fn drop(&mut self) {
        for (_, t) in lock(&self.subs).drain() {
            t.abort();
        }
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
    }
}

impl Client {
    /// See the `untrusted_caller` field.
    pub fn untrusted_caller(&self) -> bool {
        self.untrusted_caller.load(Ordering::SeqCst)
    }

    pub(crate) fn set_untrusted_caller(&self, untrusted: bool) {
        self.untrusted_caller.store(untrusted, Ordering::SeqCst);
    }

    async fn send(&self, id: u64, body: ServerMsg) {
        let _ = self.deliver(id, body).await;
    }

    /// Sends a reply; `false` when the connection is gone (the reply is lost).
    async fn deliver(&self, id: u64, body: ServerMsg) -> bool {
        self.out.send(Envelope { id, body }).await.is_ok()
    }
}

struct Inner {
    mgr: SessionManager,
    server_name: String,
    resources_interval: Duration,
    next_client: AtomicU64,
    devices: OnceLock<Arc<dyn DeviceAdmin>>,
    security: OnceLock<Arc<crate::security::Security>>,
    speech: OnceLock<Arc<Speech>>,
    tts: OnceLock<Arc<Tts>>,
    /// Spoken result summaries (`Summarize`).
    summaries: OnceLock<Arc<crate::voice_brief::Summarizer>>,
    cloud: OnceLock<Arc<Cloud>>,
    attachments: OnceLock<Arc<Attachments>>,
    shells: OnceLock<Shells>,
    restore: OnceLock<Restore>,
    /// Phones registered for approval alerts.
    push: OnceLock<Arc<PushStore>>,
    /// Speech engine setup (local control channel only).
    engines: OnceLock<Arc<crate::engines::EngineAdmin>>,
    /// The project folder picker.
    folders: OnceLock<Arc<crate::folders::Folders>>,
    /// Discovery of agent sessions running on the computer.
    discovery: OnceLock<crate::discovery::Discovery>,
    /// The conductor (opt-in per PC).
    conductor: OnceLock<crate::conductor::Conductor>,
    /// The human task inbox ("To-do").
    tasks: OnceLock<crate::tasks::Tasks>,
    /// The pronunciation dictionary of spoken alerts.
    pronunciations: OnceLock<Arc<crate::pronunciations::Pronunciations>>,
    /// Server-wide pushes to every client (remote shell notices).
    notices: broadcast::Sender<ServerMsg>,
    /// Scope checks vs. scope changes (see [`ScopeGates`]).
    scope_gates: ScopeGates,
}

/// The API core shared by all transports. Cheap to clone.
#[derive(Clone)]
pub struct Api {
    inner: Arc<Inner>,
}

fn error_msg(err: &Error) -> ServerMsg {
    ServerMsg::Error {
        code: err.code(),
        message: error_text(err),
    }
}

fn order_msg(order: crate::sessions::SessionOrder) -> ServerMsg {
    ServerMsg::SessionOrder {
        session_ids: order.session_ids,
        custom: order.custom,
    }
}

/// Which dictation a transcript belongs to, for the speech topic: the
/// selected session, else the paired device, else the connection.
fn topic_key(client: &Client, session_id: Option<&SessionId>) -> String {
    match (session_id, &client.device) {
        (Some(sid), _) => format!("session:{}", sid.0),
        (None, Some(device)) => format!("device:{}", device.device_id),
        (None, None) => format!("client:{}", client.id),
    }
}

/// Whose speech preferences apply: the paired device, else the owner's
/// local clients.
fn speech_device(client: &Client) -> String {
    client
        .device
        .as_ref()
        .map(|d| d.device_id.clone())
        .unwrap_or_else(|| crate::speech::LOCAL_DEVICE.to_owned())
}

fn forbidden() -> ServerMsg {
    error(
        ErrorCode::Forbidden,
        "this device is not allowed to do that; ask an admin device to widen its permission",
    )
}

const SPEECH_OFF: &str = "server-side transcription is off (set [speech] engine in config.toml)";

fn shells_off() -> ServerMsg {
    error(ErrorCode::Unsupported, "remote shells are not available")
}

fn remote_off() -> ServerMsg {
    error(ErrorCode::Unsupported, "the remote transport is off")
}

fn push_off() -> ServerMsg {
    error(ErrorCode::Unsupported, "approval alerts are not available")
}

fn restore_off() -> ServerMsg {
    error(ErrorCode::Unsupported, "session restore is not available")
}

const TTS_OFF: &str = "spoken alerts from the PC are off (add [[tts.engines]] to config.toml)";

const SUMMARIES_OFF: &str = "spoken result summaries are not available on this server";

fn cloud_off() -> ServerMsg {
    error(ErrorCode::Unsupported, "cloud sessions are off")
}

/// Would `scope` widen what `device_id` may do now? When the current scope
/// cannot be read, assume it would: the change then needs the verification
/// a widening needs (fails closed).
fn raises(admin: &dyn DeviceAdmin, device_id: &str, scope: vca_core::DeviceScope) -> bool {
    match admin.device_scope(device_id) {
        Ok(Some(now)) => Scope::from(scope) > now,
        Ok(None) => false,
        Err(_) => true,
    }
}

/// The device list after a management change.
fn devices_reply(admin: &dyn DeviceAdmin, current: Option<&str>) -> ServerMsg {
    match admin.list_devices(current) {
        Ok(devices) => ServerMsg::Devices { devices },
        Err(err) => error_msg(&err),
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> ServerMsg {
    ServerMsg::Error {
        code,
        message: message.into(),
    }
}

impl Api {
    pub fn new(mgr: SessionManager, server_name: String, resources_interval: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                mgr,
                server_name,
                resources_interval,
                next_client: AtomicU64::new(1),
                devices: OnceLock::new(),
                security: OnceLock::new(),
                speech: OnceLock::new(),
                tts: OnceLock::new(),
                summaries: OnceLock::new(),
                cloud: OnceLock::new(),
                attachments: OnceLock::new(),
                shells: OnceLock::new(),
                restore: OnceLock::new(),
                push: OnceLock::new(),
                engines: OnceLock::new(),
                folders: OnceLock::new(),
                discovery: OnceLock::new(),
                conductor: OnceLock::new(),
                tasks: OnceLock::new(),
                pronunciations: OnceLock::new(),
                notices: broadcast::channel(256).0,
                scope_gates: ScopeGates::default(),
            }),
        }
    }

    /// The channel whose messages reach every connected client.
    pub fn notices(&self) -> broadcast::Sender<ServerMsg> {
        self.inner.notices.clone()
    }

    /// Connects remote shells. Without them, shell messages answer
    /// `Unsupported`.
    pub fn set_shells(&self, shells: Shells) {
        let _ = self.inner.shells.set(shells);
    }

    pub fn shells(&self) -> Option<&Shells> {
        self.inner.shells.get()
    }

    /// Connects the alert registrations. Without them, push messages answer
    /// `Unsupported`.
    pub fn set_push(&self, push: Arc<PushStore>) {
        let _ = self.inner.push.set(push);
    }

    pub fn push(&self) -> Option<&Arc<PushStore>> {
        self.inner.push.get()
    }

    /// Connects speech engine setup. Without it, the engine requests of the
    /// local channel answer `Unsupported`.
    pub fn set_engines(&self, engines: Arc<crate::engines::EngineAdmin>) {
        let _ = self.inner.engines.set(engines);
    }

    pub fn engines(&self) -> Option<&Arc<crate::engines::EngineAdmin>> {
        self.inner.engines.get()
    }

    /// Connects the project folder picker with its transcript sources.
    /// Without it, the picker uses the session manager's workspace roots
    /// and the sessions only.
    pub fn set_folders(&self, folders: Arc<crate::folders::Folders>) {
        let _ = self.inner.folders.set(folders);
    }

    /// Connects session discovery. Without it (or with discovery turned
    /// off), scan requests answer `Unsupported` and `Welcome` does not list
    /// [`vca_core::protocol::FEATURE_SCAN_SESSIONS`].
    pub fn set_conductor(&self, conductor: crate::conductor::Conductor) {
        let _ = self.inner.conductor.set(conductor);
    }

    /// Without one, conductor messages answer `Unsupported`.
    pub fn conductor(&self) -> Option<&crate::conductor::Conductor> {
        self.inner.conductor.get()
    }

    pub fn set_discovery(&self, discovery: crate::discovery::Discovery) {
        let _ = self.inner.discovery.set(discovery);
    }

    /// The optional requests this server answers (`Welcome::features`).
    fn features(&self) -> Vec<String> {
        let mut out = vec![
            vca_core::protocol::FEATURE_PROJECT_FOLDERS.to_owned(),
            vca_core::protocol::FEATURE_SESSION_ORDER.to_owned(),
            vca_core::protocol::FEATURE_DISMISS_SESSION.to_owned(),
            vca_core::protocol::FEATURE_DISMISS_STOP.to_owned(),
        ];
        if self.inner.restore.get().is_some() {
            out.push(vca_core::protocol::FEATURE_WINDOW_ARRANGE.to_owned());
        }
        if self.inner.shells.get().is_some() {
            out.push(vca_core::protocol::FEATURE_SHELL_KEEP_ALIVE.to_owned());
            out.push(vca_core::protocol::FEATURE_SHELL_JOIN.to_owned());
            out.push(vca_core::protocol::FEATURE_SHELL_CONFIRM_LINES.to_owned());
        }
        if self.inner.discovery.get().is_some_and(|d| d.enabled()) {
            out.push(vca_core::protocol::FEATURE_SCAN_SESSIONS.to_owned());
        }
        if self.inner.conductor.get().is_some() {
            out.push(vca_core::conductor::FEATURE_CONDUCTOR.to_owned());
            out.push(vca_core::protocol::FEATURE_COMMAND_CENTER.to_owned());
        }
        if self.inner.tasks.get().is_some() {
            out.push(vca_core::task::FEATURE_HUMAN_TASKS.to_owned());
        }
        if self.inner.pronunciations.get().is_some() {
            out.push(vca_core::FEATURE_PRONUNCIATIONS.to_owned());
        }
        if self.inner.speech.get().is_some() {
            out.push(vca_core::FEATURE_PERSONAL_TERMS.to_owned());
        }
        out
    }

    /// Connects the pronunciation dictionary. Without it, pronunciation
    /// messages answer `Unsupported` and `Welcome` does not list
    /// [`vca_core::FEATURE_PRONUNCIATIONS`].
    pub fn set_pronunciations(&self, dictionary: Arc<crate::pronunciations::Pronunciations>) {
        let _ = self.inner.pronunciations.set(dictionary);
    }

    /// Pronunciation requests (scopes are checked before).
    fn pronunciation_request(&self, msg: ClientMsg) -> ServerMsg {
        let Some(dict) = self.inner.pronunciations.get() else {
            return error(
                ErrorCode::Unsupported,
                "this server has no pronunciation dictionary",
            );
        };
        let result = match msg {
            ClientMsg::ListPronunciations { language } => {
                return ServerMsg::Pronunciations {
                    entries: dict.list(language.as_deref()),
                };
            }
            ClientMsg::SetPronunciation {
                term,
                spoken,
                language,
            } => dict.set(&term, &spoken, language.as_deref()),
            ClientMsg::DeletePronunciation { term, language } => {
                match dict.delete(&term, language.as_deref()) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(Error::api(
                        ErrorCode::NotFound,
                        "no pronunciation for this term",
                    )),
                    Err(err) => Err(err),
                }
            }
            _ => return error(ErrorCode::InvalidRequest, "unexpected request"),
        };
        match result {
            Ok(()) => dict.list_msg(),
            Err(err) => error_msg(&err),
        }
    }

    /// Saves the commonly misheard words (the scope is checked before) and
    /// audits it, with counts only.
    async fn save_personal_terms(
        &self,
        client: &Client,
        terms: Vec<vca_core::PersonalTerm>,
    ) -> ServerMsg {
        let Some(speech) = self.inner.speech.get().cloned() else {
            return error(ErrorCode::Unsupported, SPEECH_OFF);
        };
        // A file write: off the async workers.
        let saved = tokio::task::spawn_blocking(move || speech.save_personal_terms(terms)).await;
        match saved {
            Ok(Ok(terms)) => {
                let variants: usize = terms.iter().map(|t| t.variants.len()).sum();
                self.restore_audit(
                    client,
                    audit_events::PERSONAL_TERMS_SAVE,
                    &format!("terms={} variants={variants}", terms.len()),
                );
                ServerMsg::PersonalTerms { terms }
            }
            Ok(Err(err)) => error_msg(&err),
            Err(_) => error(ErrorCode::Internal, "cannot save the word list"),
        }
    }

    /// Connects the human task inbox. Without it, task messages answer
    /// `Unsupported` and `Welcome` does not list
    /// [`vca_core::task::FEATURE_HUMAN_TASKS`].
    pub fn set_tasks(&self, tasks: crate::tasks::Tasks) {
        let _ = self.inner.tasks.set(tasks);
    }

    pub fn tasks(&self) -> Option<&crate::tasks::Tasks> {
        self.inner.tasks.get()
    }

    /// Task requests. Scopes are checked before (registry); opening an
    /// administrator shell and the settings are for this PC's own app only.
    async fn task_request(&self, client: &Client, msg: ClientMsg) -> ServerMsg {
        let Some(tasks) = self.inner.tasks.get() else {
            return error(ErrorCode::Unsupported, "this server has no task list");
        };
        let actor = self.actor(client);
        let local = client.device.is_none() && client.transport == "local";
        let reply = match msg {
            ClientMsg::TaskList => Ok(tasks.list()),
            ClientMsg::TaskCreate { draft } => tasks
                .create(&actor, draft)
                .map(|task| ServerMsg::TaskSaved { task }),
            ClientMsg::TaskUpdate { task_id, change } => tasks
                .update(&actor, &task_id, change)
                .map(|task| ServerMsg::TaskSaved { task }),
            ClientMsg::TaskSendResult {
                task_id,
                result,
                preview,
            } => tasks
                .send_result(&actor, &task_id, result, preview)
                .await
                .map(|(task, text, sent)| ServerMsg::TaskResultSent { task, text, sent }),
            ClientMsg::TaskOpenAdminShell { task_id } => tasks
                .open_admin_shell(&actor, &task_id, local)
                .map(|()| ServerMsg::Ok),
            ClientMsg::TaskExport { format, session_id } => Ok(ServerMsg::TaskExported {
                format,
                text: tasks.export(format, session_id.as_ref()),
            }),
            ClientMsg::TaskDigest { language } => Ok(ServerMsg::Briefing {
                text: tasks.digest(language.as_deref()),
            }),
            ClientMsg::TaskConfigure { .. } if !local => Err(Error::api(
                ErrorCode::Forbidden,
                "the task settings change only in the app on the PC itself",
            )),
            ClientMsg::TaskConfigure { settings } => Ok(tasks.configure(&actor, settings)),
            _ => Err(Error::api(ErrorCode::InvalidRequest, "not a task request")),
        };
        reply.unwrap_or_else(|err| error_msg(&err))
    }

    /// Scan, hide and unhide requests of session discovery.
    async fn discovery_request(&self, msg: ClientMsg) -> ServerMsg {
        let Some(discovery) = self.inner.discovery.get() else {
            return error(
                ErrorCode::Unsupported,
                "this server cannot look for sessions",
            );
        };
        let result = match msg {
            ClientMsg::ScanSessions => discovery.scan().await,
            ClientMsg::HideObserved { session_id, pid } => discovery.hide(session_id, pid).await,
            ClientMsg::UnhideObserved {
                session_id,
                pid,
                all,
            } => discovery.unhide(session_id, pid, all).await,
            _ => Err(Error::api(
                ErrorCode::InvalidRequest,
                "not a discovery request",
            )),
        };
        result.unwrap_or_else(|err| error_msg(&err))
    }

    fn folders(&self) -> Arc<crate::folders::Folders> {
        Arc::clone(self.inner.folders.get_or_init(|| {
            Arc::new(crate::folders::Folders::new(
                self.inner.mgr.workspace_roots().clone(),
                crate::folders::TranscriptSources::default(),
            ))
        }))
    }

    /// Folder picker requests: rate-limited per device (or connection),
    /// file system work off the async runtime.
    async fn folder_request(&self, client: &Client, msg: ClientMsg) -> ServerMsg {
        let folders = self.folders();
        let key = client
            .device
            .as_ref()
            .map(|d| format!("device:{}", d.device_id))
            .unwrap_or_else(|| format!("client:{}", client.id));
        if let Err(err) = folders.check_rate(&key) {
            return error_msg(&err);
        }
        let sessions = self.inner.mgr.list();
        let work = tokio::task::spawn_blocking(move || match msg {
            ClientMsg::ProjectFolders => Ok(folders.project_folders(&sessions)),
            ClientMsg::ListFolder { path, show_hidden } => {
                folders.list_folder(&path, show_hidden, &sessions)
            }
            ClientMsg::FolderPath { parent, name } => folders.folder_path(&parent, &name),
            _ => Err(Error::api(
                ErrorCode::InvalidRequest,
                "not a folder request",
            )),
        });
        match work.await {
            Ok(Ok(reply)) => reply,
            Ok(Err(err)) => error_msg(&err),
            Err(_) => error(ErrorCode::Internal, "the folder request failed"),
        }
    }

    /// The phones for the desktop app, after deleting those whose device
    /// is no longer paired (a revocation the push store missed).
    fn push_phones(&self, push: &PushStore) -> ServerMsg {
        if let Some(admin) = self.inner.devices.get() {
            let pruned = push.retain(|id| !matches!(admin.device_scope(id), Ok(None)));
            if let Err(err) = pruned {
                return error_msg(&err);
            }
        }
        ServerMsg::PushPhones {
            phones: push.list(),
        }
    }

    /// Drops a revoked device's alert registration.
    /// Drops what this PC keeps per device once it is revoked: its speech
    /// preferences and its remote shell step-up and wrong-code records.
    fn forget_device(&self, device_id: &str) {
        self.forget_push_phone(device_id);
        if let Some(shells) = self.shells() {
            shells.forget_device(device_id);
        }
        if let Some(speech) = self.inner.speech.get() {
            let speech = Arc::clone(speech);
            let device = device_id.to_owned();
            // A file write: off the async workers.
            tokio::task::spawn_blocking(move || {
                if let Err(err) = speech.forget_device(&device) {
                    tracing::warn!(device = %device, "cannot drop speech preferences: {err}");
                }
            });
        }
    }

    fn forget_push_phone(&self, device_id: &str) {
        if let Some(push) = self.push() {
            if let Err(err) = push.remove(device_id) {
                tracing::warn!(
                    device = device_id,
                    "cannot delete the alert registration: {err}"
                );
            }
        }
    }

    /// Connects session restore. Without it, restore messages answer
    /// `Unsupported`.
    pub fn set_restore(&self, restore: Restore) {
        let _ = self.inner.restore.set(restore);
    }

    pub fn restore(&self) -> Option<&Restore> {
        self.inner.restore.get()
    }

    /// The list after a change.
    async fn restore_list(&self, restore: &Restore) -> ServerMsg {
        match restore.overview().await {
            Ok(overview) => ServerMsg::RestoreList { overview },
            Err(err) => error_msg(&err),
        }
    }

    fn restore_audit(&self, client: &Client, event: &str, detail: &str) {
        if let Some(shells) = self.shells() {
            shells.record_audit(&self.actor(client), event, detail);
        }
    }

    /// A remote run needs the step-up window or a fresh TOTP code and opens
    /// only folders inside the workspace roots; the owner's local clients
    /// need neither. A dry run opens nothing, so it needs no code.
    async fn restore_run(
        &self,
        client: &Client,
        restore: &Restore,
        request: vca_core::RestoreRunRequest,
    ) -> Result<vca_core::RestoreJob, vca_core::Error> {
        let actor = self.actor(client);
        let remote = client.device.is_some();
        let mut stop: Option<StopCheck> = None;
        if remote && !request.dry_run {
            let Some(shells) = self.shells() else {
                return Err(vca_core::Error::api(
                    ErrorCode::Unsupported,
                    "remote restore needs the remote transport and TOTP on this computer",
                ));
            };
            shells.require_step_up(&actor, request.totp.as_deref(), "restore_run")?;
            let shells = shells.clone();
            stop = Some(Arc::new(move || shells.kill_switch_tripped()));
        }
        let job = restore
            .run(&actor.device_name, &request, remote, stop)
            .await?;
        if !request.dry_run {
            let opening = job
                .items
                .iter()
                .filter(|i| i.status == vca_core::RestoreItemStatus::Waiting)
                .count();
            self.restore_audit(
                client,
                audit_events::RESTORE_RUN,
                &format!(
                    "profile={} windows={} of {} remote={remote}",
                    job.profile,
                    opening,
                    job.items.len()
                ),
            );
        }
        Ok(job)
    }

    /// The state of the screen layout tab.
    async fn arrange_list(&self, restore: &Restore) -> ServerMsg {
        match restore.arrange_overview().await {
            Ok(overview) => ServerMsg::ArrangeList { overview },
            Err(err) => error_msg(&err),
        }
    }

    /// Moving windows from a paired device needs the step-up window or a
    /// fresh TOTP code, as a restore run does, and the kill switch stops it;
    /// the owner's local clients need neither. Returns the stop check of an
    /// allowed remote arrangement.
    fn arrange_gate(
        &self,
        client: &Client,
        totp: Option<&str>,
    ) -> Result<Option<StopCheck>, vca_core::Error> {
        if client.device.is_none() {
            return Ok(None);
        }
        let Some(shells) = self.shells() else {
            return Err(vca_core::Error::api(
                ErrorCode::Unsupported,
                "arranging windows remotely needs the remote transport and TOTP on this computer",
            ));
        };
        shells.require_step_up(&self.actor(client), totp, "window_arrange")?;
        let shells = shells.clone();
        Ok(Some(Arc::new(move || shells.kill_switch_tripped())))
    }

    /// Reports an arrangement with the state after it, and audits it.
    async fn arrange_done(
        &self,
        client: &Client,
        restore: &Restore,
        op: &str,
        done: Result<vca_core::ArrangeResult, vca_core::Error>,
    ) -> ServerMsg {
        let result = match done {
            Ok(result) => result,
            Err(err) => return error_msg(&err),
        };
        self.restore_audit(
            client,
            audit_events::WINDOW_ARRANGE,
            &format!(
                "op={op} moved={} skipped={} failed={} remote={}",
                result.moved,
                result.skipped,
                result.failed,
                client.device.is_some()
            ),
        );
        match restore.arrange_overview().await {
            Ok(overview) => ServerMsg::ArrangeDone { result, overview },
            Err(err) => error_msg(&err),
        }
    }

    /// The device behind a client, for shell audit entries and notices.
    fn actor(&self, client: &Client) -> Actor {
        let Some(device) = &client.device else {
            return Actor::local();
        };
        let name = self
            .inner
            .devices
            .get()
            .and_then(|admin| admin.list_devices(None).ok())
            .and_then(|list| list.into_iter().find(|d| d.id == device.device_id))
            .map(|d| d.name)
            .unwrap_or_else(|| device.device_id.clone());
        Actor {
            device_id: device.device_id.clone(),
            device_name: name,
        }
    }

    /// Would `scope` widen what the device may do now?
    pub fn scope_raises(&self, device_id: &str, scope: vca_core::DeviceScope) -> bool {
        self.inner
            .devices
            .get()
            .is_some_and(|admin| raises(admin.as_ref(), device_id, scope))
    }

    /// Sets a device's permission for the local channel. `verified`: the
    /// caller checked local verification (needed when this widens it). The
    /// widening check is repeated under the device's gate, so a concurrent
    /// narrowing cannot turn an unverified change into a widening. Returns
    /// once requests authorized by the old scope have finished.
    pub async fn apply_device_scope(
        &self,
        device_id: &str,
        scope: vca_core::DeviceScope,
        verified: bool,
    ) -> ServerMsg {
        let Some(admin) = self.inner.devices.get() else {
            return error(ErrorCode::Unsupported, "the remote transport is off");
        };
        let _change = self.inner.scope_gates.change(device_id).await;
        if !verified && raises(admin.as_ref(), device_id, scope) {
            return error(
                ErrorCode::Conflict,
                "the device's permission changed meanwhile; try again",
            );
        }
        match admin.set_device_scope(device_id, scope.into(), "local") {
            Ok(true) => devices_reply(admin.as_ref(), None),
            Ok(false) => error(ErrorCode::NotFound, format!("no active device {device_id}")),
            Err(err) => error_msg(&err),
        }
    }

    /// Connects the security service (local verification, mail, recovery).
    pub fn set_security(&self, security: Arc<crate::security::Security>) {
        let _ = self.inner.security.set(security);
    }

    /// The security service, when connected.
    pub fn security(&self) -> Option<&Arc<crate::security::Security>> {
        self.inner.security.get()
    }

    /// Connects device administration (the remote transport). Without it,
    /// device messages answer `Unsupported`.
    pub fn set_device_admin(&self, admin: Arc<dyn DeviceAdmin>) {
        let _ = self.inner.devices.set(admin);
    }

    /// Handles the speech credential requests of the owner-only local
    /// channel (`vca speech login/logout`).
    pub(crate) fn speech_local_request(
        &self,
        req: crate::control::LocalRequest,
    ) -> crate::control::LocalResponse {
        use crate::control::{LocalRequest, LocalResponse};
        let id = req.id();
        let Some(speech) = self.inner.speech.get() else {
            return LocalResponse::error(
                id,
                ErrorCode::Unsupported,
                "server-side transcription is off (set [speech] in config.toml)",
            );
        };
        let fail = |err: Error| LocalResponse::error(id, err.code(), crate::util::error_text(&err));
        match req {
            LocalRequest::SpeechCredentialSet {
                profile, secret, ..
            } => match speech.set_credential(&profile, secret.expose()) {
                Ok(()) => LocalResponse::Done { id },
                Err(err) => fail(err),
            },
            LocalRequest::SpeechCredentialDelete { profile, .. } => {
                match speech.delete_credential(&profile) {
                    Ok(true) => LocalResponse::Done { id },
                    Ok(false) => LocalResponse::error(
                        id,
                        ErrorCode::NotFound,
                        format!("no stored credential for speech profile {profile}"),
                    ),
                    Err(err) => fail(err),
                }
            }
            _ => LocalResponse::error(id, ErrorCode::InvalidRequest, "unexpected request"),
        }
    }

    /// Connects the server-side speech pipeline. Without it, `Transcribe`
    /// answers `Unsupported`.
    pub fn set_speech(&self, speech: Arc<Speech>) {
        let _ = self.inner.speech.set(speech);
    }

    /// Connects the TTS engines. Without them, `Synthesize` answers
    /// `Unsupported` and `ListTtsEngines` an empty list.
    pub fn set_tts(&self, tts: Arc<Tts>) {
        let _ = self.inner.tts.set(tts);
    }

    /// Connects spoken result summaries. Without them, `SetVoiceSummary`
    /// and `Summarize` answer `Unsupported`.
    pub fn set_summaries(&self, summaries: Arc<crate::voice_brief::Summarizer>) {
        let _ = self.inner.summaries.set(summaries);
    }

    /// Handles the TTS credential requests of the owner-only local channel
    /// (`vca tts login/logout`).
    pub(crate) fn tts_local_request(
        &self,
        req: crate::control::LocalRequest,
    ) -> crate::control::LocalResponse {
        use crate::control::{LocalRequest, LocalResponse};
        let id = req.id();
        let Some(tts) = self.inner.tts.get() else {
            return LocalResponse::error(id, ErrorCode::Unsupported, TTS_OFF);
        };
        let fail = |err: Error| LocalResponse::error(id, err.code(), error_text(&err));
        match req {
            LocalRequest::TtsCredentialSet { engine, secret, .. } => {
                match tts.set_credential(&engine, secret.expose()) {
                    Ok(()) => LocalResponse::Done { id },
                    Err(err) => fail(err),
                }
            }
            LocalRequest::TtsCredentialDelete { engine, .. } => {
                match tts.delete_credential(&engine) {
                    Ok(true) => LocalResponse::Done { id },
                    Ok(false) => LocalResponse::error(
                        id,
                        ErrorCode::NotFound,
                        format!("no stored credential for TTS engine {engine}"),
                    ),
                    Err(err) => fail(err),
                }
            }
            _ => LocalResponse::error(id, ErrorCode::InvalidRequest, "unexpected request"),
        }
    }

    /// Connects the cloud accounts. Without them, cloud messages answer
    /// `Unsupported`.
    pub fn set_cloud(&self, cloud: Arc<Cloud>) {
        let _ = self.inner.cloud.set(cloud);
    }

    /// Connects attachment storage. Without it, attachment messages answer
    /// `Unsupported`.
    pub fn set_attachments(&self, attachments: Arc<Attachments>) {
        let _ = self.inner.attachments.set(attachments);
    }

    pub fn attachments(&self) -> Option<&Arc<Attachments>> {
        self.inner.attachments.get()
    }

    pub fn cloud(&self) -> Option<&Arc<Cloud>> {
        self.inner.cloud.get()
    }

    pub fn sessions(&self) -> &SessionManager {
        &self.inner.mgr
    }

    pub fn server_name(&self) -> &str {
        &self.inner.server_name
    }

    /// Registers a client. Session card changes are pushed to it right away;
    /// session events only after `Subscribe`, resources after
    /// `GetResources`. Dropping the client stops all of its pushes.
    pub fn connect(
        &self,
        transport: &'static str,
    ) -> (Arc<Client>, mpsc::Receiver<Envelope<ServerMsg>>) {
        self.connect_as(transport, None)
    }

    /// Registers a client for a paired device, whose requests are limited
    /// to its scope.
    pub fn connect_as(
        &self,
        transport: &'static str,
        device: Option<DeviceIdentity>,
    ) -> (Arc<Client>, mpsc::Receiver<Envelope<ServerMsg>>) {
        let (out, rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let client = Arc::new(Client {
            id: self.inner.next_client.fetch_add(1, Ordering::Relaxed),
            transport,
            device,
            out: out.clone(),
            subs: Mutex::new(HashMap::new()),
            tasks: Mutex::new(Vec::new()),
            wants_resources: Arc::new(AtomicBool::new(false)),
            resources_viewer: Mutex::new(None),
            uploads: crate::attachments::Uploads::default(),
            untrusted_caller: AtomicBool::new(false),
        });

        let mgr = self.inner.mgr.clone();
        let mut updates = mgr.subscribe_updates();
        let tx = out.clone();
        let forward = tokio::spawn(async move {
            loop {
                let body = match updates.recv().await {
                    Ok(Update::Changed(session)) => ServerMsg::SessionUpdated { session: *session },
                    Ok(Update::Removed(session_id)) => ServerMsg::SessionRemoved { session_id },
                    Err(broadcast::error::RecvError::Lagged(_)) => ServerMsg::Sessions {
                        sessions: mgr.list(),
                    },
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if tx.send(Envelope { id: 0, body }).await.is_err() {
                    return;
                }
            }
        });

        let mgr = self.inner.mgr.clone();
        let wants = Arc::clone(&client.wants_resources);
        let every = self
            .inner
            .resources_interval
            .max(Duration::from_millis(200));
        let resources = tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if !wants.load(Ordering::Relaxed) {
                    continue;
                }
                let Some(broker) = mgr.broker() else { return };
                let body = ServerMsg::Resources {
                    snapshot: broker.snapshot(),
                };
                if out.send(Envelope { id: 0, body }).await.is_err() {
                    return;
                }
            }
        });
        let mut notices = self.inner.notices.subscribe();
        let tx = client.out.clone();
        let conductor = self.conductor().cloned();
        let notify = tokio::spawn(async move {
            loop {
                let body = match notices.recv().await {
                    Ok(body) => body,
                    // Notices were missed: the conductor's state (approvals
                    // waiting for the person) is sent whole again; other
                    // notices are announcements without a state to resend.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "a client missed notices; resending the state");
                        match &conductor {
                            Some(c) => c.state_msg(),
                            None => continue,
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if tx.send(Envelope { id: 0, body }).await.is_err() {
                    return;
                }
            }
        });
        let mgr = self.inner.mgr.clone();
        let mut orders = mgr.subscribe_order();
        let tx = client.out.clone();
        let order = tokio::spawn(async move {
            loop {
                let order = match orders.recv().await {
                    Ok(order) => order,
                    Err(broadcast::error::RecvError::Lagged(_)) => mgr.session_order(),
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if tx
                    .send(Envelope {
                        id: 0,
                        body: order_msg(order),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        lock(&client.tasks).extend([forward, resources, notify, order]);
        (client, rx)
    }

    /// Handles one request. Responses carry the request's id.
    pub async fn handle(&self, client: &Arc<Client>, env: Envelope<ClientMsg>) {
        let id = env.id;
        let mgr = &self.inner.mgr;
        let devices = self.inner.devices.get();
        // Local clients (the owner) are not restricted.
        let mut admin = true;
        // Held from the scope check until the reply is ready, so a scope
        // change waits for this request (see `ScopeGates`).
        let mut gate = None;
        if let Some(device) = &client.device {
            gate = Some(self.inner.scope_gates.request(&device.device_id).await);
            // The registry is the source of truth on every request, so a
            // scope change (or revocation) applies to live connections at
            // once; the scope from connect time is only a fallback when no
            // device administration is attached.
            let scope = match devices {
                Some(admin) => admin.device_scope(&device.device_id).unwrap_or(None),
                None => Some(device.scope),
            };
            if !scope.is_some_and(|s| s.permits(&env.body)) {
                client.send(id, forbidden()).await;
                return;
            }
            admin = scope == Some(Scope::Admin);
        }
        // Shell sessions: typing into or watching a shell needs admin
        // scope, attachments do not go there and restarts (a new shell
        // without TOTP) are refused.
        if let Some(reply) = self.shell_session_guard(client, admin, &env.body).await {
            client.send(id, reply).await;
            return;
        }
        let current = client.device.as_ref().map(|d| d.device_id.as_str());
        let by = current.unwrap_or("local");
        let reply = match env.body {
            ClientMsg::Hello {
                protocol_version,
                app_version,
            } => {
                if let (Some(admin), Some(device)) = (devices, current) {
                    admin.note_app_version(device, &app_version);
                }
                if protocol_version == PROTOCOL_VERSION {
                    ServerMsg::Welcome {
                        protocol_version: PROTOCOL_VERSION,
                        server_version: env!("CARGO_PKG_VERSION").to_owned(),
                        server_name: self.inner.server_name.clone(),
                        features: self.features(),
                    }
                } else {
                    error(
                        ErrorCode::Unsupported,
                        format!(
                            "protocol version {protocol_version} is not supported \
                             (server speaks {PROTOCOL_VERSION})"
                        ),
                    )
                }
            }
            ClientMsg::Ping { at } => ServerMsg::Pong { at },
            ClientMsg::ListSessions => ServerMsg::Sessions {
                sessions: mgr.list(),
            },
            ClientMsg::Subscribe {
                session_id,
                from_seq,
            } => match mgr.get(&session_id) {
                Some(session) => {
                    client.send(id, ServerMsg::Ok).await;
                    self.subscribe(client, session_id, session.log(), from_seq);
                    return;
                }
                None => error(ErrorCode::NotFound, format!("no session {session_id}")),
            },
            ClientMsg::History {
                session_id,
                before_seq,
                limit,
            } => match mgr.get(&session_id) {
                Some(session) => {
                    let (events, has_more) = lock(session.log())
                        .page_before(before_seq, usize::try_from(limit).unwrap_or(usize::MAX));
                    ServerMsg::History {
                        session_id,
                        events,
                        has_more,
                    }
                }
                None => error(ErrorCode::NotFound, format!("no session {session_id}")),
            },
            ClientMsg::Unsubscribe { session_id } => {
                if let Some(t) = lock(&client.subs).remove(&session_id) {
                    t.abort();
                }
                ServerMsg::Ok
            }
            ClientMsg::SendInput { session_id, input } => {
                // Typing to the conductor holds back its event wake-ups.
                if let Some(conductor) = self.conductor() {
                    conductor.note_input(&session_id);
                }
                // A submitted prompt is "seen by the agent" once its prompt
                // hook or transcript record arrives (see `crate::progress`).
                let seen = matches!(input, vca_core::Input::Text { submit: true, .. })
                    .then(|| mgr.get(&session_id))
                    .flatten()
                    .map(|s| crate::progress::prompt_watch(&s));
                match mgr.send_input(&session_id, input) {
                    Ok(done) => {
                        client
                            .send(
                                id,
                                ServerMsg::InputAck {
                                    session_id: session_id.clone(),
                                    status: DeliveryStatus::Queued,
                                },
                            )
                            .await;
                        // Weak: a disconnected client is not kept alive by
                        // inputs still waiting (at most `INPUT_QUEUE` per
                        // session, so these tasks are bounded too).
                        let client = Arc::downgrade(client);
                        tokio::spawn(async move {
                            let result = done.await;
                            let Some(client) = client.upgrade() else {
                                return;
                            };
                            match result {
                                Ok(Ok(())) => {
                                    client
                                        .send(
                                            0,
                                            ServerMsg::InputAck {
                                                session_id: session_id.clone(),
                                                status: DeliveryStatus::Delivered,
                                            },
                                        )
                                        .await;
                                    let Some((mut prompts, mark)) = seen else {
                                        return;
                                    };
                                    // Not kept alive by this wait either.
                                    let client = Arc::downgrade(&client);
                                    let took = tokio::time::timeout(
                                        ACK_WAIT,
                                        prompts.wait_for(|n| *n != mark),
                                    )
                                    .await
                                    .is_ok_and(|r| r.is_ok());
                                    if let (true, Some(client)) = (took, client.upgrade()) {
                                        client
                                            .send(
                                                0,
                                                ServerMsg::InputAck {
                                                    session_id,
                                                    status: DeliveryStatus::Acknowledged,
                                                },
                                            )
                                            .await;
                                    }
                                }
                                // The request was answered (`Queued`) and its
                                // id is retired: a later failure goes out as an
                                // event (id 0), never as a second answer.
                                Ok(Err(err)) => client.send(0, error_msg(&err)).await,
                                Err(_) => {
                                    client
                                        .send(0, error(ErrorCode::Unavailable, "input dropped"))
                                        .await;
                                }
                            }
                        });
                        return;
                    }
                    Err(err) => error_msg(&err),
                }
            }
            ClientMsg::Control {
                session_id,
                action: SessionAction::Stop,
            } if self.is_shell_session(&session_id) => {
                let actor = self.actor(client);
                let closed = match self.shells() {
                    Some(shells) => {
                        shells
                            .close(&session_id, CloseReason::User, Some(&actor))
                            .await
                    }
                    None => false,
                };
                if closed {
                    ServerMsg::Ok
                } else {
                    match mgr.control(&session_id, SessionAction::Stop).await {
                        Ok(()) => ServerMsg::Ok,
                        Err(err) => error_msg(&err),
                    }
                }
            }
            ClientMsg::Control { session_id, action } => {
                match mgr.control(&session_id, action).await {
                    Ok(()) => ServerMsg::Ok,
                    Err(err) => error_msg(&err),
                }
            }
            ClientMsg::OpenShell { request } => match self.shells() {
                Some(shells) => match shells.open(&self.actor(client), request).await {
                    Ok(session) => ServerMsg::Launched { session },
                    Err(err) => error_msg(&err),
                },
                None => shells_off(),
            },
            ClientMsg::ShellRun {
                session_id,
                command,
                totp,
                confirm,
            } => match self.shells() {
                Some(shells) => {
                    let actor = self.actor(client);
                    match shells
                        .run(&actor, &session_id, &command, totp.as_deref(), confirm)
                        .await
                    {
                        Ok(crate::shell::RunOutcome::Ran(danger)) => {
                            ServerMsg::ShellRan { session_id, danger }
                        }
                        Ok(crate::shell::RunOutcome::Confirm { command, lines }) => {
                            ServerMsg::ShellConfirm {
                                session_id,
                                command,
                                lines,
                            }
                        }
                        Err(err) => error_msg(&err),
                    }
                }
                None => shells_off(),
            },
            ClientMsg::ShellJoin { session_id, totp } => match self.shells() {
                Some(shells) => match shells.join(&self.actor(client), &session_id, &totp) {
                    Ok(()) => ServerMsg::Ok,
                    Err(err) => error_msg(&err),
                },
                None => shells_off(),
            },
            ClientMsg::ShellKeepAlive { session_id } => match self.shells() {
                Some(shells) => match shells.keep_alive(&self.actor(client), &session_id) {
                    Ok(_) => ServerMsg::Ok,
                    Err(err) => error_msg(&err),
                },
                None => shells_off(),
            },
            ClientMsg::ClassifyCommand { command } => {
                let c = crate::shell::danger::classify(&command);
                let voice_ok = crate::shell::danger::voice_approvable(&command);
                ServerMsg::CommandDanger {
                    level: c.level,
                    spans: c.wire_spans(),
                    reasons: c.reasons.iter().map(|r| (*r).to_owned()).collect(),
                    voice_ok,
                }
            }
            ClientMsg::ListAudit { limit } => match self.shells() {
                Some(shells) => ServerMsg::Audit {
                    entries: shells.list_audit(limit),
                },
                None => shells_off(),
            },
            ClientMsg::RestoreList => match self.restore() {
                Some(restore) => self.restore_list(restore).await,
                None => restore_off(),
            },
            ClientMsg::RestoreCapture { profile } => match self.restore() {
                Some(restore) => match restore.snapshot(profile.as_deref()).await {
                    Ok(_) => {
                        self.restore_audit(client, audit_events::RESTORE_EDIT, "op=capture");
                        self.restore_list(restore).await
                    }
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::RestoreEdit { edit } => match self.restore() {
                Some(restore) => match restore.edit(&edit, client.device.is_none()) {
                    Ok(()) => {
                        let detail = format!("op={}", edit.kind());
                        self.restore_audit(client, audit_events::RESTORE_EDIT, &detail);
                        self.restore_list(restore).await
                    }
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::RestoreImport { layout, profile } => match self.restore() {
                Some(restore) => match restore.import(&layout, profile.as_deref()).await {
                    Ok(count) => {
                        let detail = format!("op=import windows={count}");
                        self.restore_audit(client, audit_events::RESTORE_EDIT, &detail);
                        self.restore_list(restore).await
                    }
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::RestoreRun { request } => match self.restore() {
                Some(restore) => match self.restore_run(client, restore, request).await {
                    Ok(job) => ServerMsg::RestoreProgress { job },
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            // The focus of this computer's screen: never from a paired device.
            ClientMsg::FocusTerminal { .. } if client.device.is_some() => error(
                ErrorCode::Forbidden,
                "bringing a terminal to the front works only from this computer's own app",
            ),
            ClientMsg::FocusTerminal { session_id } => match self.restore() {
                Some(restore) => match restore.focus_session(&session_id).await {
                    Ok(()) => ServerMsg::Ok,
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::ArrangeList => match self.restore() {
                Some(restore) => self.arrange_list(restore).await,
                None => restore_off(),
            },
            ClientMsg::ArrangeApply { monitors, totp } => match self.restore() {
                Some(restore) => match self.arrange_gate(client, totp.as_deref()) {
                    Ok(stop) => {
                        let done = restore.arrange_apply(&monitors, stop).await;
                        self.arrange_done(client, restore, "apply", done).await
                    }
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::ArrangeApplyLayout { layout, totp } => match self.restore() {
                Some(restore) => match self.arrange_gate(client, totp.as_deref()) {
                    Ok(stop) => {
                        let done = restore.arrange_apply_layout(&layout, stop).await;
                        self.arrange_done(client, restore, "apply_layout", done)
                            .await
                    }
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::ArrangeUndo { totp } => match self.restore() {
                Some(restore) => match self.arrange_gate(client, totp.as_deref()) {
                    Ok(stop) => {
                        let done = restore.arrange_undo(stop).await;
                        self.arrange_done(client, restore, "undo", done).await
                    }
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::ArrangeEdit { edit } => match self.restore() {
                Some(restore) => match restore.arrange_edit(&edit).await {
                    Ok(()) => {
                        let detail = format!("op={}", edit.kind());
                        self.restore_audit(client, audit_events::WINDOW_ARRANGE, &detail);
                        self.arrange_list(restore).await
                    }
                    Err(err) => error_msg(&err),
                },
                None => restore_off(),
            },
            ClientMsg::Launch { request } => match mgr.launch(&request) {
                Ok(session) => ServerMsg::Launched { session },
                Err(err) => error_msg(&err),
            },
            msg @ (ClientMsg::ProjectFolders
            | ClientMsg::ListFolder { .. }
            | ClientMsg::FolderPath { .. }) => self.folder_request(client, msg).await,
            msg @ (ClientMsg::ScanSessions
            | ClientMsg::HideObserved { .. }
            | ClientMsg::UnhideObserved { .. }) => self.discovery_request(msg).await,
            ClientMsg::LaunchCloud { request } => match self.cloud() {
                Some(cloud) => match cloud.launch(mgr, &request).await {
                    Ok(session) => ServerMsg::Launched { session },
                    Err(err) => error_msg(&err),
                },
                None => cloud_off(),
            },
            ClientMsg::ListCloudProviders => match self.cloud() {
                Some(cloud) => ServerMsg::CloudProviders {
                    providers: cloud.providers().await,
                },
                None => cloud_off(),
            },
            ClientMsg::Transcribe {
                audio,
                format,
                sample_rate,
                language,
                session_id,
                context,
                is_final,
            } => {
                let Some(speech) = self.inner.speech.get().cloned() else {
                    let reply = error(
                        ErrorCode::Unsupported,
                        "server-side transcription is off (set [speech] engine in config.toml)",
                    );
                    client.send(id, reply).await;
                    return;
                };
                let sessions = mgr.list();
                let ctx = crate::speech::intent_context(&sessions, session_id.as_ref());
                let project_dir = session_id
                    .as_ref()
                    .and_then(|sid| mgr.get(sid))
                    .and_then(|s| s.project_dir());
                let req = TranscribeRequest {
                    audio,
                    format,
                    sample_rate,
                    language,
                    project_dir,
                    context,
                    is_final,
                    topic_key: Some(topic_key(client, session_id.as_ref())),
                    device: client.device.as_ref().map(|d| d.device_id.clone()),
                };
                // Recognition takes seconds; keep serving the client meanwhile.
                let client = Arc::clone(client);
                tokio::spawn(async move {
                    let started = std::time::Instant::now();
                    let reply = match speech.transcribe(req, &ctx).await {
                        Ok(t) => ServerMsg::Transcript {
                            text: t.text,
                            raw_text: t.raw_text,
                            language: t.language,
                            corrections: t.corrections,
                            intent: t.intent,
                            engine: t.engine,
                            topic: t.topic,
                        },
                        Err(err) => error_msg(&err),
                    };
                    // The reply goes to the connection that asked; after a
                    // reconnect it is lost and the app times out instead.
                    if !client.deliver(id, reply).await {
                        tracing::warn!(
                            request = id,
                            is_final,
                            elapsed_ms =
                                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                            "transcript reply not delivered: the client disconnected"
                        );
                    }
                });
                return;
            }
            ClientMsg::ListSpeechOptions => match self.inner.speech.get() {
                Some(speech) => speech.options(&speech_device(client)),
                // Nothing to choose from: speech is off on this server.
                None => ServerMsg::SpeechOptions {
                    engines: Vec::new(),
                    llms: Vec::new(),
                    current_engine: None,
                    current_llm: None,
                },
            },
            ClientMsg::SetSpeechPreferences { engine, llm } => match self.inner.speech.get() {
                Some(speech) => {
                    let device = speech_device(client);
                    match speech.set_preferences(&device, engine, llm) {
                        Ok(()) => speech.options(&device),
                        Err(err) => error_msg(&err),
                    }
                }
                None => error(ErrorCode::Unsupported, SPEECH_OFF),
            },
            ClientMsg::ListPersonalTerms => match self.inner.speech.get() {
                Some(speech) => ServerMsg::PersonalTerms {
                    terms: speech.personal_terms(),
                },
                None => error(ErrorCode::Unsupported, SPEECH_OFF),
            },
            ClientMsg::SavePersonalTerms { terms } => self.save_personal_terms(client, terms).await,
            ClientMsg::ListTtsEngines => ServerMsg::TtsEngines {
                engines: self
                    .inner
                    .tts
                    .get()
                    .map(|t| t.engines())
                    .unwrap_or_default(),
            },
            ClientMsg::Synthesize {
                text,
                language,
                engine,
                voice,
            } => {
                let Some(tts) = self.inner.tts.get().cloned() else {
                    client
                        .send(id, error(ErrorCode::Unsupported, TTS_OFF))
                        .await;
                    return;
                };
                // Synthesis takes a moment; keep serving the client meanwhile.
                let device = speech_device(client);
                let client = Arc::clone(client);
                tokio::spawn(async move {
                    let reply = match tts
                        .synthesize(
                            &device,
                            &text,
                            language.as_deref(),
                            engine.as_deref(),
                            voice.as_deref(),
                        )
                        .await
                    {
                        Ok(out) => ServerMsg::Audio {
                            format: out.format.as_str().to_owned(),
                            data: out.audio,
                        },
                        Err(err) => error_msg(&err),
                    };
                    client.send(id, reply).await;
                });
                return;
            }
            // Built from the session list this client already receives.
            ClientMsg::Briefing { language, order } => ServerMsg::Briefing {
                text: crate::voice_brief::briefing(
                    &mgr.list(),
                    &order,
                    language.as_deref(),
                    vca_core::now_millis(),
                ),
            },
            ClientMsg::SetVoiceSummary { enabled, details } => match self.inner.summaries.get() {
                Some(summaries) => {
                    summaries.set_opt_in(&speech_device(client), enabled, details);
                    ServerMsg::Ok
                }
                None => error(ErrorCode::Unsupported, SUMMARIES_OFF),
            },
            ClientMsg::Summarize {
                session_id,
                language,
                number,
            } => {
                let Some(summaries) = self.inner.summaries.get().cloned() else {
                    client
                        .send(id, error(ErrorCode::Unsupported, SUMMARIES_OFF))
                        .await;
                    return;
                };
                let Some(session) = mgr.get(&session_id) else {
                    let reply = error(ErrorCode::NotFound, format!("no session {session_id}"));
                    client.send(id, reply).await;
                    return;
                };
                // Only what the device could read itself: a shell's output
                // needs admin scope, like `Subscribe`.
                if session.is_shell() && !admin {
                    client.send(id, forbidden()).await;
                    return;
                }
                let info = session.info();
                let (events, _) =
                    lock(session.log()).page_before(None, crate::events::MAX_PAGE_EVENTS);
                let device = speech_device(client);
                // The LLM takes a moment; keep serving the client meanwhile.
                let client = Arc::clone(client);
                tokio::spawn(async move {
                    let reply = match summaries
                        .summarize(&device, &info, &events, language.as_deref(), number)
                        .await
                    {
                        Ok(out) => ServerMsg::Summary {
                            session_id,
                            text: out.text,
                            fallback: out.fallback,
                        },
                        Err(err) => error_msg(&err),
                    };
                    client.send(id, reply).await;
                });
                return;
            }
            msg @ (ClientMsg::Attach { .. }
            | ClientMsg::AttachBegin { .. }
            | ClientMsg::AttachChunk { .. }
            | ClientMsg::AttachEnd { .. }
            | ClientMsg::AttachCancel { .. }
            | ClientMsg::AttachFolderBegin { .. }
            | ClientMsg::AttachFolderEnd { .. }
            | ClientMsg::AttachBatchEnd { .. }) => match self.inner.attachments.get() {
                Some(att) => att.handle(mgr, &client.uploads, current, msg).await,
                None => error(ErrorCode::Unsupported, "attachments are off"),
            },
            ClientMsg::ListDevices => match devices.map(|d| d.list_devices(current)) {
                Some(Ok(devices)) => ServerMsg::Devices { devices },
                Some(Err(err)) => error_msg(&err),
                None => remote_off(),
            },
            ClientMsg::RevokeDevice { device_id } => {
                drop(gate.take());
                let msg = ClientMsg::RevokeDevice {
                    device_id: device_id.clone(),
                };
                let _gates = match self.change_gates(client, &device_id, &msg).await {
                    Ok(gates) => gates,
                    Err(reply) => {
                        client.send(id, reply).await;
                        return;
                    }
                };
                match devices.map(|d| d.revoke_device(&device_id)) {
                    Some(Ok(true)) => {
                        self.forget_device(&device_id);
                        ServerMsg::Ok
                    }
                    Some(Ok(false)) => {
                        error(ErrorCode::NotFound, format!("no active device {device_id}"))
                    }
                    Some(Err(err)) => error_msg(&err),
                    None => remote_off(),
                }
            }
            // The phone is the connection's device, never one the message
            // names.
            ClientMsg::PushRegister {
                machine,
                platform,
                token,
                e2e_pubkey,
                locale,
            } => match (self.push(), current) {
                (None, _) => push_off(),
                (Some(_), None) => error(
                    ErrorCode::InvalidRequest,
                    "only a paired phone registers for approval alerts",
                ),
                (Some(push), Some(device)) => {
                    let reg = PushRegistration {
                        machine,
                        platform,
                        token,
                        e2e_pubkey,
                        locale,
                    };
                    match push.register(device, reg) {
                        Ok(()) => ServerMsg::Ok,
                        Err(err) => error_msg(&err),
                    }
                }
            },
            ClientMsg::PushUnregister => match (self.push(), current) {
                (None, _) => push_off(),
                (Some(_), None) => error(
                    ErrorCode::InvalidRequest,
                    "only a paired phone registers for approval alerts",
                ),
                (Some(push), Some(device)) => match push.remove(device) {
                    Ok(_) => ServerMsg::Ok,
                    Err(err) => error_msg(&err),
                },
            },
            // Every phone's token: only for the owner's local channel (the
            // desktop app), never for a paired device or the dev socket.
            ClientMsg::ListPushPhones => match self.push() {
                _ if client.device.is_some() || client.transport != "local" => error(
                    ErrorCode::Forbidden,
                    "only the desktop app of this computer reads the alert phones",
                ),
                Some(push) => self.push_phones(push),
                None => push_off(),
            },
            ClientMsg::RenameDevice { device_id, name } => match devices {
                Some(admin) => match admin.rename_device(&device_id, &name, by) {
                    Ok(true) => devices_reply(admin.as_ref(), current),
                    Ok(false) => {
                        error(ErrorCode::NotFound, format!("no active device {device_id}"))
                    }
                    Err(err) => error_msg(&err),
                },
                None => remote_off(),
            },
            ClientMsg::SetDeviceScope { device_id, scope } => match devices {
                // Nobody can widen (or lock out) their own permission.
                Some(_) if current == Some(device_id.as_str()) => error(
                    ErrorCode::Forbidden,
                    "a device cannot change its own permission",
                ),
                // Widening needs local verification, which a message cannot
                // carry: see `LocalRequest::DeviceScope`. Remote devices can
                // still narrow a permission, revoke and trip the kill switch.
                Some(admin) => {
                    drop(gate.take());
                    let msg = ClientMsg::SetDeviceScope {
                        device_id: device_id.clone(),
                        scope,
                    };
                    match self.change_gates(client, &device_id, &msg).await {
                        Err(reply) => reply,
                        // Checked under the target's gate, against the scope
                        // the change really replaces.
                        Ok(_gates) if raises(admin.as_ref(), &device_id, scope) => error(
                            ErrorCode::Forbidden,
                            "a device's permission can only be widened on the computer itself",
                        ),
                        Ok(_gates) => match admin.set_device_scope(&device_id, scope.into(), by) {
                            Ok(true) => devices_reply(admin.as_ref(), current),
                            Ok(false) => {
                                error(ErrorCode::NotFound, format!("no active device {device_id}"))
                            }
                            Err(err) => error_msg(&err),
                        },
                    }
                }
                None => remote_off(),
            },
            ClientMsg::KillSwitch => match devices.map(|d| d.trip_kill_switch(by)) {
                Some(Ok(())) => ServerMsg::Ok,
                Some(Err(err)) => error_msg(&err),
                None => remote_off(),
            },
            ClientMsg::GetResources => match mgr.broker() {
                Some(broker) => {
                    if !client.wants_resources.swap(true, Ordering::Relaxed) {
                        *lock(&client.resources_viewer) = Some(broker.resources_viewer());
                    }
                    ServerMsg::Resources {
                        snapshot: broker.snapshot(),
                    }
                }
                None => error(ErrorCode::Unsupported, "the resource broker is off"),
            },
            ClientMsg::ReorderQueue { job_id, position } => match mgr.broker() {
                Some(b) if b.admission().reorder(&job_id, position) => ServerMsg::Ok,
                Some(_) => error(ErrorCode::NotFound, format!("no queued job {job_id}")),
                None => error(ErrorCode::Unsupported, "the resource broker is off"),
            },
            ClientMsg::CancelQueued { job_id } => match mgr.broker() {
                Some(b) if b.admission().cancel(&job_id) => ServerMsg::Ok,
                Some(_) => error(ErrorCode::NotFound, format!("no queued job {job_id}")),
                None => error(ErrorCode::Unsupported, "the resource broker is off"),
            },
            ClientMsg::MarkRead { session_id } => match mgr.mark_read(&session_id) {
                Ok(()) => ServerMsg::Ok,
                Err(err) => error_msg(&err),
            },
            ClientMsg::GetSessionOrder => order_msg(mgr.session_order()),
            // The new order reaches every client as `SessionOrder`.
            ClientMsg::SetSessionOrder { session_ids } => {
                match mgr.set_session_order(&session_ids) {
                    Ok(order) => order_msg(order),
                    Err(err) => error_msg(&err),
                }
            }
            ClientMsg::ResetSessionOrder => match mgr.reset_session_order() {
                Ok(order) => order_msg(order),
                Err(err) => error_msg(&err),
            },
            // The new name reaches every client as `SessionUpdated`.
            ClientMsg::RenameSession { session_id, name } => {
                match mgr.rename(&session_id, name.as_deref()) {
                    Ok(_) => ServerMsg::Ok,
                    Err(err) => error_msg(&err),
                }
            }
            // Every client drops the card on `SessionRemoved`.
            // `stop` ("Remove from VCA") stops a running session VCA
            // started first.
            ClientMsg::DismissSession { session_id, stop } => {
                let removed = if stop {
                    self.remove_from_vca(client, &session_id).await
                } else {
                    mgr.dismiss(&session_id).map(|info| (info, false))
                };
                self.removed_reply(client, &session_id, removed)
            }
            msg @ (ClientMsg::ConductorGet
            | ClientMsg::ConductorConfigure { .. }
            | ClientMsg::ConductorStart
            | ClientMsg::ConductorStop { .. }
            | ClientMsg::CcPause { .. }
            | ClientMsg::ConductorDecide { .. }
            | ClientMsg::ConductorAllowSession { .. }
            | ClientMsg::ConductorAsk { .. }
            | ClientMsg::ConductorTrustFolder) => match self.conductor() {
                Some(conductor) => conductor.handle_client(self, client, msg).await,
                None => error(ErrorCode::Unsupported, "this server has no conductor"),
            },
            msg @ (ClientMsg::TaskList
            | ClientMsg::TaskCreate { .. }
            | ClientMsg::TaskUpdate { .. }
            | ClientMsg::TaskSendResult { .. }
            | ClientMsg::TaskOpenAdminShell { .. }
            | ClientMsg::TaskExport { .. }
            | ClientMsg::TaskDigest { .. }
            | ClientMsg::TaskConfigure { .. }) => self.task_request(client, msg).await,
            msg @ (ClientMsg::ListPronunciations { .. }
            | ClientMsg::SetPronunciation { .. }
            | ClientMsg::DeletePronunciation { .. }) => self.pronunciation_request(msg),
        };
        drop(gate);
        client.send(id, reply).await;
    }

    /// The gates for a change of `target` by `client` (see
    /// [`ScopeGates::change_by`]), after re-checking under them that the
    /// asking device's scope still permits `msg`: it may have been narrowed
    /// since the request's first check.
    async fn change_gates(
        &self,
        client: &Client,
        target: &str,
        msg: &ClientMsg,
    ) -> Result<(Option<RequestGuard>, ChangeGuard), ServerMsg> {
        let gates = &self.inner.scope_gates;
        let Some(device) = &client.device else {
            return Ok((None, gates.change(target).await));
        };
        let held = gates.change_by(&device.device_id, target).await;
        let scope = match self.inner.devices.get() {
            Some(admin) => admin.device_scope(&device.device_id).unwrap_or(None),
            None => Some(device.scope),
        };
        if scope.is_some_and(|s| s.permits(msg)) {
            Ok(held)
        } else {
            Err(forbidden())
        }
    }

    fn is_shell_session(&self, id: &SessionId) -> bool {
        self.inner.mgr.get(id).is_some_and(|s| s.is_shell())
    }

    /// "Remove from VCA" (`DismissSession` with `stop`): a running remote
    /// shell is closed the way its Close does (audited), then
    /// [`SessionManager::stop_and_dismiss`]. Runs as its own task, so it
    /// completes even when the asking client disconnects meanwhile.
    async fn remove_from_vca(
        &self,
        client: &Client,
        id: &SessionId,
    ) -> Result<(vca_core::SessionInfo, bool), Error> {
        let api = self.clone();
        let actor = self.actor(client);
        let id = id.clone();
        let task = tokio::spawn(async move {
            let running =
                api.inner.mgr.get(&id).is_some_and(|s| {
                    !matches!(s.info().state, vca_core::SessionState::Exited { .. })
                });
            if running
                && api.is_shell_session(&id)
                && let Some(shells) = api.shells()
            {
                shells.close(&id, CloseReason::User, Some(&actor)).await;
            }
            api.inner.mgr.stop_and_dismiss(&id).await
        });
        task.await
            .unwrap_or_else(|err| Err(Error::api(ErrorCode::Internal, err.to_string())))
    }

    /// The reply to a removal, which is audited and logged.
    fn removed_reply(
        &self,
        client: &Client,
        session_id: &SessionId,
        removed: Result<(vca_core::SessionInfo, bool), Error>,
    ) -> ServerMsg {
        let (info, stopped) = match removed {
            Ok(done) => done,
            Err(err) => return error_msg(&err),
        };
        let actor = self.actor(client);
        let detail = if stopped {
            format!("name={} stopped=true", info.display_name)
        } else {
            format!("name={}", info.display_name)
        };
        if let Some(shells) = self.shells() {
            shells.record_session_audit(
                &actor,
                crate::shell::audit::events::SESSION_DISMISS,
                session_id,
                &detail,
            );
        }
        tracing::info!(
            session = %session_id,
            device = %actor.device_id,
            stopped,
            "session removed from VCA"
        );
        ServerMsg::Ok
    }

    /// Extra rules for requests that target a shell session; `Some` is the
    /// refusal to send.
    async fn shell_session_guard(
        &self,
        client: &Client,
        admin: bool,
        msg: &ClientMsg,
    ) -> Option<ServerMsg> {
        let session_id = match msg {
            ClientMsg::SendInput { session_id, .. }
            | ClientMsg::Subscribe { session_id, .. }
            | ClientMsg::History { session_id, .. }
            | ClientMsg::Summarize { session_id, .. }
            | ClientMsg::Control { session_id, .. }
            | ClientMsg::Attach { session_id, .. }
            | ClientMsg::AttachBegin { session_id, .. }
            | ClientMsg::AttachFolderBegin { session_id, .. } => session_id,
            _ => return None,
        };
        if !self.is_shell_session(session_id) {
            return None;
        }
        match msg {
            ClientMsg::SendInput { input, .. } => {
                if !admin {
                    return Some(forbidden());
                }
                let shells = self.shells()?;
                match shells.check_input(&self.actor(client), session_id, input) {
                    Ok(crate::shell::InputCheck::Pass) => None,
                    // Several lines are typed only after a confirmation,
                    // through `ShellRun` with `confirm`.
                    Ok(crate::shell::InputCheck::Confirm { text, lines }) => {
                        Some(ServerMsg::ShellConfirm {
                            session_id: session_id.clone(),
                            command: text,
                            lines,
                        })
                    }
                    Err(err) => Some(error_msg(&err)),
                }
            }
            // A shell's output is admin only, however it is read.
            ClientMsg::Subscribe { .. }
            | ClientMsg::History { .. }
            | ClientMsg::Summarize { .. }
                if !admin =>
            {
                Some(forbidden())
            }
            // An admin device reads another device's shell only after
            // joining it with a code.
            ClientMsg::Subscribe { .. }
            | ClientMsg::History { .. }
            | ClientMsg::Summarize { .. }
            | ClientMsg::Control {
                action: SessionAction::Pause | SessionAction::Resume,
                ..
            } => self
                .shells()?
                .check_access(&self.actor(client), session_id)
                .err()
                .map(|err| error_msg(&err)),
            ClientMsg::Control {
                action: SessionAction::Restart,
                ..
            } => Some(error(
                ErrorCode::Unsupported,
                "a shell cannot be restarted; open a new one",
            )),
            ClientMsg::Attach { .. }
            | ClientMsg::AttachBegin { .. }
            | ClientMsg::AttachFolderBegin { .. } => Some(error(
                ErrorCode::Unsupported,
                "files cannot be attached to a shell session",
            )),
            _ => None,
        }
    }

    /// Streams a session's events to the client, replaying from `from_seq`
    /// first. Replaces an existing subscription to the same session.
    fn subscribe(
        &self,
        client: &Arc<Client>,
        session_id: SessionId,
        log: &Arc<Mutex<crate::events::EventLog>>,
        from_seq: Option<u64>,
    ) {
        // Only the newest page is replayed (and copied); earlier ones come
        // on request.
        let (replay, mut stream) =
            EventStream::open_latest(log, from_seq, crate::events::PAGE_EVENTS);
        let out = client.out.clone();
        let sid = session_id.clone();
        let task = tokio::spawn(async move {
            let send = |body| out.send(Envelope { id: 0, body });
            if let Some(first) = replay.gap {
                let gap = ServerMsg::StreamGap {
                    session_id: sid.clone(),
                    first_available_seq: first,
                };
                if send(gap).await.is_err() {
                    return;
                }
            }
            for event in replay.events {
                let msg = ServerMsg::Event {
                    session_id: sid.clone(),
                    event,
                };
                if send(msg).await.is_err() {
                    return;
                }
            }
            while let Some(item) = stream.recv().await {
                let msg = match item {
                    StreamItem::Event(event) => ServerMsg::Event {
                        session_id: sid.clone(),
                        event,
                    },
                    StreamItem::Gap(first) => ServerMsg::StreamGap {
                        session_id: sid.clone(),
                        first_available_seq: first,
                    },
                };
                if send(msg).await.is_err() {
                    return;
                }
            }
        });
        let mut subs = lock(&client.subs);
        // Streams of removed sessions end on their own; forget them so a
        // long connection does not collect one handle per session ever
        // watched.
        subs.retain(|_, t| !t.is_finished());
        if let Some(old) = subs.insert(session_id, task) {
            old.abort();
        }
    }
}

#[cfg(test)]
mod raises_tests {
    use vca_core::DeviceScope;

    use super::*;

    /// A device store whose scopes are `Ok(scope)` or unreadable.
    struct Store(Result<Option<Scope>, ()>);

    impl DeviceAdmin for Store {
        fn list_devices(&self, _: Option<&str>) -> Result<Vec<PairedDevice>, Error> {
            Ok(Vec::new())
        }
        fn revoke_device(&self, _: &str) -> Result<bool, Error> {
            Ok(false)
        }
        fn trip_kill_switch(&self, _: &str) -> Result<(), Error> {
            Ok(())
        }
        fn rename_device(&self, _: &str, _: &str, _: &str) -> Result<bool, Error> {
            Ok(false)
        }
        fn set_device_scope(&self, _: &str, _: Scope, _: &str) -> Result<bool, Error> {
            Ok(true)
        }
        fn device_scope(&self, _: &str) -> Result<Option<Scope>, Error> {
            self.0
                .map_err(|()| Error::api(ErrorCode::Internal, "the device store is unreadable"))
        }
    }

    /// An unreadable scope never lets a change skip the verification a
    /// widening needs.
    #[test]
    fn an_unreadable_scope_counts_as_widening() {
        assert!(raises(&Store(Err(())), "d1", DeviceScope::Admin));
        assert!(raises(&Store(Err(())), "d1", DeviceScope::View));
        assert!(raises(
            &Store(Ok(Some(Scope::View))),
            "d1",
            DeviceScope::Admin
        ));
        assert!(!raises(
            &Store(Ok(Some(Scope::Admin))),
            "d1",
            DeviceScope::View
        ));
        assert!(!raises(&Store(Ok(None)), "d1", DeviceScope::Admin));
    }
}

/// Scope checks are atomic with respect to scope changes (`ScopeGates`).
#[cfg(test)]
mod scope_race_tests {
    use std::sync::mpsc as std_mpsc;

    use vca_core::DeviceScope;

    use super::*;
    use crate::events::LogLimits;
    use crate::sessions::{ManagerConfig, SessionManager};
    use crate::workspace::WorkspaceRoots;

    /// Stops a call inside the device store until the test releases it.
    struct Pause {
        entered: std_mpsc::Sender<()>,
        release: Mutex<std_mpsc::Receiver<()>>,
    }

    impl Pause {
        fn wait(&self) {
            let _ = self.entered.send(());
            let _ = lock(&self.release).recv();
        }
    }

    /// The test's side of a [`Pause`].
    struct Handles {
        entered: std_mpsc::Receiver<()>,
        release: std_mpsc::Sender<()>,
    }

    impl Handles {
        /// Waits (off the runtime threads) until the paused call is reached.
        async fn entered(self) -> Self {
            tokio::task::spawn_blocking(move || {
                self.entered
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the paused call was not reached");
                self
            })
            .await
            .unwrap()
        }
    }

    fn pause() -> (Pause, Handles) {
        let (entered_tx, entered_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        (
            Pause {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            },
            Handles {
                entered: entered_rx,
                release: release_tx,
            },
        )
    }

    /// Paired devices in memory; logs renames and scope changes in order.
    #[derive(Default)]
    struct Devices {
        scopes: Mutex<HashMap<String, Scope>>,
        log: Mutex<Vec<String>>,
        /// Pauses the next scope read of this device.
        pause_read: Mutex<Option<(String, Pause)>>,
        /// Pauses the next scope change before it is stored.
        pause_change: Mutex<Option<Pause>>,
    }

    impl DeviceAdmin for Devices {
        fn list_devices(&self, _: Option<&str>) -> Result<Vec<PairedDevice>, Error> {
            Ok(Vec::new())
        }
        fn revoke_device(&self, device_id: &str) -> Result<bool, Error> {
            Ok(lock(&self.scopes).remove(device_id).is_some())
        }
        fn trip_kill_switch(&self, _: &str) -> Result<(), Error> {
            Ok(())
        }
        fn rename_device(&self, device_id: &str, name: &str, _: &str) -> Result<bool, Error> {
            lock(&self.log).push(format!("rename {device_id} {name}"));
            Ok(true)
        }
        fn set_device_scope(&self, device_id: &str, scope: Scope, _: &str) -> Result<bool, Error> {
            let paused = lock(&self.pause_change).take();
            if let Some(p) = paused {
                p.wait();
            }
            lock(&self.log).push(format!("scope {device_id} {scope:?}"));
            Ok(lock(&self.scopes)
                .get_mut(device_id)
                .map(|s| *s = scope)
                .is_some())
        }
        fn device_scope(&self, device_id: &str) -> Result<Option<Scope>, Error> {
            let scope = lock(&self.scopes).get(device_id).copied();
            let paused = {
                let mut slot = lock(&self.pause_read);
                match slot.take() {
                    Some((id, p)) if id == device_id => Some(p),
                    other => {
                        *slot = other;
                        None
                    }
                }
            };
            if let Some(p) = paused {
                p.wait();
            }
            Ok(scope)
        }
    }

    fn fixture(tmp: &tempfile::TempDir) -> (Api, Arc<Devices>) {
        let mgr = SessionManager::new(
            ManagerConfig {
                limits: LogLimits {
                    max_events: 100,
                    max_bytes: 1 << 20,
                },
                exited_retention: Duration::from_secs(60),
                pause_scope: vca_broker::contain::Scope::ChildrenOnly,
                heuristics_interval: Duration::from_millis(500),
                control_endpoint: tmp.path().join("ctl"),
                attach_dir: None,
                workspace_roots: WorkspaceRoots::new([tmp.path().to_owned()]),
                session_names: None,
                allow_generic_launch: false,
            },
            None,
        );
        let api = Api::new(mgr, "test".into(), Duration::from_secs(60));
        let devices = Arc::new(Devices::default());
        for (id, scope) in [
            ("dev_a", Scope::Admin),
            ("dev_b", Scope::Admin),
            ("dev_view", Scope::View),
        ] {
            lock(&devices.scopes).insert(id.into(), scope);
        }
        api.set_device_admin(Arc::clone(&devices) as Arc<dyn DeviceAdmin>);
        (api, devices)
    }

    /// One request from `device_id` on a fresh connection; its reply.
    async fn call(api: &Api, device_id: &str, body: ClientMsg) -> ServerMsg {
        let (client, mut rx) = api.connect_as(
            "test",
            Some(DeviceIdentity {
                device_id: device_id.into(),
                scope: Scope::View,
            }),
        );
        api.handle(&client, Envelope { id: 7, body }).await;
        loop {
            let env = rx.recv().await.expect("reply");
            if env.id == 7 {
                return env.body;
            }
        }
    }

    fn rename(name: &str) -> ClientMsg {
        ClientMsg::RenameDevice {
            device_id: "dev_view".into(),
            name: name.into(),
        }
    }

    fn forbidden(resp: &ServerMsg) -> bool {
        matches!(
            resp,
            ServerMsg::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        )
    }

    /// The downgrade is being applied when an admin-only request arrives:
    /// the request waits for it and is then refused under the new scope.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_request_racing_a_downgrade_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (api, devices) = fixture(&tmp);
        let (p, h) = pause();
        *lock(&devices.pause_change) = Some(p);

        let change = tokio::spawn({
            let api = api.clone();
            async move {
                api.apply_device_scope("dev_a", DeviceScope::View, false)
                    .await
            }
        });
        // The store still reports admin; the request must not act on it.
        let h = h.entered().await;
        let request = tokio::spawn({
            let api = api.clone();
            async move { call(&api, "dev_a", rename("x")).await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!request.is_finished(), "the request ran during the change");
        h.release.send(()).unwrap();

        let resp = request.await.unwrap();
        assert!(forbidden(&resp), "{resp:?}");
        let resp = change.await.unwrap();
        assert!(matches!(resp, ServerMsg::Devices { .. }), "{resp:?}");
        assert_eq!(*lock(&devices.log), vec!["scope dev_a View".to_owned()]);
    }

    /// A request already past its check finishes before the downgrade
    /// returns; after it returns the old scope authorizes nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_downgrade_waits_for_requests_the_old_scope_authorized() {
        let tmp = tempfile::tempdir().unwrap();
        let (api, devices) = fixture(&tmp);
        let (p, h) = pause();
        *lock(&devices.pause_read) = Some(("dev_a".into(), p));

        let request = tokio::spawn({
            let api = api.clone();
            async move { call(&api, "dev_a", rename("first")).await }
        });
        let h = h.entered().await;
        let change = tokio::spawn({
            let api = api.clone();
            async move {
                api.apply_device_scope("dev_a", DeviceScope::View, false)
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !change.is_finished(),
            "the downgrade returned while an authorized request was running"
        );
        h.release.send(()).unwrap();

        let resp = request.await.unwrap();
        assert!(matches!(resp, ServerMsg::Devices { .. }), "{resp:?}");
        let resp = change.await.unwrap();
        assert!(matches!(resp, ServerMsg::Devices { .. }), "{resp:?}");
        assert_eq!(
            *lock(&devices.log),
            vec![
                "rename dev_view first".to_owned(),
                "scope dev_a View".to_owned()
            ]
        );
        let resp = call(&api, "dev_a", rename("second")).await;
        assert!(forbidden(&resp), "{resp:?}");
        assert_eq!(lock(&devices.log).len(), 2);
    }

    /// Two admins narrowing each other at once cannot deadlock, and each
    /// change is checked against the other: exactly one applies.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn admins_narrowing_each_other_do_not_deadlock() {
        for _ in 0..25 {
            let tmp = tempfile::tempdir().unwrap();
            let (api, devices) = fixture(&tmp);
            let narrow = |actor: &'static str, target: &'static str| {
                let api = api.clone();
                tokio::spawn(async move {
                    let msg = ClientMsg::SetDeviceScope {
                        device_id: target.into(),
                        scope: DeviceScope::View,
                    };
                    call(&api, actor, msg).await
                })
            };
            let (a, b) = (narrow("dev_a", "dev_b"), narrow("dev_b", "dev_a"));
            let (a, b) = tokio::time::timeout(Duration::from_secs(10), async {
                (a.await.unwrap(), b.await.unwrap())
            })
            .await
            .expect("deadlock");
            let applied = [&a, &b]
                .iter()
                .filter(|r| matches!(r, ServerMsg::Devices { .. }))
                .count();
            assert_eq!(applied, 1, "{a:?} {b:?}");
            assert_eq!(lock(&devices.log).len(), 1);
        }
    }
}
