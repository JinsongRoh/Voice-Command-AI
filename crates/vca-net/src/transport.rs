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
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
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
use crate::client_tls::TransportOptions;
use crate::clock::{SharedClock, system_clock};
use crate::error::{AuthFailure, CloseReason, NetError, Result};
use crate::keys::{PublicKey, StaticKeypair};
use crate::killswitch::KillSwitch;
use crate::noise::{
    FrameTransport, MAX_NOISE_MESSAGE, NoiseConfig, NoiseSession, handshake_initiator,
    handshake_responder,
};
use crate::pairing::{MAX_PAIRING_CODE_ATTEMPTS, PairingManager, PairingPayload, RedeemError};
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

/// Control frames carry pairing secrets, codes and tokens: they are
/// written straight into one zeroized buffer, sized so that it does not
/// grow (a grown buffer leaves its old copy behind unzeroized).
fn encode_control(c: &Control) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(CONTROL_BUFFER));
    out.push(KIND_CONTROL);
    rmp_serde::encode::write_named(&mut *out, c).expect("control frames serialize");
    out
}

/// Room for any control frame without growing its buffer.
const CONTROL_BUFFER: usize = 4096;

fn decode_control(body: &[u8]) -> Result<Control> {
    rmp_serde::from_slice(body).map_err(|e| NetError::Protocol(format!("bad control frame: {e}")))
}

async fn send_control<S>(sink: &mut S, session: &mut NoiseSession, c: &Control) -> Result<()>
where
    S: Sink<Vec<u8>, Error = NetError> + Unpin + ?Sized,
{
    let bytes = encode_control(c);
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
    rx: mpsc::Receiver<Inbound>,
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
        let msg = self.rx.recv().await?;
        Some(rmp_serde::from_slice(&msg.bytes).map_err(NetError::encoding))
    }

    /// Sends an envelope.
    pub async fn send(&self, msg: &Envelope<Out>) -> Result<()> {
        self.sender.send(msg).await
    }

    /// A cloneable sender, e.g. for a task that streams events.
    pub fn sender(&self) -> ConnectionSender<Out> {
        self.sender.clone()
    }

    /// A sender for envelopes of another message type over this connection.
    /// For protocol tests that check how a peer treats messages outside its
    /// API (any peer can send arbitrary bytes anyway).
    #[doc(hidden)]
    pub fn sender_as<T>(&self) -> ConnectionSender<T> {
        ConnectionSender {
            tx: self.sender.tx.clone(),
            state: self.sender.state.clone(),
            max_message_size: self.sender.max_message_size,
            _out: PhantomData,
        }
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

/// An inbound application message with its share of the connection's
/// inbound byte budget, given back once the application took it.
struct Inbound {
    bytes: Vec<u8>,
    _budget: OwnedSemaphorePermit,
}

/// Least inbound byte budget of a connection.
const MIN_INBOUND_BUDGET: usize = 1024 * 1024;

/// Bytes of application messages one connection may have waiting for the
/// application: one message of the largest size, or a megabyte of small
/// ones. The queue length alone would let a peer park `queue` messages of
/// the largest size (gigabytes) in memory while the application is busy.
fn inbound_budget(max_message_size: usize) -> usize {
    max_message_size
        .max(MIN_INBOUND_BUDGET)
        .min(u32::MAX as usize)
        .min(Semaphore::MAX_PERMITS)
}

/// A place in the inbound queue and the budget for `len` bytes. `None`
/// once the application has dropped its end.
async fn reserve_inbound<'a>(
    in_tx: &'a mpsc::Sender<Inbound>,
    budget: &Arc<Semaphore>,
    total: usize,
    len: usize,
) -> Option<(mpsc::Permit<'a, Inbound>, OwnedSemaphorePermit)> {
    let n = u32::try_from(len.min(total)).unwrap_or(u32::MAX);
    let permit = budget.clone().acquire_many_owned(n).await.ok()?;
    let slot = in_tx.reserve().await.ok()?;
    Some((slot, permit))
}

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
    in_tx: mpsc::Sender<Inbound>,
    mut out_rx: mpsc::Receiver<Outgoing>,
) -> CloseReason {
    let (mut sink, mut stream) = transport.split();
    let heartbeat = params.heartbeat;
    // The stop signals win over everything, even a blocked write, so the
    // kill switch closes connections promptly.
    let (reason, notify_peer) = tokio::select! {
        r = stop_signal(&mut params.kill, &mut params.closer) => (r, true),
        r = io_loop(
            &mut sink,
            &mut stream,
            &mut session,
            heartbeat,
            &in_tx,
            inbound_budget(params.max_message_size),
            &mut out_rx,
        ) => r,
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
    in_tx: &mpsc::Sender<Inbound>,
    budget_total: usize,
    out_rx: &mut mpsc::Receiver<Outgoing>,
) -> (CloseReason, bool)
where
    Si: Sink<Vec<u8>, Error = NetError> + Unpin,
    St: Stream<Item = Result<Vec<u8>>> + Unpin,
{
    let mut ticker = tokio::time::interval_at(Instant::now() + heartbeat, heartbeat);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
    // When the oldest unanswered ping went out. A peer is only judged dead
    // after it had a whole interval to answer a ping: if this task was
    // starved (a stalled runtime or machine) the first tick can come late
    // with nothing sent yet, and the silence is ours, not the peer's.
    let mut ping_sent: Option<Instant> = None;
    let mut ping_n: u64 = 0;
    let send_err = |e: NetError| match e {
        NetError::NonceExhausted => (CloseReason::NonceExhausted, false),
        _ => (CloseReason::TransportError, false),
    };
    let budget = Arc::new(Semaphore::new(budget_total));
    // A message read while the application had no room for it. Until it is
    // handed over, nothing more is read, but outbound messages and pings
    // still flow: awaiting the queue here would stop them too, and an
    // application that waits on its own sends before it reads again would
    // never get to read (a deadlock no heartbeat ends).
    let mut held: Option<Vec<u8>> = None;
    loop {
        // Biased: inbound frames win over the heartbeat tick. After a
        // scheduler stall the tick and an already-arrived pong are ready at
        // once; a random pick could judge the peer dead with its answer
        // sitting unread in the socket buffer. (tokio's cooperative budget
        // bounds how long a busy stream can defer the other branches.)
        let held_len = held.as_ref().map_or(0, Vec::len);
        tokio::select! {
            biased;
            ready = reserve_inbound(in_tx, &budget, budget_total, held_len), if held.is_some() => {
                let Some((slot, permit)) = ready else {
                    return (CloseReason::Normal, true);
                };
                if let Some(bytes) = held.take() {
                    slot.send(Inbound { bytes, _budget: permit });
                }
                // The silence while nothing was read was ours, not the peer's.
                last_rx = Instant::now();
                ping_sent = None;
            }
            frame = stream.next(), if held.is_none() => {
                let ct = match frame {
                    None | Some(Err(NetError::Closed(_))) => return (CloseReason::PeerClosed, false),
                    Some(Err(NetError::Protocol(_))) => return (CloseReason::ProtocolError, true),
                    Some(Err(_)) => return (CloseReason::TransportError, false),
                    Some(Ok(ct)) => ct,
                };
                last_rx = Instant::now();
                ping_sent = None;
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
                    Some((&KIND_APP, body)) => held = Some(body.to_vec()),
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
                if held.is_none()
                    && last_rx.elapsed() > heartbeat * 2
                    && ping_sent.is_some_and(|t| t.elapsed() >= heartbeat.mul_f32(0.9))
                {
                    return (CloseReason::HeartbeatTimeout, true);
                }
                ping_n += 1;
                ping_sent.get_or_insert_with(Instant::now);
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
    /// Maximum concurrent signed-in connections. Connections still in
    /// handshake or sign-in count toward [`Self::max_pre_auth`] instead.
    pub max_connections: usize,
    /// Connections still in handshake or sign-in. They have their own pool,
    /// so anonymous peers cannot fill the slots of signed-in devices.
    pub max_pre_auth: usize,
    /// Of those, at most this many from one address or provider client.
    pub max_pre_auth_per_source: usize,
    /// Of those, at most this many from one network peer, counted before
    /// the WebSocket upgrade. A household or office behind one address fits;
    /// see [`peer_cap`] for a tunnel on this computer.
    pub max_pre_auth_per_peer: usize,
    /// A peer that has not finished the WebSocket upgrade by then is dropped.
    pub upgrade_timeout: Duration,
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
    /// How long a pairing QR code (one-time secret) is valid.
    pub pairing_ttl: Duration,
    /// Once a valid secret was presented in time, how long from that first
    /// presentation it stays usable for TOTP retries (never re-opens an
    /// expired secret).
    pub pairing_grace: Duration,
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
            max_pre_auth: 64,
            max_pre_auth_per_source: 8,
            max_pre_auth_per_peer: 16,
            upgrade_timeout: Duration::from_secs(5),
            require_totp: true,
            queue: 256,
            noise: NoiseConfig::default(),
            auth_rate_limit: RateLimitConfig::default(),
            pairing_rate_limit: RateLimitConfig {
                max_failures: 20,
                window_ms: 5 * 60 * 1000,
                lockout_ms: 5 * 60 * 1000,
            },
            pairing_ttl: crate::pairing::DEFAULT_PAIRING_TTL,
            pairing_grace: crate::pairing::PAIRING_GRACE,
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
    pre_auth_slots: Arc<Semaphore>,
    pre_auth_sources: Arc<Mutex<HashMap<String, usize>>>,
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
                pre_auth_slots: Arc::new(Semaphore::new(config.max_pre_auth.max(1))),
                pre_auth_sources: Arc::default(),
                auth_limiter: RateLimiter::new(config.auth_rate_limit, clock.clone()),
                pair_limiter: RateLimiter::new(config.pairing_rate_limit, clock.clone()),
                pairing: PairingManager::with_ttl(clock.clone(), config.pairing_ttl)
                    .with_grace(config.pairing_grace),
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
        // `tf=1` tells the phone up front that the code field is required.
        self.inner.pairing.payload_with_hint(
            &ticket,
            self.public_key(),
            &self.inner.config.server_name,
            endpoints,
            self.inner.config.require_totp,
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

    // --- Device management (rename, scope). `by` is the asking device id or
    // `local`; permission checks are the caller's job. ---

    /// Renames an active device (see [`crate::validate_device_name`]).
    /// Returns `false` if it is unknown or revoked.
    pub fn rename_device(&self, device_id: &str, name: &str, by: &str) -> Result<bool> {
        let name = crate::registry::validate_device_name(name)?;
        let inner = &self.inner;
        let Some(from) = inner.registry.rename(device_id, &name)? else {
            return Ok(false);
        };
        let mut live = inner.live.lock().expect("poisoned");
        for c in live.values_mut().filter(|c| c.info.device_id == device_id) {
            c.info.device_name = name.clone();
        }
        drop(live);
        if from != name {
            inner.audit.record(AuditEvent::DeviceRenamed {
                device_id: device_id.to_string(),
                by: by.to_string(),
                from,
                to: name,
            });
        }
        Ok(true)
    }

    /// Changes the scope of an active device. Returns `false` if it is
    /// unknown or revoked. Live connections keep running; the application
    /// layer must re-check the registry scope on every request (the
    /// `ConnectionInfo` scope is updated here for display).
    pub fn set_device_scope(&self, device_id: &str, scope: Scope, by: &str) -> Result<bool> {
        let inner = &self.inner;
        let Some(from) = inner.registry.set_scope(device_id, scope)? else {
            return Ok(false);
        };
        let mut live = inner.live.lock().expect("poisoned");
        for c in live.values_mut().filter(|c| c.info.device_id == device_id) {
            c.info.scope = scope;
        }
        drop(live);
        if from != scope {
            inner.audit.record(AuditEvent::DeviceScopeChanged {
                device_id: device_id.to_string(),
                by: by.to_string(),
                from,
                to: scope,
            });
        }
        Ok(true)
    }

    /// Current scope of an active device, or `None` when it is unknown or
    /// revoked: the authoritative per-request permission source.
    pub fn device_scope(&self, device_id: &str) -> Result<Option<Scope>> {
        Ok(self
            .inner
            .registry
            .get(device_id)?
            .filter(DeviceRecord::is_active)
            .map(|d| d.scope))
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
        let pre_auth = inner
            .pre_auth_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| NetError::Protocol("too many connections".into()))?;
        // Before anything is read: one address must not hold the whole pool
        // with connections that never say a word.
        let peer_slot = SourceSlot::take(
            &inner.pre_auth_sources,
            &format!("peer:{}", peer.ip()),
            peer_cap(&inner.config, peer),
        )
        .ok_or_else(|| NetError::Protocol("too many connections from one peer".into()))?;
        let (transport, session, device) = timeout(
            inner.config.handshake_timeout,
            self.establish(stream, peer, conn_id),
        )
        .await
        .map_err(|_| NetError::Timeout)??;
        drop(peer_slot);
        drop(pre_auth);
        // Only a signed-in device takes a slot of the main pool.
        let permit = inner
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| NetError::Protocol("too many connections".into()))?;

        let now = inner.clock.now_millis();
        let ip = device.client_ip.to_string();
        if let Err(e) = inner.registry.touch(&device.device_id, now, Some(&ip)) {
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
        let upgrade = timeout(
            inner.config.upgrade_timeout,
            tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(ws_config())),
        )
        .await
        .map_err(|_| NetError::Timeout)?;
        let ws = match upgrade {
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
        let sources = limit_keys(peer, client_ip, provider_client_id.as_deref());
        inner.audit.record(AuditEvent::ConnectionAttempt {
            conn_id,
            client_ip,
            forwarded,
            provider_client_id,
        });
        // Without a client id a tunnel's visitors are all "this computer":
        // that one address is the peer cap's to limit, not this one's.
        let source_cap =
            if sources.first().is_some_and(|s| s.starts_with("ip:")) && client_ip.is_loopback() {
                usize::MAX
            } else {
                inner.config.max_pre_auth_per_source.max(1)
            };
        let _source_slot = SourceSlot::take(
            &inner.pre_auth_sources,
            sources.first().map(String::as_str).unwrap_or_default(),
            source_cap,
        )
        .ok_or_else(|| NetError::Protocol("too many connections from one source".into()))?;
        let mut transport = ws_frames(ws);
        let noise_config = NoiseConfig {
            max_message_size: PRE_AUTH_MAX_MESSAGE,
            ..inner.config.noise
        };
        let mut session = handshake_responder(&mut transport, &inner.keypair, noise_config).await?;
        let result = self
            .authenticate(
                &mut transport,
                &mut session,
                conn_id,
                client_ip,
                forwarded,
                &sources,
            )
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
        sources: &[String],
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
        // `TotpRequired` is no guess: the caller already presented a valid
        // secret and only left out the code, which it now knows it needs.
        // Wrong codes still count.
        if !matches!(
            failure,
            AuthFailure::RateLimited
                | AuthFailure::KillSwitch
                | AuthFailure::Internal
                | AuthFailure::TotpRequired
        ) {
            for source in sources {
                self.count_failure(source);
            }
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
        sources: &[String],
    ) -> Result<AuthenticatedDevice> {
        let inner = &self.inner;
        let key = *session.remote_static();
        // The key a paired device proved in the handshake. Its sign-ins are
        // limited by device, never by address: others on the same address
        // (or a tunnel, where everybody shares one) cannot lock it out.
        let known_device = matches!(inner.registry.get_by_key(&key), Ok(Some(d)) if d.is_active());
        // The device of a `pair` frame and its secret: the secret is checked
        // there but only consumed once the following `login` passes.
        let mut paired: Option<(DeviceRecord, Zeroizing<Vec<u8>>)> = None;

        macro_rules! fail {
            ($dev:expr, $failure:expr, $detail:expr) => {
                return Err(self
                    .fail(
                        t, session, conn_id, client_ip, sources, &key, $dev, $failure, $detail,
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
            if !known_device && sources.iter().any(|s| inner.auth_limiter.check(s).is_err()) {
                fail!(None, AuthFailure::RateLimited, "client source locked out")
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
                    // Checked, not consumed: a missing or mistyped TOTP code
                    // in the following `login` must not spend the QR code.
                    let secret_bytes = Zeroizing::new(b64d(&secret).unwrap_or_default());
                    let checked = if secret_bytes.is_empty() {
                        Err(RedeemError::Invalid)
                    } else {
                        // The grace period after a presentation in time is
                        // this device's (its key), never the ticket's.
                        inner.pairing.check(&secret_bytes, &key)
                    };
                    let scope = match checked {
                        Ok(scope) => scope,
                        // The lock only holds back wrong secrets: strangers
                        // sending junk must not stop the owner's real code,
                        // and a 144-bit secret cannot be guessed past it.
                        Err(_) if inner.pair_limiter.check(GLOBAL_PAIR_KEY).is_err() => {
                            fail!(
                                None,
                                AuthFailure::RateLimited,
                                "pairing globally locked out"
                            )
                        }
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
                    paired = Some((record, secret_bytes));
                    continue;
                }
                Control::Login { totp, backup_code } => {
                    let totp = totp.map(Zeroizing::new);
                    let backup_code = backup_code.map(Zeroizing::new);
                    let (device, pair_secret) = match paired.take() {
                        Some((d, s)) => (d, Some(s)),
                        None => match self.lookup(&key) {
                            Ok(d) => (d, None),
                            Err((f, detail)) => fail!(None, f, detail),
                        },
                    };
                    let just_paired = pair_secret.is_some();
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
                        // Each code tried with a pairing secret is counted
                        // before it is checked, so parallel connections
                        // cannot try more than the limit.
                        let has_code = totp.is_some() || backup_code.is_some();
                        if let Some(secret) = pair_secret.as_deref()
                            && has_code
                            && !inner.pairing.reserve_code_attempt(secret)
                        {
                            fail!(
                                Some(&device.id),
                                AuthFailure::PairingAttemptsExhausted,
                                "pairing secret used up by wrong codes"
                            )
                        }
                        let verified = match (totp.as_deref(), backup_code.as_deref()) {
                            (Some(code), _) => mgr
                                .verify(code)
                                .map(|()| AuthMethod::Totp)
                                .map_err(|e| format!("TOTP: {e}")),
                            (None, Some(code)) => mgr
                                .verify_backup_code(code)
                                .map(|()| AuthMethod::BackupCode)
                                .map_err(|e| format!("backup code: {e}")),
                            // Not a guess: the pairing secret stays usable.
                            (None, None) => {
                                fail!(Some(&device.id), AuthFailure::TotpRequired, "no TOTP code")
                            }
                        };
                        match verified {
                            Ok(method) => method,
                            Err(detail) => {
                                // Each wrong code counts against the pairing
                                // secret; the last allowed one revokes it.
                                if let Some(secret) = pair_secret.as_deref()
                                    && inner.pairing.record_failed_code(secret)
                                {
                                    let detail = format!(
                                        "{detail}; pairing secret revoked after \
                                         {MAX_PAIRING_CODE_ATTEMPTS} wrong codes"
                                    );
                                    fail!(
                                        Some(&device.id),
                                        AuthFailure::PairingAttemptsExhausted,
                                        &detail
                                    )
                                }
                                fail!(Some(&device.id), AuthFailure::InvalidTotp, &detail)
                            }
                        }
                    } else if just_paired {
                        AuthMethod::Pairing
                    } else {
                        AuthMethod::DeviceKey
                    };
                    // Every check passed: only now is the one-time secret spent.
                    // It may have expired, been revoked or been used by another
                    // connection meanwhile.
                    if let Some(secret) = pair_secret.as_deref()
                        && let Err(e) = inner.pairing.redeem(secret, &key)
                    {
                        let detail = match e {
                            RedeemError::Expired => "pairing secret expired before sign-in",
                            RedeemError::Invalid => "pairing secret used or revoked meanwhile",
                        };
                        fail!(None, AuthFailure::InvalidPairingSecret, detail)
                    }
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
            for source in sources {
                inner.auth_limiter.record_success(source);
            }
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

/// How many unfinished connections one network peer may hold. A provider's
/// agent on this computer is one peer for all its visitors, so it may hold
/// most of the pool, and a quarter stays for everyone else.
fn peer_cap(config: &ServerConfig, peer: SocketAddr) -> usize {
    let pool = config.max_pre_auth.max(1);
    if peer.ip().is_loopback() {
        (pool - pool / 4).max(1)
    } else {
        config.max_pre_auth_per_peer.clamp(1, pool)
    }
}

/// Where the failures of a connection count. Behind a provider on this
/// computer every visitor can share the loopback address, so its client id
/// stands in for an address that says nothing; the client id is
/// unauthenticated, so a real address still counts next to it.
fn limit_keys(peer: SocketAddr, client_ip: IpAddr, client_id: Option<&str>) -> Vec<String> {
    let client = client_id
        .filter(|id| !id.is_empty() && peer.ip().is_loopback())
        .map(|id| format!("client:{id}"));
    let ip = format!("ip:{client_ip}");
    match client {
        Some(client) if client_ip.is_loopback() => vec![client],
        Some(client) => vec![client, ip],
        None => vec![ip],
    }
}

/// One connection of a source that has not signed in yet.
struct SourceSlot {
    table: Arc<Mutex<HashMap<String, usize>>>,
    key: String,
}

impl SourceSlot {
    fn take(table: &Arc<Mutex<HashMap<String, usize>>>, key: &str, max: usize) -> Option<Self> {
        let mut counts = table.lock().expect("poisoned");
        let count = counts.entry(key.to_owned()).or_insert(0);
        if *count >= max {
            return None;
        }
        *count += 1;
        Some(Self {
            table: Arc::clone(table),
            key: key.to_owned(),
        })
    }
}

impl Drop for SourceSlot {
    fn drop(&mut self) {
        let mut counts = self.table.lock().expect("poisoned");
        if let Some(count) = counts.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.key);
            }
        }
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
    /// Transport options: TLS client certificate, upgrade bearer header and
    /// extra roots (for transports such as DNSPilot `app`-mode tunnels).
    pub transport: TransportOptions,
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
            transport: TransportOptions::default(),
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
            let request = crate::client_tls::upgrade_request(&config.url, &config.transport)?;
            #[cfg(feature = "client-tls")]
            let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(ws_config()),
                true,
                crate::client_tls::connector(&config.transport)?,
            )
            .await?;
            #[cfg(not(feature = "client-tls"))]
            let (ws, _) = {
                if config.transport.client_identity.is_some()
                    || !config.transport.extra_root_certs.is_empty()
                {
                    return Err(NetError::InvalidArgument(
                        "TLS client options need the client-tls feature".into(),
                    ));
                }
                tokio_tungstenite::connect_async_with_config(request, Some(ws_config()), true)
                    .await?
            };
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

    /// The peer sends more than the inbound queue holds while the
    /// application is busy sending: outbound messages keep flowing (no
    /// deadlock), and every inbound message arrives once it reads again.
    #[tokio::test]
    async fn a_full_inbound_queue_does_not_stop_outbound_messages() {
        let config = ServerConfig {
            require_totp: false,
            queue: 4,
            ..Default::default()
        };
        let (f, mut rx) = fixture(config).await;
        let device = StaticKeypair::generate();
        let (mut t, mut s) = silent_client(&f, &device).await;
        let mut conn = rx.recv().await.unwrap();
        const SENT: usize = 20;
        for i in 0..SENT {
            s.send(&mut t, &[KIND_APP, 0x90, i as u8]).await.unwrap();
        }
        // Let the server read until its inbound queue is full.
        tokio::time::sleep(Duration::from_millis(200)).await;
        const REPLIES: usize = 50;
        let sender = conn.sender();
        let replies = tokio::spawn(async move {
            for id in 0..REPLIES as u64 {
                sender
                    .send(&Envelope {
                        id,
                        body: ServerMsg::Ok,
                    })
                    .await
                    .unwrap();
            }
        });
        let mut got = 0;
        while got < REPLIES {
            let msg = timeout(Duration::from_secs(5), s.recv(&mut t))
                .await
                .expect("outbound messages stalled behind the inbound queue")
                .unwrap()
                .unwrap();
            if msg.first() == Some(&KIND_APP) {
                got += 1;
            }
        }
        replies.await.unwrap();
        for _ in 0..SENT {
            let msg = timeout(Duration::from_secs(2), conn.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(msg.is_err(), "the test bytes are no client message");
        }
        assert_eq!(conn.close_reason(), None);
    }

    #[test]
    fn the_inbound_budget_holds_one_largest_message() {
        assert_eq!(inbound_budget(16 * 1024 * 1024), 16 * 1024 * 1024);
        assert_eq!(inbound_budget(10), MIN_INBOUND_BUDGET);
        assert!(inbound_budget(usize::MAX) <= u32::MAX as usize);
    }

    /// Messages queued for a busy application are bounded by bytes, not
    /// only by count.
    #[tokio::test]
    async fn queued_inbound_bytes_are_bounded() {
        let (in_tx, mut in_rx) = mpsc::channel::<Inbound>(256);
        let total = 1000;
        let budget = Arc::new(Semaphore::new(total));
        let (slot, permit) = reserve_inbound(&in_tx, &budget, total, 600).await.unwrap();
        slot.send(Inbound {
            bytes: vec![0; 600],
            _budget: permit,
        });
        // The queue has room, the budget has not.
        let second = timeout(
            Duration::from_millis(100),
            reserve_inbound(&in_tx, &budget, total, 600),
        )
        .await;
        assert!(second.is_err(), "over the byte budget");
        // Taken by the application: room again.
        drop(in_rx.recv().await.unwrap());
        assert!(reserve_inbound(&in_tx, &budget, total, 600).await.is_some());
        // A message larger than the whole budget still passes alone.
        drop(in_rx);
        let budget = Arc::new(Semaphore::new(total));
        let (in_tx, _in_rx) = mpsc::channel::<Inbound>(1);
        assert!(
            reserve_inbound(&in_tx, &budget, total, 5000)
                .await
                .is_some()
        );
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

    #[test]
    fn failures_count_by_provider_client_behind_a_local_agent() {
        let local: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let far: SocketAddr = "203.0.113.5:9".parse().unwrap();
        let lo = local.ip();
        // A tunnel without a forwarded address: only the client id tells.
        assert_eq!(limit_keys(local, lo, Some("c1")), ["client:c1"]);
        // A forwarded address counts too: the id is unauthenticated.
        assert_eq!(
            limit_keys(local, far.ip(), Some("c1")),
            ["client:c1", "ip:203.0.113.5"]
        );
        // Only a local agent can name a client.
        assert_eq!(limit_keys(far, far.ip(), Some("c1")), ["ip:203.0.113.5"]);
        assert_eq!(limit_keys(local, lo, None), ["ip:127.0.0.1"]);
    }

    #[test]
    fn a_tunnel_on_this_computer_may_hold_most_of_the_pool_but_not_all() {
        let config = ServerConfig::default();
        let tunnel: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let far: SocketAddr = "203.0.113.5:9".parse().unwrap();
        assert_eq!(peer_cap(&config, tunnel), 48);
        assert_eq!(peer_cap(&config, far), 16);
        let table = Arc::default();
        let first = SourceSlot::take(&table, "peer:x", 1);
        assert!(first.is_some());
        assert!(SourceSlot::take(&table, "peer:x", 1).is_none());
        assert!(SourceSlot::take(&table, "peer:y", 1).is_some());
        drop(first);
        assert!(SourceSlot::take(&table, "peer:x", 1).is_some());
    }
}
