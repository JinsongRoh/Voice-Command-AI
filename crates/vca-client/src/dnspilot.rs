//! DNSPilot app-mode tunnels on the device side
//! (<https://dns.dnspilot.kr/docs/en/tunnel-app-mode>).
//!
//! A DNSPilot `app`-mode tunnel admits only clients whose TLS client
//! certificate key is registered on the tunnel ("App clients"). This module
//! owns, per app install:
//!
//! * **one P-256 client key and self-signed certificate** (the key is
//!   pinned by its SPKI SHA-256; no CA involved), kept only in the
//!   [`KeyStore`] (OS keychain), never in the webview;
//! * the **DNSPilot OAuth tokens** from the device flow (client `vca`,
//!   scopes `tunnels:read tunnels:write devices:read`), in the key store;
//!   refresh tokens rotate and only the newest is kept;
//! * optional per-tunnel **`dpt_app_` tokens**, in the key store;
//! * the list of known tunnel hostnames (non-secret, `dnspilot.json`): the
//!   client certificate and app token are presented only to these hosts.
//!
//! The setup steps ([`DnsPilotManager`]): device authorization, a device
//! picker (`GET /api/agent-devices`), zones (`GET /api/tunnels/zones`, with
//! a typed-in zone id while that call is not available), reuse of an
//! existing app-mode tunnel (`GET /api/tunnels`) or `POST /api/tunnels`
//! (`access_mode: "app"`; the tunnel id and hostname come from the
//! response), and `GET/POST /api/tunnels/{id}/app-clients`.
//!
//! A phone needs no DNSPilot login: the signed-in PC creates a single-use
//! invite code ([`DnsPilotManager::create_invite`]) and the phone registers
//! its own certificate with it ([`DnsPilotManager::enroll`]). The code is a
//! secret until used, so it is never logged, stored or put in an error.
//!
//! The tunnel is only a transport: VCA's Noise channel with the pinned
//! server key, pairing and device tokens apply unchanged inside it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use vca_net::{KeyStore, TlsClientIdentity, TransportOptions};
use vca_providers::ProviderError;
use vca_providers::dnspilot::console::ConsoleClient;
use vca_providers::dnspilot::console::public_enroll;
use vca_providers::dnspilot::contract::{
    self, ACCESS_MODE_APP, CreateAppInvite, CreateTunnel, DEFAULT_CONSOLE_URL,
    ENROLL_READY_DEFAULT_SECS, INVITE_TTL_MAX_SECS, INVITE_TTL_MIN_SECS, IssueAppToken,
    invite_errors,
};
pub use vca_providers::dnspilot::invite::EnrollLink;
use vca_providers::dnspilot::invite::{enroll_message, enroll_request};
use vca_providers::dnspilot::keeper::{LockFile, LockGuard, TokenKeeper, TokenStore};
use vca_providers::dnspilot::oauth::{
    DeviceAuthorization, DeviceFlowClient, OAuthConfig, OAuthError, TokenSet,
};
use vca_providers::dnspilot::{
    ClientKey, normalize_hostname, random_hostname_label, spki_sha256_hex, tunnel_url,
};
use zeroize::Zeroizing;

use crate::error::ClientError;
use crate::store;

/// Key-store name of the client private key (PKCS#8 DER).
pub const CLIENT_KEY_NAME: &str = "dnspilot-client-key";
/// Key-store name of the client certificate (DER; kept next to its key).
pub const CLIENT_CERT_NAME: &str = "dnspilot-client-cert";
/// Key-store name of the DNSPilot OAuth tokens (JSON).
pub const OAUTH_TOKENS_NAME: &str = "dnspilot-oauth";
/// Non-secret list of known tunnels inside the hub's data directory.
pub const TUNNELS_FILE: &str = "dnspilot.json";
/// Cross-process lock around refreshing the DNSPilot OAuth tokens, inside
/// the hub's data directory.
pub const OAUTH_LOCK_FILE: &str = "dnspilot-oauth.lock";
/// vcad's default loopback port.
pub const DEFAULT_LOCAL_PORT: u16 = 47800;
/// Label of the app client registered on the tunnel (non-identifying).
const CLIENT_LABEL: &str = "Voice Command AI app";
/// Label of the invite embedded in a pairing QR code (it becomes the app
/// client's label).
pub const PAIRING_INVITE_LABEL: &str = "VCA phone";
/// Lifetime of that invite, in seconds.
pub const PAIRING_INVITE_TTL_SECS: u32 = 600;
/// Longest label DNSPilot accepts on an invite.
const INVITE_LABEL_MAX: usize = 64;
const CERT_COMMON_NAME: &str = "Voice Command AI app";
const FILE_VERSION: u32 = 1;

/// Key-store name of a tunnel's `dpt_app_` token.
pub fn app_token_name(hostname: &str) -> String {
    format!(
        "dnspilot-apptoken-{}",
        &spki_sha256_hex(hostname.as_bytes())[..16]
    )
}

/// The `dnspilot_*` code of a documented invite error.
fn coded(code: &str) -> &'static str {
    match code {
        invite_errors::BAD_REQUEST => "dnspilot_bad_request",
        invite_errors::INVITE_INVALID => "dnspilot_invite_invalid",
        invite_errors::ENROLL_BAD_SIGNATURE => "dnspilot_enroll_bad_signature",
        invite_errors::ENROLL_BAD_CERT => "dnspilot_enroll_bad_cert",
        invite_errors::RATE_LIMITED => "dnspilot_rate_limited",
        invite_errors::TUNNEL_NOT_APP_MODE => "dnspilot_tunnel_not_app_mode",
        invite_errors::CLIENT_LIMIT => "dnspilot_client_limit",
        invite_errors::INVITE_LIMIT => "dnspilot_invite_limit",
        _ => "dnspilot_error",
    }
}

pub(crate) fn dp_err(e: ProviderError) -> ClientError {
    if let ProviderError::RateLimited { retry_after } = e {
        return ClientError::DnsPilotRateLimited {
            retry_after_secs: retry_after.map(|d| d.as_secs()),
        };
    }
    let code = match &e {
        ProviderError::Coded { code, .. } => coded(code),
        ProviderError::Unauthorized(_) | ProviderError::NotSignedIn(_) => "dnspilot_reauthorize",
        ProviderError::Forbidden(_) => "dnspilot_forbidden",
        ProviderError::NotFound(_) => "dnspilot_not_found",
        ProviderError::Conflict(_) => "dnspilot_conflict",
        ProviderError::InvalidConfig(_) => "dnspilot_invalid",
        ProviderError::Io(_) | ProviderError::Timeout(_) => "dnspilot_unavailable",
        _ => "dnspilot_error",
    };
    ClientError::DnsPilot {
        code,
        message: e.to_string(),
    }
}

pub(crate) fn oauth_err(e: OAuthError) -> ClientError {
    if let OAuthError::Provider(p) = e {
        return dp_err(p);
    }
    let code = match &e {
        OAuthError::AccessDenied => "dnspilot_denied",
        OAuthError::Expired => "dnspilot_expired",
        OAuthError::InvalidGrant => "dnspilot_reauthorize",
        OAuthError::Disabled => "dnspilot_disabled",
        _ => "dnspilot_error",
    };
    ClientError::DnsPilot {
        code,
        message: e.to_string(),
    }
}

/// DNSPilot allows tunnels only on the devices picked when VCA was approved.
fn device_not_allowed(detail: &str) -> ClientError {
    ClientError::DnsPilot {
        code: "dnspilot_device_not_allowed",
        message: format!(
            "DNSPilot refused to create the tunnel on this PC ({detail}). It was probably not              selected when you approved VCA: disconnect DNSPilot, connect again and select it."
        ),
    }
}

fn invalid(message: impl Into<String>) -> ClientError {
    ClientError::DnsPilot {
        code: "dnspilot_invalid",
        message: message.into(),
    }
}

/// The app's client certificate. Only [`DnsPilotIdentityInfo`] leaves the
/// native side.
pub struct DnsPilotIdentity {
    cert_der: Vec<u8>,
    key_pkcs8_der: Zeroizing<Vec<u8>>,
    spki_sha256: String,
    public_key_pem: String,
}

impl std::fmt::Debug for DnsPilotIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DnsPilotIdentity")
            .field("spki_sha256", &self.spki_sha256)
            .finish_non_exhaustive()
    }
}

/// Public parts of the client identity (safe to show and copy).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotIdentityInfo {
    /// 64 hex characters: what DNSPilot pins.
    pub spki_sha256: String,
    /// `PUBLIC KEY` PEM (accepted by the console as well).
    pub public_key_pem: String,
}

impl DnsPilotIdentity {
    fn from_parts(
        cert_der: Vec<u8>,
        key_pkcs8_der: Zeroizing<Vec<u8>>,
    ) -> Result<Self, ClientError> {
        let key = rcgen::KeyPair::try_from(key_pkcs8_der.as_slice()).map_err(|_| {
            ClientError::Storage("the stored DNSPilot client key is corrupt".into())
        })?;
        let spki = rcgen::PublicKeyData::subject_public_key_info(&key);
        Ok(Self {
            cert_der,
            key_pkcs8_der,
            spki_sha256: spki_sha256_hex(&spki),
            public_key_pem: key.public_key_pem(),
        })
    }

    /// Generates a new P-256 key and a self-signed certificate.
    pub fn generate() -> Result<Self, ClientError> {
        let internal = |e: rcgen::Error| ClientError::Internal(format!("client certificate: {e}"));
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(internal)?;
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(internal)?;
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, CERT_COMMON_NAME);
        params.distinguished_name = dn;
        let cert = params.self_signed(&key).map_err(internal)?;
        Self::from_parts(cert.der().to_vec(), Zeroizing::new(key.serialize_der()))
    }

    /// Loads the identity from the key store, creating and storing one on
    /// first use.
    pub fn load_or_create(secrets: &dyn KeyStore) -> Result<Self, ClientError> {
        if let (Some(key), Some(cert)) = (
            secrets.load(CLIENT_KEY_NAME)?,
            secrets.load(CLIENT_CERT_NAME)?,
        ) {
            return Self::from_parts(cert.to_vec(), Zeroizing::new(key.to_vec()));
        }
        let id = Self::generate()?;
        secrets.save(CLIENT_KEY_NAME, &id.key_pkcs8_der)?;
        secrets.save(CLIENT_CERT_NAME, &id.cert_der)?;
        Ok(id)
    }

    pub fn info(&self) -> DnsPilotIdentityInfo {
        DnsPilotIdentityInfo {
            spki_sha256: self.spki_sha256.clone(),
            public_key_pem: self.public_key_pem.clone(),
        }
    }

    pub fn spki_sha256(&self) -> &str {
        &self.spki_sha256
    }

    /// The certificate later presented for mTLS (DER).
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// ECDSA P-256 with SHA-256 over `message`, DER (ASN.1) encoded.
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, ClientError> {
        let key = rcgen::KeyPair::try_from(self.key_pkcs8_der.as_slice()).map_err(|_| {
            ClientError::Storage("the stored DNSPilot client key is corrupt".into())
        })?;
        rcgen::SigningKey::sign(&key, message)
            .map_err(|e| ClientError::Internal(format!("signing failed: {e}")))
    }

    pub fn to_tls(&self) -> TlsClientIdentity {
        TlsClientIdentity {
            cert_chain_der: vec![self.cert_der.clone()],
            private_key_pkcs8_der: self.key_pkcs8_der.clone(),
        }
    }
}

/// A DNSPilot tunnel the app knows (non-secret metadata).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotTunnel {
    /// `name.zone`.
    pub hostname: String,
    /// 443 or 9443.
    pub public_port: u16,
    /// Console id of the tunnel, when known.
    pub tunnel_id: Option<i64>,
    /// Agent device that serves it, when known.
    pub device_id: Option<i64>,
    /// Created by this app's DNSPilot authorization (only then may the app
    /// register its key through the API).
    pub created_by_app: bool,
    /// This app's client key is registered on the tunnel (as far as the
    /// app knows).
    pub client_registered: bool,
    /// A `dpt_app_` token is stored for this tunnel.
    pub has_app_token: bool,
    /// Console status (`active`, `suspended`, `disabled`), when known.
    #[serde(default)]
    pub status: Option<String>,
}

impl DnsPilotTunnel {
    /// `wss://<host>[:9443]/v1/ws`.
    pub fn ws_url(&self) -> String {
        format!(
            "{}{}",
            tunnel_url(&self.hostname, self.public_port),
            vca_net::DEFAULT_WS_PATH
        )
    }
}

#[derive(Serialize, Deserialize)]
struct TunnelsFile {
    version: u32,
    tunnels: Vec<DnsPilotTunnel>,
}

fn load_tunnels(dir: &Path) -> Result<Vec<DnsPilotTunnel>, ClientError> {
    let path = dir.join(TUNNELS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ClientError::Storage(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let file: TunnelsFile = serde_json::from_str(&text)
        .map_err(|e| ClientError::Storage(format!("corrupt {}: {e}", path.display())))?;
    if file.version != FILE_VERSION {
        return Err(ClientError::Storage(format!(
            "unsupported {} version {}",
            path.display(),
            file.version
        )));
    }
    Ok(file
        .tunnels
        .into_iter()
        .filter(|t| normalize_hostname(&t.hostname).ok().as_deref() == Some(t.hostname.as_str()))
        .collect())
}

/// Host part of a `ws://` / `wss://` URL (lowercase, without port).
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("wss://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.rsplit_once(':') {
        Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    Some(host.to_ascii_lowercase())
}

/// What the app shows while the person approves VCA in DNSPilot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationPrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
}

/// Status of the DNSPilot connection, for the UI (no secrets).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotStatus {
    /// OAuth tokens are stored.
    pub connected: bool,
    /// Granted scopes (space-separated), when connected.
    pub scope: Option<String>,
    /// An authorization is waiting for approval.
    pub pending: bool,
    pub identity: Option<DnsPilotIdentityInfo>,
    pub tunnels: Vec<DnsPilotTunnel>,
    /// Where the person approves and manages things.
    pub console_url: String,
    /// The stored grant cannot be renewed safely (an earlier renewal's
    /// result was lost): connect DNSPilot again.
    #[serde(default)]
    pub needs_reauthorization: bool,
}

/// Creates an app-mode tunnel for vcad's port.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateTunnelRequest {
    /// The PC's Agent device id (from [`DnsPilotManager::devices`]; it must
    /// be one of the devices picked when VCA was approved).
    pub device_id: i64,
    /// Zone (domain) id (from [`DnsPilotManager::zones`], or typed in from
    /// the console while that call is not available).
    pub zone_id: i64,
    /// vcad's loopback port (default 47800).
    pub local_port: Option<u16>,
}

/// An active Agent device the person can pick.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotDevice {
    pub id: i64,
    pub name: String,
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub online: bool,
    pub agent_version: Option<String>,
}

/// A zone tunnels can be created in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotZone {
    pub id: i64,
    pub name: String,
}

/// An app-mode tunnel in the person's DNSPilot organization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotRemoteTunnel {
    pub id: i64,
    pub hostname: String,
    pub public_port: u16,
    pub local_port: Option<u16>,
    pub device_id: Option<i64>,
    pub device_name: Option<String>,
    /// `active`, `suspended` or `disabled`.
    pub status: Option<String>,
    /// Already in this app's list.
    pub known: bool,
}

/// An app client (registered key) of a tunnel, for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotAppClient {
    pub id: i64,
    pub label: Option<String>,
    /// `local`, `console`, `oauth` or `invite`.
    pub source: Option<String>,
    pub last_seen_at: Option<String>,
    /// The key of this app install.
    pub this_app: bool,
}

/// A device invite as the console lists it. Codes are never listed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotInvite {
    pub id: i64,
    pub label: Option<String>,
    /// `active`, `used`, `expired` or `revoked`.
    pub status: Option<String>,
    pub expires_at: Option<String>,
}

/// The outcome of registering this app's key with an invite code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsPilotEnrollment {
    pub hostname: String,
    pub tunnel_id: i64,
    /// `None` when the code was used already and this app knows the tunnel.
    pub client_id: Option<i64>,
    /// How long connections may still be refused: the Agent learns a new
    /// key on its poll. 0 means connect at once.
    pub ready_within_sec: u32,
}

/// What a pairing code gets from DNSPilot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingInvite {
    /// The pairing code does not go through a DNSPilot tunnel.
    NotApplicable,
    /// Embed this invite in the QR code.
    Included(EnrollLink),
    /// A plain QR code is shown instead. `reason`: `not_connected`,
    /// `not_owned`, `rate_limited`, `invite_limit` or `failed`.
    Skipped { reason: &'static str },
}

/// Result of registering the app's client key on a tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Registration {
    /// Registered now through the API.
    Registered { client_id: i64 },
    /// The key was already on the tunnel's App clients list.
    AlreadyRegistered,
    /// The person must register the key in the console (tunnel details ->
    /// App clients, which asks for a re-authentication), or with
    /// `dnspilot client add <tunnel-id> --spki <hex>` on the PC.
    NeedsConsole {
        spki_sha256: String,
        tunnel_id: Option<i64>,
        /// `unknown_tunnel_id`, `not_connected` or `not_created_by_app`.
        reason: String,
    },
}

/// A grant's tokens in the key store, as JSON under `name`, with a lock file
/// shared by every process that opens the same data directory.
pub(crate) struct KeyStoreTokens {
    secrets: Arc<dyn KeyStore>,
    name: &'static str,
    lock: LockFile,
}

impl KeyStoreTokens {
    pub(crate) fn new(secrets: Arc<dyn KeyStore>, name: &'static str, lock: LockFile) -> Self {
        Self {
            secrets,
            name,
            lock,
        }
    }
}

impl TokenStore for KeyStoreTokens {
    fn load(&self) -> vca_providers::Result<Option<TokenSet>> {
        let bytes = self
            .secrets
            .load(self.name)
            .map_err(|e| ProviderError::InvalidConfig(format!("key store: {e}")))?;
        bytes
            .map(|b| serde_json::from_slice(&b))
            .transpose()
            .map_err(|_| ProviderError::Decode("the stored DNSPilot tokens are corrupt".into()))
    }

    fn save(&self, tokens: &TokenSet) -> vca_providers::Result<()> {
        let json = Zeroizing::new(
            serde_json::to_vec(tokens)
                .map_err(|_| ProviderError::Decode("cannot encode the DNSPilot tokens".into()))?,
        );
        self.secrets.save(self.name, &json).map_err(|e| {
            ProviderError::InvalidConfig(format!("cannot store the DNSPilot tokens: {e}"))
        })
    }

    fn try_lock(&self) -> vca_providers::Result<Option<LockGuard>> {
        self.lock.try_acquire()
    }
}

/// DNSPilot state and setup steps of a [`crate::ClientHub`].
pub struct DnsPilotManager {
    data_dir: PathBuf,
    secrets: Arc<dyn KeyStore>,
    oauth: OAuthConfig,
    /// The only refresher of the grant's tokens in this process; every
    /// console call shares it.
    keeper: Arc<TokenKeeper>,
    tunnels: Mutex<Vec<DnsPilotTunnel>>,
    pending: Mutex<Option<DeviceAuthorization>>,
    /// Serializes tunnel creation: a second request finds the first one's
    /// tunnel instead of creating another.
    creating: tokio::sync::Mutex<()>,
    /// `(tunnel_id, invite_id)` of the latest pairing invite, revoked when
    /// the next one replaces it (at most 10 may be active per tunnel).
    pairing_invite: Mutex<Option<(i64, i64)>>,
}

/// The app-mode tunnel that already serves `local_port` on the device, if
/// any; an active one wins.
fn existing_tunnel(
    items: &[contract::Tunnel],
    device_id: i64,
    local_port: u16,
) -> Option<&contract::Tunnel> {
    let same = || {
        items.iter().filter(|t| {
            t.is_app_mode()
                && !t.hostname.is_empty()
                && t.device_id == Some(device_id)
                && t.local_port == Some(local_port)
        })
    };
    same()
        .find(|t| t.status.as_deref() == Some("active"))
        .or_else(|| same().next())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DnsPilotManager {
    pub(crate) fn open(
        data_dir: PathBuf,
        secrets: Arc<dyn KeyStore>,
        oauth: OAuthConfig,
    ) -> Result<Self, ClientError> {
        let tunnels = load_tunnels(&data_dir)?;
        let store = KeyStoreTokens::new(
            Arc::clone(&secrets),
            OAUTH_TOKENS_NAME,
            LockFile::new(data_dir.join(OAUTH_LOCK_FILE)),
        );
        let keeper = Arc::new(TokenKeeper::new(
            DeviceFlowClient::new(oauth.clone()),
            Arc::new(store),
        ));
        Ok(Self {
            data_dir,
            secrets,
            oauth,
            keeper,
            tunnels: Mutex::new(tunnels),
            pending: Mutex::new(None),
            creating: tokio::sync::Mutex::new(()),
            pairing_invite: Mutex::new(None),
        })
    }

    fn save_tunnels(&self, tunnels: &[DnsPilotTunnel]) -> Result<(), ClientError> {
        let data = serde_json::to_vec_pretty(&TunnelsFile {
            version: FILE_VERSION,
            tunnels: tunnels.to_vec(),
        })
        .map_err(ClientError::storage)?;
        store::write_private(&self.data_dir, TUNNELS_FILE, &data)
    }

    fn update_tunnel(
        &self,
        hostname: &str,
        f: impl FnOnce(&mut DnsPilotTunnel),
    ) -> Result<DnsPilotTunnel, ClientError> {
        let mut tunnels = lock(&self.tunnels);
        let mut next = tunnels.clone();
        let t = next
            .iter_mut()
            .find(|t| t.hostname == hostname)
            .ok_or_else(|| ClientError::NotFound(hostname.to_owned()))?;
        f(t);
        let out = t.clone();
        self.save_tunnels(&next)?;
        *tunnels = next;
        Ok(out)
    }

    /// The app's client identity, created on first use.
    pub fn identity(&self) -> Result<DnsPilotIdentityInfo, ClientError> {
        Ok(DnsPilotIdentity::load_or_create(self.secrets.as_ref())?.info())
    }

    fn tokens(&self) -> Result<Option<TokenSet>, ClientError> {
        match self.secrets.load(OAUTH_TOKENS_NAME)? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| ClientError::Storage("the stored DNSPilot tokens are corrupt".into())),
        }
    }

    fn save_tokens(secrets: &dyn KeyStore, tokens: &TokenSet) -> Result<(), ClientError> {
        let json = Zeroizing::new(serde_json::to_vec(tokens).map_err(ClientError::storage)?);
        Ok(secrets.save(OAUTH_TOKENS_NAME, &json)?)
    }

    /// A console client on the shared keeper: concurrent calls never
    /// refresh the same refresh token twice.
    fn console(&self) -> Result<ConsoleClient, ClientError> {
        if self.tokens()?.is_none() {
            return Err(ClientError::DnsPilot {
                code: "dnspilot_not_connected",
                message: "connect DNSPilot first".into(),
            });
        }
        Ok(ConsoleClient::with_keeper(Arc::clone(&self.keeper)))
    }

    /// Current state for the UI.
    pub fn status(&self) -> Result<DnsPilotStatus, ClientError> {
        let tokens = self.tokens()?;
        let identity = match (
            self.secrets.load(CLIENT_KEY_NAME)?,
            self.secrets.load(CLIENT_CERT_NAME)?,
        ) {
            (Some(_), Some(_)) => Some(self.identity()?),
            _ => None,
        };
        Ok(DnsPilotStatus {
            needs_reauthorization: tokens
                .as_ref()
                .is_some_and(|t| t.refresh_in_flight.is_some()),
            connected: tokens.is_some(),
            scope: tokens.map(|t| t.scope),
            pending: lock(&self.pending).is_some(),
            identity,
            tunnels: self.tunnels(),
            console_url: self.oauth.console_url.clone(),
        })
    }

    pub fn tunnels(&self) -> Vec<DnsPilotTunnel> {
        lock(&self.tunnels).clone()
    }

    /// Step 1: starts the device authorization. Show `user_code` and the
    /// activation URL; then call [`wait_for_authorization`](Self::wait_for_authorization).
    pub async fn start_authorization(&self) -> Result<AuthorizationPrompt, ClientError> {
        let auth = DeviceFlowClient::new(self.oauth.clone())
            .start()
            .await
            .map_err(oauth_err)?;
        let prompt = AuthorizationPrompt {
            user_code: auth.user_code.clone(),
            verification_uri: auth.verification_uri.clone(),
            verification_uri_complete: auth.verification_uri_complete.clone(),
            expires_in: auth.expires_in_secs,
        };
        *lock(&self.pending) = Some(auth);
        Ok(prompt)
    }

    /// The activation URL of the pending authorization, only if it is on
    /// the configured console (for opening the system browser natively).
    pub fn activation_url(&self) -> Option<String> {
        let pending = lock(&self.pending);
        let auth = pending.as_ref()?;
        let url = auth
            .verification_uri_complete
            .clone()
            .unwrap_or_else(|| auth.verification_uri.clone());
        let console = self.oauth.console_url.trim_end_matches('/');
        url.starts_with(&format!("{console}/")).then_some(url)
    }

    /// Step 2: polls until the person approves (or denies, or the code
    /// expires) and stores the tokens in the key store.
    pub async fn wait_for_authorization(&self) -> Result<DnsPilotStatus, ClientError> {
        let auth = lock(&self.pending)
            .clone()
            .ok_or_else(|| ClientError::DnsPilot {
                code: "dnspilot_expired",
                message: "no DNSPilot authorization is in progress; start again".into(),
            })?;
        let result = DeviceFlowClient::new(self.oauth.clone()).poll(&auth).await;
        {
            let mut pending = lock(&self.pending);
            if pending.as_ref().map(|p| &p.user_code) == Some(&auth.user_code) {
                *pending = None;
            }
        }
        let tokens = result.map_err(oauth_err)?;
        Self::save_tokens(self.secrets.as_ref(), &tokens)?;
        self.keeper.invalidate().await;
        tracing::info!("DNSPilot connected: new grant stored");
        self.status()
    }

    /// Forgets a pending authorization (the poll task should be dropped by
    /// the caller).
    pub fn cancel_authorization(&self) {
        lock(&self.pending).take();
    }

    /// Records a tunnel the person created in the console (or one created
    /// earlier). Replaces an entry with the same hostname.
    pub fn add_tunnel(
        &self,
        hostname: &str,
        public_port: u16,
        tunnel_id: Option<i64>,
    ) -> Result<DnsPilotTunnel, ClientError> {
        let hostname = normalize_hostname(hostname).map_err(dp_err)?;
        if !matches!(public_port, 443 | 9443) {
            return Err(invalid("DNSPilot tunnels use public port 443 or 9443"));
        }
        let mut tunnels = lock(&self.tunnels);
        let mut next = tunnels.clone();
        let previous = next.iter().position(|t| t.hostname == hostname);
        let entry = DnsPilotTunnel {
            hostname: hostname.clone(),
            public_port,
            tunnel_id: tunnel_id.or_else(|| previous.and_then(|i| next[i].tunnel_id)),
            device_id: previous.and_then(|i| next[i].device_id),
            created_by_app: previous.is_some_and(|i| next[i].created_by_app),
            client_registered: previous.is_some_and(|i| next[i].client_registered),
            has_app_token: previous.is_some_and(|i| next[i].has_app_token),
            status: previous.and_then(|i| next[i].status.clone()),
        };
        match previous {
            Some(i) => next[i] = entry.clone(),
            None => next.push(entry.clone()),
        }
        self.save_tunnels(&next)?;
        *tunnels = next;
        Ok(entry)
    }

    /// Sets the console id of a known tunnel (read from the console).
    pub fn set_tunnel_id(
        &self,
        hostname: &str,
        tunnel_id: i64,
    ) -> Result<DnsPilotTunnel, ClientError> {
        if tunnel_id <= 0 {
            return Err(invalid("a tunnel id is a positive number"));
        }
        self.update_tunnel(hostname, |t| t.tunnel_id = Some(tunnel_id))
    }

    /// The person confirms the key is registered (for example after adding
    /// it in the console).
    pub fn mark_registered(&self, hostname: &str) -> Result<DnsPilotTunnel, ClientError> {
        self.update_tunnel(hostname, |t| t.client_registered = true)
    }

    /// Forgets a tunnel and its stored app token (nothing changes in
    /// DNSPilot).
    pub fn remove_tunnel(&self, hostname: &str) -> Result<(), ClientError> {
        let mut tunnels = lock(&self.tunnels);
        let next: Vec<_> = tunnels
            .iter()
            .filter(|t| t.hostname != hostname)
            .cloned()
            .collect();
        self.secrets.delete(&app_token_name(hostname))?;
        self.save_tunnels(&next)?;
        *tunnels = next;
        Ok(())
    }

    /// Active Agent devices of the organization (`GET /api/agent-devices`)
    /// that the VCA approval allows tunnels on (`oauth_allowed_device_ids`
    /// of `GET /api/tunnels`; every active device when the console does not
    /// report it).
    pub async fn devices(&self) -> Result<Vec<DnsPilotDevice>, ClientError> {
        let console = self.console()?;
        let devices = console.agent_devices().await.map_err(dp_err)?;
        let tunnels = console.tunnels().await.map_err(dp_err)?;
        Ok(devices
            .into_iter()
            .filter(|d| d.is_active() && tunnels.device_allowed(d.id))
            .map(|d| DnsPilotDevice {
                id: d.id,
                name: d
                    .name
                    .clone()
                    .or_else(|| d.hostname.clone())
                    .unwrap_or_else(|| format!("device {}", d.id)),
                hostname: d.hostname,
                os: d.os,
                online: d.online.unwrap_or(false),
                agent_version: d.agent_version,
            })
            .collect())
    }

    /// Zones tunnels can be created in (`GET /api/tunnels/zones`); `None`
    /// while DNSPilot does not offer that call (enter the zone id instead).
    pub async fn zones(&self) -> Result<Option<Vec<DnsPilotZone>>, ClientError> {
        let zones = self.console()?.zones().await.map_err(dp_err)?;
        Ok(zones.map(|z| {
            z.into_iter()
                .map(|z| DnsPilotZone {
                    id: z.id,
                    name: z.name,
                })
                .collect()
        }))
    }

    /// App-mode tunnels of the organization (`GET /api/tunnels`), to reuse
    /// one instead of creating another.
    pub async fn remote_tunnels(&self) -> Result<Vec<DnsPilotRemoteTunnel>, ClientError> {
        let list = self.console()?.tunnels().await.map_err(dp_err)?;
        let known = self.tunnels();
        Ok(list
            .items
            .into_iter()
            .filter(|t| t.is_app_mode() && !t.hostname.is_empty())
            .map(|t| DnsPilotRemoteTunnel {
                known: known
                    .iter()
                    .any(|k| k.tunnel_id == Some(t.id) || k.hostname == t.hostname),
                id: t.id,
                public_port: t.public_port(),
                device_name: t.device_name(),
                hostname: t.hostname,
                local_port: t.local_port,
                device_id: t.device_id,
                status: t.status,
            })
            .collect())
    }

    /// Updates the console status of the known tunnels.
    pub async fn refresh(&self) -> Result<DnsPilotStatus, ClientError> {
        if self.tokens()?.is_some() {
            let list = self.console()?.tunnels().await.map_err(dp_err)?;
            let mut tunnels = lock(&self.tunnels);
            let mut next = tunnels.clone();
            for t in &mut next {
                if let Some(remote) = list
                    .items
                    .iter()
                    .find(|r| Some(r.id) == t.tunnel_id || r.hostname == t.hostname)
                {
                    t.tunnel_id = Some(remote.id);
                    t.status = remote.status.clone();
                }
            }
            if next != *tunnels {
                self.save_tunnels(&next)?;
                *tunnels = next;
            }
        }
        self.status()
    }

    /// Records a tunnel from the console (`GET /api/tunnels/{id}`) and
    /// registers this app's key on it.
    pub async fn use_tunnel(&self, tunnel_id: i64) -> Result<Registration, ClientError> {
        let t = self.console()?.tunnel(tunnel_id).await.map_err(dp_err)?;
        if !t.is_app_mode() {
            return Err(invalid(
                "this tunnel does not use access mode App; VCA only uses app-mode tunnels",
            ));
        }
        let entry = self.record(&t, false)?;
        self.register_client(&entry.hostname).await
    }

    fn record(
        &self,
        t: &vca_providers::dnspilot::contract::Tunnel,
        created_by_app: bool,
    ) -> Result<DnsPilotTunnel, ClientError> {
        let entry = self.add_tunnel(&t.hostname, t.public_port(), Some(t.id))?;
        self.update_tunnel(&entry.hostname, |e| {
            e.device_id = t.device_id.or(e.device_id);
            e.status = t.status.clone();
            e.created_by_app |= created_by_app;
        })
    }

    /// Creates an app-mode tunnel to `127.0.0.1:<local_port>` (http, public
    /// port 443) on the person's PC with a random, non-identifying name,
    /// then registers this app's key on it. DNSPilot emails the
    /// organization a "tunnel opened" notice with a kill link.
    ///
    /// When an app-mode tunnel already serves that port on that device, no
    /// tunnel is created: the existing one is adopted (and returned, with
    /// `client_registered` false if the key cannot be added by the app).
    ///
    /// If the key cannot be registered, the tunnel stays recorded and the
    /// error is `dnspilot_key_registration_failed`: register again, do not
    /// create another tunnel.
    pub async fn create_tunnel(
        &self,
        req: &CreateTunnelRequest,
    ) -> Result<DnsPilotTunnel, ClientError> {
        if req.device_id <= 0 || req.zone_id <= 0 {
            return Err(invalid("the device id and zone id are positive numbers"));
        }
        let _one_at_a_time = self.creating.lock().await;
        let console = self.console()?;
        let list = console.tunnels().await.map_err(dp_err)?;
        if !list.device_allowed(req.device_id) {
            return Err(device_not_allowed("not selected on the approval screen"));
        }
        let local_port = req.local_port.unwrap_or(DEFAULT_LOCAL_PORT);
        if let Some(found) = existing_tunnel(&list.items, req.device_id, local_port) {
            let entry = self.record(found, false)?;
            return self.register_new(&entry.hostname).await;
        }
        let created = console
            .create_app_tunnel(&CreateTunnel {
                device_id: req.device_id,
                zone_id: req.zone_id,
                name: random_hostname_label(),
                local_port,
                access_mode: ACCESS_MODE_APP.into(),
                local_scheme: "http".into(),
                public_port: 443,
            })
            .await
            .map_err(|e| match e {
                ProviderError::Forbidden(detail) => device_not_allowed(&detail),
                other => dp_err(other),
            })?;
        let entry = self.record(&created, true)?;
        self.register_new(&entry.hostname).await
    }

    /// Registers the key on a tunnel that was just recorded.
    async fn register_new(&self, hostname: &str) -> Result<DnsPilotTunnel, ClientError> {
        if let Err(e) = self.register_client(hostname).await {
            return Err(ClientError::DnsPilot {
                code: "dnspilot_key_registration_failed",
                message: format!(
                    "the tunnel {hostname} is ready, but registering this app's key failed: {e}"
                ),
            });
        }
        self.tunnels()
            .into_iter()
            .find(|t| t.hostname == hostname)
            .ok_or_else(|| ClientError::NotFound(hostname.to_owned()))
    }

    /// The app's client certificate (DER; public).
    pub fn client_certificate(&self) -> Result<Vec<u8>, ClientError> {
        Ok(DnsPilotIdentity::load_or_create(self.secrets.as_ref())?
            .cert_der()
            .to_vec())
    }

    /// The DNSPilot device id of the Agent on this computer (desktop app on
    /// the vcad PC), from the Agent's `GET /v1/status`; for preselecting
    /// the device.
    pub async fn local_device_id(&self) -> Option<i64> {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            None
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let status = vca_providers::dnspilot::local::LocalApiClient::default()
                .status()
                .await
                .ok()?;
            status.logged_in.then_some(status.device_id).flatten()
        }
    }

    /// Registers this app's client key on the tunnel through the API when
    /// possible; otherwise says what the person has to do in the console.
    pub async fn register_client(&self, hostname: &str) -> Result<Registration, ClientError> {
        let tunnel = self
            .tunnels()
            .into_iter()
            .find(|t| t.hostname == hostname)
            .ok_or_else(|| ClientError::NotFound(hostname.to_owned()))?;
        let identity = DnsPilotIdentity::load_or_create(self.secrets.as_ref())?;
        let spki = identity.spki_sha256().to_owned();
        let needs = |reason: &str| Registration::NeedsConsole {
            spki_sha256: spki.clone(),
            tunnel_id: tunnel.tunnel_id,
            reason: reason.to_owned(),
        };
        let Some(tunnel_id) = tunnel.tunnel_id else {
            return Ok(needs("unknown_tunnel_id"));
        };
        if self.tokens()?.is_none() {
            return Ok(needs("not_connected"));
        }
        let console = self.console()?;
        let existing = console.app_clients(tunnel_id).await.map_err(dp_err)?;
        if existing.iter().any(|c| {
            c.is_active()
                && c.spki_sha256.as_deref().map(str::to_ascii_lowercase) == Some(spki.clone())
        }) {
            self.mark_registered(hostname)?;
            return Ok(Registration::AlreadyRegistered);
        }
        let request = ClientKey::SpkiSha256(spki.clone()).to_request(Some(CLIENT_LABEL.into()));
        match console.register_app_client(tunnel_id, &request).await {
            Ok(client) => {
                self.mark_registered(hostname)?;
                Ok(Registration::Registered {
                    client_id: client.id,
                })
            }
            // Only tunnels this grant created can be changed by the app.
            Err(ProviderError::Forbidden(_)) => Ok(needs("not_created_by_app")),
            Err(e) => Err(dp_err(e)),
        }
    }

    /// `GET /api/tunnels/{id}/app-clients`: the active clients.
    pub async fn app_clients(&self, tunnel_id: i64) -> Result<Vec<DnsPilotAppClient>, ClientError> {
        let spki = DnsPilotIdentity::load_or_create(self.secrets.as_ref())?
            .spki_sha256()
            .to_owned();
        let clients = self
            .console()?
            .app_clients(tunnel_id)
            .await
            .map_err(dp_err)?;
        Ok(clients
            .into_iter()
            .filter(contract::AppClient::is_active)
            .map(|c| DnsPilotAppClient {
                this_app: c.spki_sha256.as_deref().map(str::to_ascii_lowercase)
                    == Some(spki.clone()),
                id: c.id,
                label: c.label,
                source: c.source,
                last_seen_at: c.last_seen_at,
            })
            .collect())
    }

    /// `DELETE /api/tunnels/{id}/app-clients/{cid}`: the Agent drops that
    /// client within about a minute. Revoking this app's own key also
    /// clears `client_registered`.
    pub async fn revoke_app_client(
        &self,
        tunnel_id: i64,
        client_id: i64,
    ) -> Result<(), ClientError> {
        let own = self
            .app_clients(tunnel_id)
            .await?
            .iter()
            .any(|c| c.id == client_id && c.this_app);
        self.console()?
            .revoke_app_client(tunnel_id, client_id)
            .await
            .map_err(dp_err)?;
        let known = self
            .tunnels()
            .into_iter()
            .find(|t| t.tunnel_id == Some(tunnel_id));
        if let (true, Some(t)) = (own, known) {
            self.update_tunnel(&t.hostname, |e| e.client_registered = false)?;
        }
        Ok(())
    }

    /// Creates a single-use invite code for another device (needs the OAuth
    /// token, so on the signed-in PC). DNSPilot allows this on app-mode
    /// tunnels this grant created. `ttl_secs`: 60 to 3600 (default 600).
    ///
    /// The result holds the only copy of the code. Its `Debug` output hides it.
    pub async fn create_invite(
        &self,
        tunnel_id: i64,
        label: Option<&str>,
        ttl_secs: Option<u32>,
    ) -> Result<EnrollLink, ClientError> {
        Ok(self
            .create_invite_inner(tunnel_id, label, ttl_secs)
            .await?
            .0)
    }

    async fn create_invite_inner(
        &self,
        tunnel_id: i64,
        label: Option<&str>,
        ttl_secs: Option<u32>,
    ) -> Result<(EnrollLink, i64), ClientError> {
        if ttl_secs.is_some_and(|t| !(INVITE_TTL_MIN_SECS..=INVITE_TTL_MAX_SECS).contains(&t)) {
            return Err(invalid("an invite lasts 60 to 3600 seconds"));
        }
        let request = CreateAppInvite {
            label: label.map(|l| l.chars().take(INVITE_LABEL_MAX).collect()),
            ttl_sec: ttl_secs,
        };
        let created = self
            .console()?
            .create_app_invite(tunnel_id, &request)
            .await
            .map_err(dp_err)?;
        let id = created.invite.id;
        let link = EnrollLink::new(&created.hostname, tunnel_id, created.code)
            .map_err(|_| invalid("DNSPilot returned an unusable invite"))?;
        Ok((link, id))
    }

    /// `GET /api/tunnels/{id}/app-invites`: newest first, all states.
    pub async fn invites(&self, tunnel_id: i64) -> Result<Vec<DnsPilotInvite>, ClientError> {
        let list = self
            .console()?
            .app_invites(tunnel_id)
            .await
            .map_err(dp_err)?;
        Ok(list
            .items
            .into_iter()
            .map(|i| DnsPilotInvite {
                id: i.id,
                label: i.label,
                status: i.status,
                expires_at: i.expires_at,
            })
            .collect())
    }

    /// `DELETE /api/tunnels/{id}/app-invites/{invite_id}`: an unused code
    /// stops working immediately.
    pub async fn revoke_invite(&self, tunnel_id: i64, invite_id: i64) -> Result<(), ClientError> {
        self.console()?
            .revoke_app_invite(tunnel_id, invite_id)
            .await
            .map_err(dp_err)
    }

    /// The invite for a new pairing code whose endpoints go through a
    /// DNSPilot tunnel. Never fails: when no invite can be made the caller
    /// shows a plain pairing code, with the reason as a note. The previous
    /// pairing invite is revoked first, so new codes do not pile up.
    pub async fn pairing_invite(&self, endpoints: &[String]) -> PairingInvite {
        let hosts: Vec<String> = endpoints.iter().filter_map(|e| url_host(e)).collect();
        if hosts.is_empty() {
            return PairingInvite::NotApplicable;
        }
        match self.pairing_invite_inner(&hosts).await {
            Ok(link) => PairingInvite::Included(link),
            Err(reason) => PairingInvite::Skipped { reason },
        }
    }

    async fn pairing_invite_inner(&self, hosts: &[String]) -> Result<EnrollLink, &'static str> {
        let reason = |e: ClientError| -> &'static str {
            tracing::info!("no DNSPilot invite for this pairing code: {e}");
            match e.code() {
                "dnspilot_not_connected" | "dnspilot_reauthorize" => "not_connected",
                "dnspilot_forbidden" | "dnspilot_tunnel_not_app_mode" => "not_owned",
                "dnspilot_rate_limited" => "rate_limited",
                "dnspilot_invite_limit" => "invite_limit",
                _ => "failed",
            }
        };
        let console = self.console().map_err(reason)?;
        let known = self
            .tunnels()
            .into_iter()
            .find(|t| hosts.contains(&t.hostname))
            .and_then(|t| t.tunnel_id);
        let tunnel_id = match known {
            Some(id) => id,
            None => console
                .tunnels()
                .await
                .map_err(|e| reason(dp_err(e)))?
                .items
                .iter()
                .find(|t| t.is_app_mode() && hosts.contains(&t.hostname))
                .map(|t| t.id)
                .ok_or("not_owned")?,
        };
        let previous = lock(&self.pairing_invite).take();
        if let Some((tunnel, invite)) = previous {
            if let Err(e) = console.revoke_app_invite(tunnel, invite).await {
                tracing::debug!("cannot revoke the previous pairing invite: {e}");
            }
        }
        let (link, id) = self
            .create_invite_inner(
                tunnel_id,
                Some(PAIRING_INVITE_LABEL),
                Some(PAIRING_INVITE_TTL_SECS),
            )
            .await
            .map_err(reason)?;
        *lock(&self.pairing_invite) = Some((tunnel_id, id));
        Ok(link)
    }

    /// Registers this app's certificate with an invite code (no DNSPilot
    /// login): signs `"dpt-app-enroll:v1:" + code` with the client key,
    /// posts to the configured console (a link cannot change where the code
    /// goes) and records the tunnel as registered. The tunnel's host name
    /// and id come from the console's answer and must match the link.
    ///
    /// Idempotent: the console accepts a key that is already registered,
    /// and a code this app used already is not an error for a tunnel it
    /// knows as registered.
    pub async fn enroll(&self, link: &EnrollLink) -> Result<DnsPilotEnrollment, ClientError> {
        let identity = DnsPilotIdentity::load_or_create(self.secrets.as_ref())?;
        let signature = identity.sign(&enroll_message(&link.code))?;
        let request = enroll_request(&link.code, identity.cert_der(), &signature);
        let result = match public_enroll(&self.oauth.console_url, &request).await {
            Ok(result) => result,
            Err(ProviderError::Coded { code, .. })
                if code == invite_errors::INVITE_INVALID && self.enrolled(link) =>
            {
                // The console cannot say how far along the Agent is, so
                // allow for the whole wait.
                return Ok(DnsPilotEnrollment {
                    hostname: link.hostname.clone(),
                    tunnel_id: link.tunnel_id,
                    client_id: None,
                    ready_within_sec: ENROLL_READY_DEFAULT_SECS,
                });
            }
            Err(e) => return Err(dp_err(e)),
        };
        let hostname = normalize_hostname(&result.hostname).map_err(dp_err)?;
        if hostname != link.hostname
            || result.tunnel_id != link.tunnel_id
            || !result
                .spki_sha256
                .eq_ignore_ascii_case(identity.spki_sha256())
        {
            return Err(invalid("DNSPilot's answer does not match this invite"));
        }
        let public_port = self
            .tunnels()
            .iter()
            .find(|t| t.hostname == hostname)
            .map_or(443, |t| t.public_port);
        self.add_tunnel(&hostname, public_port, Some(result.tunnel_id))?;
        self.mark_registered(&hostname)?;
        Ok(DnsPilotEnrollment {
            ready_within_sec: result.ready_within(),
            client_id: Some(result.client_id),
            tunnel_id: result.tunnel_id,
            hostname,
        })
    }

    /// [`enroll`](Self::enroll) for the text of a link: the console's QR
    /// link or a `vca://pair` link with an invite. `None` when a pairing
    /// link carries no invite. The error never quotes the link.
    pub async fn enroll_link(&self, raw: &str) -> Result<Option<DnsPilotEnrollment>, ClientError> {
        let link = EnrollLink::parse(raw).map_err(|_| ClientError::DnsPilot {
            code: "dnspilot_bad_link",
            message: "this is not a valid DNSPilot invite link".into(),
        })?;
        match link {
            Some(link) => self.enroll(&link).await.map(Some),
            None => Ok(None),
        }
    }

    /// This app already registered its key on the link's tunnel.
    fn enrolled(&self, link: &EnrollLink) -> bool {
        lock(&self.tunnels).iter().any(|t| {
            t.hostname == link.hostname
                && t.tunnel_id == Some(link.tunnel_id)
                && t.client_registered
        })
    }

    /// Advanced fallback, not a second factor: turns on "Allow app tokens"
    /// for a tunnel this app created, issues a `dpt_app_` token and stores
    /// it in the key store; the app then sends it on every connection to
    /// that tunnel. While tokens are allowed, a token **alone** opens the
    /// tunnel, so keep it off for VCA (the client certificate is the
    /// default) and use a short expiry.
    pub async fn issue_app_token(
        &self,
        hostname: &str,
        expires_days: u16,
    ) -> Result<DnsPilotTunnel, ClientError> {
        let tunnel = self
            .tunnels()
            .into_iter()
            .find(|t| t.hostname == hostname)
            .ok_or_else(|| ClientError::NotFound(hostname.to_owned()))?;
        let tunnel_id = tunnel
            .tunnel_id
            .ok_or_else(|| invalid("enter the tunnel id first"))?;
        let console = self.console()?;
        console
            .set_app_token_allowed(tunnel_id, true)
            .await
            .map_err(dp_err)?;
        let token = console
            .issue_app_token(
                tunnel_id,
                &IssueAppToken {
                    expires_days: Some(expires_days.clamp(1, 365)),
                    label: Some(CLIENT_LABEL.into()),
                },
            )
            .await
            .map_err(dp_err)?;
        let secret = token
            .token
            .ok_or_else(|| invalid("DNSPilot returned no token"))?;
        self.secrets
            .save(&app_token_name(hostname), secret.expose().as_bytes())?;
        self.update_tunnel(hostname, |t| t.has_app_token = true)
    }

    /// Disconnects VCA from the DNSPilot account (revokes the grant: the
    /// tunnels it created are disabled, its app clients and tokens revoked)
    /// and deletes the stored tokens.
    pub async fn disconnect(&self) -> Result<(), ClientError> {
        if self.tokens()?.is_some() {
            let console = self.console()?;
            if let Err(e) = console.disconnect().await {
                tracing::warn!("revoking the DNSPilot authorization failed: {e}");
            }
        }
        self.secrets.delete(OAUTH_TOKENS_NAME)?;
        self.keeper.invalidate().await;
        lock(&self.pending).take();
        Ok(())
    }

    /// Transport options for one endpoint: the client certificate (and app
    /// token) only for known DNSPilot tunnel hosts.
    pub(crate) fn transport_for(
        &self,
        url: &str,
        extra_root_certs: &[Vec<u8>],
    ) -> Result<TransportOptions, ClientError> {
        let mut options = TransportOptions {
            extra_root_certs: if url.starts_with("wss://") {
                extra_root_certs.to_vec()
            } else {
                Vec::new()
            },
            ..TransportOptions::default()
        };
        let Some(host) = url_host(url) else {
            return Ok(options);
        };
        let known = lock(&self.tunnels).iter().any(|t| t.hostname == host);
        if !known {
            return Ok(options);
        }
        let identity = DnsPilotIdentity::load_or_create(self.secrets.as_ref())?;
        options.client_identity = Some(identity.to_tls());
        if let Some(token) = self.secrets.load(&app_token_name(&host))? {
            let token = String::from_utf8(token.to_vec())
                .map_err(|_| ClientError::Storage("the stored app token is corrupt".into()))?;
            options.bearer = Some(Zeroizing::new(token));
        }
        Ok(options)
    }
}

/// Console settings used by the hub (default: the public DNSPilot console
/// and the registered client `vca`).
pub fn default_oauth_config() -> OAuthConfig {
    OAuthConfig {
        console_url: DEFAULT_CONSOLE_URL.into(),
        ..OAuthConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_net::MemoryKeyStore;

    #[test]
    fn identity_is_created_once_and_pinned_by_spki() {
        let store = MemoryKeyStore::default();
        let a = DnsPilotIdentity::load_or_create(&store).unwrap();
        let b = DnsPilotIdentity::load_or_create(&store).unwrap();
        assert_eq!(a.spki_sha256(), b.spki_sha256());
        assert_eq!(a.spki_sha256().len(), 64);
        let pem_pin = ClientKey::from_pem(&a.info().public_key_pem)
            .unwrap()
            .sha256_hex()
            .unwrap();
        assert_eq!(pem_pin, a.spki_sha256());
        assert!(!format!("{a:?}").contains("BEGIN"));
    }

    #[test]
    fn hosts_are_parsed_from_urls() {
        assert_eq!(
            url_host("wss://A.example.com/v1/ws").as_deref(),
            Some("a.example.com")
        );
        assert_eq!(
            url_host("wss://a.example.com:9443/v1/ws").as_deref(),
            Some("a.example.com")
        );
        assert_eq!(url_host("ws://a.example.com/v1/ws"), None);
        assert_eq!(
            app_token_name("a.example.com").len(),
            "dnspilot-apptoken-".len() + 16
        );
    }

    #[test]
    fn certificates_go_only_to_known_tunnels() {
        let dir = tempfile::tempdir().unwrap();
        let m = DnsPilotManager::open(
            dir.path().to_path_buf(),
            Arc::new(MemoryKeyStore::default()),
            default_oauth_config(),
        )
        .unwrap();
        assert!(
            m.transport_for("wss://vca-a.example.com/v1/ws", &[])
                .unwrap()
                .is_default()
        );
        m.add_tunnel("https://VCA-a.example.com/", 443, Some(12))
            .unwrap();
        let t = m
            .transport_for("wss://vca-a.example.com/v1/ws", &[])
            .unwrap();
        assert!(t.client_identity.is_some() && t.bearer.is_none());
        assert!(
            m.transport_for("wss://other.example.com/v1/ws", &[])
                .unwrap()
                .is_default()
        );
        assert!(
            m.transport_for("ws://vca-a.example.com/v1/ws", &[])
                .unwrap()
                .is_default()
        );
        // Persisted.
        let again = DnsPilotManager::open(
            dir.path().to_path_buf(),
            Arc::new(MemoryKeyStore::default()),
            default_oauth_config(),
        )
        .unwrap();
        assert_eq!(again.tunnels()[0].tunnel_id, Some(12));
        assert_eq!(again.tunnels()[0].ws_url(), "wss://vca-a.example.com/v1/ws");
        m.remove_tunnel("vca-a.example.com").unwrap();
        assert!(m.tunnels().is_empty());
        assert!(m.add_tunnel("vca-a.example.com", 8443, None).is_err());
    }
}
