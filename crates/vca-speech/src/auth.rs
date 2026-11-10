//! How a request to an OpenAI-compatible engine carries its credentials.
//!
//! By default the engine's API key goes in `Authorization: Bearer <key>`.
//! An engine can sit behind a gateway or reverse proxy that authenticates
//! every request with its own bearer token (for example a zero-trust
//! tunnel) and strips `Authorization` before forwarding. Then the engine
//! key moves to another header ([`EngineAuth::key_header`], sent raw) and
//! the gateway token takes `Authorization` ([`EngineAuth::gateway_token`]).
//!
//! Neither secret is ever printed: [`EngineAuth`]'s `Debug` redacts the
//! token, and both values are marked sensitive on the request.

use reqwest::RequestBuilder;
use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue};

use crate::error::{Result, SpeechError};

/// Longest accepted key header name.
const MAX_HEADER_CHARS: usize = 64;
/// Redirects followed within one host.
const MAX_REDIRECTS: usize = 5;

/// Why a key may not go to a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyUrlError {
    /// A private network address, and the engine does not allow plain http.
    NeedsPlainHttpFlag,
    /// Plain http to a public address, or to a name that says nothing about
    /// where it leads.
    PublicHttp,
}

impl std::fmt::Display for KeyUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NeedsPlainHttpFlag => {
                "this private network address is plain http, so the key would travel \
                 unencrypted: turn on \"Allow plain http on my private network\" for this \
                 engine, or use https"
            }
            Self::PublicHttp => {
                "a key cannot go to a public address over plain http: use https, or http on \
                 this computer"
            }
        })
    }
}

/// How plain http to `host` stands: `Some(true)` is always fine, `Some(false)`
/// needs the engine's flag, `None` is refused.
fn plain_http_allowed(host: &str) -> Option<bool> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return Some(true);
    }
    let ip: std::net::IpAddr = host.parse().ok()?;
    let ip = ip.to_canonical();
    match ip {
        std::net::IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            // 100.64.0.0/10, the range Tailscale uses.
            if v4.is_loopback() || (a == 100 && (64..128).contains(&b)) {
                Some(true)
            } else if v4.is_private() || v4.is_link_local() {
                Some(false)
            } else {
                None
            }
        }
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            if v6.is_loopback() {
                Some(true)
            } else if (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 {
                Some(false)
            } else {
                None
            }
        }
    }
}

/// May a key or token go to `url`? https always; plain http to this computer
/// and to the Tailscale range; to a private network address only with the
/// engine's `allow_plain_http`. Everything else is refused, also for a
/// profile saved before this rule.
pub fn check_key_url(url: &str, allow_plain_http: bool) -> std::result::Result<(), KeyUrlError> {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return Ok(());
    };
    if parsed.scheme() != "http" {
        return Ok(());
    }
    match parsed.host_str().and_then(plain_http_allowed) {
        Some(true) => Ok(()),
        Some(false) if allow_plain_http => Ok(()),
        Some(false) => Err(KeyUrlError::NeedsPlainHttpFlag),
        None => Err(KeyUrlError::PublicHttp),
    }
}

/// Is `url` plain http to a private network address (where the flag means
/// anything)?
pub fn is_private_plain_http(url: &str) -> bool {
    check_key_url(url, false) == Err(KeyUrlError::NeedsPlainHttpFlag)
}

/// The redirect policy of every client that sends an API key. reqwest drops
/// `Authorization` on a redirect to another host, but not a custom key
/// header, so such a redirect is not followed at all: the caller sees the
/// redirect as a failed request and the key stays home.
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_place = attempt.previous().first().is_some_and(|first| {
            first.scheme() == attempt.url().scheme()
                && first.host_str() == attempt.url().host_str()
                && first.port_or_known_default() == attempt.url().port_or_known_default()
        });
        if same_place && attempt.previous().len() <= MAX_REDIRECTS {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Headers the HTTP client manages itself; the key can never go there.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-type",
    "content-length",
    "transfer-encoding",
    "connection",
    "cookie",
];

/// Why a gateway token needs a key header.
pub const GATEWAY_NEEDS_KEY_HEADER: &str = "a gateway token is sent as `Authorization: Bearer \
     <token>`, so the engine's API key needs another header: set api_key_header (for example \
     X-VCA-Key) or remove the gateway token";

/// Where the API key and an optional gateway token go on each request.
/// The default sends the key as `Authorization: Bearer <key>`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EngineAuth {
    /// Header carrying the API key, sent raw (no `Bearer` prefix);
    /// `None` keeps `Authorization: Bearer <key>`.
    pub key_header: Option<String>,
    /// Sent as `Authorization: Bearer <token>` on every request. Needs a
    /// `key_header` other than `Authorization`.
    pub gateway_token: Option<String>,
}

// The token is never printed.
impl std::fmt::Debug for EngineAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineAuth")
            .field("key_header", &self.key_header)
            .field(
                "gateway_token",
                &self.gateway_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Checks a key header name: an HTTP header name of at most 64 characters
/// that the HTTP client does not manage itself.
pub fn check_key_header(name: &str) -> std::result::Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_HEADER_CHARS {
        return Err(format!(
            "the API key header must be 1-{MAX_HEADER_CHARS} characters"
        ));
    }
    if HeaderName::from_bytes(trimmed.as_bytes()).is_err() {
        return Err(format!(
            "{trimmed:?} is not an HTTP header name (letters, digits and - only, e.g. X-VCA-Key)"
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    if RESERVED_HEADERS.contains(&lower.as_str()) {
        return Err(format!(
            "{trimmed:?} cannot carry the API key; use a custom header such as X-VCA-Key"
        ));
    }
    Ok(())
}

/// The header is `Authorization` (in any case).
pub fn is_authorization(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(AUTHORIZATION.as_str())
}

impl EngineAuth {
    /// Today's behaviour: `Authorization: Bearer <key>`, no gateway.
    pub fn is_default(&self) -> bool {
        self.key_header.is_none() && self.gateway_token.is_none()
    }

    /// Rejects an invalid key header and a gateway token without a key
    /// header of its own (both would want `Authorization`).
    pub fn validate(&self) -> Result<()> {
        if let Some(h) = &self.key_header {
            check_key_header(h).map_err(SpeechError::Config)?;
        }
        if self.gateway_token.is_some() && self.key_header.as_deref().is_none_or(is_authorization) {
            return Err(SpeechError::Config(GATEWAY_NEEDS_KEY_HEADER.into()));
        }
        Ok(())
    }

    /// Adds the gateway token and the API key to `req`.
    pub fn apply(&self, mut req: RequestBuilder, api_key: Option<&str>) -> RequestBuilder {
        if let Some(token) = &self.gateway_token {
            req = req.bearer_auth(token);
        }
        let Some(key) = api_key else {
            return req;
        };
        match self.key_header.as_deref().map(str::trim) {
            None => req.bearer_auth(key),
            Some(name) => match (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(key),
            ) {
                (Ok(name), Ok(mut value)) => {
                    value.set_sensitive(true);
                    req.header(name, value)
                }
                // Rejected by `validate`; let the client report it without
                // the value.
                _ => req.header(name, "invalid"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_headers_are_checked() {
        for ok in ["X-VCA-Key", "x-api-key", "Authorization", "X_Key"] {
            assert!(check_key_header(ok).is_ok(), "{ok}");
        }
        for bad in ["", " ", "X VCA", "X:Key", "Host", "content-type", "é"] {
            assert!(check_key_header(bad).is_err(), "{bad}");
        }
        assert!(check_key_header(&"x".repeat(65)).is_err());
    }

    #[test]
    fn a_gateway_token_needs_its_own_key_header() {
        assert!(EngineAuth::default().validate().is_ok());
        let only_header = EngineAuth {
            key_header: Some("X-VCA-Key".into()),
            gateway_token: None,
        };
        assert!(only_header.validate().is_ok());
        for header in [None, Some("authorization".to_owned())] {
            let err = EngineAuth {
                key_header: header,
                gateway_token: Some("gw-secret-token".into()),
            }
            .validate()
            .unwrap_err()
            .to_string();
            assert!(err.contains("api_key_header"), "{err}");
            assert!(!err.contains("gw-secret-token"));
        }
    }

    #[test]
    fn debug_hides_the_gateway_token() {
        let auth = EngineAuth {
            key_header: Some("X-VCA-Key".into()),
            gateway_token: Some("gw-secret-token".into()),
        };
        let text = format!("{auth:?}");
        assert!(!text.contains("gw-secret-token"));
        assert!(text.contains("X-VCA-Key"));
    }

    /// A key must not follow a redirect to another host, in a custom header
    /// (which reqwest would keep) or in `Authorization`.
    #[tokio::test]
    async fn keys_do_not_follow_a_redirect_to_another_host() {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let thief = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&thief)
            .await;
        let origin = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(307).insert_header("location", thief.uri().as_str()),
            )
            .mount(&origin)
            .await;
        for auth in [
            EngineAuth {
                key_header: Some("X-VCA-Key".into()),
                gateway_token: None,
            },
            EngineAuth::default(),
        ] {
            let client = reqwest::Client::builder()
                .redirect(redirect_policy())
                .build()
                .unwrap();
            let resp = auth
                .apply(client.post(origin.uri()), Some("secret-key"))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 307, "the redirect stops here");
        }
        thief.verify().await;
    }

    #[tokio::test]
    async fn a_redirect_on_the_same_host_is_followed() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(path("/old"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/new"))
            .mount(&server)
            .await;
        Mock::given(path("/new"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = reqwest::Client::builder()
            .redirect(redirect_policy())
            .build()
            .unwrap();
        let resp = client
            .get(format!("{}/old", server.uri()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn where_a_key_may_go_over_plain_http() {
        let ok = |url, flag| check_key_url(url, flag).is_ok();
        for always in [
            "https://engine.example.com/v1",
            "http://127.0.0.1:8000/v1",
            "http://localhost:8000/v1",
            "http://[::1]:8000/v1",
            "http://100.64.0.1/v1",
            "http://100.127.255.254/v1",
        ] {
            assert!(ok(always, false), "{always}");
        }
        for private in [
            "http://192.168.0.10:8000/v1",
            "http://10.1.2.3/v1",
            "http://172.20.0.1/v1",
            "http://169.254.3.4/v1",
            "http://[fd00::5]/v1",
            "http://[fe80::1]/v1",
            "http://[::ffff:192.168.0.10]/v1",
        ] {
            assert_eq!(
                check_key_url(private, false),
                Err(KeyUrlError::NeedsPlainHttpFlag),
                "{private}"
            );
            assert!(ok(private, true), "{private}");
            assert!(is_private_plain_http(private), "{private}");
        }
        for public in [
            "http://203.0.113.9/v1",
            "http://100.128.0.1/v1",
            "http://engine.example.com/v1",
            "http://desk.local/v1",
            "http://[2001:db8::1]/v1",
        ] {
            assert_eq!(
                check_key_url(public, true),
                Err(KeyUrlError::PublicHttp),
                "{public}"
            );
        }
    }
}
