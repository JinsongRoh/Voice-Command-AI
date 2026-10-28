use serde::{Deserialize, Serialize};

/// Machine-readable error codes shared by the server and clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidRequest,
    Unauthorized,
    Forbidden,
    Conflict,
    Unsupported,
    Unavailable,
    Internal,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{code:?}: {message}")]
    Api { code: ErrorCode, message: String },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn api(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Api {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Api { code, .. } => *code,
            Self::Io(_) => ErrorCode::Internal,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
