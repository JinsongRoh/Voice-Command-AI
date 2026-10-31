//! DNS Pilot provider (docs/dnspilot-requirements.md, track A).
//!
//! Exposes the server's loopback port through a DNS Pilot tunnel in `app`
//! access mode (mTLS public-key pinning), with:
//!
//! - a random, non-identifying hostname label (C6),
//! - local scheme `http` to `127.0.0.1:<port>` (C2) and public port 443,
//! - each paired device's client public key registered with the tunnel
//!   through the Agent's local API, for mTLS pinning.
//!
//! The tunnel is created either by the local Agent (`POST /v1/tunnels`) or
//! by the console with an OAuth access token from the device flow
//! ([`oauth`]); DNS Pilot only lets OAuth grants create `app`-mode tunnels
//! and modify tunnels created by the same grant. The tunnel is an untrusted
//! transport (C1): the console holds the certificate keys, so VCA's Noise
//! channel provides confidentiality.
//!
//! The DNS Pilot APIs are not yet public; see [`contract`] for the field
//! names that remain to be confirmed.

pub mod contract;
pub mod events;
pub mod local;
pub mod oauth;
pub mod sse;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rand::Rng;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use self::contract::{
    ACCESS_MODE_APP, AppClient, CreateTunnel, PatchTunnel, RegisterAppClient, Tunnel,
};
use self::local::LocalApiClient;
use self::oauth::{DeviceFlowClient, TokenSet};
use crate::error::{ProviderError, Result};
use crate::provider::{
    ConnectivityProvider, Detection, Diagnostic, Endpoint, EndpointKind, ProviderId, ProviderStatus,
};

/// Length of the random part of a hostname label.
const LABEL_RANDOM_LEN: usize = 10;

/// Generates a random, non-identifying hostname label such as
/// `vca-k3x9q2ab4m` (C6). Labels never contain user, machine or project
/// names, because every tunnel hostname appears in public Certificate
/// Transparency logs.
pub fn random_hostname_label() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    let suffix: String = (0..LABEL_RANDOM_LEN)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect();
    format!("vca-{suffix}")
}

/// A paired device's client public key, for mTLS pinning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientKey {
    /// Base64 SHA-256 of the DER SubjectPublicKeyInfo.
    SpkiSha256(String),
    /// PEM `PUBLIC KEY` block.
    Pem(String),
}

impl ClientKey {
    /// Pins a DER-encoded SubjectPublicKeyInfo by its SHA-256.
    pub fn from_spki_der(der: &[u8]) -> Self {
        Self::SpkiSha256(B64.encode(Sha256::digest(der)))
    }

    /// Accepts a PEM `PUBLIC KEY` block (validated, sent as PEM).
    pub fn from_pem(pem: &str) -> Result<Self> {
        spki_der_from_pem(pem)?;
        Ok(Self::Pem(pem.trim().to_owned()))
    }

    /// The SHA-256 pin of this key.
    pub fn sha256(&self) -> Result<String> {
        match self {
            Self::SpkiSha256(h) => Ok(h.clone()),
            Self::Pem(p) => Ok(B64.encode(Sha256::digest(spki_der_from_pem(p)?))),
        }
    }

    fn to_request(&self, label: String) -> RegisterAppClient {
        match self {
            Self::SpkiSha256(h) => RegisterAppClient {
                public_key_sha256: Some(h.clone()),
                public_key_pem: None,
                label,
            },
            Self::Pem(p) => RegisterAppClient {
                public_key_sha256: None,
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

/// How the tunnel is created.
pub enum TunnelCreation {
    /// The signed-in local Agent creates it (`POST /v1/tunnels`).
    LocalAgent,
    /// The console creates it with an OAuth access token (device flow).
    /// `device_id` is the target PC chosen on the approval screen, when the
    /// console requires it in the request.
    Console {
        oauth: DeviceFlowClient,
        tokens: Arc<Mutex<TokenSet>>,
        device_id: Option<String>,
    },
}

impl std::fmt::Debug for TunnelCreation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalAgent => f.write_str("LocalAgent"),
            Self::Console { device_id, .. } => f
                .debug_struct("Console")
                .field("device_id", device_id)
                .finish_non_exhaustive(),
        }
    }
}

/// DNS Pilot provider.
#[derive(Debug)]
pub struct DnsPilotProvider {
    local: LocalApiClient,
    creation: TunnelCreation,
    http: reqwest::Client,
    /// The tunnel VCA manages (persist its ID across restarts).
    tunnel: Mutex<Option<Tunnel>>,
    tunnel_id: Mutex<Option<String>>,
}

impl DnsPilotProvider {
    pub fn new(local: LocalApiClient, creation: TunnelCreation) -> Self {
        Self {
            local,
            creation,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            tunnel: Mutex::new(None),
            tunnel_id: Mutex::new(None),
        }
    }

    /// Restores the ID of a tunnel created earlier.
    pub fn with_tunnel_id(self, id: impl Into<String>) -> Self {
        *self.tunnel_id.try_lock().expect("not shared yet") = Some(id.into());
        self
    }

    pub fn local(&self) -> &LocalApiClient {
        &self.local
    }

    /// ID of the managed tunnel, for persistence.
    pub async fn tunnel_id(&self) -> Option<String> {
        self.tunnel_id.lock().await.clone()
    }

    /// Reloads tunnel state from the Agent (after an event-stream reset, C10).
    pub async fn resync(&self) -> Result<Option<Tunnel>> {
        let Some(id) = self.tunnel_id.lock().await.clone() else {
            return Ok(None);
        };
        let found = self
            .local
            .list_tunnels()
            .await?
            .into_iter()
            .find(|t| t.id == id);
        *self.tunnel.lock().await = found.clone();
        Ok(found)
    }

    async fn create(&self, local_port: u16) -> Result<Tunnel> {
        let mut req = CreateTunnel {
            hostname_label: random_hostname_label(),
            access_mode: ACCESS_MODE_APP.into(),
            local_scheme: "http".into(),
            local_host: "127.0.0.1".into(),
            local_port,
            public_port: 443,
            device_id: None,
        };
        match &self.creation {
            TunnelCreation::LocalAgent => self.local.create_tunnel(&req).await,
            TunnelCreation::Console {
                oauth,
                tokens,
                device_id,
            } => {
                req.device_id = device_id.clone();
                let mut guard = tokens.lock().await;
                if guard.expires_within(Duration::from_secs(60)) {
                    *guard = oauth.refresh(&guard).await.map_err(oauth_to_provider)?;
                }
                let url = format!(
                    "{}{}",
                    oauth.config().console_url.trim_end_matches('/'),
                    contract::paths::CONSOLE_TUNNELS
                );
                let resp = self
                    .http
                    .post(url)
                    .bearer_auth(guard.access_token.expose())
                    .json(&req)
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let message = resp
                        .text()
                        .await
                        .unwrap_or_default()
                        .chars()
                        .take(300)
                        .collect();
                    return Err(match status.as_u16() {
                        401 | 403 => ProviderError::Unauthorized(message),
                        s => ProviderError::Api { status: s, message },
                    });
                }
                Ok(resp.json().await?)
            }
        }
    }

    /// Registers a paired device's client key with the tunnel (mTLS pinning).
    pub async fn register_app_client(&self, key: &ClientKey) -> Result<AppClient> {
        let id = self
            .tunnel_id
            .lock()
            .await
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("no DNS Pilot tunnel yet".into()))?;
        // The label is random too: it may show up in DNS Pilot logs.
        let label = format!("vca-device-{}", &random_hostname_label()[4..]);
        self.local
            .register_app_client(&id, &key.to_request(label))
            .await
    }

    /// Removes a device's client key (when the device is unpaired).
    pub async fn remove_app_client(&self, client_id: &str) -> Result<()> {
        let id = self
            .tunnel_id
            .lock()
            .await
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("no DNS Pilot tunnel yet".into()))?;
        self.local.remove_app_client(&id, client_id).await
    }
}

fn oauth_to_provider(e: oauth::OAuthError) -> ProviderError {
    match e {
        oauth::OAuthError::Provider(p) => p,
        other => ProviderError::Unauthorized(other.to_string()),
    }
}

fn endpoint_of(t: &Tunnel) -> Endpoint {
    let port = match t.public_port {
        Some(443) | None => String::new(),
        Some(p) => format!(":{p}"),
    };
    Endpoint::new(
        format!("wss://{}{port}", t.hostname),
        EndpointKind::DnsPilotApp,
    )
}

#[async_trait]
impl ConnectivityProvider for DnsPilotProvider {
    fn id(&self) -> ProviderId {
        ProviderId::DnsPilot
    }

    fn name(&self) -> &'static str {
        "DNS Pilot"
    }

    async fn detect(&self) -> Result<Detection> {
        match self.local.status().await {
            Ok(s) => Ok(Detection {
                installed: true,
                signed_in: Some(s.activated),
                version: s.version,
                detail: Some(if s.killed {
                    "kill switch engaged".into()
                } else if s.activated {
                    "Agent activated".into()
                } else {
                    "Agent not activated".into()
                }),
            }),
            Err(ProviderError::NotInstalled(_)) => Ok(Detection::default()),
            Err(e) => Err(e),
        }
    }

    async fn status(&self) -> ProviderStatus {
        match &*self.tunnel.lock().await {
            Some(t) if t.enabled => ProviderStatus::Active {
                endpoint: endpoint_of(t),
            },
            Some(t) if t.state.as_deref() == Some("error") => ProviderStatus::Error {
                message: "tunnel error".into(),
            },
            _ => ProviderStatus::Inactive,
        }
    }

    async fn ensure_endpoint(&self, local_port: u16) -> Result<Endpoint> {
        let status = self.local.status().await?;
        if !status.activated {
            return Err(ProviderError::NotSignedIn(
                "DNS Pilot Agent is not activated".into(),
            ));
        }
        if status.killed {
            return Err(ProviderError::InvalidConfig(
                "DNS Pilot kill switch is engaged".into(),
            ));
        }
        let existing = self.resync().await?;
        let tunnel = match existing {
            Some(t) if t.enabled && t.local_port == local_port => t,
            Some(t) => {
                let patch = PatchTunnel {
                    enabled: (!t.enabled).then_some(true),
                    local_port: (t.local_port != local_port).then_some(local_port),
                };
                self.local.patch_tunnel(&t.id, &patch).await?
            }
            None => {
                let t = self.create(local_port).await?;
                *self.tunnel_id.lock().await = Some(t.id.clone());
                t
            }
        };
        if tunnel.access_mode != ACCESS_MODE_APP && !tunnel.access_mode.is_empty() {
            return Err(ProviderError::InvalidConfig(format!(
                "tunnel uses access mode {:?}, expected \"app\"",
                tunnel.access_mode
            )));
        }
        let endpoint = endpoint_of(&tunnel);
        *self.tunnel.lock().await = Some(tunnel);
        Ok(endpoint)
    }

    /// Disables the tunnel but keeps it, so paired devices keep their
    /// hostname when it is enabled again.
    async fn teardown(&self) -> Result<()> {
        let Some(id) = self.tunnel_id.lock().await.clone() else {
            return Ok(());
        };
        let t = self
            .local
            .patch_tunnel(
                &id,
                &PatchTunnel {
                    enabled: Some(false),
                    local_port: None,
                },
            )
            .await?;
        *self.tunnel.lock().await = Some(t);
        Ok(())
    }

    async fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match self.local.status().await {
            Err(ProviderError::NotInstalled(_)) => out.push(Diagnostic::error(
                "The DNS Pilot Agent is not installed or not running.",
            )),
            Err(e) => out.push(Diagnostic::error(format!("DNS Pilot Agent: {e}"))),
            Ok(s) => {
                if !s.activated {
                    out.push(Diagnostic::error(
                        "Activate this device in DNS Pilot (browser approval required).",
                    ));
                }
                if s.killed {
                    out.push(Diagnostic::warning("The DNS Pilot kill switch is engaged."));
                }
                if s.relay_connected == Some(false) {
                    out.push(Diagnostic::warning(
                        "The DNS Pilot Agent is not connected to a relay.",
                    ));
                }
            }
        }
        out.push(Diagnostic::info(
            "Tunnel hostnames are public in Certificate Transparency logs; VCA uses a random label.",
        ));
        out.push(Diagnostic::info(
            "DNS Pilot terminates TLS on this machine with console-issued keys; VCA's end-to-end encryption protects the content.",
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
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
        let host = whoami_like();
        assert!(!a.contains(&host));
    }

    fn whoami_like() -> String {
        std::env::var("USER")
            .unwrap_or_else(|_| "nobody-xyz".into())
            .to_lowercase()
    }

    #[test]
    fn client_key_pins() {
        // A real P-256 SubjectPublicKeyInfo.
        let pem = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEEVs/o5+uQbTjL3chynL4wXgUg2R9\n\
q9UU8I5mEovUf86QZ7kOBIjJwqnzD1omageEHWwHdBO6B+dFabmdT9POxg==\n\
-----END PUBLIC KEY-----\n";
        let der = spki_der_from_pem(pem).unwrap();
        let k = ClientKey::from_pem(pem).unwrap();
        assert_eq!(
            k.sha256().unwrap(),
            ClientKey::from_spki_der(&der).sha256().unwrap()
        );
        assert_eq!(k.sha256().unwrap().len(), 44);
        assert!(ClientKey::from_pem("garbage").is_err());
        let req = ClientKey::from_spki_der(&der).to_request("l".into());
        assert!(req.public_key_sha256.is_some() && req.public_key_pem.is_none());
    }

    #[test]
    fn endpoint_from_tunnel() {
        let t = Tunnel {
            hostname: "vca-abc.example.net".into(),
            public_port: Some(443),
            ..Tunnel::default()
        };
        assert_eq!(endpoint_of(&t).url, "wss://vca-abc.example.net");
        let t9443 = Tunnel {
            public_port: Some(9443),
            ..t
        };
        assert_eq!(endpoint_of(&t9443).url, "wss://vca-abc.example.net:9443");
    }
}
