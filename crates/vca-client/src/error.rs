//! Error type of the client hub. Messages never contain secrets.

use vca_net::{AuthFailure, NetError};

/// Errors returned by [`crate::ClientHub`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No paired machine with this id.
    #[error("unknown machine {0}")]
    NotFound(String),
    /// The pairing URI is malformed, expired or otherwise unusable.
    #[error("invalid pairing code: {0}")]
    InvalidPairing(String),
    /// The server refused authentication. `code` is one of the stable codes
    /// documented on [`ClientError::code`].
    #[error("authentication failed: {message}")]
    Auth { code: &'static str, message: String },
    /// There is no live connection to the machine.
    #[error("not connected")]
    NotConnected,
    /// No endpoint of the machine could be reached.
    #[error("server unreachable: {0}")]
    Unreachable(String),
    /// The endpoint answered with a different server key than the pinned
    /// one (possible interception; the connection was refused).
    #[error("server key mismatch: the endpoint is not the paired server")]
    ServerKeyMismatch,
    /// Malformed message or protocol violation.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Reading or writing local state (metadata file or key store) failed.
    #[error("storage error: {0}")]
    Storage(String),
    /// Internal failure, e.g. no tokio runtime available.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ClientError {
    /// Stable machine-readable code: `not_found`, `invalid_pairing`,
    /// `not_connected`, `unreachable`, `server_key_mismatch`, `protocol`,
    /// `storage`, `internal`, or for [`ClientError::Auth`] one of
    /// `revoked`, `totp_required`, `invalid_totp`, `totp_not_enrolled`,
    /// `auth_failed`, `invalid_pairing`, `already_paired`, `rate_limited`,
    /// `kill_switch`, `protocol`, `server_error`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::InvalidPairing(_) => "invalid_pairing",
            Self::Auth { code, .. } => code,
            Self::NotConnected => "not_connected",
            Self::Unreachable(_) => "unreachable",
            Self::ServerKeyMismatch => "server_key_mismatch",
            Self::Protocol(_) => "protocol",
            Self::Storage(_) => "storage",
            Self::Internal(_) => "internal",
        }
    }

    pub(crate) fn storage(e: impl std::fmt::Display) -> Self {
        Self::Storage(e.to_string())
    }
}

/// Stable code for an authentication failure reported by the server.
pub(crate) fn auth_code(f: AuthFailure) -> &'static str {
    match f {
        AuthFailure::UnknownDevice | AuthFailure::DeviceRevoked => "revoked",
        AuthFailure::TotpRequired => "totp_required",
        AuthFailure::InvalidTotp => "invalid_totp",
        AuthFailure::TotpNotEnrolled => "totp_not_enrolled",
        AuthFailure::InvalidToken | AuthFailure::TokenReuse => "auth_failed",
        AuthFailure::InvalidPairingSecret => "invalid_pairing",
        AuthFailure::AlreadyPaired => "already_paired",
        AuthFailure::RateLimited => "rate_limited",
        AuthFailure::KillSwitch => "kill_switch",
        AuthFailure::Protocol => "protocol",
        AuthFailure::Internal => "server_error",
    }
}

/// True for errors where trying the next endpoint (or retrying later) may
/// help: the failure happened before the authenticated server answered.
pub(crate) fn is_transport_error(e: &NetError) -> bool {
    matches!(
        e,
        NetError::Io(_)
            | NetError::WebSocket(_)
            | NetError::Timeout
            | NetError::Closed(_)
            | NetError::Noise(_)
            | NetError::Protocol(_)
            | NetError::ServerKeyMismatch { .. }
            | NetError::KillSwitch
    )
}

impl From<NetError> for ClientError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::Auth(f) => Self::Auth {
                code: auth_code(f),
                message: f.to_string(),
            },
            NetError::ServerKeyMismatch { .. } => Self::ServerKeyMismatch,
            NetError::Io(_)
            | NetError::WebSocket(_)
            | NetError::Timeout
            | NetError::Closed(_)
            | NetError::KillSwitch => Self::Unreachable(e.to_string()),
            NetError::Storage(m) => Self::Storage(m),
            NetError::InvalidPairingPayload(m) => Self::InvalidPairing(m),
            NetError::MessageTooLarge { .. }
            | NetError::Noise(_)
            | NetError::InvalidKey(_)
            | NetError::Encoding(_)
            | NetError::Protocol(_)
            | NetError::NonceExhausted
            | NetError::InvalidArgument(_) => Self::Protocol(e.to_string()),
        }
    }
}
