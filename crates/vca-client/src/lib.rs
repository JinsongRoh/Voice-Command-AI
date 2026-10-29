//! Device-side connection manager for Voice Command AI.
//!
//! This crate is what the phone and desktop shells (Tauri) wrap. It owns:
//!
//! * **Pairing** from a `vca://pair?...` QR URI ([`ClientHub::pair`]): a fresh
//!   device keypair per server, the server key pinned from the QR code.
//! * **Storage**: non-secret metadata in `machines.json` (written
//!   atomically, owner-only on Unix); every secret (device static private
//!   key, refresh token) only in a [`vca_net::KeyStore`].
//! * **Supervised connections** ([`ClientHub::connect`]): refresh-token
//!   authentication with rotation, endpoint fallback, exponential backoff
//!   with jitter, and clear terminal states ([`LinkState::LoginRequired`],
//!   revoked device, kill switch) reported through a [`LinkSink`].
//!
//! It does not depend on any UI framework.

mod backoff;
mod error;
mod hub;
mod store;

use serde::{Deserialize, Serialize};

pub use backoff::Backoff;
pub use error::ClientError;
pub use hub::{ClientHub, ClientOptions};
pub use store::{MACHINES_FILE, device_key_name, is_valid_server_id, refresh_token_name};
pub use vca_net::Scope;

/// A paired server ("machine"), as stored in `machines.json`. Contains no
/// secrets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MachineInfo {
    /// Server id from the pairing payload (16 lowercase hex characters,
    /// derived from the server's static key).
    pub id: String,
    /// Display name of the server (sanitized; updated from the server on
    /// every successful authentication).
    pub name: String,
    /// Connection endpoints (`ws://` / `wss://`), tried in order.
    pub endpoints: Vec<String>,
    /// Id the server assigned to this device.
    pub device_id: String,
    /// Permission scope granted to this device.
    pub scope: Scope,
    /// When the device was paired, Unix milliseconds.
    pub paired_at: u64,
}

/// State of one machine connection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    LoginRequired,
}

/// State plus optional error details of one machine connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    pub state: LinkState,
    /// Human-readable error, never containing secrets.
    pub error: Option<String>,
    /// Stable machine-readable code, e.g. `totp_required`, `auth_failed`,
    /// `revoked`, `unreachable`, `kill_switch`, `rate_limited`,
    /// `server_key_mismatch`, `protocol`, `storage`, `not_found`.
    pub code: Option<String>,
}

impl LinkStatus {
    /// A status without error details.
    pub fn new(state: LinkState) -> Self {
        Self {
            state,
            error: None,
            code: None,
        }
    }

    /// A status with an error message and code.
    pub fn with_error(state: LinkState, code: &str, error: impl Into<String>) -> Self {
        Self {
            state,
            error: Some(error.into()),
            code: Some(code.to_string()),
        }
    }

    /// `Disconnected` without error details.
    pub fn disconnected() -> Self {
        Self::new(LinkState::Disconnected)
    }
}

/// Receives events for one machine connection. Called from tokio tasks;
/// implementations must not block for long. Calling back into the
/// [`ClientHub`] from these methods is allowed.
pub trait LinkSink: Send + Sync + 'static {
    /// The connection state changed.
    fn status(&self, status: LinkStatus);
    /// One server envelope (`Envelope<ServerMsg>`) re-encoded with
    /// `rmp_serde::to_vec_named` (structs as maps).
    fn message(&self, msgpack: Vec<u8>);
}
