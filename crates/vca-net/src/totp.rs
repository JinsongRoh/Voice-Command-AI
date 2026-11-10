//! RFC 6238 TOTP (SEC-02): enrollment, verification with replay
//! protection, and backup codes.
//!
//! Parameters are the authenticator-app defaults: HMAC-SHA1, 6 digits,
//! 30-second steps, a 160-bit secret. Verification accepts the previous,
//! current and next step (±30 s clock skew) and rejects any step that is not
//! newer than the last accepted one, so a code cannot be used twice.
//!
//! Backup codes have 80 bits of entropy (16 base32 characters) and are
//! stored as salted SHA-256 hashes; at that entropy a fast hash is not
//! brute-forceable offline.

use std::path::PathBuf;
use std::sync::Mutex;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use vca_core::UnixMillis;
use zeroize::Zeroizing;

use crate::clock::SharedClock;
use crate::error::{NetError, Result};
use crate::util::{fill_random, write_private_file};

/// Step length in seconds.
pub const TOTP_STEP_SECS: u64 = 30;
/// Number of digits.
pub const TOTP_DIGITS: u32 = 6;
/// Secret length in bytes.
pub const TOTP_SECRET_LEN: usize = 20;
/// Number of backup codes generated per enrollment.
pub const BACKUP_CODE_COUNT: usize = 10;

const BACKUP_CODE_BYTES: usize = 10;

/// HOTP value (RFC 4226) for `counter`, truncated to `digits` digits.
pub fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    let mut mac =
        <Hmac<Sha1> as KeyInit>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([
        digest[offset],
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]) & 0x7fff_ffff;
    bin % 10u32.pow(digits)
}

/// TOTP code for a Unix time in seconds, formatted with leading zeros.
pub fn totp_code_at(secret: &[u8], unix_secs: u64) -> String {
    format!(
        "{:0width$}",
        hotp(secret, unix_secs / TOTP_STEP_SECS, TOTP_DIGITS),
        width = TOTP_DIGITS as usize
    )
}

fn base32_encode(bytes: &[u8]) -> String {
    data_encoding::BASE32_NOPAD.encode(bytes)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Builds an `otpauth://totp/...` URI understood by authenticator apps.
pub fn otpauth_uri(secret: &[u8], issuer: &str, account: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={}&period={}",
        percent_encode(issuer),
        percent_encode(account),
        base32_encode(secret),
        percent_encode(issuer),
        TOTP_DIGITS,
        TOTP_STEP_SECS
    )
}

/// Why a TOTP or backup code was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TotpError {
    #[error("TOTP is not enrolled")]
    NotEnrolled,
    #[error("invalid code")]
    Invalid,
    #[error("code already used")]
    Replayed,
    #[error("storage failure")]
    Storage,
}

/// An enrollment that has been started but not yet confirmed with a code.
#[derive(Clone)]
pub struct PendingEnrollment {
    secret: Zeroizing<Vec<u8>>,
    issuer: String,
    account: String,
    /// URI to show as a QR code to the authenticator app.
    pub otpauth_uri: Zeroizing<String>,
}

impl PendingEnrollment {
    /// Base32 secret for manual entry.
    pub fn secret_base32(&self) -> Zeroizing<String> {
        Zeroizing::new(base32_encode(&self.secret))
    }
}

impl std::fmt::Debug for PendingEnrollment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingEnrollment { .. }")
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct BackupCode {
    salt: String,
    hash: String,
    used_at: Option<UnixMillis>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Enrollment {
    /// Base32 secret.
    secret: String,
    enrolled_at: UnixMillis,
    last_used_step: Option<u64>,
    backup_codes: Vec<BackupCode>,
    /// The entry name in the authenticator app (absent in files written
    /// before it was recorded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
}

/// What the owner may see about the enrollment (never the secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpStatus {
    pub enrolled_at: UnixMillis,
    /// Issuer of the authenticator entry (`None` for older enrollments).
    pub issuer: Option<String>,
    /// Account label of the authenticator entry (`None` for older
    /// enrollments).
    pub account: Option<String>,
    pub backup_codes_remaining: usize,
    pub backup_codes_total: usize,
}

/// Which kind of code [`TotpManager::verify_code_or_backup`] accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    Totp,
    BackupCode,
}

/// True when `code` has the shape of an authenticator code (six digits,
/// spaces ignored) rather than a backup code.
pub fn looks_like_totp(code: &str) -> bool {
    let mut n = 0;
    for c in code.chars().filter(|c| !c.is_whitespace()) {
        if !c.is_ascii_digit() {
            return false;
        }
        n += 1;
    }
    n == TOTP_DIGITS as usize
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct TotpState {
    version: u32,
    enrollment: Option<Enrollment>,
}

impl Drop for TotpState {
    fn drop(&mut self) {
        if let Some(e) = self.enrollment.as_mut() {
            zeroize::Zeroize::zeroize(&mut e.secret);
        }
    }
}

/// Server-side TOTP state for the owner account. Optionally persisted to
/// a JSON file with owner-only permissions (it contains the TOTP secret).
pub struct TotpManager {
    clock: SharedClock,
    path: Option<PathBuf>,
    state: Mutex<TotpState>,
}

fn normalize_backup(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn hash_backup(salt: &[u8], normalized: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"vca-backup-code-v1");
    h.update(salt);
    h.update(normalized.as_bytes());
    h.finalize().into()
}

impl TotpManager {
    /// Volatile manager (tests).
    pub fn in_memory(clock: SharedClock) -> Self {
        Self {
            clock,
            path: None,
            state: Mutex::new(TotpState {
                version: 1,
                enrollment: None,
            }),
        }
    }

    /// Loads (or starts empty) a manager persisted at `path`.
    pub fn open(path: impl Into<PathBuf>, clock: SharedClock) -> Result<Self> {
        let path = path.into();
        let state = match std::fs::read(&path) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let s: TotpState = serde_json::from_slice(&bytes)
                    .map_err(|e| NetError::Storage(format!("corrupt TOTP state: {e}")))?;
                if s.version != 1 {
                    return Err(NetError::Storage("unsupported TOTP state version".into()));
                }
                s
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TotpState {
                version: 1,
                enrollment: None,
            },
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            clock,
            path: Some(path),
            state: Mutex::new(state),
        })
    }

    fn persist(&self, state: &TotpState) -> Result<()> {
        if let Some(path) = &self.path {
            let json = Zeroizing::new(serde_json::to_vec(state).map_err(NetError::encoding)?);
            write_private_file(path, &json)?;
        }
        Ok(())
    }

    /// Applies `f` to a copy of the state, persists it, then commits.
    fn mutate<T>(&self, f: impl FnOnce(&mut TotpState) -> T) -> Result<T> {
        let mut guard = self.state.lock().expect("poisoned");
        let mut next = guard.clone();
        let out = f(&mut next);
        self.persist(&next)?;
        *guard = next;
        Ok(out)
    }

    pub fn is_enrolled(&self) -> bool {
        self.state.lock().expect("poisoned").enrollment.is_some()
    }

    /// Starts enrollment: generates a secret and the `otpauth://` URI.
    /// Nothing changes until [`TotpManager::confirm_enrollment`] succeeds.
    pub fn begin_enrollment(&self, issuer: &str, account: &str) -> PendingEnrollment {
        let mut secret = Zeroizing::new(vec![0u8; TOTP_SECRET_LEN]);
        fill_random(&mut secret);
        let uri = Zeroizing::new(otpauth_uri(&secret, issuer, account));
        PendingEnrollment {
            secret,
            issuer: issuer.to_owned(),
            account: account.to_owned(),
            otpauth_uri: uri,
        }
    }

    /// Confirms enrollment with a code from the authenticator app, replacing
    /// any previous enrollment (which stays valid until this succeeds, so a
    /// failed re-enrollment never locks the owner out). Returns the plaintext backup codes, which
    /// must be shown to the user once and are not stored.
    pub fn confirm_enrollment(
        &self,
        pending: &PendingEnrollment,
        code: &str,
    ) -> Result<Vec<String>, TotpError> {
        let now_secs = self.clock.now_secs();
        let step = match_step(&pending.secret, code, now_secs).ok_or(TotpError::Invalid)?;
        let (stored, codes) = new_backup_codes();
        let enrollment = Enrollment {
            secret: base32_encode(&pending.secret),
            enrolled_at: self.clock.now_millis(),
            last_used_step: Some(step),
            backup_codes: stored,
            issuer: Some(pending.issuer.clone()),
            account: Some(pending.account.clone()),
        };
        self.mutate(|s| s.enrollment = Some(enrollment))
            .map_err(|_| TotpError::Storage)?;
        Ok(codes)
    }

    /// Does `code` fit the enrollment in progress? Checks only: nothing is
    /// stored, so the code still works for [`Self::confirm_enrollment`].
    pub fn enrollment_code_fits(&self, pending: &PendingEnrollment, code: &str) -> bool {
        match_step(&pending.secret, code, self.clock.now_secs()).is_some()
    }

    /// Removes the enrollment.
    pub fn disable(&self) -> Result<()> {
        self.mutate(|s| s.enrollment = None)
    }

    /// Takes in what another manager on the same file already used (the
    /// last accepted step, spent backup codes) for the same secret, so a
    /// second instance (an overlapping server during a restart or update)
    /// cannot accept a code the first one took. Only ever adds markers: an
    /// unreadable file or another secret changes nothing here.
    fn merge_used_from_disk(&self, state: &mut TotpState) {
        let Some(path) = &self.path else { return };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let bytes = Zeroizing::new(bytes);
        let Ok(disk) = serde_json::from_slice::<TotpState>(&bytes) else {
            return;
        };
        let (Some(mine), Some(theirs)) = (state.enrollment.as_mut(), disk.enrollment.as_ref())
        else {
            return;
        };
        if !bool::from(mine.secret.as_bytes().ct_eq(theirs.secret.as_bytes())) {
            return;
        }
        mine.last_used_step = mine.last_used_step.max(theirs.last_used_step);
        for bc in &mut mine.backup_codes {
            if bc.used_at.is_none()
                && let Some(used) = theirs
                    .backup_codes
                    .iter()
                    .find(|t| t.salt == bc.salt && t.hash == bc.hash)
                    .and_then(|t| t.used_at)
            {
                bc.used_at = Some(used);
            }
        }
    }

    /// Verifies a TOTP code. A code is accepted at most once, also across
    /// managers sharing the same file.
    pub fn verify(&self, code: &str) -> Result<(), TotpError> {
        let now_secs = self.clock.now_secs();
        let mut guard = self.state.lock().expect("poisoned");
        self.merge_used_from_disk(&mut guard);
        let enrollment = guard.enrollment.as_ref().ok_or(TotpError::NotEnrolled)?;
        let secret = Zeroizing::new(
            data_encoding::BASE32_NOPAD
                .decode(enrollment.secret.as_bytes())
                .map_err(|_| TotpError::Storage)?,
        );
        let step = match_step(&secret, code, now_secs).ok_or(TotpError::Invalid)?;
        if enrollment.last_used_step.is_some_and(|last| step <= last) {
            return Err(TotpError::Replayed);
        }
        let mut next = guard.clone();
        if let Some(e) = next.enrollment.as_mut() {
            e.last_used_step = Some(step);
        }
        self.persist(&next).map_err(|_| TotpError::Storage)?;
        *guard = next;
        Ok(())
    }

    /// Verifies and consumes a backup code.
    pub fn verify_backup_code(&self, code: &str) -> Result<(), TotpError> {
        let normalized = Zeroizing::new(normalize_backup(code));
        let now = self.clock.now_millis();
        let mut guard = self.state.lock().expect("poisoned");
        self.merge_used_from_disk(&mut guard);
        let enrollment = guard.enrollment.as_ref().ok_or(TotpError::NotEnrolled)?;
        let mut found = None;
        for (i, bc) in enrollment.backup_codes.iter().enumerate() {
            let (Ok(salt), Ok(hash)) = (
                data_encoding::HEXLOWER.decode(bc.salt.as_bytes()),
                data_encoding::HEXLOWER.decode(bc.hash.as_bytes()),
            ) else {
                continue;
            };
            let candidate = hash_backup(&salt, &normalized);
            if bool::from(candidate.ct_eq(hash.as_slice())) && bc.used_at.is_none() {
                found = Some(i);
            }
        }
        let i = found.ok_or(TotpError::Invalid)?;
        let mut next = guard.clone();
        if let Some(e) = next.enrollment.as_mut() {
            e.backup_codes[i].used_at = Some(now);
        }
        self.persist(&next).map_err(|_| TotpError::Storage)?;
        *guard = next;
        Ok(())
    }

    /// Verifies an authenticator code, or else consumes a backup code: six
    /// digits are an authenticator code, anything else a backup code. Both
    /// keep their single-use rules.
    pub fn verify_code_or_backup(&self, code: &str) -> Result<CodeKind, TotpError> {
        if looks_like_totp(code) {
            self.verify(code).map(|()| CodeKind::Totp)
        } else {
            self.verify_backup_code(code).map(|()| CodeKind::BackupCode)
        }
    }

    /// The enrollment's date, authenticator entry and backup code count;
    /// `None` when nothing is enrolled.
    pub fn status(&self) -> Option<TotpStatus> {
        let guard = self.state.lock().expect("poisoned");
        let e = guard.enrollment.as_ref()?;
        Some(TotpStatus {
            enrolled_at: e.enrolled_at,
            issuer: e.issuer.clone(),
            account: e.account.clone(),
            backup_codes_remaining: e
                .backup_codes
                .iter()
                .filter(|c| c.used_at.is_none())
                .count(),
            backup_codes_total: e.backup_codes.len(),
        })
    }

    /// Number of unused backup codes.
    pub fn remaining_backup_codes(&self) -> usize {
        self.state
            .lock()
            .expect("poisoned")
            .enrollment
            .as_ref()
            .map_or(0, |e| {
                e.backup_codes
                    .iter()
                    .filter(|c| c.used_at.is_none())
                    .count()
            })
    }

    /// Replaces all backup codes; returns the new plaintext codes.
    pub fn regenerate_backup_codes(&self) -> Result<Vec<String>, TotpError> {
        let (stored, codes) = new_backup_codes();
        let enrolled = self
            .mutate(|s| match s.enrollment.as_mut() {
                Some(e) => {
                    e.backup_codes = stored;
                    true
                }
                None => false,
            })
            .map_err(|_| TotpError::Storage)?;
        if enrolled {
            Ok(codes)
        } else {
            Err(TotpError::NotEnrolled)
        }
    }
}

/// Finds the step (current ±1) whose code matches, without early exit.
fn match_step(secret: &[u8], code: &str, now_secs: u64) -> Option<u64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != TOTP_DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let current = now_secs / TOTP_STEP_SECS;
    let mut matched = None;
    for step in [current.saturating_sub(1), current, current + 1] {
        let expected = format!(
            "{:0width$}",
            hotp(secret, step, TOTP_DIGITS),
            width = TOTP_DIGITS as usize
        );
        if bool::from(expected.as_bytes().ct_eq(code.as_bytes())) {
            // Prefer the latest matching step.
            matched = Some(step);
        }
    }
    matched
}

fn new_backup_codes() -> (Vec<BackupCode>, Vec<String>) {
    let mut stored = Vec::with_capacity(BACKUP_CODE_COUNT);
    let mut plain = Vec::with_capacity(BACKUP_CODE_COUNT);
    for _ in 0..BACKUP_CODE_COUNT {
        let mut raw = [0u8; BACKUP_CODE_BYTES];
        fill_random(&mut raw);
        let code = data_encoding::BASE32_NOPAD.encode(&raw);
        debug_assert_eq!(code.len(), 16);
        let mut salt = [0u8; 16];
        fill_random(&mut salt);
        let hash = hash_backup(&salt, &code);
        stored.push(BackupCode {
            salt: data_encoding::HEXLOWER.encode(&salt),
            hash: data_encoding::HEXLOWER.encode(&hash),
            used_at: None,
        });
        plain.push(format!(
            "{}-{}-{}-{}",
            &code[0..4],
            &code[4..8],
            &code[8..12],
            &code[12..16]
        ));
    }
    (stored, plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Clock, ManualClock};

    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc4226_hotp_vectors() {
        let expected = [
            755224, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (i, e) in expected.iter().enumerate() {
            assert_eq!(hotp(RFC_SECRET, i as u64, 6), *e);
        }
    }

    #[test]
    fn rfc6238_sha1_vectors() {
        let cases = [
            (59u64, 94287082u32),
            (1111111109, 7081804),
            (1111111111, 14050471),
            (1234567890, 89005924),
            (2000000000, 69279037),
            (20000000000, 65353130),
        ];
        for (t, e) in cases {
            assert_eq!(hotp(RFC_SECRET, t / 30, 8), e, "t={t}");
        }
        assert_eq!(totp_code_at(RFC_SECRET, 59), "287082");
        assert_eq!(totp_code_at(RFC_SECRET, 1111111109), "081804");
    }

    #[test]
    fn otpauth_uri_format() {
        let uri = otpauth_uri(RFC_SECRET, "Voice Command AI", "owner@desk");
        assert_eq!(
            uri,
            "otpauth://totp/Voice%20Command%20AI:owner%40desk?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Voice%20Command%20AI&algorithm=SHA1&digits=6&period=30"
        );
    }

    fn enrolled(clock: &std::sync::Arc<ManualClock>) -> (TotpManager, Vec<u8>, Vec<String>) {
        let mgr = TotpManager::in_memory(clock.clone());
        let pending = mgr.begin_enrollment("VCA", "owner");
        let secret = pending.secret.to_vec();
        assert!(
            pending
                .otpauth_uri
                .starts_with("otpauth://totp/VCA:owner?secret=")
        );
        assert!(!mgr.is_enrolled());
        assert_eq!(
            mgr.confirm_enrollment(&pending, "000000x"),
            Err(TotpError::Invalid)
        );
        let code = totp_code_at(&secret, clock.now_secs());
        let backups = mgr.confirm_enrollment(&pending, &code).unwrap();
        assert!(mgr.is_enrolled());
        (mgr, secret, backups)
    }

    #[test]
    fn verify_accepts_window_and_rejects_replay() {
        let clock = ManualClock::new(1_700_000_000_000);
        let (mgr, secret, _) = enrolled(&clock);
        // The enrollment code itself cannot be replayed.
        let same = totp_code_at(&secret, clock.now_secs());
        assert_eq!(mgr.verify(&same), Err(TotpError::Replayed));
        clock.advance_secs(30);
        // Previous-step code (skew) is older than the last used step: replay.
        assert_eq!(mgr.verify(&same), Err(TotpError::Replayed));
        // Next step's code (+1 skew) is accepted once.
        let ahead = totp_code_at(&secret, clock.now_secs() + 30);
        assert_eq!(mgr.verify(&ahead), Ok(()));
        assert_eq!(mgr.verify(&ahead), Err(TotpError::Replayed));
        // Two steps ahead is outside the window.
        clock.advance_secs(60);
        let far = totp_code_at(&secret, clock.now_secs() + 60);
        let now_code = totp_code_at(&secret, clock.now_secs());
        if far != now_code && far != totp_code_at(&secret, clock.now_secs() + 30) {
            assert_eq!(mgr.verify(&far), Err(TotpError::Invalid));
        }
        assert_eq!(mgr.verify(&now_code), Ok(()));
    }

    #[test]
    fn verify_rejects_malformed() {
        let clock = ManualClock::new(1_700_000_000_000);
        let (mgr, _, _) = enrolled(&clock);
        for bad in ["", "12345", "1234567", "abcdef", "12 34 5x"] {
            assert_eq!(mgr.verify(bad), Err(TotpError::Invalid), "{bad}");
        }
    }

    #[test]
    fn not_enrolled() {
        let mgr = TotpManager::in_memory(ManualClock::new(0));
        assert_eq!(mgr.verify("123456"), Err(TotpError::NotEnrolled));
        assert_eq!(mgr.verify_backup_code("AAAA"), Err(TotpError::NotEnrolled));
        assert_eq!(mgr.regenerate_backup_codes(), Err(TotpError::NotEnrolled));
    }

    #[test]
    fn backup_codes_are_single_use() {
        let clock = ManualClock::new(1_700_000_000_000);
        let (mgr, _, backups) = enrolled(&clock);
        assert_eq!(backups.len(), BACKUP_CODE_COUNT);
        assert_eq!(mgr.remaining_backup_codes(), BACKUP_CODE_COUNT);
        // Case and separators are normalized.
        let code = backups[3].to_lowercase().replace('-', " ");
        assert_eq!(mgr.verify_backup_code(&code), Ok(()));
        assert_eq!(mgr.verify_backup_code(&backups[3]), Err(TotpError::Invalid));
        assert_eq!(
            mgr.verify_backup_code("AAAA-BBBB-CCCC-DDDD"),
            Err(TotpError::Invalid)
        );
        assert_eq!(mgr.remaining_backup_codes(), BACKUP_CODE_COUNT - 1);
        let fresh = mgr.regenerate_backup_codes().unwrap();
        assert_eq!(mgr.verify_backup_code(&backups[0]), Err(TotpError::Invalid));
        assert_eq!(mgr.verify_backup_code(&fresh[0]), Ok(()));
    }

    #[test]
    fn re_enrollment_keeps_the_old_secret_until_confirmed() {
        let clock = ManualClock::new(1_700_000_000_000);
        let (mgr, old, old_backups) = enrolled(&clock);
        let first = mgr.status().unwrap();
        assert_eq!(first.issuer.as_deref(), Some("VCA"));
        assert_eq!(first.account.as_deref(), Some("owner"));

        // Starting again changes nothing yet.
        let pending = mgr.begin_enrollment("Voice Command AI", "owner");
        let new = pending.secret.to_vec();
        clock.advance_secs(30);
        assert_eq!(mgr.verify(&totp_code_at(&old, clock.now_secs())), Ok(()));
        // A wrong confirmation keeps the old enrollment.
        assert_eq!(
            mgr.confirm_enrollment(&pending, "abcdef"),
            Err(TotpError::Invalid)
        );
        clock.advance_secs(30);
        assert_eq!(mgr.verify(&totp_code_at(&old, clock.now_secs())), Ok(()));
        assert_eq!(mgr.status().unwrap(), first);

        // Confirming replaces the secret and the backup codes.
        clock.advance_secs(30);
        let fresh = mgr
            .confirm_enrollment(&pending, &totp_code_at(&new, clock.now_secs()))
            .unwrap();
        let status = mgr.status().unwrap();
        assert_eq!(status.issuer.as_deref(), Some("Voice Command AI"));
        assert_eq!(status.backup_codes_remaining, BACKUP_CODE_COUNT);
        clock.advance_secs(30);
        let old_code = totp_code_at(&old, clock.now_secs());
        if old_code != totp_code_at(&new, clock.now_secs()) {
            assert_eq!(mgr.verify(&old_code), Err(TotpError::Invalid));
        }
        assert_eq!(mgr.verify(&totp_code_at(&new, clock.now_secs())), Ok(()));
        assert_eq!(
            mgr.verify_backup_code(&old_backups[0]),
            Err(TotpError::Invalid)
        );
        assert_eq!(mgr.verify_backup_code(&fresh[0]), Ok(()));
    }

    #[test]
    fn one_field_takes_an_authenticator_or_a_backup_code() {
        let clock = ManualClock::new(1_700_000_000_000);
        let (mgr, secret, backups) = enrolled(&clock);
        assert!(looks_like_totp("123 456"));
        assert!(!looks_like_totp("1234567"));
        assert!(!looks_like_totp(&backups[0]));
        clock.advance_secs(30);
        let code = totp_code_at(&secret, clock.now_secs());
        assert_eq!(mgr.verify_code_or_backup(&code), Ok(CodeKind::Totp));
        assert_eq!(mgr.verify_code_or_backup(&code), Err(TotpError::Replayed));
        let backup = backups[1].to_lowercase();
        assert_eq!(mgr.verify_code_or_backup(&backup), Ok(CodeKind::BackupCode));
        assert_eq!(mgr.verify_code_or_backup(&backup), Err(TotpError::Invalid));
        assert_eq!(
            mgr.status().unwrap().backup_codes_remaining,
            BACKUP_CODE_COUNT - 1
        );
        assert_eq!(
            mgr.verify_code_or_backup("not-a-code"),
            Err(TotpError::Invalid)
        );
        assert!(TotpManager::in_memory(clock).status().is_none());
    }

    #[test]
    fn a_code_is_single_use_across_managers_on_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("totp.json");
        let clock = ManualClock::new(1_700_000_000_000);
        let first = TotpManager::open(&path, clock.clone()).unwrap();
        let pending = first.begin_enrollment("VCA", "owner");
        let secret = pending.secret.to_vec();
        let backups = first
            .confirm_enrollment(&pending, &totp_code_at(&secret, clock.now_secs()))
            .unwrap();
        // A second server on the same file (an overlapping restart).
        let second = TotpManager::open(&path, clock.clone()).unwrap();
        clock.advance_secs(30);
        let code = totp_code_at(&secret, clock.now_secs());
        assert_eq!(first.verify(&code), Ok(()));
        assert_eq!(second.verify(&code), Err(TotpError::Replayed));
        assert_eq!(first.verify(&code), Err(TotpError::Replayed));
        // An earlier step is refused too; the next one works once.
        let earlier = totp_code_at(&secret, clock.now_secs() - 30);
        assert_eq!(second.verify(&earlier), Err(TotpError::Replayed));
        clock.advance_secs(30);
        let next = totp_code_at(&secret, clock.now_secs());
        assert_eq!(second.verify(&next), Ok(()));
        assert_eq!(first.verify(&next), Err(TotpError::Replayed));
        // Backup codes stay single-use across managers as well.
        assert_eq!(second.verify_backup_code(&backups[2]), Ok(()));
        assert_eq!(
            first.verify_backup_code(&backups[2]),
            Err(TotpError::Invalid)
        );
    }

    #[test]
    fn persisted_state_survives_restart_including_replay_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("totp.json");
        let clock = ManualClock::new(1_700_000_000_000);
        let mgr = TotpManager::open(&path, clock.clone()).unwrap();
        let pending = mgr.begin_enrollment("VCA", "owner");
        let secret = pending.secret.to_vec();
        let code = totp_code_at(&secret, clock.now_secs());
        let backups = mgr.confirm_enrollment(&pending, &code).unwrap();
        drop(mgr);
        let mgr = TotpManager::open(&path, clock.clone()).unwrap();
        assert!(mgr.is_enrolled());
        assert_eq!(mgr.verify(&code), Err(TotpError::Replayed));
        mgr.verify_backup_code(&backups[0]).unwrap();
        let mgr = TotpManager::open(&path, clock.clone()).unwrap();
        assert_eq!(mgr.verify_backup_code(&backups[0]), Err(TotpError::Invalid));
        // The file does not contain backup codes in plaintext.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&backups[1].replace('-', "")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        mgr.disable().unwrap();
        assert!(!TotpManager::open(&path, clock).unwrap().is_enrolled());
    }
}
