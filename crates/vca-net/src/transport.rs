//! WebSocket transport: connect, Noise handshake, authenticate, then
//! exchange encrypted MessagePack [`Envelope`]s with a heartbeat.
//!
//! Connection lifecycle (both sides):
//!
//! 1. HTTP/1.1 WebSocket upgrade (plain HTTP; the server binds loopback by
//!    default and providers terminate TLS in front of it, dnspilot C2/C3).
//! 2. Noise `XX` handshake ([`crate::noise`]). The device pins the server
//!    key; the server learns the device key.
//! 3. Authentication, inside Noise, with control frames:
//!    * `pair` (one-time QR secret) followed by `login`;
//!    * `login` (TOTP or backup code when required by policy);
//!    * `token` (access token) or `refresh` (rotating refresh token).
//!
//!    Any failure ends the connection after an encrypted `auth_error`.
//! 4. Application phase: each Noise message carries a one-byte kind
//!    ([`KIND_CONTROL`] or [`KIND_APP`]) and a MessagePack body. Control
//!    frames are heartbeat `ping`/`pong` and `close`.
//!
//! Heartbeat: each side pings every `heartbeat_interval` (at most 30 s,
//! dnspilot C4) and closes the connection when nothing has been received
//! for two intervals.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt, future};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::time::{Instant, timeout};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use vca_core::{ClientMsg, Envelope, ServerMsg, UnixMillis};
use zeroize::Zeroizing;

use crate::audit::{
    AuditEvent, AuditLog, AuthMethod, X_DNSPILOT_CLIENT_ID, X_FORWARDED_FOR, client_ip_from_headers,
};
use crate::clock::{SharedClock, system_clock};
use crate::error::{AuthFailure, CloseReason, NetError, Result};
use crate::keys::{PublicKey, StaticKeypair};
use crate::killswitch::KillSwitch;
use crate::noise::{
    FrameTransport, MAX_NOISE_MESSAGE, NoiseConfig, NoiseSession, handshake_initiator,
    handshake_responder,
};
use crate::pairing::{PairingManager, PairingPayload, RedeemError};
use crate::ratelimit::{RateLimitConfig, RateLimiter};
use crate::registry::{DeviceRecord, DeviceRegistry, Scope};
use crate::tokens::{TokenError, TokenPair, TokenService};
use crate::totp::TotpManager;
use crate::util::{b64d, b64e, sanitize_display};

/// Message kind byte: control frame (auth, heartbeat, close).
pub const KIND_CONTROL: u8 = 0;
/// Message kind byte: application envelope.
pub const KIND_APP: u8 = 1;

/// Upper bound for the heartbeat interval (dnspilot C4).
pub const MAX_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum message size before authentication completes.
const PRE_AUTH_MAX_MESSAGE: usize = 8 * 1024;
/// Maximum auth frames per connection (`pair` + `login`).
const MAX_AUTH_FRAMES: usize = 2;
/// Time allowed for the final `close` frame and WebSocket close.
const CLOSE_GRACE: Duration = Duration::from_millis(300);
const DEVICE_NAME_MAX: usize = 64;
const GLOBAL_PAIR_KEY: &str = "pair:global";

/// Control frames inside the Noise channel. Not `Debug`, because some
/// variants carry secrets.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Control {
    Pair {
        secret: String,
        device_name: String,
        #[serde(default)]
        platform: String,
    },
    Login {
        totp: Option<String>,
        backup_code: Option<String>,
    },
    Token {
        access_token: String,
    },
    Refresh {
        refresh_token: String,
    },
    Paired {
        device_id: String,
        scope: Scope,
    },
    AuthOk {
        device_id: String,
        scope: Scope,
        server_name: String,
        tokens: Option<TokenPair>,
    },
    AuthError {
        failure: AuthFailure,
    },
    Ping {
        n: u64,
    },
    Pong {
        n: u64,
    },
    Close {
        reason: CloseReason,
    },
}

fn encode_control(c: &Control) -> Vec<u8> {
    let mut out = vec![KIND_CONTROL];
    out.extend(rmp_serde::to_vec_named(c).expect("control frames serialize"));
    out
}

fn decode_control(body: &[u8]) -> Result<Control> {
    rmp_serde::from_slice(body).map_err(|e| NetError::Protocol(format!("bad control frame: {e}")))
}

async fn send_control<S>(sink: &mut S, session: &mut NoiseSession, c: &Control) -> Result<()>
where
    S: Sink<Vec<u8>, Error = NetError> + Unpin + ?Sized,
{
    let bytes = Zeroizing::new(encode_control(c));
    session.send(sink, &bytes).await
}

async fn recv_control<S>(stream: &mut S, session: &mut NoiseSession) -> Result<Control>
where
    S: Stream<Item = Result<Vec<u8>>> + Unpin + ?Sized,
{
    let msg = Zeroizing::new(
        session
            .recv(stream)
            .await?
            .ok_or_else(|| NetError::Protocol("peer closed during authentication".into()))?,
    );
    match msg.split_first() {
        Some((&KIND_CONTROL, body)) => decode_control(body),
        _ => Err(NetError::Protocol("expected a control frame".into())),
    }
}

fn ws_config() -> WebSocketConfig {
    // One WebSocket message carries exactly one Noise message.
    WebSocketConfig::default()
        .max_message_size(Some(MAX_NOISE_MESSAGE + 64))
        .max_frame_size(Some(MAX_NOISE_MESSAGE + 64))
}

/// Adapts a WebSocket stream into a [`FrameTransport`] of binary messages.
/// Text frames are a protocol error; WebSocket ping/pong frames are handled
/// by the WebSocket layer and skipped.
pub fn ws_frames<S>(ws: WebSocketStream<S>) -> impl FrameTransport
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    ws.sink_map_err(NetError::from)
        .with(|b: Vec<u8>| future::ready(Ok::<_, NetError>(Message::Binary(b.into()))))
        .filter_map(|m| {
            future::ready(match m {
                Ok(Message::Binary(b)) => Some(Ok(Vec::from(b))),
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => None,
                Ok(Message::Text(_)) => Some(Err(NetError::Protocol(
                    "text frames are not allowed".into(),
                ))),
                Ok(Message::Close(_)) => Some(Err(NetError::Closed(CloseReason::PeerClosed))),
                Err(e) => Some(Err(e.into())),
            })
        })
}

// ---------------------------------------------------------------------------
// Generic connection (both sides)
// ---------------------------------------------------------------------------

enum Outgoing {
    App(Vec<u8>),
    Close(CloseReason),
}

/// Cloneable handle for sending envelopes on a connection.
pub struct ConnectionSender<Out> {
    tx: mpsc::Sender<Outgoing>,
    state: watch::Receiver<Option<CloseReason>>,
    max_message_size: usize,
    _out: PhantomData<fn(Out)>,
}

impl<Out> Clone for ConnectionSender<Out> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            state: self.state.clone(),
            max_message_size: self.max_message_size,
            _out: PhantomData,
        }
    }
}

impl<Out: Serialize> ConnectionSender<Out> {
    fn closed_error(&self) -> NetError {
        NetError::Closed(self.state.borrow().unwrap_or(CloseReason::Normal))
    }

    /// Encodes and queues an envelope. Waits if the outgoing queue is full.
    pub async fn send(&self, msg: &Envelope<Out>) -> Result<()> {
        let mut bytes = Vec::with_capacity(64);
        bytes.push(KIND_APP);
        rmp_serde::encode::write_named(&mut bytes, msg).map_err(NetError::encoding)?;
        if bytes.len() > self.max_message_size {
            return Err(NetError::MessageTooLarge {
                size: bytes.len(),
                limit: self.max_message_size,
            });
        }
        self.tx
            .send(Outgoing::App(bytes))
            .await
            .map_err(|_| self.closed_error())
    }

    /// Requests an orderly close after already queued messages are sent.
    pub async fn close(&self) {
        let _ = self.tx.send(Outgoing::Close(CloseReason::Normal)).await;
    }

    /// True once the connection has terminated.
    pub fn is_closed(&self) -> bool {
        self.state.borrow().is_some()
    }
}

/// An authenticated, encrypted connection carrying `Envelope<In>` inbound
/// and `Envelope<Out>` outbound.
pub struct Connection<In, Out> {
    rx: mpsc::Receiver<Vec<u8>>,
    sender: ConnectionSender<Out>,
    state: watch::Receiver<Option<CloseReason>>,
    _in: PhantomData<fn() -> In>,
}

impl<In: DeserializeOwned, Out: Serialize> Connection<In, Out> {
    /// Receives the next envelope. `None` once the connection is closed (see
    /// [`Connection::close_reason`]). A message that fails to decode yields
    /// `Some(Err(..))` and the connection stays open, so the application
    /// can answer with an error.
    pub async fn recv(&mut self) -> Option<Result<Envelope<In>>> {
        let bytes = self.rx.recv().await?;
        Some(rmp_serde::from_slice(&bytes).map_err(NetError::encoding))
    }

    /// Sends an envelope.
    pub async fn send(&self, msg: &Envelope<Out>) -> Result<()> {
        self.sender.send(msg).await
    }

    /// A cloneable sender, e.g. for a task that streams events.
    pub fn sender(&self) -> ConnectionSender<Out> {
        self.sender.clone()
    }

    /// Closes the connection after queued messages are sent and waits
    /// until it is closed.
    pub async fn close(&mut self) -> CloseReason {
        self.sender.close().await;
        self.rx.close();
        self.closed().await
    }

    /// Why the connection closed, or `None` while it is open.
    pub fn close_reason(&self) -> Option<CloseReason> {
        *self.state.borrow()
    }

    /// Waits until the connection is closed.
    pub async fn closed(&self) -> CloseReason {
        let mut state = self.state.clone();
        match state.wait_for(Option::is_some).await {
            Ok(r) => r.unwrap_or(CloseReason::Normal),
            Err(_) => CloseReason::Normal,
        }
    }
}

struct DriverParams {
    heartbeat: Duration,
    queue: usize,
    max_message_size: usize,
    kill: Option<watch::Receiver<bool>>,
    closer: Option<watch::Receiver<Option<CloseReason>>>,
}

type OnClose = Box<dyn FnOnce(CloseReason) + Send>;

/// Spawns the connection driver task and returns the application handle.
fn spawn_connection<T, In, Out>(
    transport: T,
    session: NoiseSession,
    params: DriverParams,
    on_close: Option<OnClose>,
) -> Connection<In, Out>
where
    T: FrameTransport + 'static,
{
    let (in_tx, in_rx) = mpsc::channel(params.queue);
    let (out_tx, out_rx) = mpsc::channel(params.queue);
    let (state_tx, state_rx) = watch::channel(None);
    let sender = ConnectionSender {
        tx: out_tx,
        state: state_rx.clone(),
        max_message_size: params.max_message_size,
        _out: PhantomData,
    };
    tokio::spawn(async move {
        let reason = drive(transport, session, params, in_tx, out_rx).await;
        state_tx.send_replace(Some(reason));
        if let Some(f) = on_close {
            f(reason);
        }
    });
    Connection {
        rx: in_rx,
        sender,
        state: state_rx,
        _in: PhantomData,
    }
}

async fn stop_signal(
    kill: &mut Option<watch::Receiver<bool>>,
    closer: &mut Option<watch::Receiver<Option<CloseReason>>>,
) -> CloseReason {
    let kill_fut = async {
        if let Some(rx) = kill {
            if rx.wait_for(|t| *t).await.is_ok() {
                return CloseReason::KillSwitch;
            }
        }
        future::pending::<CloseReason>().await
    };
    let closer_fut = async {
        if let Some(rx) = closer {
            let r = rx
                .wait_for(Option::is_some)
                .await
                .ok()
                .map(|g| g.unwrap_or(CloseReason::Normal));
            if let Some(r) = r {
                return r;
            }
        }
        future::pending::<CloseReason>().await
    };
    tokio::select! {
        r = kill_fut => r,
        r = closer_fut => r,
    }
}

async fn drive<T: FrameTransport>(
    transport: T,
    mut session: NoiseSession,
    mut params: DriverParams,
    in_tx: mpsc::Sender<Vec<u8>>,
    mut out_rx: mpsc::Receiver<Outgoing>,
) -> CloseReason {
    let (mut sink, mut stream) = transport.split();
    let heartbeat = params.heartbeat;
    // The stop signals win over everything, even a blocked write, so the
    // kill switch closes connections promptly.
    let (reason, notify_peer) = tokio::select! {
        r = stop_signal(&mut params.kill, &mut params.closer) => (r, true),
        r = io_loop(&mut sink, &mut stream, &mut session, heartbeat, &in_tx, &mut out_rx) => r,
    };
    if notify_peer {
        let _ = timeout(
            CLOSE_GRACE,
            send_control(&mut sink, &mut session, &Control::Close { reason }),
        )
        .await;
    }
    let _ = timeout(CLOSE_GRACE, sink.close()).await;
    reason
}

/// Runs until the connection ends. Returns the reason and whether a
/// `close` frame should be sent to the peer.
async fn io_loop<Si, St>(
    sink: &mut Si,
    stream: &mut St,
    session: &mut NoiseSession,
    heartbeat: Duration,
    in_tx: &mpsc::Sender<Vec<u8>>,
    out_rx: &mut mpsc::Receiver<Outgoing>,
) -> (CloseReason, bool)
where
    Si: Sink<Vec<u8>, Error = NetError> + Unpin,
    St: Stream<Item = Result<Vec<u8>>> + Unpin,
{
    let mut ticker = tokio::time::interval_at(Instant::now() + heartbeat, heartbeat);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
    let mut ping_n: u64 = 0;
    let send_err = |e: NetError| match e {
        NetError::NonceExhausted => (CloseReason::NonceExhausted, false),
        _ => (CloseReason::TransportError, false),
    };
    loop {
        tokio::select! {
            frame = stream.next() => {
                let ct = match frame {
                    None | Some(Err(NetError::Closed(_))) => return (CloseReason::PeerClosed, false),
                    Some(Err(NetError::Protocol(_))) => return (CloseReason::ProtocolError, true),
                    Some(Err(_)) => return (CloseReason::TransportError, false),
                    Some(Ok(ct)) => ct,
                };
                last_rx = Instant::now();
                let msg = match session.decrypt(&ct) {
                    Ok(None) => continue,
                    Ok(Some(msg)) => msg,
                    Err(NetError::NonceExhausted) => return (CloseReason::NonceExhausted, false),
                    Err(e) => {
                        tracing::debug!("closing connection: {e}");
                        return (CloseReason::ProtocolError, false);
                    }
                };
                match msg.split_first() {
                    Some((&KIND_APP, body)) => {
                        if in_tx.send(body.to_vec()).await.is_err() {
                            return (CloseReason::Normal, true);
                        }
                    }
                    Some((&KIND_CONTROL, body)) => match decode_control(body) {
                        Ok(Control::Ping { n }) => {
                            if let Err(e) = send_control(sink, session, &Control::Pong { n }).await {
                                return send_err(e);
                            }
                        }
                        Ok(Control::Pong { .. }) => {}
                        Ok(Control::Close { reason }) => {
                            // Report the peer's reason (e.g. kill switch) unless it is a plain close.
                            let r = if reason == CloseReason::Normal { CloseReason::PeerClosed } else { reason };
                            return (r, false);
                        }
                        _ => return (CloseReason::ProtocolError, true),
                    },
                    _ => return (CloseReason::ProtocolError, true),
                }
            }
            out = out_rx.recv() => match out {
                None => return (CloseReason::Normal, true),
                Some(Outgoing::Close(r)) => return (r, true),
                Some(Outgoing::App(bytes)) => {
                    if let Err(e) = session.send(sink, &bytes).await {
                        return send_err(e);
                    }
                }
            },
            _ = ticker.tick() => {
                if last_rx.elapsed() > heartbeat * 2 {
                    return (CloseReason::HeartbeatTimeout, true);
                }
                ping_n += 1;
                if let Err(e) = send_control(sink, session, &Control::Ping { n: ping_n }).await {
                    return send_err(e);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Server settings.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Display name sent to devices.
    pub server_name: String,
    /// WebSocket path; other paths get 404.
    pub ws_path: String,
    /// Heartbeat interval (clamped to at most 30 s).
    pub heartbeat_interval: Duration,
    /// Deadline for WebSocket upgrade + Noise handshake + authentication.
    pub handshake_timeout: Duration,
    /// Maximum application message size (after reassembly).
    pub max_message_size: usize,
    /// Maximum concurrent connections (including ones still handshaking).
    pub max_connections: usize,
    /// Require a TOTP (or backup) code for `login`. When true and TOTP is
    /// not enrolled, logins fail with `totp_not_enrolled`; pairing still
    /// works, and token-based reconnects are unaffected.
    pub require_totp: bool,
    /// Per-direction message queue length.
    pub queue: usize,
    pub noise: NoiseConfig,
    /// Failed authentication attempts per client IP and per device.
    pub auth_rate_limit: RateLimitConfig,
    /// Failed pairing attempts across all clients.
    pub pairing_rate_limit: RateLimitConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_name: "VCA".into(),
            ws_path: crate::DEFAULT_WS_PATH.into(),
            heartbeat_interval: Duration::from_secs(20),
            handshake_timeout: Duration::from_secs(10),
            max_message_size: 16 * 1024 * 1024,
            max_connections: 64,
            require_totp: true,
            queue: 256,
            noise: NoiseConfig::default(),
            auth_rate_limit: RateLimitConfig::default(),
            pairing_rate_limit: RateLimitConfig {
                max_failures: 20,
                window_ms: 5 * 60 * 1000,
                lockout_ms: 5 * 60 * 1000,
            },
        }
    }
}

/// Identity of an authenticated device, attached to a [`ServerConnection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedDevice {
    pub conn_id: u64,
    pub device_id: String,
    pub device_name: String,
    pub public_key: PublicKey,
    pub scope: Scope,
    pub method: AuthMethod,
    /// Client IP for display/audit (see [`client_ip_from_headers`]).
    pub client_ip: IpAddr,
    pub forwarded: bool,
}

/// A live connection, for "active connections" views (SEC-10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub conn_id: u64,
    pub device_id: String,
    pub device_name: String,
    pub scope: Scope,
    pub client_ip: IpAddr,
    pub connected_at: UnixMillis,
}

/// Server side of an authenticated connection: receives
/// `Envelope<ClientMsg>`, sends `Envelope<ServerMsg>`.
pub struct ServerConnection {
    pub device: AuthenticatedDevice,
    conn: Connection<ClientMsg, ServerMsg>,
}

impl ServerConnection {
    pub fn into_parts(self) -> (AuthenticatedDevice, Connection<ClientMsg, ServerMsg>) {
        (self.device, self.conn)
    }
}

impl std::ops::Deref for ServerConnection {
    type Target = Connection<ClientMsg, ServerMsg>;
    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl std::ops::DerefMut for ServerConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.conn
    }
}

struct LiveConn {
    info: ConnectionInfo,
    closer: watch::Sender<Option<CloseReason>>,
}

struct Inner {
    config: ServerConfig,
    keypair: StaticKeypair,
    registry: Arc<dyn DeviceRegistry>,
    tokens: Arc<TokenService>,
    totp: Option<Arc<TotpManager>>,
    audit: Arc<AuditLog>,
    kill: KillSwitch,
    pairing: PairingManager,
    auth_limiter: RateLimiter,
    pair_limiter: RateLimiter,
    clock: SharedClock,
    next_conn: AtomicU64,
    live: Mutex<HashMap<u64, LiveConn>>,
    slots: Arc<Semaphore>,
}

/// Builder for [`NetServer`].
pub struct NetServerBuilder {
    keypair: StaticKeypair,
    registry: Arc<dyn DeviceRegistry>,
    tokens: Arc<TokenService>,
    config: ServerConfig,
    clock: SharedClock,
    totp: Option<Arc<TotpManager>>,
    audit: Option<Arc<AuditLog>>,
    kill: Option<KillSwitch>,
}

impl NetServerBuilder {
    pub fn new(
        keypair: StaticKeypair,
        registry: Arc<dyn DeviceRegistry>,
        tokens: Arc<TokenService>,
    ) -> Self {
        Self {
            keypair,
            registry,
            tokens,
            config: ServerConfig::default(),
            clock: system_clock(),
            totp: None,
            audit: None,
            kill: None,
        }
    }

    pub fn config(mut self, config: ServerConfig) -> Self {
        self.config = config;
        self
    }

    /// Clock for pairing expiry and rate limiting (heartbeats use tokio time).
    pub fn clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    pub fn totp(mut self, totp: Arc<TotpManager>) -> Self {
        self.totp = Some(totp);
        self
    }

    pub fn audit(mut self, audit: Arc<AuditLog>) -> Self {
        self.audit = Some(audit);
        self
    }

    pub fn kill_switch(mut self, kill: KillSwitch) -> Self {
        self.kill = Some(kill);
        self
    }

    pub fn build(self) -> NetServer {
        let mut config = self.config;
        config.heartbeat_interval = config
            .heartbeat_interval
            .clamp(Duration::from_millis(10), MAX_HEARTBEAT_INTERVAL);
        config.queue = config.queue.max(1);
        let clock = self.clock;
        NetServer {
            inner: Arc::new(Inner {
                slots: Arc::new(Semaphore::new(config.max_connections.max(1))),
                auth_limiter: RateLimiter::new(config.auth_rate_limit, clock.clone()),
                pair_limiter: RateLimiter::new(config.pairing_rate_limit, clock.clone()),
                pairing: PairingManager::new(clock.clone()),
                audit: self
                    .audit
                    .unwrap_or_else(|| Arc::new(AuditLog::memory(clock.clone()))),
                kill: self.kill.unwrap_or_else(KillSwitch::in_memory),
                config,
                keypair: self.keypair,
                registry: self.registry,
                tokens: self.tokens,
                totp: self.totp,
                clock,
                next_conn: AtomicU64::new(1),
                live: Mutex::new(HashMap::new()),
            }),
        }
    }
}

/// Binds a TCP listener (use [`crate::DEFAULT_BIND_ADDR`] for loopback).
pub async fn bind(addr: &str) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

/// The VCA network server. Cheap to clone.
#[derive(Clone)]
pub struct NetServer {
    inner: Arc<Inner>,
}

impl NetServer {
    pub fn builder(
        keypair: StaticKeypair,
        registry: Arc<dyn DeviceRegistry>,
        tokens: Arc<TokenService>,
    ) -> NetServerBuilder {
        NetServerBuilder::new(keypair, registry, tokens)
    }

    pub fn public_key(&self) -> &PublicKey {
        self.inner.keypair.public()
    }

    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    pub fn pairing(&self) -> &PairingManager {
        &self.inner.pairing
    }

    pub fn registry(&self) -> &Arc<dyn DeviceRegistry> {
        &self.inner.registry
    }

    pub fn tokens(&self) -> &Arc<TokenService> {
        &self.inner.tokens
    }

    pub fn audit(&self) -> &Arc<AuditLog> {
        &self.inner.audit
    }

    pub fn kill_switch(&self) -> &KillSwitch {
        &self.inner.kill
    }

    /// Issues a one-time pairing secret and returns the QR payload.
    pub fn create_pairing(&self, scope: Scope, endpoints: Vec<String>) -> Result<PairingPayload> {
        let ticket = self.inner.pairing.issue(scope);
        self.inner.pairing.payload(
            &ticket,
            self.public_key(),
            &self.inner.config.server_name,
            endpoints,
        )
    }

    /// Currently connected devices.
    pub fn active_connections(&self) -> Vec<ConnectionInfo> {
        let live = self.inner.live.lock().expect("poisoned");
        let mut v: Vec<_> = live.values().map(|c| c.info.clone()).collect();
        v.sort_by_key(|c| c.conn_id);
        v
    }

    /// Revokes a device: marks it revoked, revokes its tokens, and closes
    /// its live connections. Returns `false` if it was unknown or already
    /// revoked.
    pub fn revoke_device(&self, device_id: &str) -> Result<bool> {
        let inner = &self.inner;
        let changed = inner.registry.revoke(device_id, inner.clock.now_millis())?;
        inner
            .tokens
            .revoke_device(device_id)
            .map_err(|e| NetError::Storage(e.to_string()))?;
        let live = inner.live.lock().expect("poisoned");
        for c in live.values().filter(|c| c.info.device_id == device_id) {
            c.closer.send_replace(Some(CloseReason::DeviceRevoked));
        }
        drop(live);
        if changed {
            inner.audit.record(AuditEvent::DeviceRevoked {
                device_id: device_id.to_string(),
            });
        }
        Ok(changed)
    }

    /// Trips the kill switch: every connection closes within a second and
    /// new ones are refused until [`NetServer::reset_kill_switch`].
    pub fn trip_kill_switch(&self, by: &str) -> Result<()> {
        let r = self.inner.kill.trip();
        self.inner
            .audit
            .record(AuditEvent::KillSwitchTripped { by: by.to_string() });
        r
    }

    pub fn reset_kill_switch(&self, by: &str) -> Result<()> {
        self.inner.kill.reset()?;
        self.inner
            .audit
            .record(AuditEvent::KillSwitchReset { by: by.to_string() });
        Ok(())
    }

    /// Accept loop: authenticates each incoming connection in its own task
    /// and hands authenticated ones to `tx`. Returns when `tx` is closed.
    pub async fn serve(
        &self,
        listener: TcpListener,
        tx: mpsc::Sender<ServerConnection>,
    ) -> std::io::Result<()> {
        loop {
            let accepted = tokio::select! {
                a = listener.accept() => a,
                _ = tx.closed() => return Ok(()),
            };
            let (stream, peer) = match accepted {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let _ = stream.set_nodelay(true);
            let server = self.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                match server.accept(stream, peer).await {
                    Ok(conn) => {
                        let _ = tx.send(conn).await;
                    }
                    Err(e) => tracing::debug!(%peer, "connection rejected: {e}"),
                }
            });
        }
    }

    /// Runs WebSocket upgrade, Noise handshake and authentication on an
    /// accepted stream. `peer` is the TCP peer address.
    pub async fn accept<S>(&self, stream: S, peer: SocketAddr) -> Result<ServerConnection>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let inner = &self.inner;
        let conn_id = inner.next_conn.fetch_add(1, Ordering::Relaxed);
        let permit = inner
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| NetError::Protocol("too many connections".into()))?;
        let (transport, session, device) = timeout(
            inner.config.handshake_timeout,
            self.establish(stream, peer, conn_id),
        )
        .await
        .map_err(|_| NetError::Timeout)??;

        let now = inner.clock.now_millis();
        if let Err(e) = inner.registry.touch(&device.device_id, now) {
            tracing::warn!("cannot record last-seen time: {e}");
        }
        let (closer_tx, closer_rx) = watch::channel(None);
        inner.live.lock().expect("poisoned").insert(
            conn_id,
            LiveConn {
                info: ConnectionInfo {
                    conn_id,
                    device_id: device.device_id.clone(),
                    device_name: device.device_name.clone(),
                    scope: device.scope,
                    client_ip: device.client_ip,
                    connected_at: now,
                },
                closer: closer_tx,
            },
        );
        let weak = Arc::downgrade(&self.inner);
        let device_id = device.device_id.clone();
        let on_close: OnClose = Box::new(move |reason| {
            drop(permit);
            if let Some(inner) = weak.upgrade() {
                inner.live.lock().expect("poisoned").remove(&conn_id);
                inner.audit.record(AuditEvent::Disconnected {
                    conn_id,
                    device_id,
                    reason,
                });
            }
        });
        let conn = spawn_connection(
            transport,
            session,
            DriverParams {
                heartbeat: inner.config.heartbeat_interval,
                queue: inner.config.queue,
                max_message_size: inner.config.max_message_size,
                kill: Some(inner.kill.subscribe()),
                closer: Some(closer_rx),
            },
            Some(on_close),
        );
        // A revocation that raced with authentication (the driver observes
        // the kill switch itself, including one tripped before it started).
        if !matches!(inner.registry.get(&device.device_id), Ok(Some(d)) if d.is_active()) {
            if let Some(c) = inner.live.lock().expect("poisoned").get(&conn_id) {
                c.closer.send_replace(Some(CloseReason::DeviceRevoked));
            }
        }
        Ok(ServerConnection { device, conn })
    }

    // The callback signature (and its large error type) is fixed by tungstenite.
    #[allow(clippy::result_large_err)]
    async fn establish<S>(
        &self,
        stream: S,
        peer: SocketAddr,
        conn_id: u64,
    ) -> Result<(
        impl FrameTransport + 'static,
        NoiseSession,
        AuthenticatedDevice,
    )>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let inner = &self.inner;
        let headers: Arc<Mutex<(Option<String>, Option<String>)>> = Arc::default();
        let captured = headers.clone();
        let ws_path = inner.config.ws_path.clone();
        let kill = inner.kill.clone();
        let callback = move |req: &Request,
                             resp: Response|
              -> std::result::Result<Response, ErrorResponse> {
            let reject = |status: StatusCode| {
                let mut r = ErrorResponse::new(None);
                *r.status_mut() = status;
                Err(r)
            };
            if req.uri().path() != ws_path {
                return reject(StatusCode::NOT_FOUND);
            }
            if kill.is_tripped() {
                return reject(StatusCode::SERVICE_UNAVAILABLE);
            }
            let get = |name: &str| {
                req.headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| sanitize_display(s, 256))
            };
            *captured.lock().expect("poisoned") = (get(X_FORWARDED_FOR), get(X_DNSPILOT_CLIENT_ID));
            Ok(resp)
        };
        let ws = match tokio_tungstenite::accept_hdr_async_with_config(
            stream,
            callback,
            Some(ws_config()),
        )
        .await
        {
            Ok(ws) => ws,
            Err(e) => {
                if inner.kill.is_tripped() {
                    inner.audit.record(AuditEvent::ConnectionRejected {
                        conn_id,
                        client_ip: peer.ip(),
                        reason: "kill switch engaged".into(),
                    });
                    return Err(NetError::KillSwitch);
                }
                return Err(e.into());
            }
        };
        let (xff, provider_client_id) = headers.lock().expect("poisoned").clone();
        let (client_ip, forwarded) = client_ip_from_headers(peer, xff.as_deref());
        inner.audit.record(AuditEvent::ConnectionAttempt {
            conn_id,
            client_ip,
            forwarded,
            provider_client_id,
        });
        let mut transport = ws_frames(ws);
        let noise_config = NoiseConfig {
            max_message_size: PRE_AUTH_MAX_MESSAGE,
            ..inner.config.noise
        };
        let mut session = handshake_responder(&mut transport, &inner.keypair, noise_config).await?;
        let result = self
            .authenticate(&mut transport, &mut session, conn_id, client_ip, forwarded)
            .await;
        match result {
            Ok(device) => {
                session.set_max_message_size(inner.config.max_message_size);
                Ok((transport, session, device))
            }
            Err(e) => {
                let _ = timeout(CLOSE_GRACE, transport.close()).await;
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn fail<T: FrameTransport>(
        &self,
        t: &mut T,
        session: &mut NoiseSession,
        conn_id: u64,
        client_ip: IpAddr,
        key: &PublicKey,
        device_id: Option<&str>,
        failure: AuthFailure,
        detail: &str,
    ) -> NetError {
        let inner = &self.inner;
        inner.audit.record(AuditEvent::AuthFailed {
            conn_id,
            device_id: device_id.map(str::to_string),
            key_fingerprint: key.fingerprint().short(),
            failure,
            detail: detail.to_string(),
            client_ip,
        });
        if !matches!(
            failure,
            AuthFailure::RateLimited | AuthFailure::KillSwitch | AuthFailure::Internal
        ) {
            self.count_failure(&format!("ip:{client_ip}"));
            if let Some(id) = device_id {
                self.count_failure(&format!("device:{id}"));
            }
        }
        let _ = timeout(
            CLOSE_GRACE,
            send_control(t, session, &Control::AuthError { failure }),
        )
        .await;
        NetError::Auth(failure)
    }

    fn count_failure(&self, key: &str) {
        let inner = &self.inner;
        if inner.auth_limiter.record_failure(key) {
            let locked_until = inner.auth_limiter.check(key).err().unwrap_or(0);
            inner.audit.record(AuditEvent::RateLimited {
                key: key.to_string(),
                locked_until,
            });
        }
    }

    fn lookup(
        &self,
        key: &PublicKey,
    ) -> std::result::Result<DeviceRecord, (AuthFailure, &'static str)> {
        match self.inner.registry.get_by_key(key) {
            Ok(Some(d)) if d.is_active() => Ok(d),
            Ok(Some(_)) => Err((AuthFailure::DeviceRevoked, "device revoked")),
            Ok(None) => Err((AuthFailure::UnknownDevice, "unknown device key")),
            Err(_) => Err((AuthFailure::Internal, "registry error")),
        }
    }

    async fn authenticate<T: FrameTransport>(
        &self,
        t: &mut T,
        session: &mut NoiseSession,
        conn_id: u64,
        client_ip: IpAddr,
        forwarded: bool,
    ) -> Result<AuthenticatedDevice> {
        let inner = &self.inner;
        let key = *session.remote_static();
        let ip_key = format!("ip:{client_ip}");
        let mut paired: Option<DeviceRecord> = None;

        macro_rules! fail {
            ($dev:expr, $failure:expr, $detail:expr) => {
                return Err(self
                    .fail(
                        t, session, conn_id, client_ip, &key, $dev, $failure, $detail,
                    )
                    .await)
            };
        }

        for _ in 0..MAX_AUTH_FRAMES {
            let req = match recv_control(t, session).await {
                Ok(c) => c,
                Err(e) => {
                    let detail = e.to_string();
                    fail!(None, AuthFailure::Protocol, &detail)
                }
            };
            if inner.kill.is_tripped() {
                fail!(None, AuthFailure::KillSwitch, "kill switch engaged")
            }
            if inner.auth_limiter.check(&ip_key).is_err() {
                fail!(None, AuthFailure::RateLimited, "client IP locked out")
            }
            let (device, method, tokens) = match req {
                Control::Pair {
                    secret,
                    device_name,
                    platform,
                } => {
                    let secret = Zeroizing::new(secret);
                    if paired.is_some() {
                        fail!(None, AuthFailure::Protocol, "pair sent twice")
                    }
                    match inner.registry.get_by_key(&key) {
                        Ok(Some(d)) if d.is_active() => {
                            fail!(
                                Some(&d.id),
                                AuthFailure::AlreadyPaired,
                                "key already paired"
                            )
                        }
                        Ok(Some(d)) => fail!(
                            Some(&d.id),
                            AuthFailure::DeviceRevoked,
                            "revoked key tried to pair"
                        ),
                        Ok(None) => {}
                        Err(_) => fail!(None, AuthFailure::Internal, "registry error"),
                    }
                    if inner.pair_limiter.check(GLOBAL_PAIR_KEY).is_err() {
                        fail!(
                            None,
                            AuthFailure::RateLimited,
                            "pairing globally locked out"
                        )
                    }
                    let redeemed = match b64d(&secret) {
                        Ok(bytes) => inner.pairing.redeem(&Zeroizing::new(bytes)),
                        Err(_) => Err(RedeemError::Invalid),
                    };
                    let scope = match redeemed {
                        Ok(scope) => scope,
                        Err(e) => {
                            if inner.pair_limiter.record_failure(GLOBAL_PAIR_KEY) {
                                inner.audit.record(AuditEvent::RateLimited {
                                    key: GLOBAL_PAIR_KEY.into(),
                                    locked_until: inner
                                        .pair_limiter
                                        .check(GLOBAL_PAIR_KEY)
                                        .err()
                                        .unwrap_or(0),
                                });
                            }
                            let detail = match e {
                                RedeemError::Expired => "pairing secret expired",
                                RedeemError::Invalid => "pairing secret wrong or already used",
                            };
                            fail!(None, AuthFailure::InvalidPairingSecret, detail)
                        }
                    };
                    let mut name = sanitize_display(&device_name, DEVICE_NAME_MAX);
                    if name.is_empty() {
                        name = "Unnamed device".into();
                    }
                    // Registered only once the following `login` succeeds, so a
                    // stolen QR code without the TOTP leaves no device behind.
                    let mut record = DeviceRecord::new(name, key, scope, inner.clock.now_millis());
                    record.platform = sanitize_display(&platform, 32);
                    let reply = Control::Paired {
                        device_id: record.id.clone(),
                        scope,
                    };
                    send_control(t, session, &reply).await?;
                    paired = Some(record);
                    continue;
                }
                Control::Login { totp, backup_code } => {
                    let totp = totp.map(Zeroizing::new);
                    let backup_code = backup_code.map(Zeroizing::new);
                    let just_paired = paired.is_some();
                    let device = match paired.take() {
                        Some(d) => d,
                        None => match self.lookup(&key) {
                            Ok(d) => d,
                            Err((f, detail)) => fail!(None, f, detail),
                        },
                    };
                    if inner
                        .auth_limiter
                        .check(&format!("device:{}", device.id))
                        .is_err()
                    {
                        fail!(
                            Some(&device.id),
                            AuthFailure::RateLimited,
                            "device locked out"
                        )
                    }
                    let method = if inner.config.require_totp {
                        let Some(mgr) = inner.totp.as_ref().filter(|m| m.is_enrolled()) else {
                            fail!(
                                Some(&device.id),
                                AuthFailure::TotpNotEnrolled,
                                "TOTP required but not enrolled"
                            )
                        };
                        match (totp.as_deref(), backup_code.as_deref()) {
                            (Some(code), _) => match mgr.verify(code) {
                                Ok(()) => AuthMethod::Totp,
                                Err(e) => {
                                    let detail = format!("TOTP: {e}");
                                    fail!(Some(&device.id), AuthFailure::InvalidTotp, &detail)
                                }
                            },
                            (None, Some(code)) => match mgr.verify_backup_code(code) {
                                Ok(()) => AuthMethod::BackupCode,
                                Err(e) => {
                                    let detail = format!("backup code: {e}");
                                    fail!(Some(&device.id), AuthFailure::InvalidTotp, &detail)
                                }
                            },
                            (None, None) => {
                                fail!(Some(&device.id), AuthFailure::TotpRequired, "no TOTP code")
                            }
                        }
                    } else if just_paired {
                        AuthMethod::Pairing
                    } else {
                        AuthMethod::DeviceKey
                    };
                    if just_paired {
                        if inner.registry.register(device.clone()).is_err() {
                            fail!(None, AuthFailure::Internal, "cannot register device")
                        }
                        inner.audit.record(AuditEvent::DevicePaired {
                            device_id: device.id.clone(),
                            name: device.name.clone(),
                            scope: device.scope,
                            key_fingerprint: key.fingerprint().short(),
                            client_ip,
                        });
                    }
                    let tokens = match inner.tokens.issue(&device.id, device.scope) {
                        Ok(p) => p,
                        Err(_) => fail!(
                            Some(&device.id),
                            AuthFailure::Internal,
                            "token issue failed"
                        ),
                    };
                    (device, method, Some(tokens))
                }
                Control::Token { access_token } => {
                    let access_token = Zeroizing::new(access_token);
                    if paired.is_some() {
                        fail!(None, AuthFailure::Protocol, "token after pair")
                    }
                    let device = match self.lookup(&key) {
                        Ok(d) => d,
                        Err((f, detail)) => fail!(None, f, detail),
                    };
                    if let Err(e) = inner.tokens.verify_access(&access_token, &device.id) {
                        let detail = format!("access token: {e}");
                        fail!(Some(&device.id), AuthFailure::InvalidToken, &detail)
                    }
                    (device, AuthMethod::AccessToken, None)
                }
                Control::Refresh { refresh_token } => {
                    let refresh_token = Zeroizing::new(refresh_token);
                    if paired.is_some() {
                        fail!(None, AuthFailure::Protocol, "refresh after pair")
                    }
                    let device = match self.lookup(&key) {
                        Ok(d) => d,
                        Err((f, detail)) => fail!(None, f, detail),
                    };
                    match inner.tokens.refresh(&refresh_token, &device.id) {
                        Ok(pair) => (device, AuthMethod::RefreshToken, Some(pair)),
                        Err(TokenError::Reuse) => {
                            inner.audit.record(AuditEvent::TokenReuseDetected {
                                device_id: device.id.clone(),
                                client_ip,
                            });
                            fail!(
                                Some(&device.id),
                                AuthFailure::TokenReuse,
                                "refresh token reuse"
                            )
                        }
                        Err(e) => {
                            let detail = format!("refresh token: {e}");
                            fail!(Some(&device.id), AuthFailure::InvalidToken, &detail)
                        }
                    }
                }
                _ => fail!(None, AuthFailure::Protocol, "unexpected frame"),
            };

            let reply = Control::AuthOk {
                device_id: device.id.clone(),
                scope: device.scope,
                server_name: inner.config.server_name.clone(),
                tokens,
            };
            send_control(t, session, &reply).await?;
            inner.auth_limiter.record_success(&ip_key);
            inner
                .auth_limiter
                .record_success(&format!("device:{}", device.id));
            inner.audit.record(AuditEvent::AuthSucceeded {
                conn_id,
                device_id: device.id.clone(),
                method,
                scope: device.scope,
                client_ip,
            });
            return Ok(AuthenticatedDevice {
                conn_id,
                device_id: device.id,
                device_name: device.name,
                public_key: key,
                scope: device.scope,
                method,
                client_ip,
                forwarded,
            });
        }
        fail!(
            None,
            AuthFailure::Protocol,
            "too many authentication frames"
        )
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Client settings.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `ws://` (or `wss://` with the `client-tls` feature) endpoint URL.
    pub url: String,
    /// Pinned server static key (from the pairing payload).
    pub server_key: PublicKey,
    pub device_keypair: StaticKeypair,
    pub heartbeat_interval: Duration,
    /// Deadline for connect + handshake + authentication.
    pub connect_timeout: Duration,
    pub max_message_size: usize,
    pub queue: usize,
    pub noise: NoiseConfig,
}

impl ClientConfig {
    pub fn new(
        url: impl Into<String>,
        server_key: PublicKey,
        device_keypair: StaticKeypair,
    ) -> Self {
        Self {
            url: url.into(),
            server_key,
            device_keypair,
            heartbeat_interval: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(15),
            max_message_size: 16 * 1024 * 1024,
            queue: 256,
            noise: NoiseConfig::default(),
        }
    }

    /// Config for the `index`-th endpoint of a pairing payload.
    pub fn from_pairing(
        payload: &PairingPayload,
        index: usize,
        device_keypair: StaticKeypair,
    ) -> Result<Self> {
        let url = payload
            .endpoints
            .get(index)
            .ok_or_else(|| NetError::InvalidArgument("no such endpoint".into()))?;
        Ok(Self::new(url.clone(), payload.server_key, device_keypair))
    }
}

/// How the client authenticates after the handshake.
pub enum ClientAuth {
    /// First connection: redeem the QR secret, then sign in (with TOTP if
    /// the server requires it).
    Pair {
        secret: Zeroizing<Vec<u8>>,
        device_name: String,
        /// Informational platform name, e.g. `ios`, `android`, `desktop`.
        platform: String,
        totp: Option<String>,
    },
    /// Sign in again with TOTP (or a backup code).
    Login {
        totp: Option<String>,
        backup_code: Option<String>,
    },
    /// Reconnect with a valid access token.
    AccessToken(String),
    /// Reconnect by rotating a refresh token (store the new pair!).
    RefreshToken(String),
}

impl std::fmt::Debug for ClientAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Pair { .. } => "Pair",
            Self::Login { .. } => "Login",
            Self::AccessToken(_) => "AccessToken",
            Self::RefreshToken(_) => "RefreshToken",
        };
        write!(f, "ClientAuth::{kind}(..)")
    }
}

/// Result of a successful client authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSession {
    pub device_id: String,
    pub scope: Scope,
    pub server_name: String,
    /// New tokens (after pairing, login or refresh); `None` after an
    /// access-token reconnect.
    pub tokens: Option<TokenPair>,
}

/// Client side of a connection: receives `Envelope<ServerMsg>`, sends
/// `Envelope<ClientMsg>`.
pub struct ClientConnection {
    pub session: ClientSession,
    conn: Connection<ServerMsg, ClientMsg>,
}

impl std::ops::Deref for ClientConnection {
    type Target = Connection<ServerMsg, ClientMsg>;
    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl std::ops::DerefMut for ClientConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.conn
    }
}

async fn client_auth<T: FrameTransport>(
    t: &mut T,
    session: &mut NoiseSession,
    auth: ClientAuth,
) -> Result<ClientSession> {
    let expect_result = |c: Control| -> Result<ClientSession> {
        match c {
            Control::AuthOk {
                device_id,
                scope,
                server_name,
                tokens,
            } => Ok(ClientSession {
                device_id,
                scope,
                server_name: sanitize_display(&server_name, 64),
                tokens,
            }),
            Control::AuthError { failure } => Err(NetError::Auth(failure)),
            _ => Err(NetError::Protocol("unexpected authentication reply".into())),
        }
    };
    let first = match auth {
        ClientAuth::Pair {
            secret,
            device_name,
            platform,
            totp,
        } => {
            send_control(
                t,
                session,
                &Control::Pair {
                    secret: b64e(&secret),
                    device_name,
                    platform,
                },
            )
            .await?;
            match recv_control(t, session).await? {
                Control::Paired { .. } => {}
                Control::AuthError { failure } => return Err(NetError::Auth(failure)),
                _ => return Err(NetError::Protocol("unexpected pairing reply".into())),
            }
            Control::Login {
                totp,
                backup_code: None,
            }
        }
        ClientAuth::Login { totp, backup_code } => Control::Login { totp, backup_code },
        ClientAuth::AccessToken(access_token) => Control::Token { access_token },
        ClientAuth::RefreshToken(refresh_token) => Control::Refresh { refresh_token },
    };
    send_control(t, session, &first).await?;
    expect_result(recv_control(t, session).await?)
}

impl ClientConnection {
    /// Connects, performs the handshake (pinning `config.server_key`) and
    /// authenticates.
    pub async fn connect(config: ClientConfig, auth: ClientAuth) -> Result<ClientConnection> {
        let noise_config = NoiseConfig {
            max_message_size: PRE_AUTH_MAX_MESSAGE,
            ..config.noise
        };
        let (transport, session, info) = timeout(config.connect_timeout, async {
            let (ws, _) = tokio_tungstenite::connect_async_with_config(
                config.url.as_str(),
                Some(ws_config()),
                true,
            )
            .await?;
            let mut transport = ws_frames(ws);
            let mut session = handshake_initiator(
                &mut transport,
                &config.device_keypair,
                &config.server_key,
                noise_config,
            )
            .await?;
            match client_auth(&mut transport, &mut session, auth).await {
                Ok(info) => Ok::<_, NetError>((transport, session, info)),
                Err(e) => {
                    let _ = timeout(CLOSE_GRACE, transport.close()).await;
                    Err(e)
                }
            }
        })
        .await
        .map_err(|_| NetError::Timeout)??;
        let mut session = session;
        session.set_max_message_size(config.max_message_size);
        let conn = spawn_connection(
            transport,
            session,
            DriverParams {
                heartbeat: config
                    .heartbeat_interval
                    .clamp(Duration::from_millis(10), MAX_HEARTBEAT_INTERVAL),
                queue: config.queue.max(1),
                max_message_size: config.max_message_size,
                kill: None,
                closer: None,
            },
            None,
        );
        Ok(ClientConnection {
            session: info,
            conn,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::registry::MemoryDeviceRegistry;
    use tokio::net::TcpStream;

    struct Fixture {
        server: NetServer,
        addr: SocketAddr,
        _task: tokio::task::JoinHandle<()>,
    }

    async fn fixture(config: ServerConfig) -> (Fixture, mpsc::Receiver<ServerConnection>) {
        let clock = ManualClock::new(1_700_000_000_000);
        let tokens = Arc::new(
            TokenService::new(
                Zeroizing::new(vec![1u8; 32]),
                Default::default(),
                clock.clone(),
            )
            .unwrap(),
        );
        let server = NetServer::builder(
            StaticKeypair::generate(),
            Arc::new(MemoryDeviceRegistry::new()),
            tokens,
        )
        .config(config)
        .clock(clock)
        .build();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(8);
        let s = server.clone();
        let task = tokio::spawn(async move {
            let _ = s.serve(listener, tx).await;
        });
        (
            Fixture {
                server,
                addr,
                _task: task,
            },
            rx,
        )
    }

    /// Opens a raw authenticated connection that never answers heartbeats.
    async fn silent_client(
        f: &Fixture,
        device: &StaticKeypair,
    ) -> (impl FrameTransport, NoiseSession) {
        let url = format!("ws://{}{}", f.addr, crate::DEFAULT_WS_PATH);
        let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let mut t = ws_frames(ws);
        let mut s = handshake_initiator(
            &mut t,
            device,
            f.server.public_key(),
            NoiseConfig::default(),
        )
        .await
        .unwrap();
        let p = f
            .server
            .create_pairing(Scope::View, vec!["ws://127.0.0.1/v1/ws".into()])
            .unwrap();
        let info = client_auth(
            &mut t,
            &mut s,
            ClientAuth::Pair {
                secret: p.secret.clone(),
                device_name: "silent".into(),
                platform: "test".into(),
                totp: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(info.scope, Scope::View);
        (t, s)
    }

    #[tokio::test]
    async fn server_closes_silent_peer_after_two_missed_heartbeats() {
        let config = ServerConfig {
            require_totp: false,
            heartbeat_interval: Duration::from_millis(100),
            ..Default::default()
        };
        let (f, mut rx) = fixture(config).await;
        let device = StaticKeypair::generate();
        let (_t, _s) = silent_client(&f, &device).await;
        let conn = rx.recv().await.unwrap();
        let started = std::time::Instant::now();
        let reason = timeout(Duration::from_secs(3), conn.closed())
            .await
            .unwrap();
        assert_eq!(reason, CloseReason::HeartbeatTimeout);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
        assert!(f.server.active_connections().is_empty());
    }

    #[tokio::test]
    async fn garbage_after_auth_is_a_protocol_error() {
        let config = ServerConfig {
            require_totp: false,
            ..Default::default()
        };
        let (f, mut rx) = fixture(config).await;
        let device = StaticKeypair::generate();
        let (mut t, mut s) = silent_client(&f, &device).await;
        let conn = rx.recv().await.unwrap();
        // Valid Noise message with an unknown kind byte.
        s.send(&mut t, &[7, 1, 2, 3]).await.unwrap();
        let reason = timeout(Duration::from_secs(2), conn.closed())
            .await
            .unwrap();
        assert_eq!(reason, CloseReason::ProtocolError);
    }

    #[tokio::test]
    async fn auth_frame_must_be_first() {
        let config = ServerConfig {
            require_totp: false,
            ..Default::default()
        };
        let (f, _rx) = fixture(config).await;
        let device = StaticKeypair::generate();
        let url = format!("ws://{}{}", f.addr, crate::DEFAULT_WS_PATH);
        let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let mut t = ws_frames(ws);
        let mut s = handshake_initiator(
            &mut t,
            &device,
            f.server.public_key(),
            NoiseConfig::default(),
        )
        .await
        .unwrap();
        // An application message before authentication.
        s.send(&mut t, &[KIND_APP, 0x80]).await.unwrap();
        match recv_control(&mut t, &mut s).await {
            Ok(Control::AuthError { failure }) => assert_eq!(failure, AuthFailure::Protocol),
            _ => panic!("expected auth error"),
        }
    }

    #[tokio::test]
    async fn oversized_pre_auth_message_is_rejected() {
        let config = ServerConfig {
            require_totp: false,
            ..Default::default()
        };
        let (f, _rx) = fixture(config).await;
        let device = StaticKeypair::generate();
        let url = format!("ws://{}{}", f.addr, crate::DEFAULT_WS_PATH);
        let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let mut t = ws_frames(ws);
        let mut s = handshake_initiator(
            &mut t,
            &device,
            f.server.public_key(),
            NoiseConfig::default(),
        )
        .await
        .unwrap();
        s.send(&mut t, &vec![KIND_CONTROL; 100_000]).await.unwrap();
        // The server drops the connection without authenticating anyone.
        let _ = timeout(Duration::from_secs(2), t.next()).await;
        assert!(f.server.registry().list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn wrong_path_and_plain_http_are_rejected() {
        let config = ServerConfig {
            require_totp: false,
            ..Default::default()
        };
        let (f, _rx) = fixture(config).await;
        let url = format!("ws://{}/other", f.addr);
        assert!(tokio_tungstenite::connect_async(url).await.is_err());
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = TcpStream::connect(f.addr).await.unwrap();
        s.write_all(b"GET /v1/ws HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = vec![0u8; 256];
        let n = timeout(Duration::from_secs(2), s.read(&mut buf))
            .await
            .unwrap()
            .unwrap_or(0);
        let text = String::from_utf8_lossy(&buf[..n]);
        assert!(!text.contains("101"), "{text}");
    }

    #[test]
    fn control_frames_roundtrip() {
        let c = Control::Ping { n: 9 };
        let bytes = encode_control(&c);
        assert_eq!(bytes[0], KIND_CONTROL);
        assert!(matches!(
            decode_control(&bytes[1..]).unwrap(),
            Control::Ping { n: 9 }
        ));
        assert!(decode_control(&[0xc1]).is_err());
    }
}
