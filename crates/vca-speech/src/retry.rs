//! One retry for idempotent speech requests (transcription) whose connection
//! failed before the server answered: a restarting speech server or a short
//! network blip should not reach the user as an error.
//!
//! Only transport failures *before any response* are retried. Once the
//! server has answered (any status, 4xx or 5xx included) the answer stands.
//! A request that timed out after it was sent is not retried either: the
//! server may still be transcribing (and billing) it, and a second upload
//! would double both the wait and the cost.
//! LLM calls must not use this: a resent completion may be billed twice.
//!
//! Errors leave without their URL: a Google API key travels as `?key=`.

use std::future::Future;
use std::time::Duration;

/// Pause before the single retry.
pub const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Whether a `send()` failure happened before the server answered, so the
/// request can safely be sent again: connection refused or reset, the
/// request could not be written, or a timeout with no response yet.
pub fn is_transient(err: &reqwest::Error) -> bool {
    if err.status().is_some() || err.is_builder() || err.is_redirect() || err.is_decode() {
        return false;
    }
    err.is_connect() || err.is_request() || err.is_timeout()
}

/// Whether a failed `send()` is sent again: a [transient](is_transient)
/// failure, except a timeout waiting for the answer to a request that may
/// have been sent in full (a connect timeout is still retried).
pub fn should_retry(err: &reqwest::Error) -> bool {
    is_transient(err) && (err.is_connect() || !err.is_timeout())
}

/// Sends a request built by `build`, and once more after [`RETRY_DELAY`]
/// when the first attempt failed before it reached the server
/// ([`should_retry`]). `build` is called per attempt (multipart bodies
/// cannot be cloned). A returned error carries no URL.
pub async fn send_idempotent<F, Fut>(mut build: F) -> Result<reqwest::Response, reqwest::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    match build().await {
        Err(err) if should_retry(&err) => {
            // The URL can carry an API key (Google `key=`), so log the kind only.
            tracing::debug!(
                connect = err.is_connect(),
                timeout = err.is_timeout(),
                "speech request failed before a response; retrying once"
            );
            tokio::time::sleep(RETRY_DELAY).await;
            build().await.map_err(reqwest::Error::without_url)
        }
        other => other.map_err(reqwest::Error::without_url),
    }
}
