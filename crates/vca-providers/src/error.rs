//! Errors shared by connectivity providers.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The provider's software is not installed.
    #[error("{0} is not installed")]
    NotInstalled(&'static str),
    /// The user must sign in or activate the provider first.
    #[error("not signed in: {0}")]
    NotSignedIn(String),
    /// Configuration is invalid (bad URL, missing setting).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    /// A provider command failed.
    #[error("command failed: {0}")]
    Command(String),
    /// A provider API returned an error.
    #[error("provider API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },
    /// A provider API refused the request with a stable machine-readable
    /// code. The message is the provider's own text, with secrets removed.
    #[error("{message} ({code})")]
    Coded {
        status: u16,
        code: String,
        message: String,
    },
    /// Credentials were rejected.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The credentials are valid but do not allow this operation (missing
    /// scope, or a resource the caller may only read).
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("rate limited (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },
    #[error("not found: {0}")]
    NotFound(String),
    /// The request conflicts with the provider's current state (for
    /// example the DNSPilot Agent is not signed in, or already is).
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("could not decode response: {0}")]
    Decode(String),
    #[error("not supported on this platform: {0}")]
    UnsupportedPlatform(&'static str),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = ProviderError> = std::result::Result<T, E>;

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Timeout(e.to_string())
        } else if e.is_decode() {
            Self::Decode(e.to_string())
        } else {
            Self::Io(std::io::Error::other(e.to_string()))
        }
    }
}

impl From<serde_json::Error> for ProviderError {
    fn from(e: serde_json::Error) -> Self {
        Self::Decode(e.to_string())
    }
}
