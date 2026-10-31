//! The remote transport for paired devices (`vca-net`): pairing, TOTP,
//! device tokens and a Noise channel over a loopback WebSocket, with every
//! authenticated connection served by the [`Api`].
//!
//! Connectivity providers (tunnels, VPNs, LAN forwarding) only carry bytes
//! to the listen address; they are never trusted for authentication.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vca_core::error::ErrorCode;
use vca_core::{Error, PairedDevice};
use vca_net::{
    AuditEvent, AuditLog, FileDeviceRegistry, FileKeyStore, KillSwitch, NetServer,
    PendingEnrollment, Scope, ServerConfig, ServerConnection, SharedClock, TokenConfig,
    TokenService, TotpManager, load_or_generate_keypair, load_or_generate_secret, system_clock,
};

use crate::api::{Api, DeviceAdmin, DeviceIdentity};
use crate::config::RemoteConfig;
use crate::connectivity::{Connectivity, ConnectivityStatus};
use crate::util::lock;

const SERVER_KEY: &str = "server-static";
const TOKEN_KEY: &str = "token-key";
const TOKEN_KEY_LEN: usize = 32;
const AUDIT_MAX_BYTES: u64 = 10 * 1024 * 1024;
const AUDIT_KEEP: usize = 5;
const TOTP_ISSUER: &str = "Voice Command AI";

/// State of the remote transport, for `vca remote status`.
#[derive(Debug, Clone)]
pub struct RemoteStatus {
    pub listen: SocketAddr,
    pub server_fingerprint: String,
    pub totp_enrolled: bool,
    pub require_totp: bool,
    pub kill_switch_tripped: bool,
    pub paired_devices: usize,
    pub connections: usize,
    /// The connectivity provider, when one is configured.
    pub connectivity: Option<ConnectivityStatus>,
}

/// A pairing code to show as a QR code.
#[derive(Debug, Clone)]
pub struct Pairing {
    pub uri: String,
    pub expires_at: u64,
    pub server_fingerprint: String,
}

/// The running remote transport. Dropping it stops listening.
pub struct Remote {
    net: NetServer,
    totp: Arc<TotpManager>,
    pending_totp: Mutex<Option<PendingEnrollment>>,
    /// `remote.endpoints`; these win over the provider.
    endpoints: Vec<String>,
    connectivity: Option<Arc<Connectivity>>,
    listen: SocketAddr,
    require_totp: bool,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

fn net_err(e: impl std::fmt::Display) -> Error {
    Error::api(ErrorCode::Internal, e.to_string())
}

impl Remote {
    /// Loads (or creates) keys and state below `data_dir`, binds the listen
    /// address and starts accepting devices.
    pub async fn start(
        api: Api,
        cfg: &RemoteConfig,
        server_name: String,
        data_dir: &Path,
    ) -> anyhow::Result<Arc<Self>> {
        let clock: SharedClock = system_clock();
        let dir = data_dir.join("remote");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let keys = FileKeyStore::new(dir.join("keys"));
        let keypair = load_or_generate_keypair(&keys, SERVER_KEY).context("server key")?;
        let token_key =
            load_or_generate_secret(&keys, TOKEN_KEY, TOKEN_KEY_LEN).context("token key")?;
        let registry = Arc::new(FileDeviceRegistry::open(dir.join("devices.json"))?);
        let tokens = Arc::new(TokenService::open(
            token_key,
            TokenConfig::default(),
            clock.clone(),
            dir.join("tokens.json"),
        )?);
        let totp = Arc::new(TotpManager::open(dir.join("totp.json"), clock.clone())?);
        let audit = Arc::new(AuditLog::open(
            dir.join("audit.log"),
            AUDIT_MAX_BYTES,
            AUDIT_KEEP,
            clock.clone(),
        )?);
        let kill = KillSwitch::open(dir.join("killswitch"))?;
        let net = NetServer::builder(keypair, registry, tokens)
            .config(ServerConfig {
                server_name,
                require_totp: cfg.require_totp,
                heartbeat_interval: Duration::from_secs(cfg.heartbeat_secs.max(1)),
                ..ServerConfig::default()
            })
            .clock(clock)
            .totp(Arc::clone(&totp))
            .audit(audit)
            .kill_switch(kill)
            .build();

        let listener = vca_net::bind(&cfg.bind)
            .await
            .with_context(|| format!("listening on {}", cfg.bind))?;
        let listen = listener.local_addr()?;
        let connectivity = Connectivity::from_config(&cfg.provider, listen)
            .context("connectivity provider")?
            .map(Arc::new);
        let remote = Arc::new(Self {
            net: net.clone(),
            totp,
            pending_totp: Mutex::new(None),
            endpoints: cfg.endpoints.clone(),
            connectivity: connectivity.clone(),
            listen,
            require_totp: cfg.require_totp,
            tasks: Mutex::new(Vec::new()),
        });
        api.set_device_admin(Arc::clone(&remote) as Arc<dyn DeviceAdmin>);

        let (tx, mut rx) = mpsc::channel::<ServerConnection>(16);
        let accept = tokio::spawn({
            let net = net.clone();
            async move {
                if let Err(e) = net.serve(listener, tx).await {
                    tracing::warn!("remote transport stopped: {e}");
                }
            }
        });
        let task = tokio::spawn(async move {
            // Dropping the set (when this task is aborted) ends all devices.
            let mut conns = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    conn = rx.recv() => match conn {
                        Some(conn) => {
                            conns.spawn(serve_device(api.clone(), net.clone(), conn));
                        }
                        None => break,
                    },
                    Some(_) = conns.join_next(), if !conns.is_empty() => {}
                }
            }
        });
        lock(&remote.tasks).extend([accept, task]);
        if let Some(c) = connectivity {
            // Tunnels take a while to come up; start now so the first
            // pairing code does not wait.
            let warm = tokio::spawn(async move {
                if let Err(err) = c.ensure_urls().await {
                    tracing::warn!(provider = c.provider_name(), %err, "endpoint setup failed");
                }
            });
            lock(&remote.tasks).push(warm);
        }
        tracing::info!(
            %listen,
            fingerprint = %remote.net.public_key().fingerprint().short(),
            "remote transport listening"
        );
        Ok(remote)
    }

    /// Stops listening and disconnects every device. (The API keeps a
    /// reference for device administration, so dropping is not enough.)
    pub fn stop(&self) {
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
    }

    /// [`stop`](Self::stop), then removes what the connectivity provider
    /// set up (a `tailscale serve` rule, a tunnel process).
    pub async fn shutdown(&self) {
        self.stop();
        if let Some(c) = &self.connectivity {
            c.teardown().await;
        }
    }

    /// URLs for pairing codes: `remote.endpoints`, else the provider's
    /// endpoint, else the listen address.
    async fn pairing_urls(&self) -> Result<Vec<String>, Error> {
        if !self.endpoints.is_empty() {
            return Ok(self.endpoints.clone());
        }
        match &self.connectivity {
            Some(c) => c.ensure_urls().await.map_err(|e| {
                Error::api(
                    ErrorCode::Unavailable,
                    format!("{} is not ready: {e}", c.provider_name()),
                )
            }),
            None => Ok(vec![format!(
                "ws://{}{}",
                self.listen,
                vca_net::DEFAULT_WS_PATH
            )]),
        }
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    /// Issues a single-use pairing code (SEC-01).
    pub async fn pair(&self, scope: &str, endpoints: Vec<String>) -> Result<Pairing, Error> {
        let scope = match scope {
            "view" => Scope::View,
            "command" => Scope::Command,
            "admin" => Scope::Admin,
            other => {
                return Err(Error::api(
                    ErrorCode::InvalidRequest,
                    format!("unknown scope {other:?} (view, command or admin)"),
                ));
            }
        };
        let endpoints = if endpoints.is_empty() {
            self.pairing_urls().await?
        } else {
            endpoints
        };
        let payload = self
            .net
            .create_pairing(scope, endpoints)
            .map_err(|e| Error::api(ErrorCode::InvalidRequest, e.to_string()))?;
        Ok(Pairing {
            uri: payload.to_uri(),
            expires_at: payload.expires_at,
            server_fingerprint: self.net.public_key().fingerprint().to_hex(),
        })
    }

    /// Starts TOTP enrollment; returns the `otpauth://` URI and the base32
    /// secret.
    pub fn totp_begin(&self, account: &str) -> (String, String) {
        let pending = self.totp.begin_enrollment(TOTP_ISSUER, account);
        let out = (
            pending.otpauth_uri.to_string(),
            pending.secret_base32().to_string(),
        );
        *lock(&self.pending_totp) = Some(pending);
        out
    }

    /// Confirms the pending enrollment; returns the backup codes.
    pub fn totp_confirm(&self, code: &str) -> Result<Vec<String>, Error> {
        let pending = lock(&self.pending_totp).clone().ok_or_else(|| {
            Error::api(
                ErrorCode::Conflict,
                "no enrollment in progress; start one first",
            )
        })?;
        let codes = self
            .totp
            .confirm_enrollment(&pending, code)
            .map_err(|e| Error::api(ErrorCode::Unauthorized, e.to_string()))?;
        lock(&self.pending_totp).take();
        Ok(codes)
    }

    pub fn reset_kill_switch(&self, by: &str) -> Result<(), Error> {
        self.net.reset_kill_switch(by).map_err(net_err)
    }

    pub async fn status(&self) -> RemoteStatus {
        let connectivity = match &self.connectivity {
            Some(c) => Some(c.status().await),
            None => None,
        };
        RemoteStatus {
            listen: self.listen,
            server_fingerprint: self.net.public_key().fingerprint().to_hex(),
            totp_enrolled: self.totp.is_enrolled(),
            require_totp: self.require_totp,
            kill_switch_tripped: self.net.kill_switch().is_tripped(),
            paired_devices: self
                .net
                .registry()
                .list()
                .map(|l| l.iter().filter(|d| d.is_active()).count())
                .unwrap_or(0),
            connections: self.net.active_connections().len(),
            connectivity,
        }
    }
}

impl DeviceAdmin for Remote {
    fn list_devices(&self, current: Option<&str>) -> Result<Vec<PairedDevice>, Error> {
        let records = self.net.registry().list().map_err(net_err)?;
        Ok(records
            .iter()
            .filter(|d| d.is_active())
            .map(|d| d.to_paired_device(current.unwrap_or("")))
            .collect())
    }

    fn revoke_device(&self, device_id: &str) -> Result<bool, Error> {
        self.net.revoke_device(device_id).map_err(net_err)
    }

    fn trip_kill_switch(&self, by: &str) -> Result<(), Error> {
        self.net.trip_kill_switch(by).map_err(net_err)
    }
}

/// Serves one authenticated device until it disconnects.
async fn serve_device(api: Api, net: NetServer, conn: ServerConnection) {
    let (device, mut conn) = conn.into_parts();
    let identity = DeviceIdentity {
        device_id: device.device_id.clone(),
        scope: device.scope,
    };
    let (client, mut outbound) = api.connect_as("remote", Some(identity));
    let sender = conn.sender();
    let writer = tokio::spawn(async move {
        while let Some(env) = outbound.recv().await {
            if sender.send(&env).await.is_err() {
                return;
            }
        }
    });
    while let Some(msg) = conn.recv().await {
        match msg {
            Ok(env) => {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    let kind = serde_json::to_value(&env.body)
                        .ok()
                        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned));
                    tracing::debug!(device = %device.device_id, id = env.id, kind = ?kind, "remote request");
                }
                // Every state-changing request is audited (SEC-07).
                if Scope::required_for(&env.body) > Scope::View {
                    net.audit()
                        .record(AuditEvent::command(&device.device_id, &env.body));
                }
                api.handle(&client, env).await;
            }
            Err(e) => {
                tracing::debug!(device = %device.device_id, "remote message error: {e}");
                break;
            }
        }
    }
    writer.abort();
}
