//! Approval alerts on a phone whose VCA app is closed, through DNSPilot's
//! end-to-end encrypted push relay ([`vca_providers::dnspilot::push`]).
//!
//! Who does what:
//!
//! * **Phone**: gets an APNs or FCM token natively, keeps an X25519 key
//!   ([`phone_public_key`]) and hands token and public key to vcad over the
//!   Noise channel ([`register_envelope`]). A phone never has a DNSPilot login.
//! * **vcad**: knows the phones and the pending decisions, holds no OAuth
//!   token, and is never a sender.
//! * **Desktop app**: holds the OAuth grant and is the only sender. Its
//!   watcher feeds [`PushManager::tick`] with what vcad reports. While the
//!   desktop app is closed nothing is sent; a decision that is still pending
//!   when it opens again is sent then.
//!
//! The grant is separate from the tunnel grant ([`PUSH_OAUTH_TOKENS_NAME`],
//! scopes `push:register push:send` only). Refresh tokens rotate and reusing
//! an old one revokes the whole grant, so no other process shares it, and
//! alerts can be connected or disconnected without touching the tunnels.
//!
//! Privacy: the visible text is fixed by DNSPilot. The only thing VCA adds is
//! [`PushPayload`], a session reference sealed to the phone's key; prompts,
//! commands, paths and conversation text never reach this module.
//! `Noise_N_25519_ChaChaPoly_SHA256` is the one-way Noise pattern with the
//! primitives of the VCA channel. It does not prove who sent a payload, and
//! needs not: a payload can only open a session view, nothing is approved
//! from a notification.
//!
//! Alerts are off until the owner enables them ([`PushManager::set_enabled`]);
//! until then no phone token leaves this PC. Raw device tokens are not kept
//! here, only their fingerprints: vcad reports the tokens again on each sync.
//!
//! One push per decision: a phone's `(session, decision)` goes into a ledger
//! on disk after DNSPilot accepted it, and retries of the same push share a
//! collapse key.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use vca_core::Envelope;
use vca_net::{KeyStore, PublicKey, StaticKeypair, load_or_generate_keypair};
use vca_providers::dnspilot::console::ConsoleClient;
use vca_providers::dnspilot::contract::DEFAULT_CONSOLE_URL;
use vca_providers::dnspilot::keeper::{LockFile, TokenKeeper};
use vca_providers::dnspilot::oauth::{
    DeviceAuthorization, DeviceFlowClient, OAuthConfig, TokenSet,
};
pub use vca_providers::dnspilot::push::PushPlatform;
use vca_providers::dnspilot::push::{
    PUSH_SCOPES, PushCiphertext, PushSend, RegisterPushDevice, decode_base64,
};
use vca_providers::dnspilot::spki_sha256_hex;
use vca_providers::{ProviderError, Secret};
use zeroize::Zeroizing;

use crate::dnspilot::{KeyStoreTokens, dp_err, oauth_err};
use crate::error::ClientError;
use crate::store;

/// Key-store name of the push grant's tokens (JSON).
pub const PUSH_OAUTH_TOKENS_NAME: &str = "dnspilot-push-oauth";
/// Key-store name of a phone's X25519 key.
pub const PUSH_KEY_NAME: &str = "push-e2e-key";
/// Non-secret state: the opt-in, phones and the sent ledger.
pub const PUSH_FILE: &str = "push.json";
/// Cross-process lock around refreshing the push grant's tokens.
pub const PUSH_OAUTH_LOCK_FILE: &str = "dnspilot-push-oauth.lock";
/// DNSPilot's `app_id` for VCA, as DNSPilot confirmed it. Registration answers
/// `409` until a DNSPilot platform admin configures this app (APNs key, FCM
/// credentials); that is reported as `push_not_configured`.
pub const PUSH_APP_ID: Option<&str> = Some("io.metavu.voicecommandai.mobile");

/// Key of the ciphertext in a delivered notification (iOS: top-level key of
/// the APNs payload; Android: a key of the FCM `data`).
///
/// PENDING DOCUMENTATION: this comes from DNSPilot's console code, not from
/// the public OpenAPI spec. Nothing else may rely on it until DNSPilot
/// documents it; a payload without it still opens the app (no session).
pub const PUSH_CIPHERTEXT_KEY_PENDING_DOCS: &str = "c";

/// The ciphertext of a delivered notification's payload (see
/// [`PUSH_CIPHERTEXT_KEY_PENDING_DOCS`]); `None` when it is absent.
pub fn ciphertext_from_notification(payload: &serde_json::Value) -> Option<String> {
    let found = payload
        .get(PUSH_CIPHERTEXT_KEY_PENDING_DOCS)
        .or_else(|| payload.get("data")?.get(PUSH_CIPHERTEXT_KEY_PENDING_DOCS))?;
    found
        .as_str()
        .filter(|c| !c.is_empty() && c.len() <= 8192)
        .map(str::to_owned)
}

const FILE_VERSION: u32 = 1;
const NOISE_PATTERN: &str = "Noise_N_25519_ChaChaPoly_SHA256";
const NOISE_PROLOGUE: &[u8] = b"VCA push v1";
/// Noise `e` plus two AEAD tags.
const SEAL_OVERHEAD: usize = 32 + 16 + 16;
/// Pause after a failed DNSPilot call. The watcher ticks every few seconds and
/// the failure usually lasts (a revoked grant, a missing role).
const FAILURE_BACKOFF: Duration = Duration::from_secs(30);
/// Pause after DNSPilot said the app is not configured for push (`409`): that
/// takes a DNSPilot admin, not a retry.
const NOT_CONFIGURED_BACKOFF: Duration = Duration::from_secs(10 * 60);
const LEDGER_MAX: usize = 1024;
const LEDGER_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Length of the collapse key and the ledger keys, in hex characters.
const SHORT_HASH: usize = 32;

const PAYLOAD_VERSION: u32 = 1;
/// `last_error` of a phone whose token DNSPilot marked as invalid.
const TOKEN_INVALID: &str = "push_token_invalid";
/// `last_error` of a phone DNSPilot refused with `409`: no push configuration
/// for [`PUSH_APP_ID`] yet.
pub const NOT_CONFIGURED: &str = "push_not_configured";

/// What a push carries, sealed to the phone's key: a reference and nothing
/// else. The app fetches the details over the Noise channel after opening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushPayload {
    pub v: u32,
    /// Machine id as the phone knows it.
    pub m: String,
    /// Session id.
    pub s: String,
    /// Decision id.
    pub d: String,
}

/// A session waiting on a decision. Ids only, by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingAlert {
    pub session_id: String,
    pub decision_id: String,
}

/// The pending decisions of a `sessions` message, and the questions agents
/// ended their turn with (`question`, as `q:<id>`), read as JSON so a newer
/// server's fields cannot break the watcher. Prompts and question texts are
/// not read.
pub fn pending_alerts(sessions_body: &serde_json::Value) -> Vec<PendingAlert> {
    let Some(sessions) = sessions_body.get("sessions").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    sessions
        .iter()
        .filter_map(|s| {
            let id = s.get("id")?.as_str()?;
            let decision = s
                .get("pending_decision")
                .and_then(|p| p.get("decision"))
                .and_then(|d| d.get("id"))
                .and_then(|d| d.as_str())
                .map(str::to_owned);
            let question = || {
                let q = s.get("question")?.get("id")?.as_str()?;
                Some(format!("q:{q}"))
            };
            Some(PendingAlert {
                session_id: id.to_owned(),
                decision_id: decision.or_else(question)?,
            })
        })
        .collect()
}

/// A phone as vcad reports it. `Debug` hides the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneRegistration {
    /// The id vcad gave the paired phone.
    pub phone: String,
    /// The machine id the phone knows this PC by.
    pub machine: String,
    pub platform: PushPlatform,
    pub token: Secret,
    /// The phone's public key, see [`phone_public_key`].
    pub e2e_pubkey: String,
    #[serde(default)]
    pub locale: Option<String>,
}

/// The phones in a `push_phones` message. Malformed entries are skipped.
pub fn phones_from_body(body: &serde_json::Value) -> Vec<PhoneRegistration> {
    body.get("phones")
        .and_then(|p| p.as_array())
        .map(|phones| {
            phones
                .iter()
                .filter_map(|p| serde_json::from_value(p.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// What a phone sends to vcad. Mirrors the `ClientMsg` variants the server
/// adds for push; the phone id comes from the authenticated connection.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PhoneMsg<'a> {
    PushRegister {
        machine: &'a str,
        platform: PushPlatform,
        token: &'a str,
        e2e_pubkey: &'a str,
        locale: Option<&'a str>,
    },
    PushUnregister,
}

/// The MessagePack `push_register` envelope for `ClientHub::send`.
pub fn register_envelope(
    id: u64,
    machine: &str,
    platform: PushPlatform,
    token: &str,
    e2e_pubkey: &str,
    locale: Option<&str>,
) -> Result<Vec<u8>, ClientError> {
    encode_envelope(
        id,
        &PhoneMsg::PushRegister {
            machine,
            platform,
            token,
            e2e_pubkey,
            locale,
        },
    )
}

/// The MessagePack `push_unregister` envelope for `ClientHub::send`.
pub fn unregister_envelope(id: u64) -> Result<Vec<u8>, ClientError> {
    encode_envelope(id, &PhoneMsg::PushUnregister)
}

fn encode_envelope(id: u64, body: &PhoneMsg<'_>) -> Result<Vec<u8>, ClientError> {
    rmp_serde::to_vec_named(&Envelope { id, body })
        .map_err(|_| ClientError::Protocol("cannot encode the push message".into()))
}

/// This phone's public key, created on first use. The private half stays in
/// the key store.
pub fn phone_public_key(secrets: &dyn KeyStore) -> Result<String, ClientError> {
    Ok(load_or_generate_keypair(secrets, PUSH_KEY_NAME)?
        .public()
        .to_base64url())
}

fn noise_params() -> Result<snow::params::NoiseParams, ClientError> {
    NOISE_PATTERN
        .parse()
        .map_err(|_| ClientError::Internal("bad push cipher".into()))
}

/// Seals `payload` to the phone's public key.
pub fn seal(e2e_pubkey: &str, payload: &PushPayload) -> Result<Vec<u8>, ClientError> {
    let internal = |_| ClientError::Internal("cannot seal the push payload".into());
    let key = PublicKey::from_base64url(e2e_pubkey).map_err(|_| ClientError::DnsPilot {
        code: "push_bad_key",
        message: "the phone's push key is not usable".into(),
    })?;
    let plain = serde_json::to_vec(payload).map_err(ClientError::storage)?;
    let mut hs = snow::Builder::new(noise_params()?)
        .prologue(NOISE_PROLOGUE)
        .and_then(|b| b.remote_public_key(key.as_bytes()))
        .and_then(|b| b.build_initiator())
        .map_err(internal)?;
    let mut out = vec![0; plain.len() + SEAL_OVERHEAD];
    let n = hs.write_message(&plain, &mut out).map_err(internal)?;
    out.truncate(n);
    Ok(out)
}

/// Opens base64 ciphertext with this phone's key. `None` for anything that
/// was not sealed to it or is not a current payload.
pub fn open_sealed(secrets: &dyn KeyStore, ciphertext: &str) -> Option<PushPayload> {
    let sealed = decode_base64(ciphertext)?;
    let keypair = load_or_generate_keypair(secrets, PUSH_KEY_NAME).ok()?;
    open_with(&keypair, &sealed)
}

fn open_with(keypair: &StaticKeypair, sealed: &[u8]) -> Option<PushPayload> {
    let mut hs = snow::Builder::new(noise_params().ok()?)
        .prologue(NOISE_PROLOGUE)
        .and_then(|b| b.local_private_key(keypair.secret_bytes()))
        .and_then(|b| b.build_responder())
        .ok()?;
    let mut plain = Zeroizing::new(vec![0; sealed.len()]);
    let n = hs.read_message(sealed, &mut plain).ok()?;
    let payload: PushPayload = serde_json::from_slice(&plain[..n]).ok()?;
    (payload.v == PAYLOAD_VERSION).then_some(payload)
}

/// Push settings of a [`PushManager`].
#[derive(Debug, Clone)]
pub struct PushConfig {
    pub oauth: OAuthConfig,
    /// DNSPilot's `app_id`; `None` keeps alerts unavailable.
    pub app_id: Option<String>,
    /// Pause after a failed DNSPilot call.
    pub failure_backoff: Duration,
    /// Pause after DNSPilot answered that the app has no push configuration.
    pub not_configured_backoff: Duration,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            oauth: OAuthConfig {
                console_url: DEFAULT_CONSOLE_URL.into(),
                scopes: PUSH_SCOPES.iter().map(|s| (*s).to_owned()).collect(),
                ..OAuthConfig::default()
            },
            app_id: PUSH_APP_ID.map(str::to_owned),
            failure_backoff: FAILURE_BACKOFF,
            not_configured_backoff: NOT_CONFIGURED_BACKOFF,
        }
    }
}

/// A phone in the owner's alert settings (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushPhoneInfo {
    /// The id vcad gave the paired phone (matches the paired device list).
    pub phone: String,
    pub platform: PushPlatform,
    pub muted: bool,
    /// DNSPilot knows this phone.
    pub registered: bool,
    /// Stable code of the last failure, cleared by the next success.
    pub last_error: Option<String>,
}

/// State of approval alerts, for the UI (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushStatus {
    /// The owner turned alerts on.
    pub enabled: bool,
    /// This build knows DNSPilot's `app_id`.
    pub available: bool,
    /// The push grant is stored and has both scopes.
    pub authorized: bool,
    /// An approval is waiting in DNSPilot.
    pub pending_approval: bool,
    /// DNSPilot refused a registration with `409`: push is not configured
    /// for this app on DNSPilot's side yet.
    pub not_configured: bool,
    pub phones: Vec<PushPhoneInfo>,
}

/// The prompt shown while the person approves alerts in DNSPilot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushAuthorizationPrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
}

/// What one [`PushManager::notify`] did, phone by phone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotifyOutcome {
    pub sent: usize,
    /// Already sent for this decision.
    pub deduped: usize,
    pub muted: usize,
    pub failed: usize,
    /// A rate limit is in force; retried on a later tick.
    pub deferred: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PhoneRecord {
    phone: String,
    machine: String,
    platform: PushPlatform,
    e2e_pubkey: String,
    /// Hash of the device token, to notice a new one.
    token_hash: String,
    /// DNSPilot's device id; `None` until registered, or once it is gone.
    #[serde(default)]
    device_id: Option<i64>,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    last_error: Option<String>,
}

impl PhoneRecord {
    /// Nothing to do at DNSPilot for this token and key: the phone is
    /// registered, or DNSPilot called this token invalid already.
    fn settled(&self, token_hash: &str, e2e_pubkey: &str) -> bool {
        self.token_hash == token_hash
            && self.e2e_pubkey == e2e_pubkey
            && (self.device_id.is_some() || self.last_error.as_deref() == Some(TOKEN_INVALID))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SentEntry {
    key: String,
    /// Unix milliseconds.
    at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PushFile {
    version: u32,
    enabled: bool,
    #[serde(default)]
    phones: Vec<PhoneRecord>,
    #[serde(default)]
    sent: Vec<SentEntry>,
}

impl Default for PushFile {
    fn default() -> Self {
        Self {
            version: FILE_VERSION,
            enabled: false,
            phones: Vec::new(),
            sent: Vec::new(),
        }
    }
}

/// Anything unreadable counts as "alerts off".
fn load_file(dir: &Path) -> PushFile {
    let Ok(bytes) = std::fs::read(dir.join(PUSH_FILE)) else {
        return PushFile::default();
    };
    match serde_json::from_slice::<PushFile>(&bytes) {
        Ok(file) if file.version == FILE_VERSION => file,
        _ => {
            tracing::warn!("ignoring an unreadable {PUSH_FILE}; alerts stay off");
            PushFile::default()
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn short_hash(parts: &[&str]) -> String {
    let mut hash = spki_sha256_hex(parts.join("\0").as_bytes());
    hash.truncate(SHORT_HASH);
    hash
}

/// Registered phones and alert delivery of the desktop app.
pub struct PushManager {
    dir: PathBuf,
    secrets: Arc<dyn KeyStore>,
    config: PushConfig,
    state: Mutex<PushFile>,
    pending: Mutex<Option<DeviceAuthorization>>,
    /// No sends before this moment (from `Retry-After`).
    retry_at: Mutex<Option<Instant>>,
    /// Serializes everything that talks to DNSPilot.
    busy: tokio::sync::Mutex<()>,
    /// The only refresher of the push grant's tokens in this process.
    keeper: Arc<TokenKeeper>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn error_code(e: &ProviderError) -> &'static str {
    match e {
        ProviderError::Unauthorized(_) | ProviderError::NotSignedIn(_) => "push_reauthorize",
        ProviderError::Forbidden(_) => "push_forbidden",
        ProviderError::Conflict(_) => "push_conflict",
        ProviderError::Io(_) | ProviderError::Timeout(_) => "push_unavailable",
        _ => "push_failed",
    }
}

impl PushManager {
    /// `dir` is the app's data directory for push state.
    pub fn open(dir: PathBuf, secrets: Arc<dyn KeyStore>, config: PushConfig) -> Self {
        let state = Mutex::new(load_file(&dir));
        let keeper = Arc::new(TokenKeeper::new(
            DeviceFlowClient::new(config.oauth.clone()),
            Arc::new(KeyStoreTokens::new(
                Arc::clone(&secrets),
                PUSH_OAUTH_TOKENS_NAME,
                LockFile::new(dir.join(PUSH_OAUTH_LOCK_FILE)),
            )),
        ));
        Self {
            keeper,
            dir,
            secrets,
            config,
            state,
            pending: Mutex::new(None),
            retry_at: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
        }
    }

    fn save(&self, file: &PushFile) -> Result<(), ClientError> {
        let data = serde_json::to_vec_pretty(file).map_err(ClientError::storage)?;
        store::write_private(&self.dir, PUSH_FILE, &data)
    }

    /// Applies `f` to a copy and keeps it once it is on disk.
    fn update<T>(&self, f: impl FnOnce(&mut PushFile) -> T) -> Result<T, ClientError> {
        let mut state = lock(&self.state);
        let mut next = state.clone();
        let out = f(&mut next);
        self.save(&next)?;
        *state = next;
        Ok(out)
    }

    fn tokens(&self) -> Result<Option<TokenSet>, ClientError> {
        match self.secrets.load(PUSH_OAUTH_TOKENS_NAME)? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| ClientError::Storage("the stored push tokens are corrupt".into())),
        }
    }

    fn save_tokens(secrets: &dyn KeyStore, tokens: &TokenSet) -> Result<(), ClientError> {
        let json = Zeroizing::new(serde_json::to_vec(tokens).map_err(ClientError::storage)?);
        Ok(secrets.save(PUSH_OAUTH_TOKENS_NAME, &json)?)
    }

    /// A console client on the shared keeper (refresh tokens rotate: never
    /// two refreshes with the same one).
    fn console(&self) -> Result<ConsoleClient, ClientError> {
        if self.tokens()?.is_none() {
            return Err(ClientError::DnsPilot {
                code: "push_not_authorized",
                message: "approve approval alerts in DNSPilot first".into(),
            });
        }
        Ok(ConsoleClient::with_keeper(Arc::clone(&self.keeper)))
    }

    fn authorized(&self) -> Result<bool, ClientError> {
        Ok(self
            .tokens()?
            .is_some_and(|t| PUSH_SCOPES.iter().all(|s| t.has_scope(s))))
    }

    /// True once the owner turned alerts on. The default is off.
    pub fn enabled(&self) -> bool {
        lock(&self.state).enabled
    }

    pub fn status(&self) -> Result<PushStatus, ClientError> {
        let authorized = self.authorized()?;
        let state = lock(&self.state);
        Ok(PushStatus {
            enabled: state.enabled,
            available: self.config.app_id.is_some(),
            authorized,
            pending_approval: lock(&self.pending).is_some(),
            not_configured: state
                .phones
                .iter()
                .any(|p| p.last_error.as_deref() == Some(NOT_CONFIGURED)),
            phones: state
                .phones
                .iter()
                .map(|p| PushPhoneInfo {
                    phone: p.phone.clone(),
                    platform: p.platform,
                    muted: p.muted,
                    registered: p.device_id.is_some(),
                    last_error: p.last_error.clone(),
                })
                .collect(),
        })
    }

    /// Starts the DNSPilot approval of the push grant. Show the user code;
    /// then call [`wait_for_authorization`](Self::wait_for_authorization).
    pub async fn start_authorization(&self) -> Result<PushAuthorizationPrompt, ClientError> {
        let auth = DeviceFlowClient::new(self.config.oauth.clone())
            .start()
            .await
            .map_err(oauth_err)?;
        let prompt = PushAuthorizationPrompt {
            user_code: auth.user_code.clone(),
            verification_uri: auth.verification_uri.clone(),
            verification_uri_complete: auth.verification_uri_complete.clone(),
            expires_in: auth.expires_in_secs,
        };
        *lock(&self.pending) = Some(auth);
        Ok(prompt)
    }

    /// The activation URL of the pending approval, only on the console.
    pub fn activation_url(&self) -> Option<String> {
        let pending = lock(&self.pending);
        let auth = pending.as_ref()?;
        let url = auth
            .verification_uri_complete
            .clone()
            .unwrap_or_else(|| auth.verification_uri.clone());
        let console = self.config.oauth.console_url.trim_end_matches('/');
        url.starts_with(&format!("{console}/")).then_some(url)
    }

    /// Waits for the person's answer and stores the tokens.
    pub async fn wait_for_authorization(&self) -> Result<PushStatus, ClientError> {
        let auth = lock(&self.pending)
            .clone()
            .ok_or_else(|| ClientError::DnsPilot {
                code: "dnspilot_expired",
                message: "no approval is in progress; start again".into(),
            })?;
        let result = DeviceFlowClient::new(self.config.oauth.clone())
            .poll(&auth)
            .await;
        {
            let mut pending = lock(&self.pending);
            if pending.as_ref().map(|p| &p.user_code) == Some(&auth.user_code) {
                *pending = None;
            }
        }
        let tokens = result.map_err(oauth_err)?;
        Self::save_tokens(self.secrets.as_ref(), &tokens)?;
        self.keeper.invalidate().await;
        self.status()
    }

    pub fn cancel_authorization(&self) {
        lock(&self.pending).take();
    }

    /// The owner's switch. Turning on needs this build's `app_id` and the
    /// grant; turning off also removes the phones from DNSPilot.
    pub async fn set_enabled(&self, enabled: bool) -> Result<PushStatus, ClientError> {
        let _busy = self.busy.lock().await;
        if enabled {
            if self.config.app_id.is_none() {
                return Err(ClientError::DnsPilot {
                    code: "push_not_configured",
                    message: "this build has no DNSPilot push configuration yet".into(),
                });
            }
            if !self.authorized()? {
                return Err(ClientError::DnsPilot {
                    code: "push_not_authorized",
                    message: "approve approval alerts in DNSPilot first".into(),
                });
            }
        } else {
            self.unregister_all().await;
        }
        self.update(|f| f.enabled = enabled)?;
        self.status()
    }

    /// Best effort: a device left behind is removed again with the grant.
    async fn unregister_all(&self) {
        let ids: Vec<_> = lock(&self.state)
            .phones
            .iter()
            .filter_map(|p| p.device_id.map(|id| (p.phone.clone(), id)))
            .collect();
        if ids.is_empty() {
            return;
        }
        let Ok(console) = self.console() else {
            return;
        };
        for (phone, id) in ids {
            match console.push_unregister(id).await {
                Ok(()) | Err(ProviderError::NotFound(_)) => {
                    let cleared = self.update(|f| {
                        if let Some(p) = f.phones.iter_mut().find(|p| p.phone == phone) {
                            p.device_id = None;
                        }
                    });
                    if let Err(e) = cleared {
                        tracing::warn!("cannot update the push phones: {e}");
                    }
                }
                Err(e) => tracing::warn!("cannot remove a phone from DNSPilot: {e}"),
            }
        }
    }

    /// Per-phone mute. Survives token changes and re-registration.
    pub fn set_muted(&self, phone: &str, muted: bool) -> Result<PushStatus, ClientError> {
        self.update(|f| {
            f.phones
                .iter_mut()
                .find(|p| p.phone == phone)
                .map(|p| p.muted = muted)
        })?
        .ok_or_else(|| ClientError::NotFound(phone.to_owned()))?;
        self.status()
    }

    /// Ends the push grant: removes the phones from DNSPilot, revokes the
    /// grant, deletes the tokens and turns alerts off. Tunnels are untouched.
    pub async fn disconnect(&self) -> Result<PushStatus, ClientError> {
        let _busy = self.busy.lock().await;
        self.unregister_all().await;
        if self.tokens()?.is_some() {
            if let Err(e) = self.console()?.disconnect().await {
                tracing::warn!("revoking the push authorization failed: {e}");
            }
        }
        self.secrets.delete(PUSH_OAUTH_TOKENS_NAME)?;
        self.keeper.invalidate().await;
        lock(&self.pending).take();
        self.update(|f| f.enabled = false)?;
        self.status()
    }

    /// Brings DNSPilot in line with vcad's phones: registers new ones, moves
    /// a phone whose token or key changed, and removes phones that are no
    /// longer paired. Nothing leaves this PC while alerts are off.
    pub async fn sync_phones(
        &self,
        phones: &[PhoneRegistration],
        agent_device_id: Option<i64>,
    ) -> Result<(), ClientError> {
        if !self.enabled() || self.deferred() {
            return Ok(());
        }
        let _busy = self.busy.lock().await;
        let app_id = self
            .config
            .app_id
            .clone()
            .ok_or_else(|| ClientError::DnsPilot {
                code: "push_not_configured",
                message: "this build has no DNSPilot push configuration yet".into(),
            })?;
        let hashed: Vec<String> = phones
            .iter()
            .map(|p| spki_sha256_hex(p.token.expose().as_bytes()))
            .collect();
        let state = lock(&self.state).phones.clone();
        let gone: Vec<PhoneRecord> = state
            .iter()
            .filter(|r| !phones.iter().any(|p| p.phone == r.phone))
            .cloned()
            .collect();
        let changed: Vec<(&PhoneRegistration, &String)> = phones
            .iter()
            .zip(&hashed)
            .filter(|(p, hash)| {
                !state
                    .iter()
                    .any(|r| r.phone == p.phone && r.settled(hash, &p.e2e_pubkey))
            })
            .collect();
        if gone.is_empty() && changed.is_empty() {
            return Ok(());
        }
        let console = self.console()?;

        for record in gone {
            if let Some(id) = record.device_id {
                if let Err(e) = console.push_unregister(id).await {
                    if !matches!(e, ProviderError::NotFound(_)) {
                        tracing::warn!("cannot remove an unpaired phone from DNSPilot: {e}");
                        continue;
                    }
                }
            }
            self.update(|f| f.phones.retain(|p| p.phone != record.phone))?;
        }

        for (phone, token_hash) in changed {
            let known = state.iter().find(|r| r.phone == phone.phone);
            if let Some(id) = known.and_then(|r| r.device_id) {
                if let Err(e) = console.push_unregister(id).await {
                    tracing::debug!("cannot remove a phone's old push device: {e}");
                }
            }
            let result = console
                .push_register(&RegisterPushDevice {
                    app_id: app_id.clone(),
                    platform: phone.platform,
                    token: phone.token.clone(),
                    agent_device_id,
                    e2e_pubkey: Some(phone.e2e_pubkey.clone()),
                    locale: phone.locale.clone(),
                })
                .await;
            let (device_id, last_error) = match result {
                Ok(device) if device.invalid_at.is_none() => (Some(device.id), None),
                Ok(_) => (None, Some(TOKEN_INVALID.to_owned())),
                Err(ProviderError::RateLimited { retry_after }) => {
                    self.defer(retry_after);
                    return Err(dp_err(ProviderError::RateLimited { retry_after }));
                }
                // DNSPilot: "push not configured for this app" until its
                // admin adds the APNs and FCM credentials.
                Err(ProviderError::Conflict(_)) => {
                    tracing::info!("DNSPilot has no push configuration for this app yet");
                    self.defer(Some(self.config.not_configured_backoff));
                    (None, Some(NOT_CONFIGURED.to_owned()))
                }
                Err(e) => {
                    tracing::info!("cannot register a phone for push: {e}");
                    self.defer(Some(self.config.failure_backoff));
                    (None, Some(error_code(&e).to_owned()))
                }
            };
            self.update(|f| {
                let record = PhoneRecord {
                    phone: phone.phone.clone(),
                    machine: phone.machine.clone(),
                    platform: phone.platform,
                    e2e_pubkey: phone.e2e_pubkey.clone(),
                    token_hash: token_hash.clone(),
                    device_id,
                    muted: known.is_some_and(|r| r.muted),
                    last_error: last_error.clone(),
                };
                match f.phones.iter_mut().find(|r| r.phone == phone.phone) {
                    Some(existing) => *existing = record,
                    None => f.phones.push(record),
                }
            })?;
        }
        Ok(())
    }

    fn defer(&self, retry_after: Option<Duration>) {
        // Without a `Retry-After`, the next tick tries again.
        if let Some(wait) = retry_after {
            *lock(&self.retry_at) = Some(Instant::now() + wait);
        }
    }

    fn deferred(&self) -> bool {
        lock(&self.retry_at).is_some_and(|at| Instant::now() < at)
    }

    /// Sends the alert to every unmuted registered phone that did not get it
    /// yet. A failed phone stays out of the ledger and is retried by the next
    /// tick for as long as the decision is pending.
    pub async fn notify(&self, alert: &PendingAlert) -> Result<NotifyOutcome, ClientError> {
        let mut outcome = NotifyOutcome::default();
        if !self.enabled() {
            return Ok(outcome);
        }
        if self.deferred() {
            outcome.deferred = true;
            return Ok(outcome);
        }
        let _busy = self.busy.lock().await;
        let phones = lock(&self.state).phones.clone();
        let mut console = None;
        for phone in phones {
            let Some(device_id) = phone.device_id else {
                continue;
            };
            let key = short_hash(&[&phone.phone, &alert.session_id, &alert.decision_id]);
            if lock(&self.state).sent.iter().any(|s| s.key == key) {
                outcome.deduped += 1;
                continue;
            }
            if phone.muted {
                outcome.muted += 1;
                continue;
            }
            if console.is_none() {
                console = Some(self.console()?);
            }
            let client = console.as_ref().ok_or(ClientError::NotConnected)?;
            match self.send_one(client, &phone, device_id, alert, &key).await {
                Ok(()) => {
                    outcome.sent += 1;
                    self.record_sent(&phone.phone, key)?;
                }
                Err(ProviderError::RateLimited { retry_after }) => {
                    self.defer(retry_after);
                    outcome.deferred = true;
                    break;
                }
                Err(ProviderError::NotFound(_)) => {
                    outcome.failed += 1;
                    self.record_failure(&phone.phone, "push_device_gone", true)?;
                }
                Err(e) => {
                    outcome.failed += 1;
                    tracing::info!("a push was not accepted: {e}");
                    self.record_failure(&phone.phone, error_code(&e), false)?;
                    self.defer(Some(self.config.failure_backoff));
                    break;
                }
            }
        }
        Ok(outcome)
    }

    async fn send_one(
        &self,
        console: &ConsoleClient,
        phone: &PhoneRecord,
        device_id: i64,
        alert: &PendingAlert,
        key: &str,
    ) -> Result<(), ProviderError> {
        let sealed = seal(
            &phone.e2e_pubkey,
            &PushPayload {
                v: PAYLOAD_VERSION,
                m: phone.machine.clone(),
                s: alert.session_id.clone(),
                d: alert.decision_id.clone(),
            },
        )
        .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        let request = PushSend::new(PushCiphertext::new(&sealed)?, device_id, key.to_owned());
        console.push_send(&request).await.map(|_| ())
    }

    fn record_sent(&self, phone: &str, key: String) -> Result<(), ClientError> {
        let now = now_millis();
        self.update(|f| {
            f.sent.retain(|s| now.saturating_sub(s.at) < LEDGER_AGE_MS);
            f.sent.push(SentEntry { key, at: now });
            if f.sent.len() > LEDGER_MAX {
                let extra = f.sent.len() - LEDGER_MAX;
                f.sent.drain(..extra);
            }
            if let Some(p) = f.phones.iter_mut().find(|p| p.phone == phone) {
                p.last_error = None;
            }
        })
    }

    fn record_failure(&self, phone: &str, code: &str, gone: bool) -> Result<(), ClientError> {
        self.update(|f| {
            if let Some(p) = f.phones.iter_mut().find(|p| p.phone == phone) {
                p.last_error = Some(code.to_owned());
                if gone {
                    // The next sync registers it again.
                    p.device_id = None;
                }
            }
        })
    }

    /// One watcher round: syncs the phones (when vcad reported them) and
    /// alerts on the pending decisions of a `sessions` message.
    pub async fn tick(
        &self,
        sessions_body: &serde_json::Value,
        phones: Option<&[PhoneRegistration]>,
        agent_device_id: Option<i64>,
    ) -> Result<NotifyOutcome, ClientError> {
        let mut total = NotifyOutcome::default();
        if !self.enabled() {
            return Ok(total);
        }
        if let Some(phones) = phones {
            self.sync_phones(phones, agent_device_id).await?;
        }
        for alert in pending_alerts(sessions_body) {
            let one = self.notify(&alert).await?;
            total.sent += one.sent;
            total.deduped += one.deduped;
            total.muted += one.muted;
            total.failed += one.failed;
            total.deferred |= one.deferred;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vca_net::MemoryKeyStore;

    fn payload() -> PushPayload {
        PushPayload {
            v: PAYLOAD_VERSION,
            m: "0123456789abcdef".into(),
            // Long enough that random ciphertext never contains it by chance.
            s: "session-plaintext-marker".into(),
            d: "d1".into(),
        }
    }

    #[test]
    fn a_sealed_payload_opens_only_with_the_phones_key() {
        let phone = MemoryKeyStore::default();
        let public = phone_public_key(&phone).unwrap();
        assert_eq!(phone_public_key(&phone).unwrap(), public);

        let sealed = seal(&public, &payload()).unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("session-plaintext-marker"));
        let text = vca_providers::dnspilot::push::encode_base64(&sealed);
        assert_eq!(open_sealed(&phone, &text), Some(payload()));

        let other = MemoryKeyStore::default();
        assert_eq!(open_sealed(&other, &text), None);
        assert_eq!(open_sealed(&phone, "not base64!"), None);
    }

    #[test]
    fn sealing_is_randomized() {
        let public = phone_public_key(&MemoryKeyStore::default()).unwrap();
        assert_ne!(
            seal(&public, &payload()).unwrap(),
            seal(&public, &payload()).unwrap()
        );
    }

    #[test]
    fn a_bad_phone_key_is_refused() {
        let e = seal("short", &payload()).unwrap_err();
        assert_eq!(e.code(), "push_bad_key");
    }

    #[test]
    fn only_ids_are_read_from_sessions() {
        let body = json!({"type": "sessions", "sessions": [
            {"id": "a", "pending_decision": {
                "decision": {"id": "d1", "prompt": "rm -rf /secret", "options": []}, "at": 1}},
            {"id": "b", "pending_decision": null},
            {"id": "c"},
            {"id": "e", "question": {"id": "17-1", "text": "deploy to prod?", "at": 1}},
        ]});
        assert_eq!(
            pending_alerts(&body),
            vec![
                PendingAlert {
                    session_id: "a".into(),
                    decision_id: "d1".into()
                },
                PendingAlert {
                    session_id: "e".into(),
                    decision_id: "q:17-1".into()
                }
            ]
        );
        assert!(pending_alerts(&json!({"type": "ok"})).is_empty());
    }

    #[test]
    fn phone_envelopes_have_the_client_message_shape() {
        let bytes = register_envelope(9, "m", PushPlatform::Ios, "tok", "key", Some("ko")).unwrap();
        let v: serde_json::Value = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(v["id"], 9);
        assert_eq!(v["body"]["type"], "push_register");
        assert_eq!(v["body"]["platform"], "ios");
        assert_eq!(v["body"]["token"], "tok");
        let bytes = unregister_envelope(10).unwrap();
        let v: serde_json::Value = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(v["body"]["type"], "push_unregister");
    }

    #[test]
    fn phone_lists_skip_malformed_entries_and_hide_tokens_in_debug() {
        let body = json!({"type": "push_phones", "phones": [
            {"phone": "p1", "machine": "m", "platform": "android",
             "token": "fcm-secret", "e2e_pubkey": "k"},
            {"phone": "p2"},
        ]});
        let phones = phones_from_body(&body);
        assert_eq!(phones.len(), 1);
        assert_eq!(phones[0].locale, None);
        assert!(!format!("{phones:?}").contains("fcm-secret"));
    }

    #[test]
    fn the_app_id_is_the_one_dnspilot_confirmed() {
        assert_eq!(
            PushConfig::default().app_id.as_deref(),
            Some("io.metavu.voicecommandai.mobile")
        );
        assert_eq!(
            PushConfig::default().oauth.scopes,
            vec!["push:register".to_owned(), "push:send".to_owned()]
        );
    }

    #[test]
    fn the_ciphertext_key_is_read_from_both_payload_shapes() {
        // iOS: top-level key; Android: FCM data.
        let ios = json!({"aps": {"alert": {"title": "DNSPilot"}}, "c": "AAAA"});
        let android = json!({"data": {"c": "BBBB"}});
        assert_eq!(ciphertext_from_notification(&ios).as_deref(), Some("AAAA"));
        assert_eq!(
            ciphertext_from_notification(&android).as_deref(),
            Some("BBBB")
        );
        assert_eq!(ciphertext_from_notification(&json!({"c": 1})), None);
        assert_eq!(ciphertext_from_notification(&json!({})), None);
    }

    #[test]
    fn an_unreadable_file_means_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PUSH_FILE), b"{ not json").unwrap();
        assert!(!load_file(dir.path()).enabled);
    }
}
