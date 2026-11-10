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
    /// A DNSPilot step failed. `code` is one of the `dnspilot_*` codes
    /// documented on [`ClientError::code`].
    #[error("DNSPilot: {message}")]
    DnsPilot { code: &'static str, message: String },
    /// DNSPilot rate limited the request; `dnspilot_rate_limited`.
    #[error("DNSPilot: rate limited")]
    DnsPilotRateLimited {
        /// `Retry-After`, when the console sent it.
        retry_after_secs: Option<u64>,
    },
}

impl ClientError {
    /// Stable machine-readable code: `not_found`, `invalid_pairing`,
    /// `not_connected`, `unreachable`, `server_key_mismatch`, `protocol`,
    /// `storage`, `internal`; for [`ClientError::DnsPilot`] one of
    /// `dnspilot_not_connected` (connect DNSPilot first),
    /// `dnspilot_reauthorize` (the authorization ended; connect again),
    /// `dnspilot_denied`, `dnspilot_expired`, `dnspilot_forbidden`,
    /// `dnspilot_device_not_allowed` (the PC was not picked on approval),
    /// `dnspilot_not_found`, `dnspilot_conflict`, `dnspilot_rate_limited`,
    /// `dnspilot_disabled`, `dnspilot_unavailable`, `dnspilot_invalid`,
    /// `dnspilot_error`, `dnspilot_key_registration_failed` (the tunnel
    /// exists but this app's key is not on it), `dnspilot_bad_link`, and the
    /// invite errors `dnspilot_invite_invalid`, `dnspilot_bad_request`,
    /// `dnspilot_enroll_bad_signature`, `dnspilot_enroll_bad_cert`,
    /// `dnspilot_tunnel_not_app_mode`, `dnspilot_client_limit`,
    /// `dnspilot_invite_limit`; or for [`ClientError::Auth`] one of
    /// `revoked`, `totp_required`, `invalid_totp`, `totp_not_enrolled`,
    /// `auth_failed`, `invalid_pairing`, `pairing_attempts_exhausted` (too
    /// many wrong codes; the QR code is revoked), `already_paired`,
    /// `rate_limited`, `kill_switch`, `protocol`, `server_error`.
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
            Self::DnsPilot { code, .. } => code,
            Self::DnsPilotRateLimited { .. } => "dnspilot_rate_limited",
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
        AuthFailure::PairingAttemptsExhausted => "pairing_attempts_exhausted",
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
