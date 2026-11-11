//! Recovery from a lost authenticator.
//!
//! Replacing or turning off the authenticator without it needs BOTH local
//! verification on this computer AND a second, separate proof:
//!
//! * a 6-digit code mailed to the registered owner address (valid 10
//!   minutes, single use, 5 attempts), when mail is set up; or
//! * one of the backup codes (always possible). Where the local verification
//!   itself was a backup code (a computer with no OS verifier), this must be
//!   a second one.
//!
//! Both halves are real: no single approval can make the second proof.
//! Regenerating backup codes needs the current authenticator, and so does
//! changing where mail goes, so one approval cannot mint a backup code or
//! read the mailed code.
//!
//! The second proof opens a short **recovery grant** (10 minutes) for one
//! operation ([`GrantOp`]). It ends when that operation is done, and after
//! [`MAX_ATTEMPTS`] wrong codes for the new authenticator.

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use vca_core::UnixMillis;
use vca_net::SharedClock;

/// A mailed code is valid this long.
pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// Wrong guesses before the code is void.
pub const MAX_ATTEMPTS: u32 = 5;
/// Shortest pause between two mailed codes.
pub const RESEND_INTERVAL: Duration = Duration::from_secs(60);
/// At most this many codes per hour.
pub const MAX_CODES_PER_HOUR: usize = 5;
/// How long a successful recovery allows re-enrollment.
pub const GRANT_TTL: Duration = Duration::from_secs(10 * 60);

/// The one operation a recovery grant allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantOp {
    /// Enrolling a new authenticator.
    #[default]
    Replace,
    /// Turning the authenticator off.
    Disable,
    /// Regenerating the backup codes.
    BackupCodes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryError {
    #[error("no recovery code was sent, or it was already used")]
    NoCode,
    #[error("the recovery code has expired; start again")]
    Expired,
    #[error("wrong recovery code")]
    Invalid,
    #[error("too many wrong codes; start again")]
    AttemptsExceeded,
    #[error("wait a minute before asking for another code")]
    RateLimited,
}

struct Pending {
    salt: [u8; 16],
    hash: [u8; 32],
    expires_at: UnixMillis,
    attempts: u32,
}

struct Grant {
    until: UnixMillis,
    op: GrantOp,
    /// The pending enrollment it belongs to, once one exists.
    enrollment: Option<u64>,
    wrong: u32,
}

#[derive(Default)]
struct State {
    pending: Option<Pending>,
    sent: Vec<UnixMillis>,
    grant: Option<Grant>,
}

pub struct Recovery {
    clock: SharedClock,
    state: Mutex<State>,
}

fn hash(salt: &[u8], code: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"vca-recovery-code-v1");
    h.update(salt);
    h.update(code.as_bytes());
    h.finalize().into()
}

/// A uniformly random 6-digit code.
fn random_code() -> String {
    loop {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).expect("operating system random number generator failed");
        let n = u32::from_le_bytes(b);
        // Reject the top partial range so every code is equally likely.
        if n < u32::MAX - (u32::MAX % 1_000_000) {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

impl Recovery {
    pub fn new(clock: SharedClock) -> Self {
        Self {
            clock,
            state: Mutex::new(State::default()),
        }
    }

    /// Creates a new code (replacing any earlier one) for mailing. Returns
    /// the code and its expiry.
    pub fn issue_code(&self) -> Result<(String, UnixMillis), RecoveryError> {
        let now = self.clock.now_millis();
        let mut s = self.state.lock().expect("poisoned");
        let hour_ago = now.saturating_sub(3_600_000);
        s.sent.retain(|t| *t > hour_ago);
        let too_soon = s
            .sent
            .last()
            .is_some_and(|t| now < t + RESEND_INTERVAL.as_millis() as u64);
        if too_soon || s.sent.len() >= MAX_CODES_PER_HOUR {
            return Err(RecoveryError::RateLimited);
        }
        let code = random_code();
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).expect("operating system random number generator failed");
        let expires_at = now + CODE_TTL.as_millis() as u64;
        s.pending = Some(Pending {
            salt,
            hash: hash(&salt, &code),
            expires_at,
            attempts: 0,
        });
        s.sent.push(now);
        Ok((code, expires_at))
    }

    /// Forgets the pending code (for example when mailing it failed).
    pub fn cancel_code(&self) {
        self.state.lock().expect("poisoned").pending = None;
    }

    /// Checks and consumes the mailed code; on success opens the grant.
    pub fn redeem_code(&self, code: &str, op: GrantOp) -> Result<UnixMillis, RecoveryError> {
        let now = self.clock.now_millis();
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let mut s = self.state.lock().expect("poisoned");
        let p = s.pending.as_mut().ok_or(RecoveryError::NoCode)?;
        if now >= p.expires_at {
            s.pending = None;
            return Err(RecoveryError::Expired);
        }
        if p.attempts >= MAX_ATTEMPTS {
            s.pending = None;
            return Err(RecoveryError::AttemptsExceeded);
        }
        let ok = code.len() == 6
            && code.bytes().all(|b| b.is_ascii_digit())
            && bool::from(hash(&p.salt, &code).ct_eq(&p.hash));
        if !ok {
            p.attempts += 1;
            if p.attempts >= MAX_ATTEMPTS {
                s.pending = None;
                return Err(RecoveryError::AttemptsExceeded);
            }
            return Err(RecoveryError::Invalid);
        }
        s.pending = None;
        Ok(self.open_grant_locked(&mut s, now, op))
    }

    /// Opens the grant after another proof (a backup code verified by the
    /// caller).
    pub fn grant(&self, op: GrantOp) -> UnixMillis {
        let now = self.clock.now_millis();
        let mut s = self.state.lock().expect("poisoned");
        self.open_grant_locked(&mut s, now, op)
    }

    fn open_grant_locked(&self, s: &mut State, now: UnixMillis, op: GrantOp) -> UnixMillis {
        let until = now + GRANT_TTL.as_millis() as u64;
        s.grant = Some(Grant {
            until,
            op,
            enrollment: None,
            wrong: 0,
        });
        until
    }

    /// A recovery grant is open, for whatever operation.
    pub fn granted(&self) -> bool {
        let now = self.clock.now_millis();
        self.state
            .lock()
            .expect("poisoned")
            .grant
            .as_ref()
            .is_some_and(|g| now < g.until)
    }

    /// A recovery grant is open for `op`; for a replacement, also for the
    /// pending enrollment `enrollment`.
    pub fn granted_for(&self, op: GrantOp, enrollment: Option<u64>) -> bool {
        let now = self.clock.now_millis();
        let s = self.state.lock().expect("poisoned");
        s.grant.as_ref().is_some_and(|g| {
            now < g.until && g.op == op && (g.enrollment.is_none() || g.enrollment == enrollment)
        })
    }

    /// A recovery grant is open for `op`, whichever enrollment it is tied to.
    pub fn granted_op(&self, op: GrantOp) -> bool {
        let now = self.clock.now_millis();
        let s = self.state.lock().expect("poisoned");
        s.grant
            .as_ref()
            .is_some_and(|g| now < g.until && g.op == op)
    }

    /// Ties a replacement grant to the enrollment just started.
    pub fn bind_enrollment(&self, enrollment: u64) {
        let mut s = self.state.lock().expect("poisoned");
        if let Some(g) = s.grant.as_mut().filter(|g| g.op == GrantOp::Replace) {
            g.enrollment = Some(enrollment);
        }
    }

    /// A wrong code for the new authenticator: the last allowed one closes
    /// the grant, so it does not stay open for guessing.
    pub fn wrong_code(&self) {
        let mut s = self.state.lock().expect("poisoned");
        let spent = s.grant.as_mut().is_some_and(|g| {
            g.wrong += 1;
            g.wrong >= MAX_ATTEMPTS
        });
        if spent {
            s.grant = None;
        }
    }

    /// Closes the grant (after its operation was done).
    pub fn close_grant(&self) {
        self.state.lock().expect("poisoned").grant = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_net::ManualClock;

    #[test]
    fn mailed_code_is_single_use_and_expires() {
        let clock = ManualClock::new(1_000_000);
        let r = Recovery::new(clock.clone());
        assert_eq!(
            r.redeem_code("123456", GrantOp::Replace),
            Err(RecoveryError::NoCode)
        );
        let (code, expires) = r.issue_code().unwrap();
        assert_eq!(code.len(), 6);
        assert_eq!(expires, 1_000_000 + 600_000);
        assert!(!r.granted());
        let until = r
            .redeem_code(&format!(" {code} "), GrantOp::Replace)
            .unwrap();
        assert!(r.granted());
        assert_eq!(until, 1_000_000 + 600_000);
        assert_eq!(
            r.redeem_code(&code, GrantOp::Replace),
            Err(RecoveryError::NoCode)
        );
        clock.advance_secs(600);
        assert!(!r.granted(), "grant expires");

        let (code, _) = r.issue_code().unwrap();
        clock.advance_secs(600);
        assert_eq!(
            r.redeem_code(&code, GrantOp::Replace),
            Err(RecoveryError::Expired)
        );
        assert_eq!(
            r.redeem_code(&code, GrantOp::Replace),
            Err(RecoveryError::NoCode)
        );
    }

    #[test]
    fn attempts_are_limited() {
        let clock = ManualClock::new(1_000_000);
        let r = Recovery::new(clock.clone());
        let (code, _) = r.issue_code().unwrap();
        let wrong = if code == "000000" { "111111" } else { "000000" };
        for _ in 0..MAX_ATTEMPTS - 1 {
            assert_eq!(
                r.redeem_code(wrong, GrantOp::Replace),
                Err(RecoveryError::Invalid)
            );
        }
        assert_eq!(
            r.redeem_code(wrong, GrantOp::Replace),
            Err(RecoveryError::AttemptsExceeded)
        );
        // The right code no longer works.
        assert_eq!(
            r.redeem_code(&code, GrantOp::Replace),
            Err(RecoveryError::NoCode)
        );
        assert!(!r.granted());
    }

    #[test]
    fn sending_is_rate_limited() {
        let clock = ManualClock::new(1_000_000);
        let r = Recovery::new(clock.clone());
        r.issue_code().unwrap();
        assert_eq!(r.issue_code().unwrap_err(), RecoveryError::RateLimited);
        for _ in 1..MAX_CODES_PER_HOUR {
            clock.advance_secs(60);
            r.issue_code().unwrap();
        }
        clock.advance_secs(60);
        assert_eq!(r.issue_code().unwrap_err(), RecoveryError::RateLimited);
        clock.advance_secs(3600);
        assert!(r.issue_code().is_ok());
        r.cancel_code();
        assert_eq!(
            r.redeem_code("123456", GrantOp::Replace),
            Err(RecoveryError::NoCode)
        );
    }

    #[test]
    fn backup_code_grant_and_close() {
        let r = Recovery::new(ManualClock::new(5));
        r.grant(GrantOp::Replace);
        assert!(r.granted());
        r.close_grant();
        assert!(!r.granted());
        for _ in 0..50 {
            let c = random_code();
            assert!(c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn a_grant_is_for_one_operation_and_one_enrollment() {
        let r = Recovery::new(ManualClock::new(5));
        r.grant(GrantOp::Replace);
        assert!(r.granted_for(GrantOp::Replace, Some(1)));
        assert!(!r.granted_for(GrantOp::Disable, None));
        assert!(!r.granted_for(GrantOp::BackupCodes, None));
        r.bind_enrollment(1);
        assert!(r.granted_for(GrantOp::Replace, Some(1)));
        assert!(!r.granted_for(GrantOp::Replace, Some(2)));
        assert!(!r.granted_for(GrantOp::Replace, None));
    }

    #[test]
    fn wrong_codes_for_the_new_authenticator_end_the_grant() {
        let r = Recovery::new(ManualClock::new(5));
        r.grant(GrantOp::Replace);
        for _ in 0..MAX_ATTEMPTS - 1 {
            r.wrong_code();
            assert!(r.granted());
        }
        r.wrong_code();
        assert!(!r.granted());
    }
}
