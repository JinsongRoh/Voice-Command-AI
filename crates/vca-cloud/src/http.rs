//! Shared HTTP plumbing for the REST adapters.

use std::time::{Duration, SystemTime};

use reqwest::header::HeaderMap;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;

use crate::error::{CloudError, Result};

/// Default timeout for a single provider request.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds the HTTP client used by all adapters (rustls, no cookies).
pub fn default_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .user_agent(concat!("vca-cloud/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Joins a base URL (without trailing slash) and a path (with leading slash).
pub(crate) fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// Sends a request and maps non-success statuses to typed errors.
pub(crate) async fn send(req: RequestBuilder) -> Result<Response> {
    let resp = req.send().await?;
    if resp.status().is_success() {
        return Ok(resp);
    }
    Err(error_from_response(resp).await)
}

/// Sends a request and decodes a JSON body tolerantly.
pub(crate) async fn send_json<T: DeserializeOwned>(req: RequestBuilder) -> Result<T> {
    let resp = send(req).await?;
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        return serde_json::from_slice(b"{}").map_err(|e| CloudError::Decode(e.to_string()));
    }
    serde_json::from_slice(&bytes).map_err(|e| CloudError::Decode(e.to_string()))
}

/// Sends a request and ignores the body.
pub(crate) async fn send_empty(req: RequestBuilder) -> Result<()> {
    send(req).await.map(|_| ())
}

pub(crate) async fn error_from_response(resp: Response) -> CloudError {
    let status = resp.status();
    let retry_after = retry_after(resp.headers());
    let body = resp.text().await.unwrap_or_default();
    error_from_parts(status, retry_after, &body)
}

pub(crate) fn error_from_parts(
    status: StatusCode,
    retry_after: Option<Duration>,
    body: &str,
) -> CloudError {
    let message = extract_message(body).unwrap_or_else(|| {
        status
            .canonical_reason()
            .unwrap_or("unexpected status")
            .to_owned()
    });
    match status.as_u16() {
        401 | 403 => CloudError::Auth(message),
        404 => CloudError::NotFound(message),
        429 => CloudError::RateLimited { retry_after },
        400 | 409 | 422 => CloudError::InvalidRequest(message),
        s => CloudError::Provider { status: s, message },
    }
}

/// Pulls a human-readable message out of common error body shapes:
/// `{"error":{"message":..}}`, `{"error":".."}`, `{"message":..}`,
/// `{"detail":..}`. Long bodies are truncated.
fn extract_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let candidates = [
        v.pointer("/error/message"),
        v.get("message"),
        v.get("detail"),
        v.get("error_description"),
        v.get("error"),
    ];
    let msg = candidates
        .into_iter()
        .flatten()
        .find_map(|m| m.as_str().map(str::to_owned))?;
    Some(truncate(&msg, 500))
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Parses `Retry-After` (delta seconds or HTTP date).
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(raw).ok()?;
    Some(
        when.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderValue, RETRY_AFTER};

    #[test]
    fn maps_status_codes() {
        assert!(matches!(
            error_from_parts(StatusCode::UNAUTHORIZED, None, ""),
            CloudError::Auth(_)
        ));
        assert!(matches!(
            error_from_parts(StatusCode::NOT_FOUND, None, r#"{"error":{"message":"nope"}}"#),
            CloudError::NotFound(m) if m == "nope"
        ));
        assert!(matches!(
            error_from_parts(StatusCode::TOO_MANY_REQUESTS, Some(Duration::from_secs(3)), ""),
            CloudError::RateLimited { retry_after: Some(d) } if d.as_secs() == 3
        ));
        assert!(matches!(
            error_from_parts(StatusCode::BAD_GATEWAY, None, r#"{"message":"down"}"#),
            CloudError::Provider { status: 502, message } if message == "down"
        ));
    }

    #[test]
    fn parses_retry_after() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        assert_eq!(retry_after(&h), Some(Duration::from_secs(12)));
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&h), Some(Duration::ZERO));
        h.insert(RETRY_AFTER, HeaderValue::from_static("soon"));
        assert_eq!(retry_after(&h), None);
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("héllo", 2), "h…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
