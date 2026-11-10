use vca_core::error::ErrorCode;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// Opening the pseudo-terminal or spawning the command failed.
    #[error("failed to spawn command in a pty: {0}")]
    Spawn(String),
    /// Resizing the pseudo-terminal failed.
    #[error("failed to resize pty: {0}")]
    Resize(String),
    /// The session's input side is closed (the PTY host shut down).
    #[error("pty session is closed")]
    Closed,
    /// The requested operation is not supported for this input or platform.
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<PtyError> for vca_core::Error {
    fn from(err: PtyError) -> Self {
        match err {
            PtyError::Io(e) => vca_core::Error::Io(e),
            PtyError::Unsupported(m) => vca_core::Error::api(ErrorCode::Unsupported, m),
            PtyError::Closed => vca_core::Error::api(ErrorCode::Unavailable, err.to_string()),
            other => vca_core::Error::api(ErrorCode::Internal, other.to_string()),
        }
    }
}
