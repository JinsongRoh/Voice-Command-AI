//! Step-up: proof that the owner, sitting at this computer, just verified
//! themselves (Windows Hello, Touch ID or the login password, or the VCA
//! authenticator code), before a security-sensitive action.
//!
//! The flow has two phases, and the server chooses what is being approved:
//!
//! 1. A gated request arrives without a usable token. The server builds a
//!    [`GateTarget`] from that request and answers with a nonce
//!    ([`StepUpChallenge`]).
//! 2. The caller sends the nonce to the verification. The prompt text comes
//!    from the target, never from the caller. On success vcad issues a
//!    random **step-up token** bound to that target and to the calling
//!    process.
//! 3. The caller repeats the same request with the token.
//!
//! Rules (none of them configurable):
//!
//! * Remote clients (paired devices) can never trigger, satisfy or extend a
//!   local verification: every gated action from a remote origin is
//!   refused, whatever token it carries.
//! * Tokens are only issued on the local channel, and only after a
//!   verification vcad ran or checked itself (an OS prompt it showed, or a
//!   TOTP code it verified). A caller cannot assert "verified".
//! * A token works for the exact request it was issued for, once. The only
//!   reuse is for the action classes of [`GatedAction::shared_window`], whose
//!   flows are several requests long.
//! * A token only works for the process it was issued to, where the channel
//!   can name the process ([`Caller`]).
//! * The expiry is fixed when the token is issued; using a token never
//!   extends it.
//! * Only a SHA-256 hash of each token and nonce is kept in memory;
//!   comparison is constant time.

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use vca_core::UnixMillis;
use vca_net::SharedClock;

/// Default grace window after a successful verification.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(120);
/// Longest grace window the owner may choose.
pub const MAX_GRACE: Duration = Duration::from_secs(5 * 60);
/// How long a nonce waits for its verification. Longer than an OS prompt.
pub const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
/// At most this many live tokens (older ones are dropped first).
const MAX_TOKENS: usize = 8;
/// At most this many open challenges (older ones are dropped first).
const MAX_CHALLENGES: usize = 64;
/// At most this many open challenges per calling process, so one local
/// process cannot evict the challenge the owner is answering.
const MAX_CHALLENGES_PER_CALLER: usize = 2;
const TOKEN_BYTES: usize = 32;

/// A security-sensitive action on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatedAction {
    /// Creating a pairing QR code, link or invite.
    PairingCreate,
    /// First authenticator enrollment.
    TotpSetup,
    /// Replacing the enrolled authenticator.
    TotpReset,
    TotpDisable,
    /// Widening a device's permission.
    DeviceApprove,
    RemoteShellEnable,
    SecuritySettings,
    MailSettings,
    /// Showing (regenerating) backup codes.
    BackupCodesExport,
    /// Starting or finishing account recovery (lost authenticator).
    Recovery,
    /// Changing or starting the conductor, or allowing it a session.
    ConductorSetup,
    /// Allowing what the conductor asked to do.
    ConductorDecide,
    /// The conductor's kill switch (revokes its tools). Stopping it plainly
    /// needs nothing.
    ConductorKill,
    /// Changing the task inbox settings.
    TaskSettings,
    /// Sending existing engine credentials to a new URL.
    EngineUrl,
}

impl GatedAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PairingCreate => "pairing_create",
            Self::TotpSetup => "totp_setup",
            Self::TotpReset => "totp_reset",
            Self::TotpDisable => "totp_disable",
            Self::DeviceApprove => "device_approve",
            Self::RemoteShellEnable => "remote_shell_enable",
            Self::SecuritySettings => "security_settings",
            Self::MailSettings => "mail_settings",
            Self::BackupCodesExport => "backup_codes_export",
            Self::Recovery => "recovery",
            Self::ConductorSetup => "conductor_setup",
            Self::ConductorDecide => "conductor_decide",
            Self::ConductorKill => "conductor_kill",
            Self::TaskSettings => "task_settings",
            Self::EngineUrl => "engine_url",
        }
    }

    /// May one verification cover several requests of this class within the
    /// grace window? Everything else is one verification per request.
    ///
    /// * `TotpSetup`, `TotpReset`: enrollment is two requests, and a mistyped
    ///   code in the app must not need a new prompt.
    /// * `Recovery`: start, then finish after the mail arrived, with retries
    ///   of a mistyped code.
    /// * `MailSettings`: one save is the form and up to three secrets.
    /// * `ConductorDecide`: confirmations come in a row while the person
    ///   works with the conductor.
    pub fn shared_window(self) -> bool {
        matches!(
            self,
            Self::TotpSetup
                | Self::TotpReset
                | Self::Recovery
                | Self::MailSettings
                | Self::ConductorDecide
        )
    }
}

/// Where a request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The owner-only local control channel (desktop app, `vca` CLI).
    Local,
    /// A paired device over the remote transport (or any non-local
    /// transport, such as the development WebSocket).
    Remote,
}

/// What the local channel knows about the process that sent a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caller {
    /// The process id, where the channel can name the peer.
    pub pid: Option<u32>,
    /// The process descends from a session VCA manages (an agent).
    pub in_session: bool,
    /// The process descends from a remote shell.
    pub in_remote_shell: bool,
}

/// How the owner verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepUpMethod {
    WindowsHello,
    MacLocalAuth,
    Totp,
    /// A backup code, accepted for recovery on a computer with no OS verifier.
    BackupCode,
    /// First enrollment on a computer with neither an OS verifier nor an
    /// authenticator: a person confirmed at a terminal or in the desktop app.
    Presence,
}

/// The request a verification is for. Built by the server from the gated
/// request itself, so a token cannot approve anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateTarget {
    pub action: GatedAction,
    digest: [u8; 32],
    /// Shown in the prompt after the action's text, such as the new
    /// recipients of a mail change.
    pub detail: Option<String>,
    /// What the prompt showed decides: a token of a shared class only
    /// passes this request if it was issued for this very request.
    exact: bool,
}

impl GateTarget {
    /// `request` is what identifies the exact request, without any token.
    pub fn new(action: GatedAction, request: &impl Serialize) -> Self {
        let mut h = Sha256::new();
        h.update(b"vca-step-up-target-v1");
        h.update(action.as_str().as_bytes());
        h.update([0]);
        // Serializing plain request data cannot fail; an empty digest would
        // only make the token useless.
        h.update(serde_json::to_vec(request).unwrap_or_default());
        Self {
            action,
            digest: h.finalize().into(),
            detail: None,
            exact: false,
        }
    }

    /// See [`Self::exact`](GateTarget): for a request whose prompt shows its
    /// content.
    pub fn exact(mut self) -> Self {
        self.exact = true;
        self
    }

    /// The detail goes into the OS dialog the person approves: line breaks,
    /// control characters and bidirectional or invisible format characters
    /// are taken out, so it cannot push the real text out of sight or make
    /// it read differently. Nothing is cut: a hidden tail could be the part
    /// that matters (one more mail recipient).
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(prompt_safe(&detail.into()));
        self
    }

    /// Does a token issued for `self` pass the request `other`?
    fn matches(&self, other: &Self) -> bool {
        self.action == other.action
            && (self.digest == other.digest || (self.action.shared_window() && !other.exact))
    }
}

/// Why a gated action was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    #[error("this action is only possible on the computer itself")]
    RemoteForbidden,
    #[error("confirm it is you on this computer first (local verification)")]
    StepUpRequired,
}

/// The answer to a gated request without a usable token. The nonce goes
/// into the verification; the request is then repeated with the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepUpChallenge {
    pub nonce: String,
    pub action: GatedAction,
    pub expires_at: UnixMillis,
}

impl std::fmt::Debug for StepUpChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepUpChallenge")
            .field("nonce", &"<redacted>")
            .field("action", &self.action)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A freshly issued token. The plaintext is handed to the caller once.
#[derive(Clone)]
pub struct IssuedToken {
    pub token: String,
    pub expires_at: UnixMillis,
    pub method: StepUpMethod,
}

impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("method", &self.method)
            .finish()
    }
}

struct Live {
    hash: [u8; 32],
    expires_at: UnixMillis,
    target: GateTarget,
    pid: Option<u32>,
    /// The authenticator already proved itself for this verification.
    proved: bool,
}

struct Open {
    hash: [u8; 32],
    expires_at: UnixMillis,
    target: GateTarget,
    pid: Option<u32>,
}

/// Issues and checks step-up tokens.
pub struct StepUp {
    clock: SharedClock,
    grace: Mutex<Duration>,
    live: Mutex<Vec<Live>>,
    open: Mutex<Vec<Open>>,
}

/// Every lookup, issue and revocation goes through here, trimmed the same
/// way, so a padded token can never pass one check and miss another.
fn hash(token: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"vca-step-up-v1");
    h.update(token.trim().as_bytes());
    h.finalize().into()
}

/// A random URL-safe token with 256 bits of entropy.
pub(crate) fn random_token() -> String {
    let mut raw = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut raw).expect("operating system random number generator failed");
    let mut out = String::with_capacity(TOKEN_BYTES * 2);
    for b in raw {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The token or challenge is bound to a process and the caller is not
/// provably that process. Fails closed: a binding with an unnamed caller
/// counts as another process.
fn other_process(bound: Option<u32>, caller: &Caller) -> bool {
    match (bound, caller.pid) {
        (Some(a), Some(b)) => a != b,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

impl StepUp {
    pub fn new(clock: SharedClock, grace: Duration) -> Self {
        Self {
            clock,
            grace: Mutex::new(clamp_grace(grace)),
            live: Mutex::new(Vec::new()),
            open: Mutex::new(Vec::new()),
        }
    }

    pub fn grace(&self) -> Duration {
        *self.grace.lock().expect("poisoned")
    }

    /// Changes the grace window for tokens issued from now on.
    pub fn set_grace(&self, grace: Duration) {
        *self.grace.lock().expect("poisoned") = clamp_grace(grace);
    }

    /// Opens a challenge for `target`.
    pub fn challenge(&self, target: &GateTarget, caller: &Caller) -> StepUpChallenge {
        let nonce = random_token();
        let now = self.clock.now_millis();
        let expires_at = now + CHALLENGE_TTL.as_millis() as u64;
        let mut open = self.open.lock().expect("poisoned");
        open.retain(|c| c.expires_at > now);
        // A caller only evicts its own oldest challenges; the global cap is
        // a last resort against many processes.
        let mine = open.iter().filter(|c| c.pid == caller.pid).count();
        if mine >= MAX_CHALLENGES_PER_CALLER {
            if let Some(oldest) = open.iter().position(|c| c.pid == caller.pid) {
                open.remove(oldest);
            }
        }
        if open.len() >= MAX_CHALLENGES {
            open.remove(0);
        }
        open.push(Open {
            hash: hash(&nonce),
            expires_at,
            target: target.clone(),
            pid: caller.pid,
        });
        StepUpChallenge {
            nonce,
            action: target.action,
            expires_at,
        }
    }

    /// The target of an open challenge. It stays open until
    /// [`Self::close_challenge`], so a canceled prompt can be tried again.
    pub fn challenge_target(&self, nonce: &str, caller: &Caller) -> Option<GateTarget> {
        let now = self.clock.now_millis();
        let candidate = hash(nonce);
        let mut open = self.open.lock().expect("poisoned");
        open.retain(|c| c.expires_at > now);
        open.iter()
            .find(|c| bool::from(c.hash.ct_eq(&candidate)) && !other_process(c.pid, caller))
            .map(|c| c.target.clone())
    }

    /// A challenge is spent once its verification succeeded.
    pub fn close_challenge(&self, nonce: &str) {
        let candidate = hash(nonce);
        self.open
            .lock()
            .expect("poisoned")
            .retain(|c| !bool::from(c.hash.ct_eq(&candidate)));
    }

    /// Issues a token after a verification that vcad itself performed.
    /// Never call this for a remote origin.
    pub(crate) fn issue(
        &self,
        method: StepUpMethod,
        target: &GateTarget,
        caller: &Caller,
    ) -> IssuedToken {
        let token = random_token();
        let now = self.clock.now_millis();
        let expires_at = now + self.grace().as_millis() as u64;
        let mut live = self.live.lock().expect("poisoned");
        live.retain(|l| l.expires_at > now);
        if live.len() >= MAX_TOKENS {
            live.remove(0);
        }
        live.push(Live {
            hash: hash(&token),
            expires_at,
            target: target.clone(),
            pid: caller.pid,
            proved: method == StepUpMethod::Totp,
        });
        IssuedToken {
            token,
            expires_at,
            method,
        }
    }

    /// The gate: may `origin` perform `target` with `token`? A token of a
    /// single-use class is spent here.
    pub fn require(
        &self,
        origin: Origin,
        target: &GateTarget,
        token: Option<&str>,
        caller: &Caller,
    ) -> Result<(), GateError> {
        // Check and spend under one lock: two concurrent requests with the
        // same single-use token cannot both pass.
        let spend = !target.action.shared_window();
        self.check_and(origin, target, token, caller, spend)
    }

    /// The gate without spending the token: for a request that has more
    /// checks to pass before it counts ([`Self::revoke`] spends it after).
    pub fn check(
        &self,
        origin: Origin,
        target: &GateTarget,
        token: Option<&str>,
        caller: &Caller,
    ) -> Result<(), GateError> {
        self.check_and(origin, target, token, caller, false)
    }

    fn check_and(
        &self,
        origin: Origin,
        target: &GateTarget,
        token: Option<&str>,
        caller: &Caller,
        spend: bool,
    ) -> Result<(), GateError> {
        // A process inside a managed agent session or a remote shell never
        // steps up for the owner. The desktop app and the owner's own
        // terminals are outside both.
        if origin != Origin::Local || caller.in_remote_shell || caller.in_session {
            return Err(GateError::RemoteForbidden);
        }
        let token = token
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or(GateError::StepUpRequired)?;
        let now = self.clock.now_millis();
        let candidate = hash(token);
        let mut live = self.live.lock().expect("poisoned");
        live.retain(|l| l.expires_at > now);
        let mut found = None;
        for (i, l) in live.iter().enumerate() {
            let same = bool::from(l.hash.ct_eq(&candidate));
            if same && l.target.matches(target) && !other_process(l.pid, caller) {
                found = Some(i);
            }
        }
        if let Some(i) = found {
            if spend {
                live.remove(i);
            }
            Ok(())
        } else {
            Err(GateError::StepUpRequired)
        }
    }

    /// The token is live (and not expired).
    pub fn valid(&self, token: &str) -> bool {
        self.expires_at(token).is_some()
    }

    /// When the token expires, if it is live.
    pub fn expires_at(&self, token: &str) -> Option<UnixMillis> {
        let now = self.clock.now_millis();
        let candidate = hash(token);
        let live = self.live.lock().expect("poisoned");
        live.iter()
            .filter(|l| l.expires_at > now)
            .find(|l| bool::from(l.hash.ct_eq(&candidate)))
            .map(|l| l.expires_at)
    }

    /// Did the authenticator prove itself for this token? True when it was
    /// the verification itself: a code works once, so asking again in the
    /// same step could never succeed, and would prove nothing new.
    pub fn proved(&self, token: &str) -> bool {
        let now = self.clock.now_millis();
        let candidate = hash(token);
        let live = self.live.lock().expect("poisoned");
        live.iter()
            .filter(|l| l.expires_at > now)
            .find(|l| bool::from(l.hash.ct_eq(&candidate)))
            .is_some_and(|l| l.proved)
    }

    /// Records that a code was checked within this token's flow.
    pub fn mark_proved(&self, token: &str) {
        let candidate = hash(token);
        let mut live = self.live.lock().expect("poisoned");
        if let Some(l) = live
            .iter_mut()
            .find(|l| bool::from(l.hash.ct_eq(&candidate)))
        {
            l.proved = true;
        }
    }

    /// Ends a token early (spent, or sign out of the grace window).
    pub fn revoke(&self, token: &str) {
        let candidate = hash(token);
        self.live
            .lock()
            .expect("poisoned")
            .retain(|l| !bool::from(l.hash.ct_eq(&candidate)));
    }

    /// Ends every token.
    pub fn revoke_all(&self) {
        self.live.lock().expect("poisoned").clear();
    }
}

/// `text` as one line of plain characters (see [`GateTarget::with_detail`]).
fn prompt_safe(text: &str) -> String {
    let hidden = |c: char| {
        matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
    };
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else if !hidden(c) {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

fn clamp_grace(grace: Duration) -> Duration {
    grace.min(MAX_GRACE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_net::ManualClock;

    const ME: Caller = Caller {
        pid: Some(10),
        in_session: false,
        in_remote_shell: false,
    };

    fn step_up() -> (std::sync::Arc<ManualClock>, StepUp) {
        let clock = ManualClock::new(1_700_000_000_000);
        let s = StepUp::new(clock.clone(), DEFAULT_GRACE);
        (clock, s)
    }

    fn pair(scope: &str) -> GateTarget {
        GateTarget::new(GatedAction::PairingCreate, &scope)
    }

    fn mail() -> GateTarget {
        GateTarget::new(GatedAction::MailSettings, &"form")
    }

    #[test]
    fn remote_calls_are_always_rejected() {
        let (_, s) = step_up();
        let target = pair("admin");
        let t = s.issue(StepUpMethod::WindowsHello, &target, &ME);
        assert_eq!(
            s.require(Origin::Remote, &target, Some(&t.token), &ME),
            Err(GateError::RemoteForbidden)
        );
        assert_eq!(
            s.require(Origin::Remote, &target, None, &ME),
            Err(GateError::RemoteForbidden)
        );
    }

    #[test]
    fn a_remote_shell_cannot_use_a_token() {
        let (_, s) = step_up();
        let target = pair("admin");
        let t = s.issue(StepUpMethod::WindowsHello, &target, &ME);
        let shell = Caller {
            in_remote_shell: true,
            ..ME
        };
        assert_eq!(
            s.require(Origin::Local, &target, Some(&t.token), &shell),
            Err(GateError::RemoteForbidden)
        );
    }

    #[test]
    fn local_calls_need_a_live_token() {
        let (clock, s) = step_up();
        let a = mail();
        assert_eq!(
            s.require(Origin::Local, &a, None, &ME),
            Err(GateError::StepUpRequired)
        );
        assert_eq!(
            s.require(Origin::Local, &a, Some(""), &ME),
            Err(GateError::StepUpRequired)
        );
        assert_eq!(
            s.require(Origin::Local, &a, Some("forged"), &ME),
            Err(GateError::StepUpRequired)
        );
        let t = s.issue(StepUpMethod::Totp, &a, &ME);
        assert_eq!(t.token.len(), 64);
        assert_eq!(s.require(Origin::Local, &a, Some(&t.token), &ME), Ok(()));
        // A shared class is several requests, without extending the window.
        clock.advance_secs(100);
        assert_eq!(s.require(Origin::Local, &a, Some(&t.token), &ME), Ok(()));
        assert_eq!(s.expires_at(&t.token), Some(t.expires_at));
        clock.advance_secs(20);
        assert_eq!(
            s.require(Origin::Local, &a, Some(&t.token), &ME),
            Err(GateError::StepUpRequired)
        );
        assert!(s.expires_at(&t.token).is_none());
    }

    #[test]
    fn a_token_works_once_and_for_its_own_request_only() {
        let (_, s) = step_up();
        let admin = pair("admin");
        let view = pair("view");
        let t = s.issue(StepUpMethod::WindowsHello, &view, &ME);
        // The same action with other parameters is another request.
        assert_eq!(
            s.require(Origin::Local, &admin, Some(&t.token), &ME),
            Err(GateError::StepUpRequired)
        );
        // Another action entirely.
        assert_eq!(
            s.require(Origin::Local, &mail(), Some(&t.token), &ME),
            Err(GateError::StepUpRequired)
        );
        assert_eq!(s.require(Origin::Local, &view, Some(&t.token), &ME), Ok(()));
        assert_eq!(
            s.require(Origin::Local, &view, Some(&t.token), &ME),
            Err(GateError::StepUpRequired),
            "single use"
        );
    }

    #[test]
    fn a_shared_class_does_not_open_any_other_class() {
        let (_, s) = step_up();
        let t = s.issue(StepUpMethod::WindowsHello, &mail(), &ME);
        let other_form = GateTarget::new(GatedAction::MailSettings, &"other");
        assert_eq!(
            s.require(Origin::Local, &other_form, Some(&t.token), &ME),
            Ok(())
        );
        let recovery = GateTarget::new(GatedAction::Recovery, &());
        assert_eq!(
            s.require(Origin::Local, &recovery, Some(&t.token), &ME),
            Err(GateError::StepUpRequired)
        );
    }

    #[test]
    fn an_exact_request_needs_the_token_of_what_was_shown() {
        let (_, s) = step_up();
        let shown = GateTarget::new(GatedAction::MailSettings, &"to: me").exact();
        let t = s.issue(StepUpMethod::WindowsHello, &shown, &ME);
        let other = GateTarget::new(GatedAction::MailSettings, &"to: them").exact();
        assert_eq!(
            s.require(Origin::Local, &other, Some(&t.token), &ME),
            Err(GateError::StepUpRequired)
        );
        assert_eq!(
            s.require(Origin::Local, &shown, Some(&t.token), &ME),
            Ok(())
        );
        // The rest of the class (the secrets of the same save) still passes.
        assert_eq!(
            s.require(Origin::Local, &mail(), Some(&t.token), &ME),
            Ok(())
        );
    }

    #[test]
    fn only_the_listed_classes_share_a_window() {
        for action in [
            GatedAction::PairingCreate,
            GatedAction::TotpDisable,
            GatedAction::DeviceApprove,
            GatedAction::RemoteShellEnable,
            GatedAction::SecuritySettings,
            GatedAction::BackupCodesExport,
            GatedAction::ConductorSetup,
            GatedAction::ConductorKill,
            GatedAction::TaskSettings,
            GatedAction::EngineUrl,
        ] {
            assert!(!action.shared_window(), "{action:?}");
        }
    }

    #[test]
    fn a_token_belongs_to_the_process_it_was_issued_to() {
        let (_, s) = step_up();
        let target = pair("view");
        let t = s.issue(StepUpMethod::WindowsHello, &target, &ME);
        let other = Caller {
            pid: Some(11),
            ..ME
        };
        assert_eq!(
            s.require(Origin::Local, &target, Some(&t.token), &other),
            Err(GateError::StepUpRequired)
        );
        assert_eq!(
            s.require(Origin::Local, &target, Some(&t.token), &ME),
            Ok(())
        );
    }

    #[test]
    fn a_challenge_names_its_target_for_its_process_until_closed() {
        let (clock, s) = step_up();
        let target = pair("admin").with_detail("admin");
        let c = s.challenge(&target, &ME);
        assert_eq!(c.action, GatedAction::PairingCreate);
        assert!(!format!("{c:?}").contains(&c.nonce));
        let other = Caller {
            pid: Some(11),
            ..ME
        };
        assert_eq!(s.challenge_target(&c.nonce, &other), None);
        assert_eq!(s.challenge_target("forged", &ME), None);
        assert_eq!(s.challenge_target(&c.nonce, &ME), Some(target.clone()));
        // A canceled prompt can be tried again.
        assert_eq!(s.challenge_target(&c.nonce, &ME), Some(target.clone()));
        s.close_challenge(&c.nonce);
        assert_eq!(s.challenge_target(&c.nonce, &ME), None);

        let c = s.challenge(&target, &ME);
        clock.advance_secs(CHALLENGE_TTL.as_secs());
        assert_eq!(s.challenge_target(&c.nonce, &ME), None, "expired");
    }

    #[test]
    fn grace_is_configurable_and_bounded() {
        let (clock, s) = step_up();
        let a = mail();
        s.set_grace(Duration::from_secs(10));
        let t = s.issue(StepUpMethod::MacLocalAuth, &a, &ME);
        clock.advance_secs(9);
        assert!(s.valid(&t.token));
        clock.advance_secs(1);
        assert!(!s.valid(&t.token));
        s.set_grace(Duration::from_secs(24 * 3600));
        assert_eq!(s.grace(), MAX_GRACE);
        assert_eq!(MAX_GRACE, Duration::from_secs(300));
        // Zero grace: a token is never usable.
        s.set_grace(Duration::ZERO);
        let t = s.issue(StepUpMethod::Totp, &a, &ME);
        assert!(!s.valid(&t.token));
    }

    #[test]
    fn a_padded_token_is_the_same_token_everywhere() {
        let (_, s) = step_up();
        let target = pair("view");
        let t = s.issue(StepUpMethod::WindowsHello, &target, &ME);
        let padded = format!(" {}\n", t.token);
        assert_eq!(s.check(Origin::Local, &target, Some(&padded), &ME), Ok(()));
        assert!(s.valid(&padded));
        s.revoke(&padded);
        assert!(!s.valid(&t.token), "revoked through the padded form");
        assert_eq!(
            s.check(Origin::Local, &target, Some(&t.token), &ME),
            Err(GateError::StepUpRequired)
        );
    }

    #[test]
    fn a_bound_token_refuses_an_unnamed_caller() {
        let (_, s) = step_up();
        let target = pair("view");
        let t = s.issue(StepUpMethod::WindowsHello, &target, &ME);
        let unnamed = Caller { pid: None, ..ME };
        assert_eq!(
            s.require(Origin::Local, &target, Some(&t.token), &unnamed),
            Err(GateError::StepUpRequired)
        );
        let c = s.challenge(&target, &ME);
        assert_eq!(s.challenge_target(&c.nonce, &unnamed), None);
        assert_eq!(
            s.require(Origin::Local, &target, Some(&t.token), &ME),
            Ok(())
        );
    }

    #[test]
    fn a_session_process_cannot_use_a_token() {
        let (_, s) = step_up();
        let target = pair("view");
        let t = s.issue(StepUpMethod::WindowsHello, &target, &ME);
        let agent = Caller {
            in_session: true,
            ..ME
        };
        assert_eq!(
            s.require(Origin::Local, &target, Some(&t.token), &agent),
            Err(GateError::RemoteForbidden)
        );
    }

    #[test]
    fn one_process_cannot_evict_the_challenges_of_another() {
        let (_, s) = step_up();
        let target = pair("view");
        let owner = s.challenge(&target, &ME);
        let noisy = Caller {
            pid: Some(99),
            ..ME
        };
        let mut last = Vec::new();
        for _ in 0..(MAX_CHALLENGES * 2) {
            last.push(s.challenge(&target, &noisy).nonce);
        }
        assert_eq!(s.challenge_target(&owner.nonce, &ME), Some(target.clone()));
        // The noisy process keeps only its newest few.
        assert!(s.challenge_target(&last[0], &noisy).is_none());
        assert!(s.challenge_target(last.last().unwrap(), &noisy).is_some());
        assert!(s.open.lock().unwrap().len() <= 1 + MAX_CHALLENGES_PER_CALLER);
    }

    #[test]
    fn revoke_and_token_limit() {
        let (_, s) = step_up();
        let a = mail();
        let first = s.issue(StepUpMethod::Totp, &a, &ME);
        let second = s.issue(StepUpMethod::Totp, &a, &ME);
        s.revoke(&second.token);
        assert!(!s.valid(&second.token));
        assert!(s.valid(&first.token));
        for _ in 0..MAX_TOKENS {
            s.issue(StepUpMethod::Totp, &a, &ME);
        }
        assert!(!s.valid(&first.token), "oldest token dropped");
        s.revoke_all();
        assert!(s.live.lock().unwrap().is_empty());
        assert!(!format!("{first:?}").contains(&first.token));
    }

    #[test]
    fn prompt_details_are_one_line_of_visible_text() {
        let t = GateTarget::new(GatedAction::EngineUrl, &"p")
            .with_detail("evil.example\r\n\r\nUnlock Voice Command AI\u{202E}lmth.exe\u{200B}");
        assert_eq!(
            t.detail.as_deref(),
            Some("evil.example Unlock Voice Command AIlmth.exe")
        );
        // Long details are not cut.
        let long = "a@example.com, ".repeat(40);
        let t = GateTarget::new(GatedAction::MailSettings, &"m").with_detail(long.clone());
        assert_eq!(t.detail.as_deref(), Some(long.trim()));
        // The detail is shown, not approved: the target stays the same.
        assert!(t.matches(&GateTarget::new(GatedAction::MailSettings, &"m")));
    }
}
