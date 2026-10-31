//! Error, authentication-failure and close-reason types.

use serde::{Deserialize, Serialize};
use vca_core::error::ErrorCode;

/// Why authentication inside the Noise channel failed.
///
/// This value is sent to the peer, so it deliberately does not distinguish
/// cases that would help an attacker (for example an expired pairing secret
/// and a wrong one both map to [`AuthFailure::InvalidPairingSecret`]; the
/// audit log keeps the precise reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFailure {
    /// The device key is not in the paired-device registry.
    UnknownDevice,
    /// The device was revoked.
    DeviceRevoked,
    /// The device key is already paired.
    AlreadyPaired,
    /// Wrong, expired or already used pairing secret.
    InvalidPairingSecret,
    /// TOTP is required but was not supplied.
    TotpRequired,
    /// TOTP is required by policy but not enrolled on the server.
    TotpNotEnrolled,
    /// Wrong or replayed TOTP code or backup code.
    InvalidTotp,
    /// Malformed, expired, revoked or foreign token.
    InvalidToken,
    /// A refresh token was used twice; the whole token family is revoked.
    TokenReuse,
    /// Too many failed attempts; try again later.
    RateLimited,
    /// The kill switch is engaged.
    KillSwitch,
    /// The peer violated the authentication protocol.
    Protocol,
    /// Server-side failure (storage error and similar).
    Internal,
}

impl AuthFailure {
    /// Maps the failure to the shared API error code.
    pub fn code(self) -> ErrorCode {
        match self {
            Self::RateLimited | Self::KillSwitch => ErrorCode::Unavailable,
            Self::AlreadyPaired => ErrorCode::Conflict,
            Self::Protocol => ErrorCode::InvalidRequest,
            Self::Internal => ErrorCode::Internal,
            Self::DeviceRevoked => ErrorCode::Forbidden,
            _ => ErrorCode::Unauthorized,
        }
    }
}

impl std::fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::UnknownDevice => "unknown device",
            Self::DeviceRevoked => "device revoked",
            Self::AlreadyPaired => "device already paired",
            Self::InvalidPairingSecret => "invalid pairing secret",
            Self::TotpRequired => "TOTP code required",
            Self::TotpNotEnrolled => "TOTP is required but not enrolled",
            Self::InvalidTotp => "invalid TOTP code",
            Self::InvalidToken => "invalid token",
            Self::TokenReuse => "refresh token reuse detected",
            Self::RateLimited => "too many failed attempts",
            Self::KillSwitch => "kill switch engaged",
            Self::Protocol => "authentication protocol violation",
            Self::Internal => "internal error",
        };
        f.write_str(s)
    }
}

/// Why a connection was closed. Sent to the peer in a final encrypted
/// `Close` frame when possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Orderly close requested by the local application.
    Normal,
    /// The peer closed the connection (with or without a `Close` frame).
    PeerClosed,
    /// The kill switch was tripped.
    KillSwitch,
    /// The device was revoked while connected.
    DeviceRevoked,
    /// No traffic from the peer for two heartbeat intervals.
    HeartbeatTimeout,
    /// Undecryptable data, malformed frame or oversized message.
    ProtocolError,
    /// The Noise nonce space is (nearly) exhausted.
    NonceExhausted,
    /// The underlying WebSocket failed.
    TransportError,
    /// The server is shutting down.
    Shutdown,
}

impl std::fmt::Display for CloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("noise error: {0}")]
    Noise(String),
    #[error("websocket error: {0}")]
    WebSocket(String),
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("message too large ({size} bytes, limit {limit})")]
    MessageTooLarge { size: usize, limit: usize },
    #[error("nonce space exhausted")]
    NonceExhausted,
    #[error("server key mismatch (expected {expected}, got {actual})")]
    ServerKeyMismatch { expected: String, actual: String },
    #[error("authentication failed: {0}")]
    Auth(AuthFailure),
    #[error("connection closed: {0}")]
    Closed(CloseReason),
    #[error("kill switch engaged")]
    KillSwitch,
    #[error("timed out")]
    Timeout,
    #[error("storage error: {0}")]
    Storage(String),
    #[error("invalid pairing payload: {0}")]
    InvalidPairingPayload(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

impl NetError {
    #[cfg_attr(not(feature = "keychain"), allow(dead_code))]
    pub(crate) fn storage(e: impl std::fmt::Display) -> Self {
        Self::Storage(e.to_string())
    }

    pub(crate) fn encoding(e: impl std::fmt::Display) -> Self {
        Self::Encoding(e.to_string())
    }
}

impl From<snow::Error> for NetError {
    fn from(e: snow::Error) -> Self {
        Self::Noise(e.to_string())
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for NetError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(e.to_string())
    }
}

/// Result alias for this crate.
pub type Result<T, E = NetError> = std::result::Result<T, E>;
