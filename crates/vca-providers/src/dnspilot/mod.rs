//! DNSPilot provider (<https://dnspilot.kr>).
//!
//! Paired devices reach vcad through a DNSPilot tunnel in **`app` access
//! mode** served by the DNSPilot Agent on the user's PC:
//!
//! ```text
//! phone ──wss://<name>.<zone> (TLS, SNI)──▶ DNSPilot relay (passthrough)
//!        ──▶ Agent on the PC (ends TLS, checks the client key) ──▶ http://127.0.0.1:47800
//! ```
//!
//! During the TLS handshake the Agent requires a client certificate whose
//! SubjectPublicKeyInfo SHA-256 is on the tunnel's "App clients" list;
//! unknown keys fail before any HTTP reaches vcad. vcad keeps listening on
//! loopback only, and VCA's own pairing, TOTP and Noise channel apply inside
//! the tunnel exactly as on every other transport (the tunnel is untrusted).
//!
//! Who does what:
//!
//! - the **VCA app** creates the tunnel and registers its client key with
//!   the console API and an OAuth token from the device flow ([`oauth`],
//!   [`console`]), or the person does both in the console. The app's
//!   [`keeper::TokenKeeper`] is the only refresher of that grant's tokens;
//!   vcad and the CLI never hold or refresh them;
//! - a **phone** registers its own key with a single-use invite code from
//!   the signed-in PC ([`invite`]), without a DNSPilot login;
//! - **vcad** ([`DnsPilotProvider`]) only reads the local Agent's status
//!   ([`local`]) and writes the tunnel's `wss://` address into pairing codes.
//!   It never creates, changes or stops tunnels (the Agent's own
//!   `POST /v1/tunnels` is off by default and needs an organization admin's
//!   per-device approval).
//!
//! Only documented DNSPilot APIs are used; see [`contract`].

pub mod console;
pub mod contract;
pub mod events;
pub mod invite;
pub mod keeper;
pub mod local;
pub mod oauth;
pub mod push;
pub mod sse;
#[cfg(windows)]
mod win;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rand::Rng;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use self::contract::{AgentStatus, RegisterAppClient};
use self::local::LocalApiClient;
use crate::error::{ProviderError, Result};
use crate::provider::{
    ConnectivityProvider, Detection, Diagnostic, Endpoint, EndpointKind, ProviderId, ProviderStatus,
};

/// Length of the random part of a hostname label.
const LABEL_RANDOM_LEN: usize = 10;

/// Generates a random, non-identifying tunnel name such as `vca-k3x9q2ab4m`.
/// Names never contain user, machine or project names, because every
/// tunnel hostname appears in public Certificate Transparency logs.
pub fn random_hostname_label() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    let suffix: String = (0..LABEL_RANDOM_LEN)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect();
    format!("vca-{suffix}")
}

/// The pin DNSPilot compares: SHA-256 of the DER SubjectPublicKeyInfo as 64
/// lowercase hex characters.
pub fn spki_sha256_hex(spki_der: &[u8]) -> String {
    Sha256::digest(spki_der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// True for 64 hex characters.
pub fn is_spki_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// An app client key to register with a tunnel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientKey {
    /// 64 hex characters (SPKI SHA-256).
    SpkiSha256(String),
    /// A `PUBLIC KEY` or `CERTIFICATE` PEM block.
    Pem(String),
}

impl ClientKey {
    pub fn from_spki_der(der: &[u8]) -> Self {
        Self::SpkiSha256(spki_sha256_hex(der))
    }

    /// Accepts a 64-hex fingerprint.
    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex = hex.trim().to_ascii_lowercase();
        if !is_spki_sha256_hex(&hex) {
            return Err(ProviderError::InvalidConfig(
                "an SPKI SHA-256 fingerprint has 64 hex characters".into(),
            ));
        }
        Ok(Self::SpkiSha256(hex))
    }

    /// Accepts a PEM `PUBLIC KEY` block (validated, sent as PEM).
    pub fn from_pem(pem: &str) -> Result<Self> {
        spki_der_from_pem(pem)?;
        Ok(Self::Pem(pem.trim().to_owned()))
    }

    /// The SPKI SHA-256 pin of this key (64 hex characters).
    pub fn sha256_hex(&self) -> Result<String> {
        match self {
            Self::SpkiSha256(h) => Ok(h.clone()),
            Self::Pem(p) => Ok(spki_sha256_hex(&spki_der_from_pem(p)?)),
        }
    }

    /// The registration request (`label`: at most 64 characters).
    pub fn to_request(&self, label: Option<String>) -> RegisterAppClient {
        let label = label.map(|l| l.chars().take(64).collect());
        match self {
            Self::SpkiSha256(h) => RegisterAppClient {
                spki_sha256: Some(h.clone()),
                public_key_pem: None,
                label,
            },
            Self::Pem(p) => RegisterAppClient {
                spki_sha256: None,
                public_key_pem: Some(p.clone()),
                label,
            },
        }
    }
}

/// Decodes the DER body of a `-----BEGIN PUBLIC KEY-----` block.
pub fn spki_der_from_pem(pem: &str) -> Result<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN PUBLIC KEY-----";
    const END: &str = "-----END PUBLIC KEY-----";
    let start = pem
        .find(BEGIN)
        .ok_or_else(|| ProviderError::InvalidConfig("not a PEM PUBLIC KEY block".into()))?
        + BEGIN.len();
    let end = pem[start..]
        .find(END)
        .ok_or_else(|| ProviderError::InvalidConfig("unterminated PEM block".into()))?
        + start;
    let b64: String = pem[start..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let der = B64
        .decode(b64)
        .map_err(|e| ProviderError::InvalidConfig(format!("invalid PEM base64: {e}")))?;
    // A SubjectPublicKeyInfo is a DER SEQUENCE.
    if der.first() != Some(&0x30) {
        return Err(ProviderError::InvalidConfig(
            "PEM body is not a DER SEQUENCE".into(),
        ));
    }
    Ok(der)
}

/// Normalizes and validates a tunnel hostname (`name.zone`, no scheme, port
/// or path).
pub fn normalize_hostname(input: &str) -> Result<String> {
    let mut h = input.trim().to_ascii_lowercase();
    for prefix in ["https://", "wss://"] {
        if let Some(rest) = h.strip_prefix(prefix) {
            h = rest.to_owned();
        }
    }
    let h = h.trim_end_matches('/').trim_end_matches('.').to_owned();
    let bad = || {
        ProviderError::InvalidConfig(format!(
            "{input:?} is not a tunnel hostname (expected something like vca-k3x9q2ab4m.example.com)"
        ))
    };
    if h.len() > 253 || !h.contains('.') {
        return Err(bad());
    }
    for label in h.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !ok {
            return Err(bad());
        }
    }
    Ok(h)
}

/// The `wss://` base URL of a tunnel (public port 443 or 9443).
pub fn tunnel_url(hostname: &str, public_port: u16) -> String {
    if public_port == 443 {
        format!("wss://{hostname}")
    } else {
        format!("wss://{hostname}:{public_port}")
    }
}

/// vcad's DNSPilot settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsPilotSettings {
    /// Public hostname of the app-mode tunnel that forwards to vcad's
    /// loopback port (validated by [`normalize_hostname`]).
    pub hostname: Option<String>,
    /// 443 (default) or 9443.
    pub public_port: u16,
}

impl Default for DnsPilotSettings {
    fn default() -> Self {
        Self {
            hostname: None,
            public_port: 443,
        }
    }
}

/// vcad's side of DNSPilot: reports the app-mode tunnel's address for
/// pairing codes after checking the local Agent's documented status.
#[derive(Debug)]
pub struct DnsPilotProvider {
    local: LocalApiClient,
    settings: DnsPilotSettings,
    endpoint: Mutex<Option<Endpoint>>,
    /// The configured or discovered tunnel hostname in use.
    effective: Mutex<Option<String>>,
    local_port: Mutex<Option<u16>>,
}

impl DnsPilotProvider {
    pub fn new(local: LocalApiClient, settings: DnsPilotSettings) -> Result<Self> {
        if !matches!(settings.public_port, 443 | 9443) {
            return Err(ProviderError::InvalidConfig(
                "DNSPilot tunnels use public port 443 or 9443".into(),
            ));
        }
        let hostname = settings
            .hostname
            .as_deref()
            .map(normalize_hostname)
            .transpose()?;
        Ok(Self {
            local,
            settings: DnsPilotSettings {
                hostname,
                ..settings
            },
            endpoint: Mutex::new(None),
            effective: Mutex::new(None),
            local_port: Mutex::new(None),
        })
    }

    pub fn local(&self) -> &LocalApiClient {
        &self.local
    }

    /// Finds the app-mode tunnel this PC's Agent serves for `local_port`
    /// (`GET /v1/tunnels`), when no hostname is configured.
    async fn discover(&self, local_port: u16) -> Result<(String, u16)> {
        let tunnels = self.local.list_tunnels().await.map_err(|e| {
            ProviderError::InvalidConfig(format!(
                "cannot read the DNSPilot Agent's tunnels ({e}); set \
                 remote.provider.dnspilot.hostname to the tunnel's address"
            ))
        })?;
        let mut matching: Vec<_> = tunnels
            .into_iter()
            .filter(|t| t.is_app_mode() && t.local_port == Some(local_port))
            .collect();
        match matching.len() {
            1 => {
                let t = matching.remove(0);
                let host = normalize_hostname(&t.hostname)?;
                Ok((host, t.public_port()))
            }
            0 => Err(ProviderError::InvalidConfig(format!(
                "no DNSPilot app-mode tunnel to 127.0.0.1:{local_port} on this PC yet: choose \
                 \"Connect DNSPilot\" in the VCA app (or create one in the DNSPilot console)"
            ))),
            _ => Err(ProviderError::InvalidConfig(format!(
                "several DNSPilot app-mode tunnels forward to 127.0.0.1:{local_port}; set \
                 remote.provider.dnspilot.hostname to the one VCA should use"
            ))),
        }
    }

    pub fn settings(&self) -> &DnsPilotSettings {
        &self.settings
    }

    /// `GET /v1/status` of the local Agent.
    pub async fn agent_status(&self) -> Result<AgentStatus> {
        self.local.status().await
    }
}

#[async_trait]
impl ConnectivityProvider for DnsPilotProvider {
    fn id(&self) -> ProviderId {
        ProviderId::DnsPilot
    }

    fn name(&self) -> &'static str {
        "DNSPilot"
    }

    async fn detect(&self) -> Result<Detection> {
        match self.local.status().await {
            Ok(s) => Ok(Detection {
                installed: true,
                signed_in: Some(s.logged_in),
                version: s.version.clone(),
                detail: Some(if s.killed {
                    "kill switch engaged".into()
                } else if s.logged_in {
                    match (&s.device_name, s.device_id) {
                        (Some(n), Some(id)) => format!("Agent signed in as device {n:?} (id {id})"),
                        _ => "Agent signed in".into(),
                    }
                } else {
                    "Agent not signed in".into()
                }),
            }),
            Err(ProviderError::NotInstalled(_)) => Ok(Detection::default()),
            Err(e) => Err(e),
        }
    }

    async fn status(&self) -> ProviderStatus {
        match &*self.endpoint.lock().await {
            Some(e) => ProviderStatus::Active {
                endpoint: e.clone(),
            },
            None => ProviderStatus::Inactive,
        }
    }

    async fn ensure_endpoint(&self, local_port: u16) -> Result<Endpoint> {
        *self.local_port.lock().await = Some(local_port);
        let mut status_ok = false;
        match self.local.status().await {
            Ok(s) if s.killed => {
                return Err(ProviderError::InvalidConfig(
                    "the DNSPilot kill switch is engaged; lift it in DNSPilot first".into(),
                ));
            }
            Ok(s) if !s.logged_in => {
                return Err(ProviderError::NotSignedIn(
                    "the DNSPilot Agent on this PC is not signed in".into(),
                ));
            }
            Ok(_) => status_ok = true,
            Err(ProviderError::NotInstalled(what)) => {
                return Err(ProviderError::NotInstalled(what));
            }
            // No access to the local API (for example not in the `dnspilot`
            // group): a configured tunnel can still work.
            Err(e) => tracing::warn!("cannot read the DNSPilot Agent status: {e}"),
        }
        let (hostname, public_port) = match &self.settings.hostname {
            Some(h) => (h.clone(), self.settings.public_port),
            None if status_ok => self.discover(local_port).await?,
            None => {
                return Err(ProviderError::InvalidConfig(
                    "set remote.provider.dnspilot.hostname to the tunnel's address (the DNSPilot \
                     Agent's tunnel list could not be read)"
                        .into(),
                ));
            }
        };
        let endpoint = Endpoint::new(
            tunnel_url(&hostname, public_port),
            EndpointKind::DnsPilotApp,
        );
        *self.endpoint.lock().await = Some(endpoint.clone());
        *self.effective.lock().await = Some(hostname);
        Ok(endpoint)
    }

    /// Forgets the endpoint. The tunnel belongs to the user (or to the VCA
    /// app's grant) and keeps running; vcad never stops it.
    async fn teardown(&self) -> Result<()> {
        self.endpoint.lock().await.take();
        self.effective.lock().await.take();
        Ok(())
    }

    async fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match self.local.status().await {
            Err(ProviderError::NotInstalled(_)) => out.push(Diagnostic::error(
                "The DNSPilot Agent is not installed or not running on this PC.",
            )),
            Err(e) => out.push(Diagnostic::warning(format!(
                "Cannot read the DNSPilot Agent status: {e}"
            ))),
            Ok(s) => {
                if s.logged_in {
                    let device = match (&s.device_name, s.device_id) {
                        (Some(n), Some(id)) => format!("{n} (device id {id})"),
                        (None, Some(id)) => format!("device id {id}"),
                        (Some(n), None) => n.clone(),
                        (None, None) => "unknown device".into(),
                    };
                    out.push(Diagnostic::info(format!(
                        "DNSPilot Agent {} signed in as {device}.",
                        s.version.as_deref().unwrap_or("")
                    )));
                } else {
                    out.push(Diagnostic::error(
                        "Sign in the DNSPilot Agent on this PC (browser approval).",
                    ));
                }
                if s.killed {
                    out.push(Diagnostic::warning("The DNSPilot kill switch is engaged."));
                }
                if let Some(relay) = s.relay_summary() {
                    out.push(Diagnostic::info(format!("Relay: {relay}")));
                }
                if s.tunnels_online == Some(0) {
                    out.push(Diagnostic::warning(
                        "No DNSPilot tunnel is online on this PC.",
                    ));
                }
            }
        }
        let hostname = match &self.settings.hostname {
            Some(h) => Some(h.clone()),
            None => self.effective.lock().await.clone(),
        };
        let port = *self.local_port.lock().await;
        match self.local.list_tunnels().await {
            Ok(tunnels) => {
                let found = hostname
                    .as_deref()
                    .and_then(|h| tunnels.iter().find(|t| t.hostname.eq_ignore_ascii_case(h)));
                match (found, &hostname) {
                    (Some(t), _) => {
                        out.push(Diagnostic::info(format!(
                            "Tunnel: {} ({}).",
                            tunnel_url(&t.hostname, t.public_port()),
                            t.state_summary()
                        )));
                        if t.state.as_deref() != Some("serving") {
                            out.push(Diagnostic::warning(format!(
                                "The tunnel is not serving: {}.",
                                t.state_summary()
                            )));
                        }
                        if !t.is_app_mode() {
                            out.push(Diagnostic::error(
                                "The tunnel does not use access mode App; only use App mode for VCA.",
                            ));
                        }
                        if let (Some(want), Some(have)) = (port, t.local_port)
                            && want != have
                        {
                            out.push(Diagnostic::error(format!(
                                "The tunnel forwards to local port {have}, but VCA listens on {want}."
                            )));
                        }
                    }
                    (None, Some(h)) => out.push(Diagnostic::warning(format!(
                        "{h} is not among the tunnels this PC's DNSPilot Agent serves."
                    ))),
                    (None, None) => out.push(Diagnostic::error(
                        "No DNSPilot tunnel for VCA yet (choose \"Connect DNSPilot\" in the app).",
                    )),
                }
            }
            Err(e) => {
                if let Some(h) = &hostname {
                    out.push(Diagnostic::info(format!(
                        "Tunnel: {} (access mode App).",
                        tunnel_url(h, self.settings.public_port)
                    )));
                } else {
                    out.push(Diagnostic::error(
                        "No tunnel hostname configured (remote.provider.dnspilot.hostname).",
                    ));
                }
                out.push(Diagnostic::info(format!(
                    "Cannot read the Agent's tunnel list: {e}"
                )));
            }
        }
        if let Some(port) = port {
            out.push(Diagnostic::info(format!(
                "The tunnel must forward to 127.0.0.1:{port} over http with access mode App."
            )));
        }
        out.push(Diagnostic::info(
            "Tunnel hostnames are public in Certificate Transparency logs; use a random name.",
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_random_and_non_identifying() {
        let a = random_hostname_label();
        let b = random_hostname_label();
        assert_ne!(a, b);
        assert!(a.starts_with("vca-"));
        assert_eq!(a.len(), 4 + LABEL_RANDOM_LEN);
        assert!(normalize_hostname(&format!("{a}.example.com")).is_ok());
    }

    #[test]
    fn client_key_pins_are_64_hex() {
        // A real P-256 SubjectPublicKeyInfo.
        let pem = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEEVs/o5+uQbTjL3chynL4wXgUg2R9\n\
q9UU8I5mEovUf86QZ7kOBIjJwqnzD1omageEHWwHdBO6B+dFabmdT9POxg==\n\
-----END PUBLIC KEY-----\n";
        let der = spki_der_from_pem(pem).unwrap();
        let k = ClientKey::from_pem(pem).unwrap();
        let hex = k.sha256_hex().unwrap();
        assert_eq!(hex, ClientKey::from_spki_der(&der).sha256_hex().unwrap());
        assert!(is_spki_sha256_hex(&hex));
        assert!(ClientKey::from_pem("garbage").is_err());
        assert!(ClientKey::from_hex("abc").is_err());
        assert_eq!(
            ClientKey::from_hex(&hex.to_uppercase()).unwrap(),
            ClientKey::SpkiSha256(hex.clone())
        );
        let req = ClientKey::from_spki_der(&der).to_request(Some("x".repeat(80)));
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["spki_sha256"], hex);
        assert!(v.get("public_key_pem").is_none());
        assert_eq!(v["label"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn hostnames_are_normalized() {
        assert_eq!(
            normalize_hostname(" https://VCA-abc.Example.com/ ").unwrap(),
            "vca-abc.example.com"
        );
        assert!(normalize_hostname("localhost").is_err());
        assert!(normalize_hostname("vca.example.com:443").is_err());
        assert!(normalize_hostname("vca.example.com/v1/ws").is_err());
        assert!(normalize_hostname("-a.example.com").is_err());
        assert_eq!(tunnel_url("a.example.com", 443), "wss://a.example.com");
        assert_eq!(
            tunnel_url("a.example.com", 9443),
            "wss://a.example.com:9443"
        );
    }
}
