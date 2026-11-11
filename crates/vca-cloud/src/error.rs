//! Typed errors shared by every cloud adapter.

use std::time::Duration;

use vca_core::error::ErrorCode;

/// Errors returned by cloud adapters.
///
/// Messages never contain credentials: adapters build them from status
/// codes and provider error bodies only.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    /// The credential is missing, invalid, expired or lacks permission.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The provider throttled the request.
    #[error("rate limited by provider (retry after {retry_after:?})")]
    RateLimited {
        /// Delay requested by the provider (`Retry-After`), when given.
        retry_after: Option<Duration>,
    },
    /// The session or resource does not exist (or is not visible to the credential).
    #[error("not found: {0}")]
    NotFound(String),
    /// The request was rejected as invalid (bad repository, empty prompt, ...).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The provider does not offer this operation (see [`crate::CloudCapabilities`]).
    #[error("operation not supported by this provider: {0}")]
    Unsupported(&'static str),
    /// Any other provider-side failure.
    #[error("provider error (HTTP {status}): {message}")]
    Provider {
        /// HTTP status code, or 0 for non-HTTP failures.
        status: u16,
        message: String,
    },
    /// Network or TLS failure before a response arrived.
    #[error("transport error: {0}")]
    Transport(String),
    /// The response could not be understood.
    #[error("could not decode provider response: {0}")]
    Decode(String),
    /// A local command-line tool failed (Codex cloud).
    #[error("command failed: {0}")]
    Command(String),
}

impl CloudError {
    /// True when retrying later may succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Transport(_) => true,
            Self::Provider { status, .. } => *status == 0 || *status >= 500,
            _ => false,
        }
    }

    /// Maps the error onto the protocol-level error code.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Auth(_) => ErrorCode::Unauthorized,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::InvalidRequest(_) => ErrorCode::InvalidRequest,
            Self::Unsupported(_) => ErrorCode::Unsupported,
            Self::RateLimited { .. } | Self::Transport(_) | Self::Provider { .. } => {
                ErrorCode::Unavailable
            }
            Self::Decode(_) | Self::Command(_) => ErrorCode::Internal,
        }
    }
}

impl From<CloudError> for vca_core::Error {
    fn from(e: CloudError) -> Self {
        vca_core::Error::api(e.code(), e.to_string())
    }
}

impl From<reqwest::Error> for CloudError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_decode() {
            Self::Decode(e.to_string())
        } else {
            // reqwest error messages contain the URL but never headers, and
            // no adapter puts secrets into URLs.
            Self::Transport(e.to_string())
        }
    }
}

pub type Result<T, E = CloudError> = std::result::Result<T, E>;
