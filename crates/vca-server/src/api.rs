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
    SessionId,
};
use vca_net::Scope;

use crate::cloud::Cloud;
use crate::events::{EventStream, StreamItem};
use crate::sessions::{SessionManager, Update};
use crate::speech::{Speech, TranscribeRequest};
use crate::util::{error_text, lock};

/// Outbound queue length per client. A client that falls this far behind
/// slows down its own event forwarding; other clients are unaffected.
const OUTBOUND_CAPACITY: usize = 1024;

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
    async fn send(&self, id: u64, body: ServerMsg) {
        let _ = self.out.send(Envelope { id, body }).await;
    }
}

struct Inner {
    mgr: SessionManager,
    server_name: String,
    resources_interval: Duration,
    next_client: AtomicU64,
    devices: OnceLock<Arc<dyn DeviceAdmin>>,
    speech: OnceLock<Arc<Speech>>,
    cloud: OnceLock<Arc<Cloud>>,
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

fn remote_off() -> ServerMsg {
    error(ErrorCode::Unsupported, "the remote transport is off")
}

fn cloud_off() -> ServerMsg {
    error(ErrorCode::Unsupported, "cloud sessions are off")
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
                speech: OnceLock::new(),
                cloud: OnceLock::new(),
            }),
        }
    }

    /// Connects device administration (the remote transport). Without it,
    /// device messages answer `Unsupported`.
    pub fn set_device_admin(&self, admin: Arc<dyn DeviceAdmin>) {
        let _ = self.inner.devices.set(admin);
    }

    /// Connects the server-side speech pipeline. Without it, `Transcribe`
    /// answers `Unsupported`.
    pub fn set_speech(&self, speech: Arc<Speech>) {
        let _ = self.inner.speech.set(speech);
    }

    /// Connects the cloud accounts. Without them, cloud messages answer
    /// `Unsupported`.
    pub fn set_cloud(&self, cloud: Arc<Cloud>) {
        let _ = self.inner.cloud.set(cloud);
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
        });

        let mgr = self.inner.mgr.clone();
        let mut updates = mgr.subscribe_updates();
        let tx = out.clone();
        let forward = tokio::spawn(async move {
            loop {
                let body = match updates.recv().await {
                    Ok(Update::Changed(session)) => ServerMsg::SessionUpdated { session },
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
        lock(&client.tasks).extend([forward, resources]);
        (client, rx)
    }

    /// Handles one request. Responses carry the request's id.
    pub async fn handle(&self, client: &Arc<Client>, env: Envelope<ClientMsg>) {
        let id = env.id;
        let mgr = &self.inner.mgr;
        if let Some(device) = &client.device {
            if !device.scope.permits(&env.body) {
                let reply = error(
                    ErrorCode::Forbidden,
                    "this device is not allowed to do that; pair it with a wider scope",
                );
                client.send(id, reply).await;
                return;
            }
        }
        let current = client.device.as_ref().map(|d| d.device_id.as_str());
        let by = current.unwrap_or("local");
        let devices = self.inner.devices.get();
        let reply = match env.body {
            ClientMsg::Hello {
                protocol_version, ..
            } => {
                if protocol_version == PROTOCOL_VERSION {
                    ServerMsg::Welcome {
                        protocol_version: PROTOCOL_VERSION,
                        server_version: env!("CARGO_PKG_VERSION").to_owned(),
                        server_name: self.inner.server_name.clone(),
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
            ClientMsg::Unsubscribe { session_id } => {
                if let Some(t) = lock(&client.subs).remove(&session_id) {
                    t.abort();
                }
                ServerMsg::Ok
            }
            ClientMsg::SendInput { session_id, input } => {
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
                        let client = Arc::clone(client);
                        tokio::spawn(async move {
                            match done.await {
                                Ok(Ok(())) => {
                                    client
                                        .send(
                                            0,
                                            ServerMsg::InputAck {
                                                session_id,
                                                status: DeliveryStatus::Delivered,
                                            },
                                        )
                                        .await;
                                }
                                Ok(Err(err)) => client.send(id, error_msg(&err)).await,
                                Err(_) => {
                                    client
                                        .send(id, error(ErrorCode::Unavailable, "input dropped"))
                                        .await;
                                }
                            }
                        });
                        return;
                    }
                    Err(err) => error_msg(&err),
                }
            }
            ClientMsg::Control { session_id, action } => {
                match mgr.control(&session_id, action).await {
                    Ok(()) => ServerMsg::Ok,
                    Err(err) => error_msg(&err),
                }
            }
            ClientMsg::Launch { request } => match mgr.launch(&request) {
                Ok(session) => ServerMsg::Launched { session },
                Err(err) => error_msg(&err),
            },
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
                };
                // Recognition takes seconds; keep serving the client meanwhile.
                let client = Arc::clone(client);
                tokio::spawn(async move {
                    let reply = match speech.transcribe(req, &ctx).await {
                        Ok(t) => ServerMsg::Transcript {
                            text: t.text,
                            raw_text: t.raw_text,
                            language: t.language,
                            corrections: t.corrections,
                            intent: t.intent,
                            engine: t.engine,
                        },
                        Err(err) => error_msg(&err),
                    };
                    client.send(id, reply).await;
                });
                return;
            }
            ClientMsg::ListDevices => match devices.map(|d| d.list_devices(current)) {
                Some(Ok(devices)) => ServerMsg::Devices { devices },
                Some(Err(err)) => error_msg(&err),
                None => remote_off(),
            },
            ClientMsg::RevokeDevice { device_id } => {
                match devices.map(|d| d.revoke_device(&device_id)) {
                    Some(Ok(true)) => ServerMsg::Ok,
                    Some(Ok(false)) => {
                        error(ErrorCode::NotFound, format!("no active device {device_id}"))
                    }
                    Some(Err(err)) => error_msg(&err),
                    None => remote_off(),
                }
            }
            ClientMsg::KillSwitch => match devices.map(|d| d.trip_kill_switch(by)) {
                Some(Ok(())) => ServerMsg::Ok,
                Some(Err(err)) => error_msg(&err),
                None => remote_off(),
            },
            ClientMsg::GetResources => match mgr.broker() {
                Some(broker) => {
                    client.wants_resources.store(true, Ordering::Relaxed);
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
        };
        client.send(id, reply).await;
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
        let (replay, mut stream) = EventStream::open(log, from_seq);
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
        if let Some(old) = lock(&client.subs).insert(session_id, task) {
            old.abort();
        }
    }
}
