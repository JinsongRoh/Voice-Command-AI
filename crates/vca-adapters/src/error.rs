use std::path::PathBuf;

use vca_core::error::ErrorCode;

/// Errors produced by agent adapters and terminal injection.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid TOML in {path}: {message}")]
    Toml { path: PathBuf, message: String },
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("`{program}` failed ({status}): {stderr}")]
    CommandFailed {
        program: String,
        status: String,
        stderr: String,
    },
    #[error("file watcher error: {0}")]
    Watch(#[from] notify::Error),
}

pub type Result<T, E = AdapterError> = std::result::Result<T, E>;

impl From<AdapterError> for vca_core::Error {
    fn from(err: AdapterError) -> Self {
        let code = match &err {
            AdapterError::Io(_) | AdapterError::Watch(_) | AdapterError::CommandFailed { .. } => {
                ErrorCode::Internal
            }
            AdapterError::Json { .. }
            | AdapterError::Toml { .. }
            | AdapterError::InvalidConfig(_) => ErrorCode::InvalidRequest,
            AdapterError::Unsupported(_) => ErrorCode::Unsupported,
            AdapterError::NotFound(_) => ErrorCode::NotFound,
        };
        vca_core::Error::api(code, err.to_string())
    }
}
