//! Invite codes: how a phone registers its client key on a tunnel without
//! signing in to DNSPilot
//! (<https://dns.dnspilot.kr/docs/en/tunnel-app-mode>, "Enrolling additional
//! devices without login").
//!
//! A signed-in PC creates a single-use `dpt_inv_...` code, and the phone
//! proves possession of its key with a signature over
//! `"dpt-app-enroll:v1:" + code`. Two links carry the code:
//!
//! * the console's QR payload,
//!   `https://dns.dnspilot.kr/enroll#v=1&h=<hostname>&t=<tunnel_id>&c=<code>`
//!   (values in the fragment, so they never reach a server log);
//! * VCA's own pairing link, which adds the optional parameters `h`, `t` and
//!   `c` to `vca://pair?...`. Older apps ignore unknown parameters.
//!
//! A code is a secret until used: it is held in [`Secret`] (redacted `Debug`)
//! and parse errors never quote the input. The enrollment always goes to the
//! configured console; nothing in a link can change where a code is posted.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use url::Url;

use super::contract::{AppEnrollRequest, DEFAULT_CONSOLE_URL};
use super::normalize_hostname;
use crate::error::{ProviderError, Result};
use crate::secret::Secret;

/// What every invite code starts with.
pub const INVITE_CODE_PREFIX: &str = "dpt_inv_";
/// Characters after the prefix (`^dpt_inv_[A-Za-z0-9_-]{43}$`).
const INVITE_CODE_RANDOM_LEN: usize = 43;
/// Format version of the console's enroll link.
const ENROLL_LINK_VERSION: &str = "1";
/// What the enrollment signature covers, before the code.
const SIGNATURE_PREFIX: &str = "dpt-app-enroll:v1:";
/// PEM body line length (RFC 7468).
const PEM_LINE: usize = 64;

/// True for a well-formed `dpt_inv_...` code.
pub fn is_invite_code(code: &str) -> bool {
    code.strip_prefix(INVITE_CODE_PREFIX).is_some_and(|rest| {
        rest.len() == INVITE_CODE_RANDOM_LEN
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// Replaces every invite code in `text`, for error messages that may echo
/// a request.
pub fn redact_invite_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(INVITE_CODE_PREFIX) {
        let after = start + INVITE_CODE_PREFIX.len();
        out.push_str(&rest[..after]);
        out.push_str("<redacted>");
        let tail = &rest[after..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The bytes signed to prove possession of the client key.
pub fn enroll_message(code: &Secret) -> Vec<u8> {
    format!("{SIGNATURE_PREFIX}{}", code.expose()).into_bytes()
}

/// A certificate as a `CERTIFICATE` PEM block.
pub fn cert_pem_from_der(der: &[u8]) -> String {
    let b64 = B64.encode(der);
    let mut pem = String::with_capacity(b64.len() + b64.len() / PEM_LINE + 60);
    pem.push_str("-----BEGIN CERTIFICATE-----\n");
    for line in b64.as_bytes().chunks(PEM_LINE) {
        pem.push_str(&String::from_utf8_lossy(line));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}

/// The enrollment request: `signature` is the DER (ASN.1) ECDSA signature
/// over [`enroll_message`], `cert_der` the certificate later used for mTLS.
pub fn enroll_request(code: &Secret, cert_der: &[u8], signature: &[u8]) -> AppEnrollRequest {
    AppEnrollRequest {
        code: code.clone(),
        cert_pem: cert_pem_from_der(cert_der),
        sig: B64.encode(signature),
    }
}

/// The contents of an enroll link. `Debug` output hides the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollLink {
    /// Normalized tunnel host name.
    pub hostname: String,
    pub tunnel_id: i64,
    pub code: Secret,
}

fn bad_link() -> ProviderError {
    // Deliberately fixed text: the input may contain a code.
    ProviderError::InvalidConfig("this is not a valid DNSPilot invite link".into())
}

/// Collects `(key, value)` pairs, refusing a repeated `h`, `t`, `c` or `v`.
#[derive(Default)]
struct Fields {
    version: Option<String>,
    hostname: Option<String>,
    tunnel_id: Option<String>,
    code: Option<String>,
}

impl Fields {
    fn take(&mut self, key: &str, value: &str) -> Result<()> {
        let slot = match key {
            "v" => &mut self.version,
            "h" => &mut self.hostname,
            "t" => &mut self.tunnel_id,
            "c" => &mut self.code,
            _ => return Ok(()),
        };
        if slot.replace(value.to_owned()).is_some() {
            return Err(bad_link());
        }
        Ok(())
    }
}

impl EnrollLink {
    /// Validates the three fields.
    pub fn new(hostname: &str, tunnel_id: i64, code: Secret) -> Result<Self> {
        let hostname = normalize_hostname(hostname).map_err(|_| bad_link())?;
        if tunnel_id <= 0 || !is_invite_code(code.expose()) {
            return Err(bad_link());
        }
        Ok(Self {
            hostname,
            tunnel_id,
            code,
        })
    }

    fn from_values(hostname: Option<&str>, tunnel_id: Option<&str>, code: String) -> Result<Self> {
        let tunnel_id = tunnel_id
            .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|t| t.parse().ok())
            .ok_or_else(bad_link)?;
        Self::new(hostname.ok_or_else(bad_link)?, tunnel_id, Secret::new(code))
    }

    /// Parses the console's enroll link, strictly: exactly
    /// `https://dns.dnspilot.kr/enroll#...` (so no other scheme, host, port,
    /// path, user name or query), version 1, and all three values.
    pub fn parse_console_url(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        let prefix = format!("{DEFAULT_CONSOLE_URL}/enroll#");
        let fragment = raw.strip_prefix(prefix.as_str()).ok_or_else(bad_link)?;
        if fragment.contains('#') {
            return Err(bad_link());
        }
        let mut fields = Fields::default();
        for (key, value) in url::form_urlencoded::parse(fragment.as_bytes()) {
            fields.take(&key, &value)?;
        }
        if fields.version.as_deref() != Some(ENROLL_LINK_VERSION) {
            return Err(bad_link());
        }
        let code = fields.code.take().ok_or_else(bad_link)?;
        Self::from_values(
            fields.hostname.as_deref(),
            fields.tunnel_id.as_deref(),
            code,
        )
    }

    /// The invite a `vca://pair?...` link carries, if any. A link with only
    /// some of the fields, or with invalid ones, is an error.
    pub fn from_pairing_uri(raw: &str) -> Result<Option<Self>> {
        let url = Url::parse(raw.trim()).map_err(|_| bad_link())?;
        if url.scheme() != "vca" || url.host_str() != Some("pair") {
            return Err(bad_link());
        }
        let mut fields = Fields::default();
        for (key, value) in url.query_pairs() {
            // `v` is the pairing format version here, not the invite's.
            if key != "v" {
                fields.take(&key, &value)?;
            }
        }
        if fields.hostname.is_none() && fields.tunnel_id.is_none() && fields.code.is_none() {
            return Ok(None);
        }
        let code = fields.code.take().ok_or_else(bad_link)?;
        Self::from_values(
            fields.hostname.as_deref(),
            fields.tunnel_id.as_deref(),
            code,
        )
        .map(Some)
    }

    /// The invite in a link of either kind: the console's enroll link, or a
    /// `vca://pair?...` link (`None` when that link carries no invite).
    pub fn parse(raw: &str) -> Result<Option<Self>> {
        if raw.trim_start().starts_with("vca:") {
            Self::from_pairing_uri(raw)
        } else {
            Self::parse_console_url(raw).map(Some)
        }
    }

    /// `uri` with the optional invite parameters. Host names, ids and codes
    /// need no percent-encoding.
    pub fn append_to_pairing_uri(&self, uri: &str) -> String {
        format!(
            "{uri}&h={}&t={}&c={}",
            self.hostname,
            self.tunnel_id,
            self.code.expose()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> String {
        format!("dpt_inv_{}xyz", "aB3_-".repeat(8))
    }

    fn console_url() -> String {
        format!(
            "https://dns.dnspilot.kr/enroll#v=1&h=vca-abc.example.com&t=12&c={}",
            code()
        )
    }

    #[test]
    fn invite_codes_follow_the_documented_pattern() {
        assert!(is_invite_code(&code()));
        assert!(!is_invite_code("dpt_inv_short"));
        assert!(!is_invite_code(&format!("dpt_inv_{}", "a".repeat(44))));
        assert!(!is_invite_code(&format!("dpt_app_{}", "a".repeat(43))));
        assert!(!is_invite_code(&format!("dpt_inv_{}!", "a".repeat(42))));
    }

    #[test]
    fn console_links_are_accepted_exactly() {
        let link = EnrollLink::parse_console_url(&format!(" {} ", console_url())).unwrap();
        assert_eq!(link.hostname, "vca-abc.example.com");
        assert_eq!(link.tunnel_id, 12);
        assert_eq!(link.code.expose(), code());
        // Percent-encoded values, and an unknown field, are fine.
        let encoded = console_url().replace("vca-abc.example.com", "vca%2Dabc%2Eexample%2Ecom");
        assert_eq!(EnrollLink::parse_console_url(&encoded).unwrap(), link);
        assert!(EnrollLink::parse_console_url(&format!("{}&later=1", console_url())).is_ok());
    }

    #[test]
    fn other_links_are_rejected() {
        let good = console_url();
        let fragment = good.split_once('#').unwrap().1;
        let cases = [
            good.replace("https://", "http://"),
            good.replace("dns.dnspilot.kr", "dns.dnspilot.kr.evil.example"),
            good.replace("dns.dnspilot.kr", "evil.example"),
            good.replace("dns.dnspilot.kr", "dns.dnspilot.kr:443"),
            good.replace("dns.dnspilot.kr", "dns.dnspilot.kr@evil.example"),
            good.replace("dns.dnspilot.kr", "DNS.DNSPILOT.KR"),
            good.replace("/enroll", "/enroll/"),
            good.replace("/enroll", "/other"),
            good.replace("/enroll#", "/enroll?x=1#"),
            good.replace("v=1", "v=2"),
            good.replace("&t=12", "&t=0"),
            good.replace("&t=12", "&t=-12"),
            good.replace("&t=12", "&t=abc"),
            good.replace("&t=12", ""),
            good.replace("&h=vca-abc.example.com", ""),
            good.replace("&h=vca-abc.example.com", "&h=localhost"),
            good.replace("dpt_inv_", "dpt_app_"),
            good.replace(&code(), "dpt_inv_short"),
            format!("{good}&c={}", code()),
            format!("{good}#more"),
            format!("https://dns.dnspilot.kr/enroll?{fragment}"),
            String::new(),
        ];
        for (i, case) in cases.iter().enumerate() {
            let err = EnrollLink::parse_console_url(case).unwrap_err().to_string();
            assert!(!err.contains(&code()), "case {i} leaked the code: {err}");
        }
    }

    #[test]
    fn pairing_links_carry_optional_invites() {
        let base = "vca://pair?v=1&k=abc&id=00&n=pc&s=c2VjcmV0&x=1&e=wss%3A%2F%2Fa.example.com";
        assert_eq!(EnrollLink::from_pairing_uri(base).unwrap(), None);

        let link = EnrollLink::new("vca-abc.example.com", 12, code().as_str().into()).unwrap();
        let uri = link.append_to_pairing_uri(base);
        assert_eq!(
            EnrollLink::from_pairing_uri(&uri).unwrap().as_ref(),
            Some(&link)
        );
        assert!(EnrollLink::from_pairing_uri(&format!("{base}&h=a.example.com")).is_err());
        assert!(EnrollLink::from_pairing_uri(&format!("{uri}&t=13")).is_err());
        let bad = uri.replace("dpt_inv_", "dpt_xxx_");
        let err = EnrollLink::from_pairing_uri(&bad).unwrap_err().to_string();
        assert!(!err.contains("dpt_xxx_"), "{err}");
        assert!(EnrollLink::from_pairing_uri("https://pair?h=a.b&t=1&c=x").is_err());
    }

    #[test]
    fn either_kind_of_link_is_recognized() {
        let console = EnrollLink::parse(&console_url()).unwrap().unwrap();
        let pairing = console.append_to_pairing_uri("vca://pair?v=1&k=abc");
        assert_eq!(EnrollLink::parse(&pairing).unwrap(), Some(console));
        assert_eq!(EnrollLink::parse("vca://pair?v=1&k=abc").unwrap(), None);
        assert!(EnrollLink::parse("https://evil.example/enroll#v=1").is_err());
    }

    #[test]
    fn debug_output_hides_the_code() {
        let link = EnrollLink::parse_console_url(&console_url()).unwrap();
        assert!(!format!("{link:?}").contains(&code()));
    }

    #[test]
    fn codes_are_redacted_from_text() {
        let text = format!("bad code {} and {}!", code(), code());
        let clean = redact_invite_codes(&text);
        assert!(!clean.contains(&code()), "{clean}");
        assert_eq!(clean, "bad code dpt_inv_<redacted> and dpt_inv_<redacted>!");
        assert_eq!(redact_invite_codes("nothing here"), "nothing here");
    }

    #[test]
    fn request_uses_the_documented_encodings() {
        let code_secret = Secret::new(code());
        assert_eq!(
            enroll_message(&code_secret),
            format!("dpt-app-enroll:v1:{}", code()).into_bytes()
        );
        let der = vec![0x30u8; 100];
        let r = enroll_request(&code_secret, &der, b"signature");
        assert_eq!(r.sig, "c2lnbmF0dXJl");
        assert!(r.cert_pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(r.cert_pem.ends_with("-----END CERTIFICATE-----\n"));
        assert!(r.cert_pem.lines().all(|l| l.len() <= 64));
    }
}
