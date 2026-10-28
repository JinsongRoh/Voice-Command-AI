//! Error type shared by the speech pipeline.

use vca_core::error::ErrorCode;

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    /// The audio could not be decoded or is in an unsupported layout.
    #[error("audio error: {0}")]
    Audio(String),
    /// The requested operation is not supported by this engine or build.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Invalid configuration (missing key, bad URL, ...).
    #[error("configuration error: {0}")]
    Config(String),
    /// Transport-level HTTP failure (connection refused, timeout, ...).
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// The remote STT service answered with an error status.
    #[error("speech service returned {status}: {message}")]
    Api { status: u16, message: String },
    /// The remote service answered with a body we could not understand.
    #[error("unexpected response: {0}")]
    Decode(String),
    /// A lexicon or pattern file could not be parsed.
    #[error("failed to parse {path}: {message}")]
    Parse { path: String, message: String },
    /// Local inference failed.
    #[error("engine error: {0}")]
    Engine(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl SpeechError {
    /// Maps the error to the shared protocol error code.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Audio(_) | Self::Parse { .. } => ErrorCode::InvalidRequest,
            Self::Unsupported(_) => ErrorCode::Unsupported,
            Self::Config(_) => ErrorCode::InvalidRequest,
            Self::Http(_) => ErrorCode::Unavailable,
            Self::Api { status, .. } => match status {
                401 => ErrorCode::Unauthorized,
                403 => ErrorCode::Forbidden,
                404 => ErrorCode::NotFound,
                400 | 413 | 415 | 422 => ErrorCode::InvalidRequest,
                _ => ErrorCode::Unavailable,
            },
            Self::Decode(_) | Self::Engine(_) | Self::Io(_) => ErrorCode::Internal,
        }
    }
}

impl From<SpeechError> for vca_core::Error {
    fn from(err: SpeechError) -> Self {
        match err {
            SpeechError::Io(io) => vca_core::Error::Io(io),
            other => vca_core::Error::api(other.code(), other.to_string()),
        }
    }
}

/// Result alias for this crate.
pub type Result<T, E = SpeechError> = std::result::Result<T, E>;
