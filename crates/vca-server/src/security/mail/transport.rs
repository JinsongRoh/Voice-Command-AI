//! Mail transports: direct SMTP (lettre with rustls) and an HTTP relay.

use std::time::Duration;

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use vca_speech::auth::EngineAuth;

use super::TlsMode;

/// How long one send may take.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// What a mail is about (the relay receives it as `kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind {
    Test,
    DevicePaired,
    AuthFailures,
    TotpChanged,
    RecoveryCode,
    RecoveryStarted,
    RecoveryCompleted,
    MailSettingsChanged,
}

/// One message to send. This is also the relay's request body: it rejects
/// unknown fields, so nothing else may be serialized here.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct OutgoingMail {
    pub to: Vec<String>,
    pub subject: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    pub kind: MailKind,
}

impl std::fmt::Debug for OutgoingMail {
    // Recovery mails carry a code: never print bodies.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutgoingMail")
            .field("to", &self.to.len())
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    #[error("mail is not set up on this computer")]
    NotConfigured,
    #[error("invalid mail settings: {0}")]
    Invalid(String),
    #[error("sending failed: {0}")]
    Failed(String),
    /// The relay refused an address (`invalid_recipient`).
    #[error("the mail relay refused the recipient address")]
    InvalidRecipient,
    /// The relay is rate limiting this computer (`rate_limited`).
    #[error("the mail relay is rate limiting; try again later")]
    RateLimited,
}

#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError>;
}

/// Direct SMTP settings (with the password from the credential store).
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub username: Option<String>,
    pub password: Option<String>,
    pub sender_address: String,
    pub sender_name: Option<String>,
}

pub struct SmtpTransport {
    inner: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpTransport {
    pub fn new(cfg: SmtpConfig) -> Result<Self, MailError> {
        let tls = match cfg.tls {
            TlsMode::None => Tls::None,
            mode => {
                let params = TlsParameters::new(cfg.host.clone())
                    .map_err(|e| MailError::Invalid(e.to_string()))?;
                if mode == TlsMode::Tls {
                    Tls::Wrapper(params)
                } else {
                    Tls::Required(params)
                }
            }
        };
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host)
            .port(cfg.port)
            .tls(tls)
            .timeout(Some(SEND_TIMEOUT));
        if let (Some(user), Some(pass)) = (cfg.username, cfg.password) {
            builder = builder.credentials(Credentials::new(user, pass));
        }
        let address = cfg
            .sender_address
            .parse()
            .map_err(|_| MailError::Invalid("sender_address is not a mail address".into()))?;
        Ok(Self {
            inner: builder.build(),
            from: Mailbox::new(cfg.sender_name, address),
        })
    }
}

/// Builds the MIME message (plain text, plus HTML when given).
pub fn build_message(from: &Mailbox, mail: &OutgoingMail) -> Result<Message, MailError> {
    let mut builder = Message::builder().from(from.clone()).subject(&mail.subject);
    for to in &mail.to {
        let mb: Mailbox = to
            .parse()
            .map_err(|_| MailError::Invalid(format!("{to:?} is not a mail address")))?;
        builder = builder.to(mb);
    }
    let msg = match &mail.html {
        Some(html) => builder.multipart(MultiPart::alternative_plain_html(
            mail.text.clone(),
            html.clone(),
        )),
        None => builder
            .header(ContentType::TEXT_PLAIN)
            .body(mail.text.clone()),
    };
    msg.map_err(|e| MailError::Invalid(e.to_string()))
}

#[async_trait]
impl MailTransport for SmtpTransport {
    async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        if mail.to.is_empty() {
            return Err(MailError::NotConfigured);
        }
        let msg = build_message(&self.from, mail)?;
        self.inner
            .send(msg)
            .await
            .map(|_| ())
            .map_err(|e| MailError::Failed(e.to_string()))
    }
}

/// Path of the relay API below its base URL.
pub const RELAY_PATH: &str = "/v1/notify/mail";

/// The endpoint for a configured relay URL (a base URL, or the full path).
pub fn relay_endpoint(url: &str) -> String {
    let base = url.trim().trim_end_matches('/');
    if base.ends_with(RELAY_PATH) {
        base.to_owned()
    } else {
        format!("{base}{RELAY_PATH}")
    }
}

/// HTTP relay settings. The relay decides the sender; credentials work as
/// for speech engines ([`EngineAuth`]).
pub struct RelayConfig {
    pub url: String,
    pub key: String,
    pub auth: EngineAuth,
}

pub struct RelayTransport {
    client: reqwest::Client,
    cfg: RelayConfig,
}

/// `{ok, id, deduplicated?}`, or an error with a code.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RelayReply {
    ok: Option<bool>,
    code: Option<String>,
    error: Option<String>,
}

/// Maps a relay refusal to an error. The body is not echoed: only the
/// documented codes are recognized.
fn relay_failure(status: u16, reply: &RelayReply) -> MailError {
    let code = reply.code.as_deref().or(reply.error.as_deref());
    match code {
        Some("invalid_recipient") => MailError::InvalidRecipient,
        Some("rate_limited") => MailError::RateLimited,
        Some("send_failed") => MailError::Failed("the relay could not deliver the mail".into()),
        _ if status == 429 => MailError::RateLimited,
        _ => MailError::Failed(format!("the relay answered {status}")),
    }
}

impl RelayTransport {
    pub fn new(cfg: RelayConfig) -> Result<Self, MailError> {
        cfg.auth
            .validate()
            .map_err(|e| MailError::Invalid(e.to_string()))?;
        let client = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| MailError::Failed(e.to_string()))?;
        Ok(Self { client, cfg })
    }
}

#[async_trait]
impl MailTransport for RelayTransport {
    async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        if mail.to.is_empty() {
            return Err(MailError::NotConfigured);
        }
        let req = self.client.post(relay_endpoint(&self.cfg.url)).json(mail);
        let resp = self
            .cfg
            .auth
            .apply(req, Some(&self.cfg.key))
            .send()
            .await
            .map_err(|e| MailError::Failed(e.without_url().to_string()))?;
        let status = resp.status();
        let reply: RelayReply = resp.json().await.unwrap_or_default();
        if status.is_success() && reply.ok == Some(true) {
            return Ok(());
        }
        Err(relay_failure(status.as_u16(), &reply))
    }
}

/// Records what would have been sent (tests).
#[derive(Default)]
pub struct MemoryTransport {
    pub sent: std::sync::Mutex<Vec<OutgoingMail>>,
    pub fail: std::sync::atomic::AtomicBool,
}

impl MemoryTransport {
    pub fn sent(&self) -> Vec<OutgoingMail> {
        self.sent.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl MailTransport for MemoryTransport {
    async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(MailError::Failed("memory transport set to fail".into()));
        }
        self.sent.lock().expect("poisoned").push(mail.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    fn sample(kind: MailKind) -> OutgoingMail {
        OutgoingMail {
            to: vec!["owner@example.com".into(), "second@example.com".into()],
            subject: "[VCA] 테스트 메일".into(),
            text: "plain body".into(),
            html: Some("<p>html body</p>".into()),
            kind,
        }
    }

    /// A minimal SMTP server: accepts one message and returns the
    /// conversation (commands and data).
    async fn mock_smtp(listener: TcpListener) -> Vec<String> {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        let mut log = Vec::new();
        w.write_all(b"220 mock ESMTP ready\r\n").await.unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let cmd = line.trim_end().to_owned();
            log.push(cmd.clone());
            let upper = cmd.to_ascii_uppercase();
            if upper.starts_with("EHLO") {
                w.write_all(b"250-mock\r\n250 8BITMIME\r\n").await.unwrap();
            } else if upper.starts_with("DATA") {
                w.write_all(b"354 go ahead\r\n").await.unwrap();
                loop {
                    line.clear();
                    r.read_line(&mut line).await.unwrap();
                    if line == ".\r\n" {
                        break;
                    }
                    log.push(line.trim_end().to_owned());
                }
                w.write_all(b"250 queued\r\n").await.unwrap();
            } else if upper.starts_with("QUIT") {
                w.write_all(b"221 bye\r\n").await.unwrap();
                break;
            } else {
                w.write_all(b"250 ok\r\n").await.unwrap();
            }
        }
        log
    }

    #[tokio::test]
    async fn smtp_sends_to_a_local_mock_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(mock_smtp(listener));
        let t = SmtpTransport::new(SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            tls: TlsMode::None,
            username: None,
            password: None,
            sender_address: "vca@example.com".into(),
            sender_name: Some("Voice Command AI".into()),
        })
        .unwrap();
        t.send(&sample(MailKind::Test)).await.unwrap();
        drop(t);
        let log = tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
        let joined = log.join("\n");
        assert!(joined.contains("MAIL FROM:<vca@example.com>"), "{joined}");
        assert!(joined.contains("RCPT TO:<owner@example.com>"));
        assert!(joined.contains("RCPT TO:<second@example.com>"));
        assert!(joined.contains("multipart/alternative"));
        assert!(joined.contains("From: \"Voice Command AI\" <vca@example.com>"));
    }

    #[tokio::test]
    async fn smtp_reports_a_refused_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let t = SmtpTransport::new(SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            tls: TlsMode::None,
            username: None,
            password: None,
            sender_address: "vca@example.com".into(),
            sender_name: None,
        })
        .unwrap();
        assert!(matches!(
            t.send(&sample(MailKind::Test)).await,
            Err(MailError::Failed(_))
        ));
    }

    #[test]
    fn message_without_html_is_plain_text() {
        let from: Mailbox = "vca@example.com".parse().unwrap();
        let mut mail = sample(MailKind::RecoveryCode);
        mail.html = None;
        let text = String::from_utf8(build_message(&from, &mail).unwrap().formatted()).unwrap();
        assert!(text.contains("Content-Type: text/plain"));
        mail.to = vec!["not an address".into()];
        assert!(build_message(&from, &mail).is_err());
        assert!(!format!("{mail:?}").contains("plain body"));
    }

    fn relay(url: String, header: Option<&str>, gateway: Option<&str>) -> RelayTransport {
        RelayTransport::new(RelayConfig {
            url,
            key: "relay-key-1".into(),
            auth: EngineAuth {
                key_header: header.map(str::to_owned),
                gateway_token: gateway.map(str::to_owned),
            },
        })
        .unwrap()
    }

    #[test]
    fn relay_endpoint_accepts_a_base_or_the_full_path() {
        for url in [
            "https://relay.example.com",
            "https://relay.example.com/",
            "https://relay.example.com/v1/notify/mail",
        ] {
            assert_eq!(
                relay_endpoint(url),
                "https://relay.example.com/v1/notify/mail"
            );
        }
    }

    #[tokio::test]
    async fn relay_posts_json_with_key_header_and_gateway_token() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/notify/mail"))
            .and(header("X-VCA-Key", "relay-key-1"))
            .and(header("Authorization", "Bearer gw-token-1"))
            .and(body_json(serde_json::json!({
                "to": ["owner@example.com", "second@example.com"],
                "subject": "[VCA] 테스트 메일",
                "text": "plain body",
                "html": "<p>html body</p>",
                "kind": "device_paired",
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "id": "m1"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        relay(server.uri(), Some("X-VCA-Key"), Some("gw-token-1"))
            .send(&sample(MailKind::DevicePaired))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn relay_error_codes_are_mapped_and_bodies_not_echoed() {
        use wiremock::matchers::{header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let undelivered = MailError::Failed("the relay could not deliver the mail".into());
        let cases = [
            (
                400,
                r#"{"ok":false,"code":"invalid_recipient"}"#,
                MailError::InvalidRecipient,
            ),
            (
                429,
                r#"{"ok":false,"error":"rate_limited"}"#,
                MailError::RateLimited,
            ),
            (429, "slow down", MailError::RateLimited),
            (
                502,
                r#"{"ok":false,"code":"send_failed"}"#,
                undelivered.clone(),
            ),
            (200, r#"{"ok":false,"code":"send_failed"}"#, undelivered),
            // Only an explicit yes counts as sent.
            (
                200,
                "<html>a captive portal</html>",
                MailError::Failed("the relay answered 200".into()),
            ),
            (
                200,
                "{}",
                MailError::Failed("the relay answered 200".into()),
            ),
            (
                403,
                "secret detail",
                MailError::Failed("the relay answered 403".into()),
            ),
        ];
        for (status, body, expected) in cases {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(header("Authorization", "Bearer relay-key-1"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .mount(&server)
                .await;
            let err = relay(server.uri(), None, None)
                .send(&sample(MailKind::Test))
                .await
                .unwrap_err();
            assert_eq!(err, expected, "{status} {body}");
        }
    }

    #[test]
    fn relay_payload_has_only_the_documented_fields() {
        let keys = |mail: &OutgoingMail| {
            let value = serde_json::to_value(mail).unwrap();
            let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        let mut mail = sample(MailKind::Test);
        assert_eq!(keys(&mail), ["html", "kind", "subject", "text", "to"]);
        mail.html = None;
        assert_eq!(keys(&mail), ["kind", "subject", "text", "to"]);
    }

    #[test]
    fn relay_kinds_use_the_agreed_names() {
        for (kind, name) in [
            (MailKind::Test, "test"),
            (MailKind::DevicePaired, "device_paired"),
            (MailKind::AuthFailures, "auth_failures"),
            (MailKind::TotpChanged, "totp_changed"),
            (MailKind::RecoveryCode, "recovery_code"),
            (MailKind::RecoveryStarted, "recovery_started"),
            (MailKind::RecoveryCompleted, "recovery_completed"),
            (MailKind::MailSettingsChanged, "mail_settings_changed"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), name);
        }
    }

    #[test]
    fn a_gateway_token_needs_a_key_header() {
        assert!(
            RelayTransport::new(RelayConfig {
                url: "https://relay.example.com".into(),
                key: "k".into(),
                auth: EngineAuth {
                    key_header: None,
                    gateway_token: Some("g".into()),
                },
            })
            .is_err()
        );
    }
}
