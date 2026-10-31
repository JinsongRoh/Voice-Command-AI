//! Command failures and their exit codes.

use std::fmt;

/// Exit code for failed operations (vcad unreachable, API errors).
pub const EXIT_FAILURE: i32 = 1;
/// Exit code for invalid arguments, such as an unknown or ambiguous session.
pub const EXIT_USAGE: i32 = 2;

/// A failed command: the message is printed as `error: <message>` on stderr
/// and the process exits with `exit_code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub exit_code: i32,
    pub message: String,
}

impl CliError {
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_FAILURE,
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_USAGE,
            message: message.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

pub type CliResult<T> = Result<T, CliError>;
