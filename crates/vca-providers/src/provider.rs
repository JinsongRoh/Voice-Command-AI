//! The provider-neutral connectivity trait (requirements §3.8).

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Identifies a provider kind. No provider is a default: the user chooses,
/// and the setup wizard only recommends (CON-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Lan,
    Tailscale,
    CloudflareTunnel,
    DnsPilot,
    Custom,
}

impl ProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::Tailscale => "tailscale",
            Self::CloudflareTunnel => "cloudflare_tunnel",
            Self::DnsPilot => "dnspilot",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of probing a provider on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Detection {
    /// The provider's software (CLI, agent) is present.
    pub installed: bool,
    /// Whether the user is signed in, when the provider has accounts and
    /// this could be determined.
    pub signed_in: Option<bool>,
    pub version: Option<String>,
    /// Short human-readable explanation.
    pub detail: Option<String>,
}

/// How a provider reaches the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    /// Direct connection on the local network.
    Lan,
    /// Direct connection to the machine's tailnet address.
    TailnetDirect,
    /// `tailscale serve` HTTPS reverse proxy on the tailnet.
    TailscaleServe,
    /// Cloudflare quick tunnel (`*.trycloudflare.com`).
    CloudflareQuick,
    /// Cloudflare named tunnel with the user's hostname.
    CloudflareNamed,
    /// DNS Pilot tunnel in `app` access mode.
    DnsPilotApp,
    /// User-supplied URL.
    Custom,
}

/// An address paired devices use to reach the server.
///
/// Every endpoint is an untrusted transport (DNS Pilot design consequence
/// C1): VCA's Noise channel and device authentication apply on top of it,
/// whether or not the transport has TLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Base WebSocket URL (`ws://` or `wss://`), without a path. The server
    /// appends its WebSocket path.
    pub url: String,
    pub kind: EndpointKind,
}

impl Endpoint {
    pub fn new(url: impl Into<String>, kind: EndpointKind) -> Self {
        Self {
            url: url.into(),
            kind,
        }
    }

    /// True when the transport itself is TLS-protected (`wss://`).
    pub fn transport_tls(&self) -> bool {
        self.url.starts_with("wss://")
    }
}

/// Current state of a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Nothing is exposed through this provider.
    Inactive,
    /// An endpoint is being set up.
    Starting,
    Active {
        endpoint: Endpoint,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

/// One finding shown in the setup wizard (CON-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

impl Diagnostic {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Info,
            message: message.into(),
        }
    }
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Warning,
            message: message.into(),
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Error,
            message: message.into(),
        }
    }
}

/// A way for paired devices to reach the server's local port.
///
/// Providers are interchangeable: the server holds a list of
/// `Box<dyn ConnectivityProvider>` and treats them alike.
#[async_trait]
pub trait ConnectivityProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// Display name.
    fn name(&self) -> &'static str;

    /// Checks whether the provider is installed and signed in.
    async fn detect(&self) -> Result<Detection>;

    async fn status(&self) -> ProviderStatus;

    /// Makes the server's `127.0.0.1:local_port` reachable and returns the
    /// endpoint for the pairing QR code. Idempotent.
    async fn ensure_endpoint(&self, local_port: u16) -> Result<Endpoint>;

    /// Removes what [`ensure_endpoint`](Self::ensure_endpoint) set up.
    async fn teardown(&self) -> Result<()>;

    /// Findings for the setup wizard.
    async fn diagnostics(&self) -> Vec<Diagnostic>;
}
