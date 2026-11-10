//! Small helpers shared by the server modules.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks a mutex, ignoring poisoning (the data is plain state).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The human-readable part of an API error.
pub(crate) fn error_text(err: &vca_core::Error) -> String {
    match err {
        vca_core::Error::Api { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// Shortens `text` to at most `max` characters (keeping the end when
/// `keep_end`), adding an ellipsis.
pub(crate) fn truncate_chars(text: &str, max: usize, keep_end: bool) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    if keep_end {
        let skip = count - max.saturating_sub(1);
        format!("…{}", text.chars().skip(skip).collect::<String>())
    } else {
        format!(
            "{}…",
            text.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_by_chars() {
        assert_eq!(truncate_chars("héllo", 10, false), "héllo");
        assert_eq!(truncate_chars("héllo", 3, false), "hé…");
        assert_eq!(truncate_chars("héllo", 3, true), "…lo");
    }
}
