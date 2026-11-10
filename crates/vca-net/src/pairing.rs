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
//! * `s`: one-time pairing secret (base64url, 144 bits), valid for 60
//!   seconds by default ([`DEFAULT_PAIRING_TTL`]) and for a single successful
//!   use. A device that presented the secret in time may still use it for
//!   TOTP retries for up to [`PAIRING_GRACE`] from its first presentation;
//!   the secret's expiry does not move for anyone else.
//! * `h`, `t`, `c` (optional): a DNSPilot invite for tunnels; see
//!   `vca_providers::dnspilot::invite`. The pairing itself ignores them, like
//!   every unknown parameter.
//! * `x`: expiry, Unix seconds.
//! * `e`: connection endpoints (`ws://`/`wss://` URLs from connectivity providers), 1 to 8.
//! * `tf` (optional): `1` when the server asks for a TOTP code at pairing
//!   (2-step verification is on). A display hint only, so the phone can mark
//!   the code field as required; it carries no secret and the server never
//!   relies on it.

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
pub const DEFAULT_PAIRING_TTL: Duration = Duration::from_secs(60);
/// Once a valid secret is presented before it expires, how long from that
/// first presentation the pairing may still finish (TOTP retries).
pub const PAIRING_GRACE: Duration = Duration::from_secs(2 * 60);

const MAX_ENDPOINTS: usize = 8;
const MAX_ENDPOINT_LEN: usize = 512;
const MAX_NAME_CHARS: usize = 64;
const MAX_URI_LEN: usize = 4096;
/// Maximum number of simultaneously outstanding pairing tickets.
const MAX_OUTSTANDING: usize = 16;
/// Wrong TOTP or backup codes one pairing secret survives; the last allowed
/// wrong code revokes it. A missing code does not count.
pub const MAX_PAIRING_CODE_ATTEMPTS: u32 = 5;
/// Devices whose in-time presentation of one secret is remembered.
const MAX_PRESENTERS: usize = 4;
/// Characters of the confirm code (80 bits), without the separators.
const CONFIRM_CHARS: usize = 16;

/// Server id derived from its static key.
pub fn server_id(key: &PublicKey) -> String {
    hex(&key.fingerprint().as_bytes()[..8])
}

/// The code a person compares between the PC and the phone before
/// pairing. It comes from the server's key: a forged link carries another
/// key, so it shows another code, and the person never types a live
/// authenticator code into a server they did not mean.
///
/// 80 bits (`XXXX-XXXX-XXXX-XXXX`), in Crockford base 32: letters that
/// cannot be misread, and never all digits like an authenticator code.
/// Forty bits could be matched offline with a key made for it; eighty
/// cannot. The first two groups are the 40-bit code of earlier versions, so
/// an older app shows the start of the same code.
pub fn confirm_code(key: &PublicKey) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let bytes = key.fingerprint();
    let bytes = bytes.as_bytes();
    // 10 bytes, big-endian: the first 40 bits come first.
    let mut bits = 0u128;
    for b in &bytes[..10] {
        bits = (bits << 8) | u128::from(*b);
    }
    let mut out = String::with_capacity(CONFIRM_CHARS + 3);
    for i in 0..CONFIRM_CHARS {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        let index = (bits >> (75 - i * 5)) & 31;
        out.push(ALPHABET[index as usize] as char);
    }
    out
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
    /// The server asks for a TOTP code at pairing (`tf=1`). A hint for the
    /// pairing form only; never security relevant.
    pub totp_required: bool,
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
            .field("totp_required", &self.totp_required)
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
    // A link opened on the phone must not make it dial addresses no PC
    // serves pairing on: link-local (169.254.0.0/16 holds cloud metadata
    // services, fe80::/10), unspecified, broadcast and multicast. Loopback
    // stays: it is how a PC tests its own server (and an emulator reaches
    // the PC through a forwarded port).
    let unusable = match url.host() {
        Some(url::Host::Ipv4(v4)) => {
            v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast()
        }
        // `[::ffff:169.254.169.254]` dials the IPv4 address: same rules.
        Some(url::Host::Ipv6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => {
                v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast()
            }
            None => {
                (v6.segments()[0] & 0xffc0) == 0xfe80 || v6.is_unspecified() || v6.is_multicast()
            }
        },
        _ => false,
    };
    if unusable {
        return Err(NetError::InvalidPairingPayload(
            "endpoint address cannot serve pairing".into(),
        ));
    }
    Ok(())
}

impl PairingPayload {
    /// See [`confirm_code`].
    pub fn confirm_code(&self) -> String {
        confirm_code(&self.server_key)
    }

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
            if self.totp_required {
                q.append_pair("tf", "1");
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
        let mut tf = None;
        let mut endpoints = Vec::new();
        for (key, value) in url.query_pairs() {
            let slot = match key.as_ref() {
                "v" => &mut v,
                "k" => &mut k,
                "id" => &mut id,
                "n" => &mut n,
                "s" => &mut s,
                "x" => &mut x,
                "tf" => &mut tf,
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
            // Any other value is ignored, like an unknown parameter.
            totp_required: tf.as_deref() == Some("1"),
        })
    }

    /// True when the payload is past its expiry at `now_secs`.
    pub fn is_expired(&self, now_secs: u64) -> bool {
        now_secs >= self.expires_at
    }

    /// True when no pairing with this payload can succeed any more. Past
    /// [`Self::is_expired`] the server may still accept a secret presented
    /// in time (TOTP retries, [`PAIRING_GRACE`]); only it knows, so clients
    /// must not refuse such a link themselves before this point.
    pub fn is_past_grace(&self, now_secs: u64) -> bool {
        now_secs >= self.expires_at.saturating_add(PAIRING_GRACE.as_secs())
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
    /// TOTP or backup codes tried with this secret so far (counted before
    /// the check, so parallel attempts cannot exceed the limit).
    code_attempts: u32,
    /// Of those, the ones that failed. Only failures revoke the secret: an
    /// attempt still being checked must not lose it to a parallel one.
    code_failures: u32,
    /// Devices (fingerprints of their keys) that presented the secret
    /// before it expired, with the time of their first presentation.
    presenters: Vec<([u8; 32], UnixMillis)>,
}

impl Outstanding {
    /// Until when `presenter` may use the secret: its expiry, extended to
    /// `grace` after that device's first presentation in time. Nobody
    /// else's deadline moves.
    fn deadline_for(&self, presenter: &[u8; 32], grace: Duration) -> UnixMillis {
        self.presenters
            .iter()
            .find(|(p, _)| bool::from(p.ct_eq(presenter)))
            .map_or(self.expires_at, |(_, at)| {
                self.expires_at
                    .max(at.saturating_add(grace.as_millis() as u64))
            })
    }

    /// The latest deadline of anyone: the ticket is kept until then.
    fn last_deadline(&self, grace: Duration) -> UnixMillis {
        self.presenters
            .iter()
            .map(|(_, at)| at.saturating_add(grace.as_millis() as u64))
            .fold(self.expires_at, UnixMillis::max)
    }
}

fn presenter_id(key: &PublicKey) -> [u8; 32] {
    *key.fingerprint().as_bytes()
}

/// Index of the ticket whose hash matches; constant-time over all tickets.
fn find(out: &[Outstanding], candidate: &[u8; 32]) -> Option<usize> {
    let mut found = None;
    for (i, o) in out.iter().enumerate() {
        if bool::from(o.hash.ct_eq(candidate)) {
            found = Some(i);
        }
    }
    found
}

/// Issues and redeems one-time pairing secrets. Only SHA-256 hashes of
/// secrets are kept, and only in memory: a restart invalidates all
/// outstanding QR codes.
pub struct PairingManager {
    clock: SharedClock,
    ttl: Duration,
    grace: Duration,
    outstanding: Mutex<Vec<Outstanding>>,
}

impl PairingManager {
    pub fn new(clock: SharedClock) -> Self {
        Self::with_ttl(clock, DEFAULT_PAIRING_TTL)
    }

    pub fn with_ttl(clock: SharedClock, ttl: Duration) -> Self {
        Self {
            clock,
            ttl,
            grace: PAIRING_GRACE,
            outstanding: Mutex::new(Vec::new()),
        }
    }

    /// Sets how long a secret presented in time stays usable from its first
    /// presentation (see [`PAIRING_GRACE`]).
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Lifetime of newly issued secrets.
    pub fn ttl(&self) -> Duration {
        self.ttl
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
        let grace = self.grace;
        out.retain(|o| o.last_deadline(grace) > now);
        if out.len() >= MAX_OUTSTANDING {
            out.remove(0);
        }
        out.push(Outstanding {
            hash: Sha256::digest(&*secret).into(),
            expires_at,
            scope,
            code_attempts: 0,
            code_failures: 0,
            presenters: Vec::new(),
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
        self.payload_with_hint(ticket, server_key, server_name, endpoints, false)
    }

    /// Like [`Self::payload`], with the `tf` hint that the server asks for a
    /// TOTP code at pairing.
    pub fn payload_with_hint(
        &self,
        ticket: &PairingTicket,
        server_key: &PublicKey,
        server_name: &str,
        endpoints: Vec<String>,
        totp_required: bool,
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
            // Rounded down: a phone must never take a code for valid in
            // the last second the server already refuses it.
            expires_at: ticket.expires_at / 1000,
            endpoints,
            totp_required,
        })
    }

    /// Checks a secret without consuming it and returns its scope. An
    /// expired ticket is dropped. Pairing checks the secret with this first
    /// and only [redeems](Self::redeem) it once every other check (the TOTP
    /// code) has passed, so a missing or mistyped code does not spend the QR
    /// code. Comparison is constant-time over all outstanding tickets.
    ///
    /// `presenter` is the key of the device presenting it: a presentation
    /// before expiry gives that device (only) the grace period.
    pub fn check(&self, secret: &[u8], presenter: &PublicKey) -> Result<Scope, RedeemError> {
        let now = self.clock.now_millis();
        let candidate: [u8; 32] = Sha256::digest(secret).into();
        let who = presenter_id(presenter);
        let mut out = self.outstanding.lock().expect("poisoned");
        let grace = self.grace;
        let result = match find(&out, &candidate) {
            None => Err(RedeemError::Invalid),
            Some(i) if out[i].deadline_for(&who, grace) <= now => {
                // Late for this device; others in their grace keep it.
                if out[i].last_deadline(grace) <= now {
                    out.remove(i);
                }
                Err(RedeemError::Expired)
            }
            Some(i) => {
                let o = &mut out[i];
                let known = o.presenters.iter().any(|(p, _)| bool::from(p.ct_eq(&who)));
                // Only a presentation before expiry starts a grace period.
                if !known && now < o.expires_at && o.presenters.len() < MAX_PRESENTERS {
                    o.presenters.push((who, now));
                }
                Ok(o.scope)
            }
        };
        out.retain(|o| o.last_deadline(grace) > now);
        result
    }

    /// Takes one of the [`MAX_PAIRING_CODE_ATTEMPTS`] code attempts of
    /// `secret` before the code is checked. `false` (nothing may be checked)
    /// when they are used up or the ticket is gone. Counting before the
    /// check means parallel connections cannot try more codes than allowed.
    /// A refused attempt leaves the ticket alone: the last allowed one may
    /// still be checking a right code.
    pub fn reserve_code_attempt(&self, secret: &[u8]) -> bool {
        let candidate: [u8; 32] = Sha256::digest(secret).into();
        let mut out = self.outstanding.lock().expect("poisoned");
        let Some(i) = find(&out, &candidate) else {
            return false;
        };
        if out[i].code_attempts >= MAX_PAIRING_CODE_ATTEMPTS {
            return false;
        }
        out[i].code_attempts += 1;
        true
    }

    /// Records that a reserved code attempt ([`Self::reserve_code_attempt`])
    /// failed. Returns `true` when it was the last allowed one and the
    /// ticket is now revoked (also when the ticket is already gone).
    pub fn record_failed_code(&self, secret: &[u8]) -> bool {
        let candidate: [u8; 32] = Sha256::digest(secret).into();
        let mut out = self.outstanding.lock().expect("poisoned");
        let Some(i) = find(&out, &candidate) else {
            return true;
        };
        out[i].code_failures += 1;
        if out[i].code_failures >= MAX_PAIRING_CODE_ATTEMPTS {
            out.remove(i);
            return true;
        }
        false
    }

    /// Redeems a secret for the device `presenter`: on success the ticket is
    /// consumed and its scope returned. Comparison is constant-time over all
    /// outstanding tickets.
    pub fn redeem(&self, secret: &[u8], presenter: &PublicKey) -> Result<Scope, RedeemError> {
        let now = self.clock.now_millis();
        let candidate: [u8; 32] = Sha256::digest(secret).into();
        let who = presenter_id(presenter);
        let mut out = self.outstanding.lock().expect("poisoned");
        let grace = self.grace;
        let result = match find(&out, &candidate) {
            None => Err(RedeemError::Invalid),
            Some(i) if out[i].deadline_for(&who, grace) <= now => {
                if out[i].last_deadline(grace) <= now {
                    out.remove(i);
                }
                Err(RedeemError::Expired)
            }
            Some(i) => Ok(out.remove(i).scope),
        };
        out.retain(|o| o.last_deadline(grace) > now);
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

    /// The pairing device's key.
    fn me() -> PublicKey {
        PublicKey::from_bytes([7; 32])
    }

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
        assert_eq!(back.expires_at, 1_700_000_000 + 60);
        assert!(!back.is_expired(1_700_000_000));
        assert!(back.is_expired(1_700_000_060));
        assert!(!back.is_past_grace(1_700_000_179));
        assert!(back.is_past_grace(1_700_000_180));
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
        // So is a DNSPilot invite: same payload, with or without it.
        let invite = format!("{uri}&h=vca.example.com&t=12&c=dpt_inv_x");
        let plain = PairingPayload::parse(&uri).unwrap();
        let with = PairingPayload::parse(&invite).unwrap();
        assert_eq!(with.endpoints, plain.endpoints);
        assert_eq!(*with.secret, *plain.secret);
    }

    #[test]
    fn secret_is_single_use() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::Admin);
        assert_eq!(t.secret.len(), PAIRING_SECRET_LEN);
        assert_eq!(mgr.redeem(&t.secret, &me()), Ok(Scope::Admin));
        assert_eq!(mgr.redeem(&t.secret, &me()), Err(RedeemError::Invalid));
    }

    #[test]
    fn check_does_not_consume() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::Command);
        assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::Command));
        assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::Command));
        assert_eq!(mgr.redeem(&t.secret, &me()), Ok(Scope::Command));
        assert_eq!(mgr.check(&t.secret, &me()), Err(RedeemError::Invalid));
    }

    #[test]
    fn default_ttl_is_one_minute() {
        let clock = ManualClock::new(0);
        let mgr = PairingManager::new(clock.clone());
        assert_eq!(mgr.ttl(), Duration::from_secs(60));
        let t = mgr.issue(Scope::Command);
        assert_eq!(t.expires_at, 60_000);
        clock.advance_secs(59);
        let late = mgr.issue(Scope::View);
        clock.advance_secs(1);
        // Never presented: gone at the expiry, and never re-opened.
        assert_eq!(mgr.check(&t.secret, &me()), Err(RedeemError::Expired));
        assert_eq!(mgr.check(&t.secret, &me()), Err(RedeemError::Invalid));
        assert_eq!(mgr.redeem(&late.secret, &me()), Ok(Scope::View));
    }

    #[test]
    fn presented_secret_gets_the_grace_period() {
        let clock = ManualClock::new(0);
        let mgr = PairingManager::new(clock.clone());
        let t = mgr.issue(Scope::Command);
        clock.advance_secs(30);
        // First presentation (waiting for the TOTP code) at 30 s.
        assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::Command));
        clock.advance_secs(60);
        // Past the 60 s expiry, but within 2 minutes of the presentation.
        assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::Command));
        clock.advance_secs(29);
        assert_eq!(mgr.redeem(&t.secret, &me()), Ok(Scope::Command));

        let u = mgr.issue(Scope::Command);
        assert_eq!(mgr.check(&u.secret, &me()), Ok(Scope::Command));
        clock.advance_secs(120);
        assert_eq!(mgr.check(&u.secret, &me()), Err(RedeemError::Expired));
        assert_eq!(mgr.redeem(&u.secret, &me()), Err(RedeemError::Invalid));
    }

    #[test]
    fn ttl_and_grace_are_configurable() {
        let clock = ManualClock::new(0);
        let mgr = PairingManager::with_ttl(clock.clone(), Duration::from_secs(10))
            .with_grace(Duration::from_secs(15));
        let t = mgr.issue(Scope::View);
        clock.advance_secs(9);
        assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::View));
        clock.advance_secs(15);
        assert_eq!(mgr.redeem(&t.secret, &me()), Err(RedeemError::Expired));
    }

    #[test]
    fn wrong_codes_revoke_the_secret() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::Command);
        let other = mgr.issue(Scope::View);
        for _ in 1..MAX_PAIRING_CODE_ATTEMPTS {
            assert!(mgr.reserve_code_attempt(&t.secret));
            assert!(!mgr.record_failed_code(&t.secret));
            assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::Command));
        }
        assert!(mgr.reserve_code_attempt(&t.secret));
        assert!(mgr.record_failed_code(&t.secret));
        assert_eq!(mgr.check(&t.secret, &me()), Err(RedeemError::Invalid));
        assert_eq!(mgr.redeem(&t.secret, &me()), Err(RedeemError::Invalid));
        // Other tickets are unaffected.
        assert_eq!(mgr.redeem(&other.secret, &me()), Ok(Scope::View));
        assert!(mgr.record_failed_code(&other.secret));
    }

    /// A parallel attempt refused for the limit must not revoke the secret
    /// while the last allowed attempt is still checking a right code; only
    /// failures revoke it.
    #[test]
    fn a_refused_parallel_attempt_keeps_the_last_one_alive() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::Command);
        for _ in 1..MAX_PAIRING_CODE_ATTEMPTS {
            assert!(mgr.reserve_code_attempt(&t.secret));
            assert!(!mgr.record_failed_code(&t.secret));
        }
        // The last attempt is being checked ...
        assert!(mgr.reserve_code_attempt(&t.secret));
        // ... when a retry arrives: refused, the ticket stays.
        assert!(!mgr.reserve_code_attempt(&t.secret));
        assert_eq!(mgr.redeem(&t.secret, &me()), Ok(Scope::Command));

        // An earlier attempt failing late does not revoke a later one either.
        let t = mgr.issue(Scope::View);
        for _ in 0..MAX_PAIRING_CODE_ATTEMPTS {
            assert!(mgr.reserve_code_attempt(&t.secret));
        }
        for _ in 1..MAX_PAIRING_CODE_ATTEMPTS {
            assert!(!mgr.record_failed_code(&t.secret));
        }
        assert_eq!(mgr.redeem(&t.secret, &me()), Ok(Scope::View));
    }

    /// Parallel connections each reserve before checking: never more
    /// checks than the limit, whatever the timing.
    #[test]
    fn parallel_code_attempts_never_exceed_the_limit() {
        let mgr = std::sync::Arc::new(PairingManager::new(ManualClock::new(0)));
        let t = mgr.issue(Scope::Command);
        let granted = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let threads: Vec<_> = (0..50)
            .map(|_| {
                let (mgr, granted, secret) = (mgr.clone(), granted.clone(), t.secret.clone());
                std::thread::spawn(move || {
                    if mgr.reserve_code_attempt(&secret) {
                        granted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for th in threads {
            th.join().unwrap();
        }
        assert_eq!(
            granted.load(std::sync::atomic::Ordering::SeqCst),
            MAX_PAIRING_CODE_ATTEMPTS
        );
    }

    /// Presenting a code just before it expires extends it for that device
    /// only, never for whoever else holds it.
    #[test]
    fn the_grace_period_belongs_to_the_device_that_presented_in_time() {
        let clock = ManualClock::new(0);
        let mgr = PairingManager::new(clock.clone());
        let t = mgr.issue(Scope::Command);
        let other = PublicKey::from_bytes([9; 32]);
        clock.advance_secs(59);
        assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::Command));
        clock.advance_secs(30);
        assert_eq!(mgr.check(&t.secret, &other), Err(RedeemError::Expired));
        assert_eq!(mgr.redeem(&t.secret, &other), Err(RedeemError::Expired));
        // The device that presented in time still may.
        assert_eq!(mgr.check(&t.secret, &me()), Ok(Scope::Command));
        assert_eq!(mgr.redeem(&t.secret, &me()), Ok(Scope::Command));
    }

    #[test]
    fn the_qr_expiry_is_rounded_down() {
        let clock = ManualClock::new(1_000_005);
        let mgr = PairingManager::new(clock.clone());
        let t = mgr.issue(Scope::View);
        let key = *StaticKeypair::generate().public();
        let p = mgr
            .payload(&t, &key, "x", vec!["ws://192.168.1.2/v1/ws".into()])
            .unwrap();
        assert_eq!(t.expires_at, 1_060_005);
        assert_eq!(p.expires_at, 1_060);
    }

    #[test]
    fn links_cannot_point_at_link_local_or_special_addresses() {
        for bad in [
            "ws://169.254.169.254/v1/ws",
            "ws://[fe80::1]/v1/ws",
            "ws://0.0.0.0/v1/ws",
            "ws://[::]/v1/ws",
            "ws://255.255.255.255/v1/ws",
            "ws://224.0.0.1/v1/ws",
            // IPv4-mapped IPv6 dials the IPv4 address.
            "ws://[::ffff:169.254.169.254]/v1/ws",
            "ws://[::ffff:0.0.0.0]/v1/ws",
            "ws://[::ffff:255.255.255.255]/v1/ws",
        ] {
            assert!(validate_endpoint(bad).is_err(), "{bad}");
        }
        for good in [
            "ws://[::ffff:192.168.1.10]/v1/ws",
            "ws://127.0.0.1:47800/v1/ws",
            "ws://[::1]/v1/ws",
            "ws://192.168.1.10:47800/v1/ws",
            "wss://vca.example.net/v1/ws",
            "ws://desk.local/v1/ws",
        ] {
            assert!(validate_endpoint(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn totp_hint_roundtrip() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let (t, plain) = sample(&mgr);
        assert!(!plain.totp_required);
        assert!(!plain.to_uri().contains("tf="));
        let key = plain.server_key;
        let hinted = mgr
            .payload_with_hint(&t, &key, "My Desktop", plain.endpoints.clone(), true)
            .unwrap();
        let uri = hinted.to_uri();
        assert!(uri.contains("&tf=1"), "{uri}");
        let back = PairingPayload::parse(&uri).unwrap();
        assert!(back.totp_required);
        assert_eq!(*back.secret, *plain.secret);
        // Other values are ignored; a duplicate is refused like any other.
        let odd = PairingPayload::parse(&plain.to_uri().replacen("&e=", "&tf=yes&e=", 1)).unwrap();
        assert!(!odd.totp_required);
        assert!(PairingPayload::parse(&format!("{uri}&tf=1")).is_err());
    }

    #[test]
    fn wrong_secret_rejected() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::View);
        let mut wrong = t.secret.to_vec();
        wrong[0] ^= 1;
        assert_eq!(mgr.redeem(&wrong, &me()), Err(RedeemError::Invalid));
        assert_eq!(mgr.redeem(&[], &me()), Err(RedeemError::Invalid));
        // The genuine one still works afterwards.
        assert_eq!(mgr.redeem(&t.secret, &me()), Ok(Scope::View));
    }

    #[test]
    fn expired_secret_rejected() {
        let clock = ManualClock::new(0);
        let mgr = PairingManager::new(clock.clone());
        let t = mgr.issue(Scope::Command);
        clock.advance_secs(60);
        assert_eq!(mgr.redeem(&t.secret, &me()), Err(RedeemError::Expired));
        assert_eq!(mgr.redeem(&t.secret, &me()), Err(RedeemError::Invalid));
    }

    #[test]
    fn multiple_tickets_and_cancel() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let a = mgr.issue(Scope::View);
        let b = mgr.issue(Scope::Admin);
        assert_eq!(mgr.redeem(&b.secret, &me()), Ok(Scope::Admin));
        mgr.cancel_all();
        assert_eq!(mgr.redeem(&a.secret, &me()), Err(RedeemError::Invalid));
    }

    #[test]
    fn outstanding_tickets_are_bounded() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let first = mgr.issue(Scope::View);
        for _ in 0..MAX_OUTSTANDING {
            mgr.issue(Scope::View);
        }
        assert_eq!(mgr.redeem(&first.secret, &me()), Err(RedeemError::Invalid));
    }

    #[test]
    fn payload_rejects_bad_endpoints() {
        let mgr = PairingManager::new(ManualClock::new(0));
        let t = mgr.issue(Scope::View);
        let key = *StaticKeypair::generate().public();
        assert!(mgr.payload(&t, &key, "x", vec![]).is_err());
        assert!(mgr.payload(&t, &key, "x", vec!["ftp://h".into()]).is_err());
    }

    #[test]
    fn the_confirm_code_follows_the_server_key() {
        let (one, two) = (StaticKeypair::generate(), StaticKeypair::generate());
        let (a, b) = (one.public(), two.public());
        let code = confirm_code(a);
        assert_eq!(code.len(), 19);
        for at in [4, 9, 14] {
            assert_eq!(code.as_bytes()[at], b'-');
        }
        assert!(code.chars().all(|c| c == '-' || c.is_ascii_alphanumeric()));
        // 80 bits: 16 characters of 5 bits.
        assert_eq!(code.chars().filter(|c| *c != '-').count() * 5, 80);
        // The start is the 40-bit code older apps show.
        let fp = a.fingerprint();
        let mut bits = 0u64;
        for b in &fp.as_bytes()[..5] {
            bits = (bits << 8) | u64::from(*b);
        }
        let alphabet = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let old: String = (0..8)
            .map(|i| alphabet[((bits >> (35 - i * 5)) & 31) as usize] as char)
            .collect();
        assert_eq!(code.replace('-', "")[..8], old);
        assert_eq!(code, confirm_code(a), "stable");
        assert_ne!(code, confirm_code(b), "another key, another code");
    }
}
