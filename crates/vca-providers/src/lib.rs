//! Connectivity providers (requirements §3.8): direct LAN, Tailscale,
//! Cloudflare Tunnel, DNS Pilot and custom URLs.
//!
//! All providers implement [`ConnectivityProvider`] and are interchangeable
//! and equal: none is a default in code. The user chooses one; the setup
//! wizard may recommend one based on [`ConnectivityProvider::diagnostics`].
//!
//! Every provider is treated as an untrusted transport (DNS Pilot design
//! consequence C1): the server serves plain HTTP/1.1 WebSocket on loopback
//! (C2, C3), and VCA's own pairing keys and Noise channel provide
//! authentication and confidentiality on top of whatever the provider
//! offers.

pub mod cloudflare;
pub mod command;
pub mod custom;
pub mod dnspilot;
pub mod error;
pub mod lan;
pub mod provider;
pub mod secret;
pub mod tailscale;

pub use cloudflare::{CloudflareMode, CloudflareProvider, NamedTunnel};
pub use custom::CustomProvider;
pub use dnspilot::DnsPilotProvider;
pub use error::{ProviderError, Result};
pub use lan::LanProvider;
pub use provider::{
    ConnectivityProvider, Detection, Diagnostic, DiagnosticLevel, Endpoint, EndpointKind,
    ProviderId, ProviderStatus,
};
pub use secret::Secret;
pub use tailscale::{TailscaleMode, TailscaleProvider};
