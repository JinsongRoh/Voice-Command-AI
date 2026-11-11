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
//! They use [`send_unconnected_retry`], which resends only when no
//! connection was made at all, so the request cannot have reached a server.
//!
//! [`client_builder`] gives every speech HTTP client the same network
//! limits: a dead path fails the connect within [`CONNECT_TIMEOUT`] instead
//! of the whole request timeout, TCP keep-alive probes notice a silently
//! dropped connection, and idle pooled connections (which may have died in
//! a network change) are closed after [`POOL_IDLE_TIMEOUT`].
//!
//! Errors leave without their URL: a Google API key travels as `?key=`.

use std::future::Future;
use std::time::Duration;

/// Pause before the single retry.
pub const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Longest wait for a TCP (and TLS) connection to a speech or LLM server.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Idle time before TCP keep-alive probes start on an open connection.
pub const TCP_KEEPALIVE: Duration = Duration::from_secs(15);

/// Pooled connections idle for this long are closed, not reused.
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// A client builder with the shared network limits (see the module docs),
/// the request `timeout` and the speech redirect policy.
pub fn client_builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .tcp_keepalive(TCP_KEEPALIVE)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .redirect(crate::auth::redirect_policy())
}

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

/// Sends a request built by `build`, and once more after [`RETRY_DELAY`]
/// only when the first attempt made no connection (refused, unreachable or
/// a connect timeout): the request never left, so it cannot be billed
/// twice. For non-idempotent requests such as LLM completions. A returned
/// error carries no URL.
pub async fn send_unconnected_retry<F, Fut>(
    mut build: F,
) -> Result<reqwest::Response, reqwest::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    match build().await {
        Err(err) if err.is_connect() && err.status().is_none() => {
            tracing::debug!(
                timeout = err.is_timeout(),
                "request could not connect; retrying once"
            );
            tokio::time::sleep(RETRY_DELAY).await;
            build().await.map_err(reqwest::Error::without_url)
        }
        other => other.map_err(reqwest::Error::without_url),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// A local address nothing listens on.
    async fn closed_port() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}/")
    }

    #[test]
    fn the_builder_sets_the_network_limits() {
        assert!(client_builder(Duration::from_secs(60)).build().is_ok());
        // A request timeout below the connect timeout caps the connect.
        assert!(client_builder(Duration::from_secs(1)).build().is_ok());
    }

    #[tokio::test]
    async fn a_refused_connection_is_sent_once_more() {
        let url = closed_port().await;
        let client = client_builder(Duration::from_secs(5)).build().unwrap();
        for idempotent in [true, false] {
            let calls = AtomicUsize::new(0);
            let build = || {
                calls.fetch_add(1, Ordering::SeqCst);
                client.get(&url).send()
            };
            let err = if idempotent {
                send_idempotent(build).await.unwrap_err()
            } else {
                send_unconnected_retry(build).await.unwrap_err()
            };
            assert!(err.is_connect());
            assert_eq!(calls.load(Ordering::SeqCst), 2, "idempotent={idempotent}");
        }
    }

    #[tokio::test]
    async fn an_answer_is_never_sent_again() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = client_builder(Duration::from_secs(5)).build().unwrap();
        let calls = AtomicUsize::new(0);
        let resp = send_unconnected_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            client.get(server.uri()).send()
        })
        .await
        .unwrap();
        assert_eq!(resp.status(), 503);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_timeout_after_sending_is_not_retried_by_the_llm_path() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let client = client_builder(Duration::from_millis(200)).build().unwrap();
        let calls = AtomicUsize::new(0);
        let err = send_unconnected_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            client.get(server.uri()).send()
        })
        .await
        .unwrap_err();
        assert!(err.is_timeout());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
