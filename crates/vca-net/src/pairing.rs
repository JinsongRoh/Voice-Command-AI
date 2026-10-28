//! QR pairing payload and one-time pairing secrets (SEC-01).
//!
//! The desktop shows a QR code encoding a [`PairingPayload`] URI:
//!
//! ```text
//! vca://pair?v=1&k=<server key>&id=<server id>&n=<server name>&s=<secret>&x=<expiry>&e=<endpoint>&e=...
//! ```
//!
//! * `k`: server static X25519 public key (base64url). The device pins it
//!   for the Noise handshake, so a malicious tunnel cannot intercept pairing.
//! * `id`: server id, derived from the key (first 8 bytes of its SHA-256,
//!   hex). Checked on parse as a typo/corruption guard.
//! * `s`: one-time pairing secret (base64url, 144 bits), valid for five
//!   minutes and for a single successful use.
//! * `x`: expiry, Unix seconds.
//! * `e`: connection endpoints (`ws://`/`wss://` URLs from connectivity providers), 1 to 8.

use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;
use vca_core::UnixMillis;
use zeroize::Zeroizing;

use crate::clock::SharedClock;
use crate::error::{NetError, Result};
use crate::keys::PublicKey;
use crate::registry::Scope;
use crate::util::{b64d, b64e, fill_random, hex};

/// Payload format version.
pub const PAIRING_VERSION: u32 = 1;
/// Bytes of entropy in a pairing secret (144 bits).
pub const PAIRING_SECRET_LEN: usize = 18;
/// Minimum accepted secret length when parsing (128 bits).
pub const MIN_PAIRING_SECRET_LEN: usize = 16;
/// Default lifetime of a pairing secret.
pub const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);

const MAX_ENDPOINTS: usize = 8;
const MAX_ENDPOINT_LEN: usize = 512;
const MAX_NAME_CHARS: usize = 64;
const MAX_URI_LEN: usize = 4096;
/// Maximum number of simultaneously outstanding pairing tickets.
const MAX_OUTSTANDING: usize = 16;

/// Server id derived from its static key.
pub fn server_id(key: &PublicKey) -> String {
    hex(&key.fingerprint().as_bytes()[..8])
}

/// Contents of a pairing QR code.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingPayload {
    pub server_key: PublicKey,
    pub server_id: String,
    pub server_name: String,
    pub secret: Zeroizing<Vec<u8>>,
    /// Expiry, Unix seconds.
    pub expires_at: u64,
    pub endpoints: Vec<String>,
}

impl std::fmt::Debug for PairingPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingPayload")
            .field("server_key", &self.server_key)
            .field("server_id", &self.server_id)
            .field("server_name", &self.server_name)
            .field("secret", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("endpoints", &self.endpoints)
            .finish()
    }
}

fn validate_endpoint(e: &str) -> Result<()> {
    if e.len() > MAX_ENDPOINT_LEN {
        return Err(NetError::InvalidPairingPayload("endpoint too long".into()));
    }
    let url = Url::parse(e)
        .map_err(|_| NetError::InvalidPairingPayload("invalid endpoint URL".into()))?;
    if !matches!(url.scheme(), "ws" | "wss") || url.host().is_none() {
        return Err(NetError::InvalidPairingPayload(
            "endpoint must be a ws:// or wss:// URL".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetError::InvalidPairingPayload(
            "endpoint must not contain credentials".into(),
        ));
    }
    Ok(())
}

impl PairingPayload {
    /// Encodes the payload as a compact, URL-safe `vca://pair?...` URI.
    pub fn to_uri(&self) -> String {
        let mut url = Url::parse("vca://pair").expect("static URL");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("v", &PAIRING_VERSION.to_string())
                .append_pair("k", &self.server_key.to_base64url())
                .append_pair("id", &self.server_id)
                .append_pair("n", &self.server_name)
                .append_pair("s", &b64e(&self.secret))
                .append_pair("x", &self.expires_at.to_string());
            for e in &self.endpoints {
                q.append_pair("e", e);
            }
        }
        url.into()
    }

    /// Parses and strictly validates a pairing URI.
    pub fn parse(uri: &str) -> Result<Self> {
        let bad = |m: &str| NetError::InvalidPairingPayload(m.to_string());
        if uri.len() > MAX_URI_LEN {
            return Err(bad("payload too long"));
        }
        let url = Url::parse(uri.trim()).map_err(|_| bad("not a URI"))?;
        if url.scheme() != "vca" || url.host_str() != Some("pair") {
            return Err(bad("not a vca://pair URI"));
        }
        let (mut v, mut k, mut id, mut n, mut s, mut x) = (None, None, None, None, None, None);
        let mut endpoints = Vec::new();
        for (key, value) in url.query_pairs() {
            let slot = match key.as_ref() {
                "v" => &mut v,
                "k" => &mut k,
                "id" => &mut id,
                "n" => &mut n,
                "s" => &mut s,
                "x" => &mut x,
                "e" => {
                    if endpoints.len() == MAX_ENDPOINTS {
                        return Err(bad("too many endpoints"));
                    }
                    validate_endpoint(&value)?;
                    endpoints.push(value.into_owned());
                    continue;
                }
                // Unknown keys are ignored for forward compatibility within v1.
                _ => continue,
            };
            if slot.replace(value.into_owned()).is_some() {
                return Err(bad("duplicate parameter"));
            }
        }
        let version: u32 = v
            .ok_or_else(|| bad("missing version"))?
            .parse()
            .map_err(|_| bad("bad version"))?;
        if version != PAIRING_VERSION {
            return Err(bad("unsupported version"));
        }
        let server_key = PublicKey::from_base64url(&k.ok_or_else(|| bad("missing server key"))?)
            .map_err(|_| bad("invalid server key"))?;
        let server_id_value = id.ok_or_else(|| bad("missing server id"))?;
        if server_id_value != server_id(&server_key) {
            return Err(bad("server id does not match server key"));
        }
        let server_name = n.unwrap_or_default();
        if server_name.chars().count() > MAX_NAME_CHARS || server_name.chars().any(char::is_control)
        {
            return Err(bad("invalid server name"));
        }
        let secret = Zeroizing::new(
            b64d(&s.ok_or_else(|| bad("missing secret"))?).map_err(|_| bad("invalid secret"))?,
        );
        if secret.len() < MIN_PAIRING_SECRET_LEN || secret.len() > 64 {
            return Err(bad("secret has invalid length"));
        }
        let expires_at = x
            .ok_or_else(|| bad("missing expiry"))?
            .parse()
            .map_err(|_| bad("bad expiry"))?;
        if endpoints.is_empty() {
            return Err(bad("no endpoints"));
        }
        Ok(Self {
            server_key,
            server_id: server_id_value,
            server_name,
            secret,
            expires_at,
            endpoints,
        })
    }

    /// True when the payload is past its expiry at `now_secs`.
    pub fn is_expired(&self, now_secs: u64) -> bool {
        now_secs >= self.expires_at
    }
}

/// A freshly issued pairing secret.
#[derive(Clone)]
pub struct PairingTicket {
    pub secret: Zeroizing<Vec<u8>>,
    pub expires_at: UnixMillis,
    pub scope: Scope,
}

impl std::fmt::Debug for PairingTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingTicket")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Why a pairing secret was refused (kept server-side; the peer only sees
/// `InvalidPairingSecret`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedeemError {
    Invalid,
    Expired,
}

struct Outstanding {
    hash: [u8; 32],
    expires_at: UnixMillis,
    scope: Scope,
}

/// Issues and redeems one-time pairing secrets. Only SHA-256 hashes of
/// secrets are kept, and only in memory: a restart invalidates all
/// outstanding QR codes.
pub struct PairingManager {
    clock: SharedClock,
    ttl: Duration,
    outstanding: Mutex<Vec<Outstanding>>,
}

impl PairingManager {
    pub fn new(clock: SharedClock) -> Self {
        Self::with_ttl(clock, PAIRING_TTL)
    }

    pub fn with_ttl(clock: SharedClock, ttl: Duration) -> Self {
        Self {
            clock,
            ttl,
            outstanding: Mutex::new(Vec::new()),
        }
    }

    /// Issues a new single-use secret granting `scope` to the device that
    /// redeems it. The oldest outstanding ticket is dropped if too many are
    /// pending.
    pub fn issue(&self, scope: Scope) -> PairingTicket {
        let mut secret = Zeroizing::new(vec![0u8; PAIRING_SECRET_LEN]);
        fill_random(&mut secret);
        let now = self.clock.now_millis();
        let expires_at = now + self.ttl.as_millis() as u64;
        let mut out = self.outstanding.lock().expect("poisoned");
        out.retain(|o| o.expires_at > now);
        if out.len() >= MAX_OUTSTANDING {
            out.remove(0);
        }
        out.push(Outstanding {
            hash: Sha256::digest(&*secret).into(),
            expires_at,
            scope,
        });
        PairingTicket {
            secret,
            expires_at,
            scope,
        }
    }

    /// Builds the QR payload for a ticket.
    pub fn payload(
        &self,
        ticket: &PairingTicket,
        server_key: &PublicKey,
        server_name: &str,
        endpoints: Vec<String>,
    ) -> Result<PairingPayload> {
        if endpoints.is_empty() || endpoints.len() > MAX_ENDPOINTS {
            return Err(NetError::InvalidArgument(
                "1 to 8 endpoints required".into(),
            ));
        }
        for e in &endpoints {
            validate_endpoint(e)?;
        }
        Ok(PairingPayload {
            server_key: *server_key,
            server_id: server_id(server_key),
            server_name: crate::util::sanitize_display(server_name, MAX_NAME_CHARS),
            secret: ticket.secret.clone(),
            expires_at: ticket.expires_at.div_ceil(1000),
            endpoints,
        })
    }

    /// Redeems a secret: on success the ticket is consumed and its scope
    /// returned. Comparison is constant-time over all outstanding tickets.
    pub fn redeem(&self, secret: &[u8]) -> Result<Scope, RedeemError> {
        let now = self.clock.now_millis();
        let candidate: [u8; 32] = Sha256::digest(secret).into();
        let mut out = self.outstanding.lock().expect("poisoned");
        let mut found: Option<usize> = None;
        for (i, o) in out.iter().enumerate() {
            if bool::from(o.hash.ct_eq(&candidate)) {
                found = Some(i);
            }
        }
        let result = match found {
            None => Err(RedeemError::Invalid),
            Some(i) => {
                let o = out.remove(i);
                if o.expires_at <= now {
                    Err(RedeemError::Expired)
                } else {
                    Ok(o.scope)
                }
            }
        };
        out.retain(|o| o.expires_at > now);
        result
    }

    /// Cancels every outstanding ticket.
    pub fn cancel_all(&self) {
        self.outstanding.lock().expect("poisoned").clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::keys::StaticKeypair;

    fn sample(mgr: &PairingManager) -> (PairingTicket, PairingPayload) {
        let key = *StaticKeypair::generate().public();
        let t = mgr.issue(Scope::Command);
        let p = mgr
            .payload(
                &t,
                &key,
                "My Desktop",
                vec![
                    "wss://vca-k3x9q2.example.net/v1/ws".into(),
                    "ws://192.168.1.10:47800/v1/ws".into(),
                ],
            )
            .unwrap();
        (t, p)
    }

    #[test]
    fn uri_roundtrip() {
        let mgr = PairingManager::new(ManualClock::new(1_700_000_000_000));
        let (t, p) = sample(&mgr);
        let uri = p.to_uri();
        assert!(uri.starts_with("vca://pair?v=1&"));
        assert!(
            uri.len() < 400,
            "QR payload should stay compact: {}",
            uri.len()
        );
        let back = PairingPayload::parse(&uri).unwrap();
        assert_eq!(back, p);
        assert_eq!(*back.secret, *t.secret);
        assert_eq!(back.expires_at, 1_700_000_000 + 300);
        assert!(!back.is_expired(1_700_000_000));
        assert!(back.is_expired(1_700_000_300));
        assert!(!format!("{back:?}").contains(&b64e(&t.secret)));
    }

    #[test]
    fn parse_rejects_malformed_payloads() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let (_, p) = sample(&mgr);
        let uri = p.to_uri();
        let cases = [
            uri.replace("vca://pair", "https://pair"),
            uri.replace("vca://pair", "vca://other"),
            uri.replace("v=1", "v=2"),
            uri.replace("v=1", "v=x"),
            uri.replace(&format!("k={}", p.server_key.to_base64url()), "k=AAAA"),
            uri.replace(&format!("id={}", p.server_id), "id=0000000000000000"),
            uri.replace(&format!("s={}", b64e(&p.secret)), "s=AAAAAAAA"),
            format!("{uri}&v=1"),
            format!("{uri}&e=http%3A%2F%2Fexample.com"),
            format!("{uri}&e=ws%3A%2F%2Fuser%3Apw%40example.com"),
            uri.split("&e=").next().unwrap().to_string(),
            "garbage".to_string(),
            format!("{uri}&e={}", "a".repeat(5000)),
        ];
        for (i, c) in cases.iter().enumerate() {
            assert!(
                PairingPayload::parse(c).is_err(),
                "case {i} should fail: {c}"
            );
        }
        let mut many = uri.clone();
        for _ in 0..8 {
            many.push_str("&e=ws%3A%2F%2Fh%2Fv1%2Fws");
        }
        assert!(PairingPayload::parse(&many).is_err());
        // Unknown parameters are tolerated.
        assert!(PairingPayload::parse(&format!("{uri}&future=1")).is_ok());
    }

    #[test]
    fn secret_is_single_use() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::Admin);
        assert_eq!(t.secret.len(), PAIRING_SECRET_LEN);
        assert_eq!(mgr.redeem(&t.secret), Ok(Scope::Admin));
        assert_eq!(mgr.redeem(&t.secret), Err(RedeemError::Invalid));
    }

    #[test]
    fn wrong_secret_rejected() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::View);
        let mut wrong = t.secret.to_vec();
        wrong[0] ^= 1;
        assert_eq!(mgr.redeem(&wrong), Err(RedeemError::Invalid));
        assert_eq!(mgr.redeem(&[]), Err(RedeemError::Invalid));
        // The genuine one still works afterwards.
        assert_eq!(mgr.redeem(&t.secret), Ok(Scope::View));
    }

    #[test]
    fn expired_secret_rejected() {
        let clock = ManualClock::new(0);
        let mgr = PairingManager::new(clock.clone());
        let t = mgr.issue(Scope::Command);
        clock.advance_secs(5 * 60);
        assert_eq!(mgr.redeem(&t.secret), Err(RedeemError::Expired));
        assert_eq!(mgr.redeem(&t.secret), Err(RedeemError::Invalid));
    }

    #[test]
    fn multiple_tickets_and_cancel() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let a = mgr.issue(Scope::View);
        let b = mgr.issue(Scope::Admin);
        assert_eq!(mgr.redeem(&b.secret), Ok(Scope::Admin));
        mgr.cancel_all();
        assert_eq!(mgr.redeem(&a.secret), Err(RedeemError::Invalid));
    }

    #[test]
    fn outstanding_tickets_are_bounded() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let first = mgr.issue(Scope::View);
        for _ in 0..MAX_OUTSTANDING {
            mgr.issue(Scope::View);
        }
        assert_eq!(mgr.redeem(&first.secret), Err(RedeemError::Invalid));
    }

    #[test]
    fn payload_rejects_bad_endpoints() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::View);
        let key = *StaticKeypair::generate().public();
        assert!(mgr.payload(&t, &key, "x", vec![]).is_err());
        assert!(mgr.payload(&t, &key, "x", vec!["ftp://h".into()]).is_err());
    }
}
