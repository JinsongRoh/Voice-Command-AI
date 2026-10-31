//! [`ClientHub`]: paired machines and their supervised connections.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::watch;
use vca_core::{ClientMsg, Envelope};
use vca_net::{
    AuthFailure, ClientAuth, ClientConfig, ClientConnection, CloseReason, ConnectionSender,
    KeyStore, NetError, PairingPayload, PublicKey, StaticKeypair, TokenPair,
};
use zeroize::Zeroizing;

use crate::backoff::Backoff;
use crate::error::{ClientError, is_transport_error};
use crate::store::{self, StoredMachine, device_key_name, refresh_token_name, sanitize_display};
use crate::{LinkSink, LinkState, LinkStatus, MachineInfo};

const NAME_MAX: usize = 64;
const PLATFORM_MAX: usize = 32;

/// Tunables of a [`ClientHub`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Deadline for connect + Noise handshake + authentication, per endpoint.
    pub connect_timeout: Duration,
    /// Client heartbeat interval (at most 30 s).
    pub heartbeat_interval: Duration,
    /// Reconnect backoff.
    pub backoff: Backoff,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            heartbeat_interval: Duration::from_secs(20),
            backoff: Backoff::default(),
        }
    }
}

/// Paired machines plus one supervised connection per machine.
///
/// `Send + Sync`; clones share state (it is an `Arc` inside).
#[derive(Clone)]
pub struct ClientHub {
    inner: Arc<Inner>,
}

struct Inner {
    data_dir: PathBuf,
    secrets: Arc<dyn KeyStore>,
    options: ClientOptions,
    runtime: Option<Handle>,
    /// Metadata. Key-store writes that depend on "which pairing is current"
    /// happen while this lock is held, so a stale connection task can never
    /// overwrite the secrets of a newer pairing.
    machines: Mutex<Vec<StoredMachine>>,
    links: Mutex<HashMap<String, Link>>,
    next_generation: AtomicU64,
}

/// A requested (supervised) connection.
struct Link {
    generation: u64,
    sink: Arc<dyn LinkSink>,
    status: LinkStatus,
    sender: Option<ConnectionSender<ClientMsg>>,
    stop: watch::Sender<bool>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_millis() -> u64 {
    vca_core::now_millis()
}

/// Why a connection attempt of the supervisor failed.
enum Failure {
    /// Try again after a backoff delay.
    Retry {
        code: &'static str,
        message: String,
        long: bool,
    },
    /// Stop supervising and report this status.
    Stop(LinkStatus),
}

impl ClientHub {
    /// Opens the hub. `data_dir` holds `machines.json` (non-secret metadata
    /// only); `secrets` holds device keys and tokens.
    pub fn open(
        data_dir: impl Into<PathBuf>,
        secrets: Arc<dyn KeyStore>,
    ) -> Result<Self, ClientError> {
        Self::open_with(data_dir, secrets, ClientOptions::default())
    }

    /// [`ClientHub::open`] with explicit options.
    pub fn open_with(
        data_dir: impl Into<PathBuf>,
        secrets: Arc<dyn KeyStore>,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        // `wss://` endpoints need a rustls crypto provider; install `ring`
        // unless the application already chose one.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let data_dir = data_dir.into();
        store::create_private_dir(&data_dir)?;
        let machines = store::load(&data_dir)?;
        Ok(Self {
            inner: Arc::new(Inner {
                data_dir,
                secrets,
                options,
                runtime: Handle::try_current().ok(),
                machines: Mutex::new(machines),
                links: Mutex::new(HashMap::new()),
                next_generation: AtomicU64::new(1),
            }),
        })
    }

    /// The paired machines, in pairing order.
    pub fn machines(&self) -> Vec<MachineInfo> {
        lock(&self.inner.machines)
            .iter()
            .map(|m| m.info.clone())
            .collect()
    }

    /// Parses the `vca://pair?...` URI, generates a fresh device keypair for
    /// this server, pairs (trying endpoints in order), stores keypair and
    /// refresh token in the key store and metadata in `machines.json`.
    /// Re-pairing an already known server id replaces it (its supervised
    /// connection is stopped).
    pub async fn pair(
        &self,
        uri: &str,
        device_name: &str,
        platform: &str,
        totp: Option<String>,
    ) -> Result<MachineInfo, ClientError> {
        let payload = PairingPayload::parse(uri).map_err(|e| match e {
            NetError::InvalidPairingPayload(m) => ClientError::InvalidPairing(m),
            other => ClientError::InvalidPairing(other.to_string()),
        })?;
        if !store::is_valid_server_id(&payload.server_id) {
            return Err(ClientError::InvalidPairing("invalid server id".into()));
        }
        if payload.is_expired(now_millis() / 1000) {
            return Err(ClientError::InvalidPairing(
                "the pairing code has expired; show a new QR code".into(),
            ));
        }
        let id = payload.server_id.clone();
        let device_name = sanitize_display(device_name, NAME_MAX);
        let platform = sanitize_display(platform, PLATFORM_MAX);
        let keypair = StaticKeypair::generate();
        let totp = totp.map(Zeroizing::new);
        let mut conn = self
            .inner
            .connect_any(&payload.endpoints, payload.server_key, &keypair, || {
                ClientAuth::Pair {
                    secret: payload.secret.clone(),
                    device_name: device_name.clone(),
                    platform: platform.clone(),
                    totp: totp.as_ref().map(|t| t.to_string()),
                }
            })
            .await?;
        let Some(tokens) = conn.session.tokens.clone() else {
            conn.close().await;
            return Err(ClientError::Protocol(
                "the server sent no tokens after pairing".into(),
            ));
        };
        let mut name = sanitize_display(&conn.session.server_name, NAME_MAX);
        if name.is_empty() {
            name = sanitize_display(&payload.server_name, NAME_MAX);
        }
        let stored = StoredMachine {
            info: MachineInfo {
                id: id.clone(),
                name,
                endpoints: payload.endpoints.clone(),
                device_id: conn.session.device_id.clone(),
                scope: conn.session.scope,
                paired_at: now_millis(),
            },
            server_key: payload.server_key,
        };
        // Replace an existing pairing: stop its connection first.
        self.disconnect(&id);
        let result = self.inner.store_pairing(&stored, &keypair, &tokens);
        conn.close().await;
        result?;
        Ok(stored.info)
    }

    /// Starts (or replaces) the supervised connection to a machine and
    /// returns immediately; progress is reported through `sink`.
    ///
    /// The supervisor authenticates with the stored refresh token (the
    /// rotated pair is stored before `Connected` is reported), tries the
    /// endpoints in order and reconnects with exponential backoff after
    /// drops. Failures that need the user (expired, reused or missing
    /// refresh token, TOTP required) end in `LoginRequired`; a revoked or
    /// unknown device (`revoked`) and the kill switch (`kill_switch`) end in
    /// `Disconnected`. Supervision stops in both cases.
    ///
    /// Must be called within a tokio runtime, or the hub must have been
    /// opened within one.
    pub fn connect(&self, machine_id: &str, sink: Arc<dyn LinkSink>) -> Result<(), ClientError> {
        self.inner.machine(machine_id)?;
        let handle = self.inner.runtime_handle()?;
        Inner::spawn_link(&self.inner, &handle, machine_id, sink, None);
        Ok(())
    }

    /// Signs in again with TOTP or a backup code (as the same device: the
    /// stored device key identifies it), stores the new tokens and resumes
    /// the supervised connection if one was requested (the new connection
    /// is used directly).
    ///
    /// On a server that does not require TOTP, a login without codes
    /// succeeds for a paired, non-revoked device.
    pub async fn login(
        &self,
        machine_id: &str,
        totp: Option<String>,
        backup_code: Option<String>,
    ) -> Result<(), ClientError> {
        let inner = &self.inner;
        let machine = inner.machine(machine_id)?;
        let keypair = inner
            .device_keypair(machine_id)?
            .ok_or_else(|| ClientError::Storage("device key missing; pair again".into()))?;
        let totp = totp.map(Zeroizing::new);
        let backup_code = backup_code.map(Zeroizing::new);
        let mut conn = inner
            .connect_any(
                &machine.info.endpoints,
                machine.server_key,
                &keypair,
                || ClientAuth::Login {
                    totp: totp.as_ref().map(|t| t.to_string()),
                    backup_code: backup_code.as_ref().map(|t| t.to_string()),
                },
            )
            .await?;
        let Some(tokens) = conn.session.tokens.clone() else {
            conn.close().await;
            return Err(ClientError::Protocol(
                "the server sent no tokens after login".into(),
            ));
        };
        if let Err(e) = inner.store_rotated(machine_id, &conn, &tokens) {
            conn.close().await;
            return Err(e);
        }
        let resume = lock(&inner.links)
            .get(machine_id)
            .map(|l| Arc::clone(&l.sink));
        match (resume, inner.runtime_handle()) {
            (Some(sink), Ok(handle)) => {
                Inner::spawn_link(inner, &handle, machine_id, sink, Some(conn));
            }
            _ => {
                conn.close().await;
            }
        }
        Ok(())
    }

    /// Sends one `Envelope<ClientMsg>` (MessagePack) on the live connection.
    /// The bytes are validated first; anything else is rejected with
    /// [`ClientError::Protocol`].
    pub async fn send(&self, machine_id: &str, msgpack: &[u8]) -> Result<(), ClientError> {
        self.inner.machine(machine_id)?;
        // The decode error is not included: it may quote message content.
        let envelope: Envelope<ClientMsg> = rmp_serde::from_slice(msgpack)
            .map_err(|_| ClientError::Protocol("not a valid MessagePack client envelope".into()))?;
        let sender = lock(&self.inner.links)
            .get(machine_id)
            .and_then(|l| l.sender.clone())
            .ok_or(ClientError::NotConnected)?;
        sender.send(&envelope).await.map_err(|e| match e {
            NetError::Closed(_) => ClientError::NotConnected,
            other => ClientError::from(other),
        })
    }

    /// Current status of a machine connection. Unknown machines report
    /// `Disconnected` with code `not_found`.
    pub fn status(&self, machine_id: &str) -> LinkStatus {
        if let Some(link) = lock(&self.inner.links).get(machine_id) {
            return link.status.clone();
        }
        if self.inner.machine(machine_id).is_err() {
            return LinkStatus::with_error(
                LinkState::Disconnected,
                "not_found",
                format!("unknown machine {machine_id}"),
            );
        }
        LinkStatus::disconnected()
    }

    /// Stops supervision, closes the connection and emits `Disconnected`.
    pub fn disconnect(&self, machine_id: &str) {
        let link = lock(&self.inner.links).remove(machine_id);
        if let Some(link) = link {
            link.stop.send_replace(true);
            link.sink.status(LinkStatus::disconnected());
        }
    }

    /// Disconnects every machine.
    pub fn disconnect_all(&self) {
        let links: Vec<Link> = lock(&self.inner.links).drain().map(|(_, l)| l).collect();
        for link in links {
            link.stop.send_replace(true);
            link.sink.status(LinkStatus::disconnected());
        }
    }

    /// Disconnects and deletes the machine: device key, tokens and metadata.
    pub async fn forget(&self, machine_id: &str) -> Result<(), ClientError> {
        if !store::is_valid_server_id(machine_id) {
            return Err(ClientError::NotFound(machine_id.to_string()));
        }
        self.disconnect(machine_id);
        let inner = &self.inner;
        let mut machines = lock(&inner.machines);
        inner
            .secrets
            .delete(&refresh_token_name(machine_id))
            .map_err(ClientError::from)?;
        inner
            .secrets
            .delete(&device_key_name(machine_id))
            .map_err(ClientError::from)?;
        let before = machines.len();
        let mut next = machines.clone();
        next.retain(|m| m.info.id != machine_id);
        if next.len() == before {
            return Err(ClientError::NotFound(machine_id.to_string()));
        }
        store::save(&inner.data_dir, &next)?;
        *machines = next;
        Ok(())
    }
}

impl Inner {
    fn runtime_handle(&self) -> Result<Handle, ClientError> {
        Handle::try_current()
            .ok()
            .or_else(|| self.runtime.clone())
            .ok_or_else(|| ClientError::Internal("no tokio runtime available".into()))
    }

    fn machine(&self, id: &str) -> Result<StoredMachine, ClientError> {
        lock(&self.machines)
            .iter()
            .find(|m| m.info.id == id)
            .cloned()
            .ok_or_else(|| ClientError::NotFound(id.to_string()))
    }

    fn device_keypair(&self, id: &str) -> Result<Option<StaticKeypair>, ClientError> {
        match self.secrets.load(&device_key_name(id))? {
            None => Ok(None),
            Some(bytes) => StaticKeypair::from_secret_slice(&bytes)
                .map(Some)
                .map_err(|_| ClientError::Storage("stored device key is corrupt".into())),
        }
    }

    fn refresh_token(&self, id: &str) -> Result<Option<Zeroizing<String>>, ClientError> {
        match self.secrets.load(&refresh_token_name(id))? {
            None => Ok(None),
            Some(bytes) => String::from_utf8(bytes.to_vec())
                .map(|s| Some(Zeroizing::new(s)))
                .map_err(|_| ClientError::Storage("stored refresh token is corrupt".into())),
        }
    }

    /// Stores a new pairing (keys first, metadata last), replacing any
    /// previous one with the same id.
    fn store_pairing(
        &self,
        stored: &StoredMachine,
        keypair: &StaticKeypair,
        tokens: &TokenPair,
    ) -> Result<(), ClientError> {
        let id = &stored.info.id;
        let mut machines = lock(&self.machines);
        self.secrets
            .save(&device_key_name(id), keypair.secret_bytes())?;
        self.secrets
            .save(&refresh_token_name(id), tokens.refresh_token.as_bytes())?;
        let mut next = machines.clone();
        match next.iter_mut().find(|m| m.info.id == *id) {
            Some(m) => *m = stored.clone(),
            None => next.push(stored.clone()),
        }
        store::save(&self.data_dir, &next)?;
        *machines = next;
        Ok(())
    }

    /// Stores a rotated refresh token (and refreshed metadata) if the
    /// connection still belongs to the current pairing of `id`.
    fn store_rotated(
        &self,
        id: &str,
        conn: &ClientConnection,
        tokens: &TokenPair,
    ) -> Result<(), ClientError> {
        let mut machines = lock(&self.machines);
        let Some(pos) = machines
            .iter()
            .position(|m| m.info.id == id && m.info.device_id == conn.session.device_id)
        else {
            return Err(ClientError::NotFound(id.to_string()));
        };
        self.secrets
            .save(&refresh_token_name(id), tokens.refresh_token.as_bytes())?;
        let name = sanitize_display(&conn.session.server_name, NAME_MAX);
        let current = &machines[pos].info;
        if (!name.is_empty() && name != current.name) || conn.session.scope != current.scope {
            let mut next = machines.clone();
            if !name.is_empty() {
                next[pos].info.name = name;
            }
            next[pos].info.scope = conn.session.scope;
            match store::save(&self.data_dir, &next) {
                Ok(()) => *machines = next,
                // The token is safe; stale display metadata is not fatal.
                Err(e) => tracing::warn!(machine = id, "cannot update machine metadata: {e}"),
            }
        }
        Ok(())
    }

    /// Connects to the first endpoint that works. Errors from the
    /// authenticated server end the attempt; transport errors move on to
    /// the next endpoint. A server key mismatch is reported in preference
    /// to other transport errors.
    async fn connect_any(
        &self,
        endpoints: &[String],
        server_key: PublicKey,
        keypair: &StaticKeypair,
        mut auth: impl FnMut() -> ClientAuth,
    ) -> Result<ClientConnection, NetError> {
        let mut last: Option<NetError> = None;
        let mut mismatch: Option<NetError> = None;
        for url in endpoints {
            let mut config = ClientConfig::new(url.clone(), server_key, keypair.clone());
            config.connect_timeout = self.options.connect_timeout;
            config.heartbeat_interval = self.options.heartbeat_interval;
            match ClientConnection::connect(config, auth()).await {
                Ok(conn) => return Ok(conn),
                Err(e) if is_transport_error(&e) => {
                    tracing::debug!(endpoint = %url, "endpoint failed: {e}");
                    if matches!(e, NetError::ServerKeyMismatch { .. }) {
                        tracing::warn!(endpoint = %url, "endpoint presented a different server key");
                        mismatch = Some(e);
                    } else {
                        last = Some(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(mismatch
            .or(last)
            .unwrap_or_else(|| NetError::InvalidArgument("the machine has no endpoints".into())))
    }

    /// Registers a new link generation (stopping the previous one) and
    /// spawns its supervisor.
    fn spawn_link(
        inner: &Arc<Inner>,
        handle: &Handle,
        id: &str,
        sink: Arc<dyn LinkSink>,
        initial: Option<ClientConnection>,
    ) {
        let generation = inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let (stop_tx, stop_rx) = watch::channel(false);
        let status = LinkStatus::new(LinkState::Connecting);
        let old = lock(&inner.links).insert(
            id.to_string(),
            Link {
                generation,
                sink: Arc::clone(&sink),
                status: status.clone(),
                sender: None,
                stop: stop_tx,
            },
        );
        if let Some(old) = old {
            old.stop.send_replace(true);
        }
        sink.status(status);
        handle.spawn(supervise(
            Arc::clone(inner),
            id.to_string(),
            generation,
            sink,
            stop_rx,
            initial,
        ));
    }

    /// Updates the status of the current link generation and notifies its
    /// sink. Returns `false` if `generation` is no longer current.
    fn set_status(&self, id: &str, generation: u64, status: LinkStatus) -> bool {
        let sink = {
            let mut links = lock(&self.links);
            match links.get_mut(id) {
                Some(l) if l.generation == generation => {
                    l.status = status.clone();
                    if status.state != LinkState::Connected {
                        l.sender = None;
                    }
                    Arc::clone(&l.sink)
                }
                _ => return false,
            }
        };
        sink.status(status);
        true
    }

    /// Publishes the sender of a fresh connection and reports `Connected`.
    fn set_connected(
        &self,
        id: &str,
        generation: u64,
        sender: ConnectionSender<ClientMsg>,
    ) -> bool {
        let status = LinkStatus::new(LinkState::Connected);
        let sink = {
            let mut links = lock(&self.links);
            match links.get_mut(id) {
                Some(l) if l.generation == generation => {
                    l.status = status.clone();
                    l.sender = Some(sender);
                    Arc::clone(&l.sink)
                }
                _ => return false,
            }
        };
        sink.status(status);
        true
    }

    /// One refresh-token connection attempt across all endpoints.
    async fn connect_refresh(&self, id: &str) -> Result<ClientConnection, Failure> {
        let storage_retry = |e: ClientError| Failure::Retry {
            code: "storage",
            message: e.to_string(),
            long: false,
        };
        let machine = self.machine(id).map_err(|_| {
            Failure::Stop(LinkStatus::with_error(
                LinkState::Disconnected,
                "not_found",
                format!("unknown machine {id}"),
            ))
        })?;
        let Some(keypair) = self.device_keypair(id).map_err(storage_retry)? else {
            return Err(Failure::Stop(LinkStatus::with_error(
                LinkState::Disconnected,
                "storage",
                "the device key is missing; pair this device again",
            )));
        };
        let Some(refresh) = self.refresh_token(id).map_err(storage_retry)? else {
            return Err(Failure::Stop(LinkStatus::with_error(
                LinkState::LoginRequired,
                "auth_failed",
                "no stored session; sign in again",
            )));
        };
        let mut conn = self
            .connect_any(
                &machine.info.endpoints,
                machine.server_key,
                &keypair,
                || ClientAuth::RefreshToken(refresh.to_string()),
            )
            .await
            .map_err(failure_from_net)?;
        let Some(tokens) = conn.session.tokens.clone() else {
            conn.close().await;
            return Err(Failure::Stop(LinkStatus::with_error(
                LinkState::Disconnected,
                "protocol",
                "the server sent no tokens after a refresh",
            )));
        };
        if let Err(e) = self.store_rotated(id, &conn, &tokens) {
            conn.close().await;
            let status = match e {
                ClientError::NotFound(_) => LinkStatus::with_error(
                    LinkState::Disconnected,
                    "not_found",
                    "the machine was removed or paired again",
                ),
                other => LinkStatus::with_error(
                    LinkState::Disconnected,
                    "storage",
                    format!("cannot store the session: {other}"),
                ),
            };
            return Err(Failure::Stop(status));
        }
        Ok(conn)
    }
}

/// Maps a failed connection attempt to the supervisor's reaction.
fn failure_from_net(e: NetError) -> Failure {
    let stop = |state, code: &'static str, msg: String| {
        Failure::Stop(LinkStatus::with_error(state, code, msg))
    };
    let retry = |code: &'static str, message: String, long: bool| Failure::Retry {
        code,
        message,
        long,
    };
    match e {
        NetError::Auth(f) => {
            let msg = f.to_string();
            match f {
                AuthFailure::UnknownDevice | AuthFailure::DeviceRevoked => stop(
                    LinkState::Disconnected,
                    "revoked",
                    format!("{msg}; pair this device again"),
                ),
                AuthFailure::KillSwitch => stop(LinkState::Disconnected, "kill_switch", msg),
                AuthFailure::InvalidToken | AuthFailure::TokenReuse => stop(
                    LinkState::LoginRequired,
                    "auth_failed",
                    format!("{msg}; sign in again"),
                ),
                AuthFailure::TotpRequired
                | AuthFailure::InvalidTotp
                | AuthFailure::TotpNotEnrolled => {
                    stop(LinkState::LoginRequired, "totp_required", msg)
                }
                AuthFailure::RateLimited => retry("rate_limited", msg, true),
                AuthFailure::Internal => retry("server_error", msg, false),
                AuthFailure::Protocol
                | AuthFailure::AlreadyPaired
                | AuthFailure::InvalidPairingSecret => {
                    stop(LinkState::Disconnected, "protocol", msg)
                }
            }
        }
        NetError::ServerKeyMismatch { .. } => retry(
            "server_key_mismatch",
            "an endpoint presented a different server key (connection refused)".into(),
            false,
        ),
        other => retry("unreachable", other.to_string(), false),
    }
}

/// Resolves when supervision of this generation should stop.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    // A dropped sender (link removed) also means stop.
    let _ = stop.wait_for(|s| *s).await;
}

async fn supervise(
    inner: Arc<Inner>,
    id: String,
    generation: u64,
    sink: Arc<dyn LinkSink>,
    mut stop: watch::Receiver<bool>,
    mut initial: Option<ClientConnection>,
) {
    let is_stopped = |stop: &watch::Receiver<bool>| *stop.borrow() || stop.has_changed().is_err();
    let mut attempt: u32 = 0;
    loop {
        let mut conn = match initial.take() {
            Some(conn) => conn,
            None => {
                // Not cancelled on stop: an interrupted refresh could lose the
                // rotated token, so the attempt finishes (bounded by the
                // connect timeout) and is stored first.
                match inner.connect_refresh(&id).await {
                    Ok(conn) => conn,
                    Err(Failure::Stop(status)) => {
                        inner.set_status(&id, generation, status);
                        return;
                    }
                    Err(Failure::Retry {
                        code,
                        message,
                        long,
                    }) => {
                        if is_stopped(&stop) {
                            return;
                        }
                        let status = LinkStatus::with_error(LinkState::Reconnecting, code, message);
                        if !inner.set_status(&id, generation, status) {
                            return;
                        }
                        let backoff = inner.options.backoff;
                        let delay = if long {
                            backoff.max
                        } else {
                            backoff.delay(attempt)
                        };
                        attempt = attempt.saturating_add(1);
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => continue,
                            _ = stopped(&mut stop) => return,
                        }
                    }
                }
            }
        };
        if is_stopped(&stop) || !inner.set_connected(&id, generation, conn.sender()) {
            conn.close().await;
            return;
        }
        loop {
            tokio::select! {
                _ = stopped(&mut stop) => {
                    conn.close().await;
                    return;
                }
                msg = conn.recv() => match msg {
                    Some(Ok(envelope)) => match rmp_serde::to_vec_named(&envelope) {
                        Ok(bytes) => sink.message(bytes),
                        Err(e) => tracing::warn!(machine = %id, "cannot re-encode a server message: {e}"),
                    },
                    // E.g. a message type from a newer server; skip it.
                    Some(Err(_)) => tracing::debug!(machine = %id, "skipping an undecodable server message"),
                    None => break,
                },
            }
        }
        let reason = conn.closed().await;
        let status = match reason {
            CloseReason::DeviceRevoked => Some(LinkStatus::with_error(
                LinkState::Disconnected,
                "revoked",
                "this device was revoked by the server; pair it again",
            )),
            CloseReason::KillSwitch => Some(LinkStatus::with_error(
                LinkState::Disconnected,
                "kill_switch",
                "the server's kill switch was engaged",
            )),
            _ => None,
        };
        if let Some(status) = status {
            inner.set_status(&id, generation, status);
            return;
        }
        if is_stopped(&stop) {
            return;
        }
        let status = LinkStatus::with_error(
            LinkState::Reconnecting,
            "unreachable",
            format!("connection lost ({reason})"),
        );
        if !inner.set_status(&id, generation, status) {
            return;
        }
        let delay = inner.options.backoff.delay(0);
        attempt = 1;
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stopped(&mut stop) => return,
        }
    }
}
