//! Short-lived access tokens and rotating, device-bound refresh tokens
//! (SEC-04).
//!
//! Format: `vca1.<base64url(JSON claims)>.<base64url(HMAC-SHA256)>`, MAC
//! over `vca1.<claims>`. The MAC is verified (constant time) before the
//! claims are parsed.
//!
//! Tokens are only ever presented inside the Noise channel, and the server
//! additionally requires the token's device id to match the device whose
//! static key completed the handshake, so a stolen token is useless without
//! the device key.
//!
//! Refresh tokens belong to a *family* (one per sign-in). Every refresh
//! rotates the token; presenting an already-rotated refresh token is treated
//! as theft and revokes the whole family, including its access tokens.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use vca_core::UnixMillis;
use zeroize::Zeroizing;

use crate::clock::SharedClock;
use crate::error::{NetError, Result};
use crate::registry::Scope;
use crate::util::{b64d, b64e, random_hex, write_private_file};

const PREFIX: &str = "vca1.";
const MAX_TOKEN_LEN: usize = 1024;

/// Token lifetimes.
#[derive(Debug, Clone, Copy)]
pub struct TokenConfig {
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
}

impl Default for TokenConfig {
    fn default() -> Self {
        Self {
            access_ttl: Duration::from_secs(15 * 60),
            refresh_ttl: Duration::from_secs(30 * 24 * 60 * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Kind {
    #[serde(rename = "a")]
    Access,
    #[serde(rename = "r")]
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    typ: Kind,
    /// Device id.
    did: String,
    scp: Scope,
    /// Issued at / expires at (Unix millis).
    iat: UnixMillis,
    exp: UnixMillis,
    /// Token id.
    jti: String,
    /// Family id.
    fam: String,
}

/// Verified claims of an access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessClaims {
    pub device_id: String,
    pub scope: Scope,
    pub expires_at: UnixMillis,
    pub token_id: String,
    pub family_id: String,
}

/// An access/refresh token pair handed to a device.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenPair {
    pub access_token: String,
    pub access_expires_at: UnixMillis,
    pub refresh_token: String,
    pub refresh_expires_at: UnixMillis,
}

impl std::fmt::Debug for TokenPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenPair")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .finish_non_exhaustive()
    }
}

/// Why a token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("bad signature")]
    BadSignature,
    #[error("wrong token type")]
    WrongType,
    #[error("token expired")]
    Expired,
    #[error("token revoked")]
    Revoked,
    #[error("token bound to another device")]
    WrongDevice,
    #[error("refresh token reuse detected; token family revoked")]
    Reuse,
    #[error("storage failure")]
    Storage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Family {
    device_id: String,
    scope: Scope,
    current_jti: String,
    expires_at: UnixMillis,
    revoked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TokenState {
    version: u32,
    families: HashMap<String, Family>,
    /// Individually revoked access token ids -> their expiry.
    revoked_jtis: HashMap<String, UnixMillis>,
}

/// Issues and verifies tokens; holds refresh-token families and the
/// revocation list. Optionally persisted (JSON, owner-only).
pub struct TokenService {
    key: Zeroizing<Vec<u8>>,
    config: TokenConfig,
    clock: SharedClock,
    path: Option<PathBuf>,
    state: Mutex<TokenState>,
}

impl TokenService {
    /// Creates a volatile service. `key` must be at least 32 bytes (see
    /// [`crate::keys::load_or_generate_secret`] with
    /// [`crate::keys::TOKEN_KEY_NAME`]).
    pub fn new(key: Zeroizing<Vec<u8>>, config: TokenConfig, clock: SharedClock) -> Result<Self> {
        if key.len() < 32 {
            return Err(NetError::InvalidArgument(
                "token key must be at least 32 bytes".into(),
            ));
        }
        Ok(Self {
            key,
            config,
            clock,
            path: None,
            state: Mutex::new(TokenState {
                version: 1,
                ..Default::default()
            }),
        })
    }

    /// Creates a service persisted at `path` (families and revocations
    /// survive restarts).
    pub fn open(
        key: Zeroizing<Vec<u8>>,
        config: TokenConfig,
        clock: SharedClock,
        path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let mut svc = Self::new(key, config, clock)?;
        let path = path.into();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let state: TokenState = serde_json::from_slice(&bytes)
                    .map_err(|e| NetError::Storage(format!("corrupt token state: {e}")))?;
                if state.version != 1 {
                    return Err(NetError::Storage("unsupported token state version".into()));
                }
                svc.state = Mutex::new(state);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        svc.path = Some(path);
        Ok(svc)
    }

    pub fn config(&self) -> &TokenConfig {
        &self.config
    }

    fn mac(&self) -> Hmac<Sha256> {
        <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key).expect("HMAC accepts any key length")
    }

    fn sign(&self, claims: &Claims) -> String {
        let body = b64e(&serde_json::to_vec(claims).expect("claims serialize"));
        let signed = format!("{PREFIX}{body}");
        let mut mac = self.mac();
        mac.update(signed.as_bytes());
        let tag = mac.finalize().into_bytes();
        format!("{signed}.{}", b64e(&tag))
    }

    fn decode(&self, token: &str) -> Result<Claims, TokenError> {
        if token.len() > MAX_TOKEN_LEN || !token.starts_with(PREFIX) {
            return Err(TokenError::Malformed);
        }
        let (signed, tag) = token.rsplit_once('.').ok_or(TokenError::Malformed)?;
        if signed.len() <= PREFIX.len() {
            return Err(TokenError::Malformed);
        }
        let tag = b64d(tag).map_err(|_| TokenError::Malformed)?;
        let mut mac = self.mac();
        mac.update(signed.as_bytes());
        mac.verify_slice(&tag)
            .map_err(|_| TokenError::BadSignature)?;
        let body = b64d(&signed[PREFIX.len()..]).map_err(|_| TokenError::Malformed)?;
        serde_json::from_slice(&body).map_err(|_| TokenError::Malformed)
    }

    fn persist(&self, state: &TokenState) -> Result<(), TokenError> {
        if let Some(path) = &self.path {
            let json = serde_json::to_vec(state).map_err(|_| TokenError::Storage)?;
            write_private_file(path, &json).map_err(|_| TokenError::Storage)?;
        }
        Ok(())
    }

    fn gc(state: &mut TokenState, now: UnixMillis) {
        state.families.retain(|_, f| f.expires_at > now);
        state.revoked_jtis.retain(|_, exp| *exp > now);
    }

    fn mint(
        &self,
        device_id: &str,
        scope: Scope,
        family: &str,
        refresh_jti: &str,
        now: UnixMillis,
    ) -> TokenPair {
        let access_exp = now + self.config.access_ttl.as_millis() as u64;
        let refresh_exp = now + self.config.refresh_ttl.as_millis() as u64;
        let access = Claims {
            typ: Kind::Access,
            did: device_id.to_string(),
            scp: scope,
            iat: now,
            exp: access_exp,
            jti: random_hex(16),
            fam: family.to_string(),
        };
        let refresh = Claims {
            typ: Kind::Refresh,
            did: device_id.to_string(),
            scp: scope,
            iat: now,
            exp: refresh_exp,
            jti: refresh_jti.to_string(),
            fam: family.to_string(),
        };
        TokenPair {
            access_token: self.sign(&access),
            access_expires_at: access_exp,
            refresh_token: self.sign(&refresh),
            refresh_expires_at: refresh_exp,
        }
    }

    /// Issues a new token family for a freshly authenticated device.
    pub fn issue(&self, device_id: &str, scope: Scope) -> Result<TokenPair, TokenError> {
        let now = self.clock.now_millis();
        let family = random_hex(16);
        let jti = random_hex(16);
        let mut guard = self.state.lock().expect("poisoned");
        let mut next = guard.clone();
        Self::gc(&mut next, now);
        next.families.insert(
            family.clone(),
            Family {
                device_id: device_id.to_string(),
                scope,
                current_jti: jti.clone(),
                expires_at: now + self.config.refresh_ttl.as_millis() as u64,
                revoked: false,
            },
        );
        self.persist(&next)?;
        *guard = next;
        Ok(self.mint(device_id, scope, &family, &jti, now))
    }

    /// Verifies an access token presented by `device_id`.
    pub fn verify_access(&self, token: &str, device_id: &str) -> Result<AccessClaims, TokenError> {
        let claims = self.decode(token)?;
        if claims.typ != Kind::Access {
            return Err(TokenError::WrongType);
        }
        if claims.exp <= self.clock.now_millis() {
            return Err(TokenError::Expired);
        }
        if claims.did != device_id {
            return Err(TokenError::WrongDevice);
        }
        let state = self.state.lock().expect("poisoned");
        let family_ok = state
            .families
            .get(&claims.fam)
            .is_some_and(|f| !f.revoked && f.device_id == claims.did);
        if !family_ok || state.revoked_jtis.contains_key(&claims.jti) {
            return Err(TokenError::Revoked);
        }
        Ok(AccessClaims {
            device_id: claims.did,
            scope: claims.scp,
            expires_at: claims.exp,
            token_id: claims.jti,
            family_id: claims.fam,
        })
    }

    /// Rotates a refresh token presented by `device_id`. Reuse of an
    /// already-rotated token revokes the family and returns
    /// [`TokenError::Reuse`].
    pub fn refresh(&self, token: &str, device_id: &str) -> Result<TokenPair, TokenError> {
        let claims = self.decode(token)?;
        if claims.typ != Kind::Refresh {
            return Err(TokenError::WrongType);
        }
        let now = self.clock.now_millis();
        if claims.exp <= now {
            return Err(TokenError::Expired);
        }
        if claims.did != device_id {
            return Err(TokenError::WrongDevice);
        }
        let mut guard = self.state.lock().expect("poisoned");
        let mut next = guard.clone();
        let family = next
            .families
            .get_mut(&claims.fam)
            .ok_or(TokenError::Revoked)?;
        if family.revoked || family.device_id != claims.did {
            return Err(TokenError::Revoked);
        }
        if family.current_jti != claims.jti {
            family.revoked = true;
            self.persist(&next)?;
            *guard = next;
            return Err(TokenError::Reuse);
        }
        let jti = random_hex(16);
        family.current_jti = jti.clone();
        family.expires_at = now + self.config.refresh_ttl.as_millis() as u64;
        let scope = family.scope;
        Self::gc(&mut next, now);
        self.persist(&next)?;
        *guard = next;
        Ok(self.mint(&claims.did, scope, &claims.fam, &jti, now))
    }

    /// Revokes a single access token (by value) until it expires.
    pub fn revoke_access_token(&self, token: &str) -> Result<(), TokenError> {
        let claims = self.decode(token)?;
        let mut guard = self.state.lock().expect("poisoned");
        let mut next = guard.clone();
        next.revoked_jtis.insert(claims.jti, claims.exp);
        self.persist(&next)?;
        *guard = next;
        Ok(())
    }

    /// Revokes the family a token belongs to (sign-out).
    pub fn revoke_family_of(&self, token: &str) -> Result<(), TokenError> {
        let claims = self.decode(token)?;
        self.update_families(|id, _| id == claims.fam)
    }

    /// Revokes every token family of a device.
    pub fn revoke_device(&self, device_id: &str) -> Result<(), TokenError> {
        self.update_families(|_, f| f.device_id == device_id)
    }

    fn update_families(&self, pred: impl Fn(&str, &Family) -> bool) -> Result<(), TokenError> {
        let mut guard = self.state.lock().expect("poisoned");
        let mut next = guard.clone();
        for (id, f) in next.families.iter_mut() {
            if pred(id, f) {
                f.revoked = true;
            }
        }
        self.persist(&next)?;
        *guard = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use std::sync::Arc;

    fn svc(clock: Arc<ManualClock>) -> TokenService {
        TokenService::new(Zeroizing::new(vec![7u8; 32]), TokenConfig::default(), clock).unwrap()
    }

    #[test]
    fn issue_and_verify() {
        let clock = ManualClock::new(1_000_000);
        let s = svc(clock.clone());
        let pair = s.issue("dev_a", Scope::Command).unwrap();
        assert!(pair.access_token.starts_with("vca1."));
        let c = s.verify_access(&pair.access_token, "dev_a").unwrap();
        assert_eq!(c.scope, Scope::Command);
        assert_eq!(c.expires_at, 1_000_000 + 15 * 60 * 1000);
        assert_eq!(
            s.verify_access(&pair.access_token, "dev_b"),
            Err(TokenError::WrongDevice)
        );
        assert_eq!(
            s.verify_access(&pair.refresh_token, "dev_a"),
            Err(TokenError::WrongType)
        );
        assert!(!format!("{pair:?}").contains(&pair.access_token));
    }

    #[test]
    fn access_token_expires_after_15_minutes() {
        let clock = ManualClock::new(0);
        let s = svc(clock.clone());
        let pair = s.issue("d", Scope::View).unwrap();
        clock.advance_secs(15 * 60 - 1);
        assert!(s.verify_access(&pair.access_token, "d").is_ok());
        clock.advance_secs(1);
        assert_eq!(
            s.verify_access(&pair.access_token, "d"),
            Err(TokenError::Expired)
        );
    }

    #[test]
    fn tampering_and_foreign_keys_are_detected() {
        let clock = ManualClock::new(0);
        let s = svc(clock.clone());
        let pair = s.issue("d", Scope::View).unwrap();
        // Flip scope in the claims without re-signing.
        let (signed, tag) = pair.access_token.rsplit_once('.').unwrap();
        let body = b64d(&signed[PREFIX.len()..]).unwrap();
        let forged_body = String::from_utf8(body)
            .unwrap()
            .replace("\"view\"", "\"admin\"");
        let forged = format!("{PREFIX}{}.{tag}", b64e(forged_body.as_bytes()));
        assert_eq!(s.verify_access(&forged, "d"), Err(TokenError::BadSignature));
        let other = TokenService::new(Zeroizing::new(vec![8u8; 32]), TokenConfig::default(), clock)
            .unwrap();
        assert_eq!(
            other.verify_access(&pair.access_token, "d"),
            Err(TokenError::BadSignature)
        );
        for bad in [
            "",
            "vca1.",
            "vca1..",
            "xyz.abc.def",
            "vca1.@@@.@@@",
            &"a".repeat(5000),
        ] {
            assert!(s.verify_access(bad, "d").is_err(), "{bad}");
        }
        assert!(
            TokenService::new(
                Zeroizing::new(vec![0u8; 16]),
                TokenConfig::default(),
                ManualClock::new(0)
            )
            .is_err()
        );
    }

    #[test]
    fn refresh_rotates_and_reuse_revokes_family() {
        let clock = ManualClock::new(0);
        let s = svc(clock.clone());
        let p1 = s.issue("d", Scope::Command).unwrap();
        clock.advance_secs(60);
        let p2 = s.refresh(&p1.refresh_token, "d").unwrap();
        assert_ne!(p1.refresh_token, p2.refresh_token);
        assert!(s.verify_access(&p2.access_token, "d").is_ok());
        // Reusing the rotated token: theft signal.
        assert_eq!(s.refresh(&p1.refresh_token, "d"), Err(TokenError::Reuse));
        // Whole family is dead, including the newest tokens.
        assert_eq!(s.refresh(&p2.refresh_token, "d"), Err(TokenError::Revoked));
        assert_eq!(
            s.verify_access(&p2.access_token, "d"),
            Err(TokenError::Revoked)
        );
        assert_eq!(
            s.verify_access(&p1.access_token, "d"),
            Err(TokenError::Revoked)
        );
        // Other families are unaffected.
        let p3 = s.issue("d", Scope::Command).unwrap();
        assert!(s.verify_access(&p3.access_token, "d").is_ok());
    }

    #[test]
    fn refresh_checks_device_type_and_expiry() {
        let clock = ManualClock::new(0);
        let s = svc(clock.clone());
        let p = s.issue("d", Scope::View).unwrap();
        assert_eq!(
            s.refresh(&p.refresh_token, "other"),
            Err(TokenError::WrongDevice)
        );
        assert_eq!(s.refresh(&p.access_token, "d"), Err(TokenError::WrongType));
        clock.advance_secs(31 * 24 * 3600);
        assert_eq!(s.refresh(&p.refresh_token, "d"), Err(TokenError::Expired));
    }

    #[test]
    fn revocation() {
        let clock = ManualClock::new(0);
        let s = svc(clock);
        let a = s.issue("a", Scope::View).unwrap();
        let a2 = s.issue("a", Scope::View).unwrap();
        let b = s.issue("b", Scope::View).unwrap();
        s.revoke_access_token(&a.access_token).unwrap();
        assert_eq!(
            s.verify_access(&a.access_token, "a"),
            Err(TokenError::Revoked)
        );
        assert!(s.refresh(&a.refresh_token, "a").is_ok());
        s.revoke_family_of(&a2.refresh_token).unwrap();
        assert_eq!(s.refresh(&a2.refresh_token, "a"), Err(TokenError::Revoked));
        s.revoke_device("b").unwrap();
        assert_eq!(
            s.verify_access(&b.access_token, "b"),
            Err(TokenError::Revoked)
        );
        assert_eq!(s.refresh(&b.refresh_token, "b"), Err(TokenError::Revoked));
    }

    #[test]
    fn persisted_state_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let clock = ManualClock::new(0);
        let key = Zeroizing::new(vec![9u8; 32]);
        let s =
            TokenService::open(key.clone(), TokenConfig::default(), clock.clone(), &path).unwrap();
        let p1 = s.issue("d", Scope::Admin).unwrap();
        let p2 = s.refresh(&p1.refresh_token, "d").unwrap();
        drop(s);
        let s = TokenService::open(key, TokenConfig::default(), clock, &path).unwrap();
        assert!(s.verify_access(&p2.access_token, "d").is_ok());
        assert_eq!(s.refresh(&p1.refresh_token, "d"), Err(TokenError::Reuse));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains(&p2.refresh_token)
        );
    }
}
