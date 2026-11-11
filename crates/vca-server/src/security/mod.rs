//! The security package of the server role: local user verification
//! (step-up), mail notifications, account recovery and first-run state.
//!
//! Everything here is reached through one request on the owner-only local
//! channel ([`SecurityCall`], see [`crate::control::LocalRequest::Security`]).
//! Paired devices have no path to it; the [`Security::gate`] function
//! refuses a remote origin for anything else that adopts the gate.
//!
//! Threat model: the local channel is open to every process of the owner,
//! including AI agents running in sessions. So a caller can never assert
//! that the owner verified: vcad shows the OS prompt itself, or checks an
//! authenticator code itself, and the text of the OS prompt comes from the
//! gated request, never from the caller. Step-up raises the cost of misuse
//! and shows it; it does not contain a hostile program running as the owner,
//! which can read the owner's files (such as `totp.json`) directly: see
//! `docs/security-setup.md` for what this does and does not stop.

pub mod mail;
pub mod pairing_policy;
pub mod recovery;
pub mod stepup;
pub mod verifier;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vca_core::UnixMillis;
use vca_net::{
    AuditEvent, AuditLog, AuthFailure, CodeKind, FileKeyStore, KeyStore, PendingEnrollment,
    SharedClock, TotpError, TotpManager,
};

use crate::cloud::Secret;
use crate::shell::Shells;
use mail::notify::{Notifier, SecurityEvent};
use mail::template::{Notice, RecoveryProof, TotpChange};
use mail::{MailError, MailForm, MailMode, MailService, MailView, SaveError, SecretKind};
pub use recovery::GrantOp;
use recovery::{Recovery, RecoveryError};
pub use stepup::{
    Caller, GateError, GateTarget, GatedAction, Origin, StepUp, StepUpChallenge, StepUpMethod,
};
use verifier::{Availability, LocalVerifier, VerifierKind, VerifyOutcome};

const STATE_FILE: &str = "state.json";
const MIN_GRACE_SECS: u64 = 30;
/// Failures before [`Backoff`] starts to wait.
const FREE_TRIES: u32 = 4;
/// The longest wait after a failure. It is a delay, never a lockout: any
/// local process can cause failures, and the owner must still get in.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A quiet period this long forgets earlier failures.
const BACKOFF_RESET: Duration = Duration::from_secs(5 * 60);
/// An OS prompt nobody answers ends after this long, so it cannot hold the
/// one prompt slot. Shorter than the app's own wait.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(190);
const TEST_MAIL_INTERVAL: Duration = Duration::from_secs(10);
const MAX_TEST_MAILS_PER_HOUR: usize = 10;
/// The message of [`FailReason::TotpReplayed`]; `vca` waits on it.
pub const CODE_JUST_USED: &str = "this code was just used; wait for the next one";

/// A token or code on the local channel: never shown by `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sensitive(pub String);

impl std::fmt::Debug for Sensitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sensitive(<redacted>)")
    }
}

impl From<&str> for Sensitive {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardState {
    #[default]
    Pending,
    Done,
    Skipped,
}

/// A person's confirmation for a first enrollment on a computer with
/// neither an OS verifier nor an authenticator. Nothing can verify the owner
/// there yet, so this is the weakest step in the system: see
/// `docs/security-setup.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// `vca` confirmed on an interactive terminal.
    Terminal,
    /// The first-run confirmation of the desktop app.
    DesktopApp,
}

/// One security request on the local channel. `step_up` is the token from
/// [`SecurityCall::StepUp`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SecurityCall {
    Status {
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    /// Verifies the owner for the gated request behind `nonce` (from
    /// [`SecurityReply::StepUpRequired`]): with `totp` that code, else the OS
    /// prompt. `window` is the native window the prompt belongs to and
    /// `language` the language of the asking app.
    StepUp {
        nonce: Sensitive,
        #[serde(default)]
        window: Option<u64>,
        #[serde(default)]
        totp: Option<Sensitive>,
        #[serde(default)]
        presence: Option<Presence>,
        #[serde(default)]
        language: Option<String>,
    },
    /// Tries the verification without approving anything: no token.
    StepUpTest {
        #[serde(default)]
        window: Option<u64>,
        #[serde(default)]
        totp: Option<Sensitive>,
        #[serde(default)]
        language: Option<String>,
    },
    /// Unlocks the desktop app (its app lock): the same check as
    /// [`SecurityCall::StepUpTest`], approving nothing and issuing no
    /// token, but the OS prompt says "Unlock Voice Command AI".
    AppUnlock {
        #[serde(default)]
        window: Option<u64>,
        #[serde(default)]
        totp: Option<Sensitive>,
        #[serde(default)]
        language: Option<String>,
    },
    StepUpEnd {
        step_up: Sensitive,
    },
    TotpBegin {
        account: String,
        #[serde(default)]
        replace: bool,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    /// `current_code` proves the current authenticator when replacing it
    /// (not needed during a recovery grant).
    TotpConfirm {
        code: Sensitive,
        #[serde(default)]
        current_code: Option<Sensitive>,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    TotpDisable {
        #[serde(default)]
        current_code: Option<Sensitive>,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    /// `current_code` proves the current authenticator (not needed during
    /// a recovery grant for this).
    BackupCodes {
        #[serde(default)]
        current_code: Option<Sensitive>,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    Settings {
        #[serde(default)]
        grace_secs: Option<u64>,
        #[serde(default)]
        remote_shell: Option<bool>,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    Wizard {
        state: WizardState,
    },
    MailGet,
    /// `current_code` is needed to change where mail goes while an
    /// authenticator is enrolled.
    MailSet {
        settings: Box<MailForm>,
        #[serde(default)]
        current_code: Option<Sensitive>,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    /// Stores a mail secret; `secret: None` deletes it. Changing or deleting
    /// a stored one needs `current_code` while an authenticator is enrolled.
    MailSecret {
        which: SecretKind,
        #[serde(default, with = "crate::engines::opt_secret")]
        secret: Option<Secret>,
        #[serde(default)]
        current_code: Option<Sensitive>,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    MailTest,
    RecoveryStart {
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
    RecoveryFinish {
        #[serde(default)]
        code: Option<Sensitive>,
        #[serde(default)]
        backup_code: Option<Sensitive>,
        /// What the grant is for.
        #[serde(default)]
        purpose: GrantOp,
        #[serde(default)]
        step_up: Option<Sensitive>,
    },
}

/// Why a security request failed; the app and the CLI branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    StepUpRequired,
    RemoteForbidden,
    VerificationCanceled,
    VerificationFailed,
    /// No OS verifier and no authenticator yet.
    VerificationUnavailable,
    /// First enrollment with no verifier: a person must confirm.
    PresenceRequired,
    /// No OS verifier here: send the authenticator code.
    TotpRequired,
    TotpInvalid,
    /// The right code, but it was used already: wait for the next one.
    TotpReplayed,
    /// No OS prompt here, so a code decides, and a remote shell could type
    /// that code: refused until the shells are closed.
    RemoteShellOpen,
    CurrentCodeRequired,
    RateLimited,
    InvalidRequest,
    RecipientNotAllowed,
    Conflict,
    MailNotConfigured,
    MailFailed,
    CodeInvalid,
    CodeExpired,
    AttemptsExceeded,
    /// The authenticator store could not be opened.
    Unavailable,
    Internal,
}

impl FailReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StepUpRequired => "step_up_required",
            Self::RemoteForbidden => "remote_forbidden",
            Self::VerificationCanceled => "verification_canceled",
            Self::VerificationFailed => "verification_failed",
            Self::VerificationUnavailable => "verification_unavailable",
            Self::PresenceRequired => "presence_required",
            Self::TotpRequired => "totp_required",
            Self::TotpInvalid => "totp_invalid",
            Self::TotpReplayed => "totp_replayed",
            Self::RemoteShellOpen => "remote_shell_open",
            Self::CurrentCodeRequired => "current_code_required",
            Self::RateLimited => "rate_limited",
            Self::InvalidRequest => "invalid_request",
            Self::RecipientNotAllowed => "recipient_not_allowed",
            Self::Conflict => "conflict",
            Self::MailNotConfigured => "mail_not_configured",
            Self::MailFailed => "mail_failed",
            Self::CodeInvalid => "code_invalid",
            Self::CodeExpired => "code_expired",
            Self::AttemptsExceeded => "attempts_exceeded",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierInfo {
    pub kind: VerifierKind,
    pub available: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpInfo {
    pub enrolled: bool,
    pub enrolled_at: Option<UnixMillis>,
    pub backup_codes_remaining: usize,
    pub backup_codes_total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailInfo {
    pub configured: bool,
    pub mode: Option<MailMode>,
    pub recipients: Vec<String>,
    pub notifications: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepUpInfo {
    pub active: bool,
    pub expires_at: Option<UnixMillis>,
    pub grace_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WizardInfo {
    pub state: WizardState,
    pub updated_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteShellInfo {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPath {
    MailAndLocal,
    BackupCodeAndLocal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityStatus {
    pub verifier: VerifierInfo,
    pub totp: TotpInfo,
    pub mail: MailInfo,
    pub step_up: StepUpInfo,
    pub wizard: WizardInfo,
    pub remote_shell: RemoteShellInfo,
    pub recovery: RecoveryPath,
    pub setup_complete: bool,
}

/// The answer to a [`SecurityCall`]. A failure is an answer too, so the
/// reason survives the trip without a new shared error code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SecurityReply {
    Status {
        status: Box<SecurityStatus>,
    },
    StepUp {
        token: Sensitive,
        expires_at: UnixMillis,
        method: StepUpMethod,
    },
    /// The request needs a verification first: pass the nonce to
    /// [`SecurityCall::StepUp`], then repeat the request with the token.
    StepUpRequired {
        challenge: StepUpChallenge,
    },
    TotpPending {
        otpauth_uri: Sensitive,
        secret_base32: Sensitive,
    },
    TotpEnrolled {
        backup_codes: Vec<Sensitive>,
    },
    Mail {
        view: Box<MailView>,
    },
    MailSent {
        sent_to: Vec<String>,
    },
    RecoveryCodeSent {
        sent_to: Vec<String>,
        expires_at: UnixMillis,
    },
    RecoveryGranted {
        grant_expires_at: UnixMillis,
    },
    Done,
    Failed {
        reason: FailReason,
        message: String,
    },
}

type Reply<T = SecurityReply> = Result<T, SecurityReply>;

fn failed(reason: FailReason, message: impl Into<String>) -> SecurityReply {
    SecurityReply::Failed {
        reason,
        message: message.into(),
    }
}

/// Why a gated request was not let through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Remote,
    /// Verify for this challenge, then repeat the request.
    Required(StepUpChallenge),
}

impl Refusal {
    fn reply(self) -> SecurityReply {
        match self {
            Self::Remote => failed(
                FailReason::RemoteForbidden,
                GateError::RemoteForbidden.to_string(),
            ),
            Self::Required(challenge) => SecurityReply::StepUpRequired { challenge },
        }
    }
}

/// A growing, bounded wait after failures.
#[derive(Debug, Default)]
struct Backoff {
    failures: u32,
    last: UnixMillis,
}

impl Backoff {
    fn wait(failures: u32) -> u64 {
        if failures <= FREE_TRIES {
            return 0;
        }
        let doubled = 1u64 << (failures - FREE_TRIES).min(16);
        (doubled * 1000).min(MAX_BACKOFF.as_millis() as u64)
    }

    fn forget_old(&mut self, now: UnixMillis) {
        if now.saturating_sub(self.last) >= BACKOFF_RESET.as_millis() as u64 {
            self.failures = 0;
        }
    }

    /// Milliseconds until the next try is allowed.
    fn remaining(&mut self, now: UnixMillis) -> u64 {
        self.forget_old(now);
        (self.last + Self::wait(self.failures)).saturating_sub(now)
    }

    fn failed(&mut self, now: UnixMillis) {
        self.forget_old(now);
        self.failures += 1;
        self.last = now;
    }

    fn clear(&mut self) {
        self.failures = 0;
    }
}

fn shell_open() -> SecurityReply {
    failed(
        FailReason::RemoteShellOpen,
        "close the remote shells first: on this computer a code decides, and a remote shell \
         could type one",
    )
}

fn mail_failure(err: MailError) -> SecurityReply {
    let reason = match err {
        MailError::NotConfigured => FailReason::MailNotConfigured,
        MailError::InvalidRecipient => FailReason::RecipientNotAllowed,
        MailError::RateLimited => FailReason::RateLimited,
        MailError::Invalid(_) | MailError::Failed(_) => FailReason::MailFailed,
    };
    failed(reason, err.to_string())
}

/// How one verification should be asked.
struct Ask {
    window: Option<u64>,
    totp: Option<Sensitive>,
    language: Option<String>,
}

/// The target of everything that changes mail settings; the class is shared.
fn mail_target() -> GateTarget {
    GateTarget::new(GatedAction::MailSettings, &())
}

/// Persisted state (`<data dir>/security/state.json`; no secrets).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedState {
    grace_secs: Option<u64>,
    wizard: WizardState,
    wizard_updated_at: Option<UnixMillis>,
    /// `Some(false)`: the owner turned remote shells off.
    remote_shell: Option<bool>,
}

/// Construction parameters.
pub struct SecurityOptions {
    pub data_dir: Option<PathBuf>,
    /// Where `mail-preset.toml` lives.
    pub config_dir: Option<PathBuf>,
    pub server_name: String,
    pub clock: SharedClock,
    pub verifier: Arc<dyn LocalVerifier>,
    pub secrets: Arc<dyn KeyStore>,
}

/// The security service. Share it through an `Arc`.
pub struct Security {
    clock: SharedClock,
    verifier: Arc<dyn LocalVerifier>,
    step_up: StepUp,
    recovery: Recovery,
    notifier: Arc<Notifier>,
    totp: OnceLock<Arc<TotpManager>>,
    audit: OnceLock<Arc<AuditLog>>,
    shells: OnceLock<Shells>,
    pending_totp: Mutex<Option<(u64, PendingEnrollment)>>,
    state: Mutex<PersistedState>,
    state_path: Option<PathBuf>,
    /// Wrong authenticator or backup codes.
    codes: Mutex<Backoff>,
    /// Canceled or failed OS prompts. Kept apart from `codes`, so spamming
    /// one does not hold back the other.
    prompts: Mutex<Backoff>,
    next_enrollment: AtomicU64,
    test_mails: Mutex<Vec<UnixMillis>>,
    /// An OS prompt is on screen (one at a time).
    prompting: AtomicBool,
    prompt_timeout: Duration,
    #[cfg(test)]
    test_shell_open: AtomicBool,
}

/// Clears [`Security::prompting`] however the prompt ends.
struct PromptGuard<'a>(&'a AtomicBool);

impl Drop for PromptGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The text of the OS prompt. It comes from here, never from the caller,
/// so another local process cannot put words in a real system dialog.
fn action_text(action: GatedAction, language: &str) -> &'static str {
    use GatedAction as A;
    let [ko, ja, es, en] = match action {
        A::PairingCreate => [
            "새 기기 연결 코드 만들기",
            "新しいデバイスのペアリングコードを作成",
            "Crear un código de vinculación",
            "Create a pairing code",
        ],
        A::TotpSetup => [
            "2단계 인증 설정",
            "2段階認証の設定",
            "Configurar la verificación en dos pasos",
            "Set up 2-step verification",
        ],
        A::TotpReset => [
            "2단계 인증 앱 변경",
            "認証アプリの変更",
            "Cambiar la app de autenticación",
            "Replace the authenticator app",
        ],
        A::TotpDisable => [
            "2단계 인증 끄기",
            "2段階認証をオフにする",
            "Desactivar la verificación en dos pasos",
            "Turn off 2-step verification",
        ],
        A::DeviceApprove => [
            "기기 권한 넓히기",
            "デバイスの権限を広げる",
            "Ampliar el permiso de un dispositivo",
            "Widen a device's permission",
        ],
        A::RemoteShellEnable => [
            "원격 셸 켜기",
            "リモートシェルをオンにする",
            "Activar los shells remotos",
            "Turn on remote shells",
        ],
        A::SecuritySettings => [
            "보안 설정 변경",
            "セキュリティ設定の変更",
            "Cambiar la configuración de seguridad",
            "Change security settings",
        ],
        A::MailSettings => [
            "메일 설정 변경",
            "メール設定の変更",
            "Cambiar la configuración del correo",
            "Change mail settings",
        ],
        A::BackupCodesExport => [
            "백업 코드 새로 만들기",
            "バックアップコードを新しく作成",
            "Crear códigos de respaldo nuevos",
            "Create new backup codes",
        ],
        A::Recovery => [
            "2단계 인증 복구",
            "2段階認証の復旧",
            "Recuperar la verificación en dos pasos",
            "Recover 2-step verification",
        ],
        A::ConductorSetup => [
            "컨덕터 설정 변경",
            "コンダクター設定の変更",
            "Cambiar el conductor",
            "Change the conductor",
        ],
        A::ConductorDecide => [
            "컨덕터 요청 허용",
            "コンダクターの要求を許可",
            "Permitir lo que pide el conductor",
            "Allow what the conductor asked",
        ],
        A::ConductorKill => [
            "컨덕터 긴급 정지",
            "コンダクターの緊急停止",
            "Parada de emergencia del conductor",
            "Emergency stop of the conductor",
        ],
        A::TaskSettings => [
            "할 일 설정 변경",
            "タスク設定の変更",
            "Cambiar la configuración de tareas",
            "Change the task settings",
        ],
        A::EngineUrl => [
            "저장된 엔진 키를 새 주소로 보내기",
            "保存したエンジンのキーを新しいアドレスに送る",
            "Enviar la clave guardada a una dirección nueva",
            "Send a saved engine key to a new address",
        ],
    };
    pick_language(language, ko, ja, es, en)
}

fn pick_language(
    language: &str,
    ko: &'static str,
    ja: &'static str,
    es: &'static str,
    en: &'static str,
) -> &'static str {
    let language = language.trim().to_ascii_lowercase();
    match language.get(..2) {
        Some("ko") => ko,
        Some("ja") => ja,
        Some("es") => es,
        _ => en,
    }
}

/// The prompt of [`SecurityCall::StepUpTest`], which approves nothing.
fn test_text(language: &str) -> &'static str {
    pick_language(
        language,
        "본인 확인 시험 (아무것도 승인되지 않습니다)",
        "本人確認のテスト (何も承認されません)",
        "Prueba de verificación (no se aprueba nada)",
        "Test the verification (nothing is approved)",
    )
}

/// The prompt of [`SecurityCall::AppUnlock`], which approves nothing either.
fn unlock_text(language: &str) -> &'static str {
    pick_language(
        language,
        "Voice Command AI 잠금 해제",
        "Voice Command AI のロックを解除",
        "Desbloquear Voice Command AI",
        "Unlock Voice Command AI",
    )
}

/// What the person reads in the OS dialog for `target`.
fn prompt_for(target: &GateTarget, language: &str) -> String {
    let text = action_text(target.action, language);
    match &target.detail {
        Some(detail) => format!("{text}: {detail}"),
        None => text.to_owned(),
    }
}

impl Security {
    pub fn new(opts: SecurityOptions) -> anyhow::Result<Self> {
        let state_path = opts
            .data_dir
            .as_ref()
            .map(|d| d.join("security").join(STATE_FILE));
        let state: PersistedState = match &state_path {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    // Unknown state: remote shells stay off until the owner
                    // turns them on again.
                    tracing::warn!(path = %path.display(), "unreadable security state, remote shells off: {e}");
                    PersistedState {
                        remote_shell: Some(false),
                        ..PersistedState::default()
                    }
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => PersistedState::default(),
                Err(e) => return Err(e.into()),
            },
            None => PersistedState::default(),
        };
        let preset = opts.config_dir.as_deref().map(mail::preset_path);
        // A bad preset costs the preset, never the gates that depend on
        // this service.
        let mail = MailService::open(
            preset.as_deref(),
            opts.data_dir.as_deref(),
            Arc::clone(&opts.secrets),
        )
        .or_else(|err| {
            tracing::error!("ignoring the mail preset: {err:#}");
            MailService::open(None, opts.data_dir.as_deref(), Arc::clone(&opts.secrets))
        })?;
        let mail = Arc::new(mail);
        let grace = state
            .grace_secs
            // A window below the settings' minimum (a hand-edited `0`) would
            // issue tokens that expire at once: every gated action, the
            // settings that could fix it included, would be out of reach.
            .map_or(stepup::DEFAULT_GRACE, |secs| {
                Duration::from_secs(secs.max(MIN_GRACE_SECS))
            });
        Ok(Self {
            step_up: StepUp::new(opts.clock.clone(), grace),
            recovery: Recovery::new(opts.clock.clone()),
            notifier: Arc::new(Notifier::new(mail, opts.server_name, opts.clock.clone())),
            clock: opts.clock,
            verifier: opts.verifier,
            totp: OnceLock::new(),
            audit: OnceLock::new(),
            shells: OnceLock::new(),
            pending_totp: Mutex::new(None),
            state: Mutex::new(state),
            state_path,
            codes: Mutex::new(Backoff::default()),
            prompts: Mutex::new(Backoff::default()),
            next_enrollment: AtomicU64::new(1),
            test_mails: Mutex::new(Vec::new()),
            prompting: AtomicBool::new(false),
            prompt_timeout: PROMPT_TIMEOUT,
            #[cfg(test)]
            test_shell_open: AtomicBool::new(false),
        })
    }

    /// The production service: the platform verifier, and mail secrets in
    /// the server credential store (`<data dir>/security/mail-keys`).
    pub fn open(
        data_dir: &Path,
        config_dir: &Path,
        server_name: String,
        verifier: Option<Arc<dyn LocalVerifier>>,
    ) -> anyhow::Result<Self> {
        Self::new(SecurityOptions {
            data_dir: Some(data_dir.to_owned()),
            config_dir: Some(config_dir.to_owned()),
            server_name,
            clock: vca_net::system_clock(),
            verifier: verifier.unwrap_or_else(verifier::platform),
            secrets: Arc::new(FileKeyStore::new(
                data_dir.join("security").join("mail-keys"),
            )),
        })
    }

    /// Connects the authenticator store shared with the remote transport.
    pub fn attach_totp(&self, totp: Arc<TotpManager>, audit: Option<Arc<AuditLog>>) {
        let _ = self.totp.set(totp);
        if let Some(audit) = audit {
            let _ = self.audit.set(audit);
        }
    }

    /// Connects remote shells and applies the owner's saved switch.
    pub fn attach_shells(&self, shells: Shells) {
        let saved = lock(&self.state).remote_shell;
        if let Some(on) = saved {
            shells.set_owner_enabled(on);
        }
        let _ = self.shells.set(shells);
    }

    pub fn notifier(&self) -> &Arc<Notifier> {
        &self.notifier
    }

    fn mail(&self) -> &Arc<MailService> {
        self.notifier.mail()
    }

    fn totp(&self) -> Reply<&Arc<TotpManager>> {
        self.totp.get().ok_or_else(|| {
            failed(
                FailReason::Unavailable,
                "the authenticator is not available",
            )
        })
    }

    fn enrolled(&self) -> bool {
        self.totp.get().is_some_and(|t| t.is_enrolled())
    }

    /// The gate: may `origin` perform `target` with `token`? Without a usable
    /// token the answer is a challenge for exactly this request. A
    /// single-use token is spent here.
    pub async fn gate(
        &self,
        origin: Origin,
        target: &GateTarget,
        token: Option<&str>,
        caller: &Caller,
    ) -> Result<(), Refusal> {
        self.refuse(
            self.step_up.require(origin, target, token, caller),
            target,
            caller,
        )
    }

    /// Like [`Self::gate`], but the token stays valid: for a request with
    /// checks still to pass. [`Self::spend`] ends it once those passed.
    fn gate_unspent(
        &self,
        target: &GateTarget,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply<()> {
        let token = token.as_ref().map(|t| t.0.as_str());
        let checked = self.step_up.check(Origin::Local, target, token, caller);
        self.refuse(checked, target, caller).map_err(Refusal::reply)
    }

    fn spend(&self, target: &GateTarget, token: &Option<Sensitive>) {
        if let (false, Some(token)) = (target.action.shared_window(), token) {
            self.step_up.revoke(token.0.trim());
        }
    }

    fn refuse(
        &self,
        checked: Result<(), GateError>,
        target: &GateTarget,
        caller: &Caller,
    ) -> Result<(), Refusal> {
        match checked {
            Ok(()) => Ok(()),
            Err(GateError::RemoteForbidden) => Err(Refusal::Remote),
            Err(GateError::StepUpRequired) => {
                Err(Refusal::Required(self.step_up.challenge(target, caller)))
            }
        }
    }

    async fn gated(
        &self,
        target: &GateTarget,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply<()> {
        let token = token.as_ref().map(|t| t.0.as_str());
        self.gate(Origin::Local, target, token, caller)
            .await
            .map_err(Refusal::reply)
    }

    /// Refuses until the wait after recent failures is over.
    fn check_backoff(&self, backoff: &Mutex<Backoff>, what: &str) -> Reply<()> {
        let left = lock(backoff).remaining(self.clock.now_millis());
        if left > 0 {
            return Err(failed(
                FailReason::RateLimited,
                format!("too many {what}; wait {} seconds", left.div_ceil(1000)),
            ));
        }
        Ok(())
    }

    fn check_guesses(&self) -> Reply<()> {
        self.check_backoff(&self.codes, "wrong codes")
    }

    fn wrong_guess(&self) {
        lock(&self.codes).failed(self.clock.now_millis());
    }

    /// A record in the audit log, when one is attached.
    fn audit(&self, what: &str, detail: impl Into<String>) {
        if let Some(audit) = self.audit.get() {
            audit.record(AuditEvent::LocalSecurity {
                what: what.to_owned(),
                detail: detail.into(),
            });
        }
    }

    /// One security request from the local channel.
    pub async fn handle(&self, call: SecurityCall) -> SecurityReply {
        self.handle_from(call, &Caller::default()).await
    }

    /// One security request from the process described by `caller`.
    pub async fn handle_from(&self, call: SecurityCall, caller: &Caller) -> SecurityReply {
        self.dispatch(call, caller)
            .await
            .unwrap_or_else(|failure| failure)
    }

    async fn dispatch(&self, call: SecurityCall, caller: &Caller) -> Reply {
        match call {
            SecurityCall::Status { step_up } => Ok(self.status_reply(step_up.as_ref()).await),
            SecurityCall::StepUp {
                nonce,
                window,
                totp,
                presence,
                language,
            } => {
                let ask = Ask {
                    window,
                    totp,
                    language,
                };
                self.verify(&nonce, ask, presence, caller).await
            }
            SecurityCall::StepUpTest {
                window,
                totp,
                language,
            } => {
                let ask = Ask {
                    window,
                    totp,
                    language,
                };
                self.verify_test(ask, test_text, caller).await
            }
            SecurityCall::AppUnlock {
                window,
                totp,
                language,
            } => {
                let ask = Ask {
                    window,
                    totp,
                    language,
                };
                self.verify_test(ask, unlock_text, caller).await
            }
            SecurityCall::StepUpEnd { step_up } => {
                self.step_up.revoke(&step_up.0);
                Ok(SecurityReply::Done)
            }
            SecurityCall::TotpBegin {
                account,
                replace,
                step_up,
            } => self.totp_begin(&account, replace, &step_up, caller).await,
            SecurityCall::TotpConfirm {
                code,
                current_code,
                step_up,
            } => {
                self.totp_confirm(&code, current_code, &step_up, caller)
                    .await
            }
            SecurityCall::TotpDisable {
                current_code,
                step_up,
            } => self.totp_disable(current_code, &step_up, caller).await,
            SecurityCall::BackupCodes {
                current_code,
                step_up,
            } => self.backup_codes(current_code, &step_up, caller).await,
            SecurityCall::Settings {
                grace_secs,
                remote_shell,
                step_up,
            } => {
                self.settings(grace_secs, remote_shell, &step_up, caller)
                    .await
            }
            SecurityCall::Wizard { state } => {
                self.update_state(|s| {
                    s.wizard = state;
                    s.wizard_updated_at = Some(self.clock.now_millis());
                })?;
                Ok(self.status_reply(None).await)
            }
            SecurityCall::MailGet => Ok(self.mail_reply()),
            SecurityCall::MailSet {
                settings,
                current_code,
                step_up,
            } => {
                self.mail_set(&settings, current_code, &step_up, caller)
                    .await
            }
            SecurityCall::MailSecret {
                which,
                secret,
                current_code,
                step_up,
            } => {
                self.mail_secret(which, secret, current_code, &step_up, caller)
                    .await
            }
            SecurityCall::MailTest => self.mail_test().await,
            SecurityCall::RecoveryStart { step_up } => self.recovery_start(&step_up, caller).await,
            SecurityCall::RecoveryFinish {
                code,
                backup_code,
                purpose,
                step_up,
            } => {
                self.recovery_finish(code, backup_code, purpose, &step_up, caller)
                    .await
            }
        }
    }

    async fn status(&self, token: Option<&Sensitive>) -> SecurityStatus {
        let availability = self.verifier.availability().await;
        let (available, detail) = match availability {
            Availability::Available => (true, None),
            Availability::Unavailable(why) => (false, Some(why)),
        };
        let totp = self.totp.get().and_then(|t| t.status());
        let mail = self.mail().view();
        let state = lock(&self.state).clone();
        let shell_on = self.shells.get().is_none_or(Shells::remote_enabled);
        let expires_at = token.and_then(|t| self.step_up.expires_at(&t.0));
        let enrolled = totp.is_some();
        SecurityStatus {
            verifier: VerifierInfo {
                kind: self.verifier.kind(),
                available,
                detail,
            },
            totp: TotpInfo {
                enrolled,
                enrolled_at: totp.as_ref().map(|t| t.enrolled_at),
                backup_codes_remaining: totp.as_ref().map_or(0, |t| t.backup_codes_remaining),
                backup_codes_total: totp.as_ref().map_or(0, |t| t.backup_codes_total),
            },
            mail: MailInfo {
                configured: mail.configured,
                mode: mail.form.mode,
                recipients: mail.form.recipients.clone(),
                notifications: mail.form.notifications.enabled,
            },
            step_up: StepUpInfo {
                active: expires_at.is_some(),
                expires_at,
                grace_secs: self.step_up.grace().as_secs(),
            },
            wizard: WizardInfo {
                state: state.wizard,
                updated_at: state.wizard_updated_at,
            },
            remote_shell: RemoteShellInfo { enabled: shell_on },
            recovery: if mail.configured {
                RecoveryPath::MailAndLocal
            } else {
                RecoveryPath::BackupCodeAndLocal
            },
            setup_complete: enrolled,
        }
    }

    async fn status_reply(&self, token: Option<&Sensitive>) -> SecurityReply {
        SecurityReply::Status {
            status: Box::new(self.status(token).await),
        }
    }

    fn mail_reply(&self) -> SecurityReply {
        SecurityReply::Mail {
            view: Box::new(self.mail().view()),
        }
    }

    fn update_state(&self, f: impl FnOnce(&mut PersistedState)) -> Reply<()> {
        let mut state = lock(&self.state);
        let mut next = state.clone();
        f(&mut next);
        if let Some(path) = &self.state_path {
            let json = serde_json::to_vec_pretty(&next)
                .map_err(|e| failed(FailReason::Internal, e.to_string()))?;
            mail::write_atomic(path, &json)
                .map_err(|e| failed(FailReason::Internal, e.to_string()))?;
        }
        *state = next;
        Ok(())
    }

    fn issued(
        &self,
        nonce: &Sensitive,
        method: StepUpMethod,
        target: &GateTarget,
        caller: &Caller,
    ) -> SecurityReply {
        self.step_up.close_challenge(&nonce.0);
        let t = self.step_up.issue(method, target, caller);
        self.audit(
            "step_up_verified",
            format!("{} {:?}", target.action.as_str(), method),
        );
        SecurityReply::StepUp {
            token: Sensitive(t.token),
            expires_at: t.expires_at,
            method: t.method,
        }
    }

    async fn verify(
        &self,
        nonce: &Sensitive,
        ask: Ask,
        presence: Option<Presence>,
        caller: &Caller,
    ) -> Reply {
        // A paired device behind a remote shell has the authenticator code;
        // it must not pass for the person at this computer.
        if caller.in_remote_shell {
            return Err(failed(
                FailReason::RemoteForbidden,
                GateError::RemoteForbidden.to_string(),
            ));
        }
        let Some(target) = self.step_up.challenge_target(&nonce.0, caller) else {
            return Err(failed(
                FailReason::StepUpRequired,
                "this verification has expired; start the action again",
            ));
        };
        // Where the OS can ask the person at this computer, only it counts.
        // An authenticator code proves the phone, not who sits here: a paired
        // device in a remote shell could type one and step up without anyone
        // at the machine.
        if let Availability::Unavailable(why) = self.verifier.availability().await {
            return match ask.totp {
                Some(_) if self.remote_shell_open() => Err(shell_open()),
                Some(code) => self.verify_code(nonce, &code.0, &target, caller),
                None if target.action == GatedAction::TotpSetup && !self.enrolled() => {
                    self.verify_presence(nonce, presence, &target, caller)
                }
                None => Err(self.no_prompt(why)),
            };
        }
        let language = self.prompt_language(&ask);
        let text = prompt_for(&target, &language);
        match self.prompt(&text, ask.window).await {
            Ok(()) => {
                let method = match self.verifier.kind() {
                    VerifierKind::MacLocalAuth => StepUpMethod::MacLocalAuth,
                    // `None` never verifies; a fake reports Hello.
                    VerifierKind::WindowsHello | VerifierKind::None => StepUpMethod::WindowsHello,
                };
                Ok(self.issued(nonce, method, &target, caller))
            }
            Err(failure) => {
                self.audit("step_up_failed", target.action.as_str());
                Err(failure)
            }
        }
    }

    fn prompt_language(&self, ask: &Ask) -> String {
        ask.language
            .clone()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| self.mail().form().language)
    }

    /// One OS prompt, one at a time, with a bounded wait.
    async fn prompt(&self, text: &str, window: Option<u64>) -> Reply<()> {
        // Canceled prompts count, so a process cannot keep prompting until
        // the owner clicks yes out of habit.
        self.check_backoff(&self.prompts, "canceled prompts")?;
        if self.prompting.swap(true, Ordering::SeqCst) {
            return Err(failed(
                FailReason::RateLimited,
                "a verification prompt is already open",
            ));
        }
        let _guard = PromptGuard(&self.prompting);
        let answer = tokio::time::timeout(self.prompt_timeout, self.verifier.verify(text, window))
            .await
            .unwrap_or(VerifyOutcome::Canceled);
        let now = self.clock.now_millis();
        match answer {
            VerifyOutcome::Verified => {
                lock(&self.prompts).clear();
                Ok(())
            }
            VerifyOutcome::Canceled => {
                lock(&self.prompts).failed(now);
                Err(failed(
                    FailReason::VerificationCanceled,
                    "verification was canceled",
                ))
            }
            VerifyOutcome::Failed(why) => {
                lock(&self.prompts).failed(now);
                Err(failed(FailReason::VerificationFailed, why))
            }
            VerifyOutcome::Unavailable(why) => Err(self.no_prompt(why)),
        }
    }

    /// Tries the verification; nothing is approved and no token issued.
    /// `text` words the OS prompt in the asking app's language.
    async fn verify_test(
        &self,
        ask: Ask,
        text: fn(&str) -> &'static str,
        caller: &Caller,
    ) -> Reply {
        if caller.in_remote_shell {
            return Err(failed(
                FailReason::RemoteForbidden,
                GateError::RemoteForbidden.to_string(),
            ));
        }
        if let Availability::Unavailable(why) = self.verifier.availability().await {
            let Some(code) = ask.totp else {
                return Err(self.no_prompt(why));
            };
            if self.remote_shell_open() {
                return Err(shell_open());
            }
            self.check_code(&code.0, false)?;
            return Ok(SecurityReply::Done);
        }
        let text = text(&self.prompt_language(&ask));
        self.prompt(text, ask.window).await?;
        Ok(SecurityReply::Done)
    }

    fn no_prompt(&self, why: String) -> SecurityReply {
        if self.enrolled() {
            failed(FailReason::TotpRequired, "enter the authenticator code")
        } else {
            failed(FailReason::VerificationUnavailable, why)
        }
    }

    /// First enrollment with nothing to verify against: the only thing left
    /// is a person saying so. Refused to anything running inside a session,
    /// and on Linux to anything without a terminal.
    fn verify_presence(
        &self,
        nonce: &Sensitive,
        presence: Option<Presence>,
        target: &GateTarget,
        caller: &Caller,
    ) -> Reply {
        let Some(presence) = presence else {
            return Err(failed(
                FailReason::PresenceRequired,
                "confirm in the desktop app or run vca on an interactive terminal",
            ));
        };
        let no_terminal = caller.pid.and_then(crate::peer::has_terminal) == Some(false);
        let unproven = no_terminal
            && (presence == Presence::Terminal
                || !caller.pid.is_some_and(crate::peer::is_desktop_app));
        if caller.in_session || caller.in_remote_shell || unproven {
            self.audit("first_enrollment_refused", format!("{presence:?}"));
            return Err(failed(
                FailReason::PresenceRequired,
                "the first authenticator must be set up by a person at this computer, \
                 not from a session",
            ));
        }
        self.audit("first_enrollment_confirmed", format!("{presence:?}"));
        Ok(self.issued(nonce, StepUpMethod::Presence, target, caller))
    }

    /// Checks an authenticator code, and with `backup` a backup code too.
    fn check_code(&self, code: &str, backup: bool) -> Reply<StepUpMethod> {
        let totp = self.totp()?;
        if !totp.is_enrolled() {
            return Err(failed(
                FailReason::VerificationUnavailable,
                "no authenticator is set up",
            ));
        }
        self.check_guesses()?;
        let checked = if backup {
            totp.verify_code_or_backup(code).map(|kind| match kind {
                CodeKind::Totp => StepUpMethod::Totp,
                CodeKind::BackupCode => StepUpMethod::BackupCode,
            })
        } else {
            totp.verify(code).map(|()| StepUpMethod::Totp)
        };
        match checked {
            Ok(method) => {
                lock(&self.codes).clear();
                Ok(method)
            }
            Err(TotpError::Storage) => Err(failed(FailReason::Internal, "storage failure")),
            // A code that was right a moment ago is no guess.
            Err(TotpError::Replayed) => Err(failed(FailReason::TotpReplayed, CODE_JUST_USED)),
            Err(err) => {
                self.wrong_guess();
                Err(failed(FailReason::TotpInvalid, err.to_string()))
            }
        }
    }

    /// The owner's authenticator code (or a backup code) for an action a
    /// paired device asks for (the conductor kill switch). Wrong codes count
    /// toward the same back-off as local ones. Errors: `totp_required`
    /// (no code), `totp_invalid` (wrong or just used), `forbidden` (too many
    /// wrong codes), `unsupported` (no authenticator set up).
    pub(crate) fn verify_remote_code(
        &self,
        code: Option<&str>,
        purpose: &str,
    ) -> Result<(), vca_core::Error> {
        use vca_core::error::ErrorCode;
        let Some(code) = code.map(str::trim).filter(|c| !c.is_empty()) else {
            return Err(if self.enrolled() {
                vca_core::Error::api(
                    ErrorCode::TotpRequired,
                    "enter a code from your authenticator app",
                )
            } else {
                vca_core::Error::api(
                    ErrorCode::Unsupported,
                    "set up the authenticator on the PC first",
                )
            });
        };
        match self.check_code(code, true) {
            Ok(_) => {
                self.audit("remote_code_verified", purpose);
                Ok(())
            }
            Err(failure) => {
                self.audit("remote_code_failed", purpose);
                let (code, message) = match &failure {
                    SecurityReply::Failed { reason, message } => (
                        match reason {
                            FailReason::TotpInvalid | FailReason::TotpReplayed => {
                                ErrorCode::TotpInvalid
                            }
                            FailReason::RateLimited => ErrorCode::Forbidden,
                            FailReason::VerificationUnavailable => ErrorCode::Unsupported,
                            _ => ErrorCode::Unavailable,
                        },
                        message.clone(),
                    ),
                    _ => (
                        ErrorCode::Unavailable,
                        "the code could not be checked".into(),
                    ),
                };
                Err(vca_core::Error::api(code, message))
            }
        }
    }

    /// With no OS verifier, a lost authenticator would otherwise leave no
    /// way into recovery: there, and only there, a backup code verifies.
    fn verify_code(
        &self,
        nonce: &Sensitive,
        code: &str,
        target: &GateTarget,
        caller: &Caller,
    ) -> Reply {
        match self.check_code(code, target.action == GatedAction::Recovery) {
            Ok(method) => Ok(self.issued(nonce, method, target, caller)),
            Err(failure) => {
                self.audit("step_up_failed", target.action.as_str());
                Err(failure)
            }
        }
    }

    /// Replacing or removing an enrolled authenticator, or new backup codes,
    /// needs on top of step-up proof of the authenticator itself or a
    /// recovery grant for exactly this operation.
    fn require_possession(
        &self,
        current_code: Option<Sensitive>,
        op: GrantOp,
        enrollment: Option<u64>,
        token: &Option<Sensitive>,
    ) -> Reply<()> {
        if self.recovery.granted_for(op, enrollment) || self.token_proved(token) {
            return Ok(());
        }
        let Some(code) = current_code else {
            return Err(failed(
                FailReason::CurrentCodeRequired,
                "enter a code from the current authenticator, or use recovery",
            ));
        };
        self.check_code(&code.0, true).map(|_| ())
    }

    /// The authenticator already proved itself for this token.
    fn token_proved(&self, token: &Option<Sensitive>) -> bool {
        token.as_ref().is_some_and(|t| self.step_up.proved(&t.0))
    }

    /// A remote shell is open right now.
    fn remote_shell_open(&self) -> bool {
        #[cfg(test)]
        if self.test_shell_open.load(Ordering::SeqCst) {
            return true;
        }
        self.shells.get().is_some_and(Shells::any_open)
    }

    /// Does a recovery grant stand in for the step-up of its one operation?
    /// Only where no OS prompt exists: the authenticator is lost, so nothing
    /// else could verify, and recovery itself already took two proofs. Where
    /// an OS prompt exists it still asks. Never for a process inside a session
    /// or while a remote shell is open: those could not have recovered.
    async fn grant_stands_in(&self, granted: bool, caller: &Caller) -> bool {
        granted
            && !caller.in_session
            && !caller.in_remote_shell
            && !self.remote_shell_open()
            && matches!(
                self.verifier.availability().await,
                Availability::Unavailable(_)
            )
    }

    async fn totp_begin(
        &self,
        account: &str,
        replace: bool,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        let totp = self.totp()?;
        let enrolled = totp.is_enrolled();
        if enrolled && !replace {
            return Err(failed(
                FailReason::Conflict,
                "an authenticator is already set up",
            ));
        }
        let action = if enrolled {
            GatedAction::TotpReset
        } else {
            GatedAction::TotpSetup
        };
        let granted = enrolled && self.recovery.granted_op(GrantOp::Replace);
        if !self.grant_stands_in(granted, caller).await {
            self.gated(&GateTarget::new(action, &()), token, caller)
                .await?;
        }
        let pending = totp.begin_enrollment("Voice Command AI", account);
        let reply = SecurityReply::TotpPending {
            otpauth_uri: Sensitive(pending.otpauth_uri.to_string()),
            secret_base32: Sensitive(pending.secret_base32().to_string()),
        };
        let id = self.next_enrollment.fetch_add(1, Ordering::Relaxed);
        self.recovery.bind_enrollment(id);
        *lock(&self.pending_totp) = Some((id, pending));
        Ok(reply)
    }

    async fn totp_confirm(
        &self,
        code: &Sensitive,
        current_code: Option<Sensitive>,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        let totp = self.totp()?;
        let replaced = totp.is_enrolled();
        let action = if replaced {
            GatedAction::TotpReset
        } else {
            GatedAction::TotpSetup
        };
        let open = lock(&self.pending_totp).clone();
        let granted = replaced
            && self
                .recovery
                .granted_for(GrantOp::Replace, open.as_ref().map(|(id, _)| *id));
        if !self.grant_stands_in(granted, caller).await {
            self.gated(&GateTarget::new(action, &()), token, caller)
                .await?;
        }
        let (id, pending) = open.ok_or_else(|| {
            failed(
                FailReason::Conflict,
                "no enrollment in progress; start one first",
            )
        })?;
        // The new code first: a mistyped one spends nothing, so nothing has
        // to stay open for the next try.
        if !totp.enrollment_code_fits(&pending, &code.0) {
            self.check_guesses()?;
            self.wrong_guess();
            self.recovery.wrong_code();
            return Err(failed(
                FailReason::TotpInvalid,
                TotpError::Invalid.to_string(),
            ));
        }
        if replaced {
            self.require_possession(current_code, GrantOp::Replace, Some(id), token)?;
        }
        let codes = totp.confirm_enrollment(&pending, &code.0).map_err(|e| {
            let reason = match e {
                TotpError::Storage => FailReason::Internal,
                _ => FailReason::TotpInvalid,
            };
            failed(reason, e.to_string())
        })?;
        lock(&self.pending_totp).take();
        self.recovery.close_grant();
        if let Some(audit) = self.audit.get() {
            audit.record(AuditEvent::TotpEnrolled {
                by: "local".into(),
                replaced,
            });
        }
        self.notifier.event(SecurityEvent::TotpChanged(if replaced {
            TotpChange::Reset
        } else {
            TotpChange::Setup
        }));
        Ok(SecurityReply::TotpEnrolled {
            backup_codes: codes.into_iter().map(Sensitive).collect(),
        })
    }

    async fn totp_disable(
        &self,
        current_code: Option<Sensitive>,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        let target = GateTarget::new(GatedAction::TotpDisable, &());
        let granted = self.recovery.granted_op(GrantOp::Disable);
        if !self.grant_stands_in(granted, caller).await {
            self.gate_unspent(&target, token, caller)?;
        }
        let totp = self.totp()?;
        if !totp.is_enrolled() {
            return Err(failed(FailReason::Conflict, "no authenticator is set up"));
        }
        self.require_possession(current_code, GrantOp::Disable, None, token)?;
        totp.disable()
            .map_err(|e| failed(FailReason::Internal, e.to_string()))?;
        self.spend(&target, token);
        self.recovery.close_grant();
        tracing::info!("authenticator turned off by the owner");
        self.audit("totp_disabled", "local");
        self.notifier
            .event(SecurityEvent::TotpChanged(TotpChange::Disabled));
        Ok(self.status_reply(None).await)
    }

    async fn backup_codes(
        &self,
        current_code: Option<Sensitive>,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        let target = GateTarget::new(GatedAction::BackupCodesExport, &());
        let granted = self.recovery.granted_op(GrantOp::BackupCodes);
        if !self.grant_stands_in(granted, caller).await {
            self.gate_unspent(&target, token, caller)?;
        }
        if !self.enrolled() {
            return Err(failed(
                FailReason::Conflict,
                "no authenticator is set up; set one up first",
            ));
        }
        // One approval must not be able to mint a recovery proof.
        self.require_possession(current_code, GrantOp::BackupCodes, None, token)?;
        let codes = self
            .totp()?
            .regenerate_backup_codes()
            .map_err(|e| match e {
                TotpError::NotEnrolled => failed(
                    FailReason::Conflict,
                    "no authenticator is set up; set one up first",
                ),
                other => failed(FailReason::Internal, other.to_string()),
            })?;
        self.spend(&target, token);
        self.recovery.close_grant();
        if let Some(audit) = self.audit.get() {
            audit.record(AuditEvent::BackupCodesRegenerated { by: "local".into() });
        }
        self.notifier
            .event(SecurityEvent::TotpChanged(TotpChange::BackupCodes));
        Ok(SecurityReply::TotpEnrolled {
            backup_codes: codes.into_iter().map(Sensitive).collect(),
        })
    }

    async fn settings(
        &self,
        grace_secs: Option<u64>,
        remote_shell: Option<bool>,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        if let Some(secs) = grace_secs {
            let max = stepup::MAX_GRACE.as_secs();
            if !(MIN_GRACE_SECS..=max).contains(&secs) {
                return Err(failed(
                    FailReason::InvalidRequest,
                    format!("the grace window must be {MIN_GRACE_SECS} to {max} seconds"),
                ));
            }
        }
        // Turning remote shells off only reduces access: never gated.
        let enables = remote_shell == Some(true);
        if enables || grace_secs.is_some() {
            let action = if enables {
                GatedAction::RemoteShellEnable
            } else {
                GatedAction::SecuritySettings
            };
            let target = GateTarget::new(action, &(grace_secs, remote_shell));
            self.gated(&target, token, caller).await?;
        }
        self.update_state(|s| {
            if grace_secs.is_some() {
                s.grace_secs = grace_secs;
            }
            if remote_shell.is_some() {
                s.remote_shell = remote_shell;
            }
        })?;
        if let Some(secs) = grace_secs {
            self.step_up.set_grace(Duration::from_secs(secs));
        }
        if let (Some(on), Some(shells)) = (remote_shell, self.shells.get()) {
            shells.set_owner_enabled(on);
        }
        if enables || grace_secs.is_some() {
            self.audit(
                "security_settings_changed",
                format!("grace={grace_secs:?} remote_shell={remote_shell:?}"),
            );
        }
        Ok(self.status_reply(None).await)
    }

    async fn mail_set(
        &self,
        form: &MailForm,
        current_code: Option<Sensitive>,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        let invalid = |e: mail::InvalidSetting| {
            let reason = if e.not_allowed {
                FailReason::RecipientNotAllowed
            } else {
                FailReason::InvalidRequest
            };
            failed(reason, e.to_string())
        };
        let checked = self.mail().validate(form).map_err(invalid)?;
        // The prompt shows the recipients, so the token is for this form.
        let target = GateTarget::new(GatedAction::MailSettings, &checked)
            .with_detail(checked.recipients.join(", "))
            .exact();
        self.gated(&target, token, caller).await?;
        // Mail carries the recovery code: one approval must not be able to
        // send it somewhere else.
        let moves = mail::delivery_changed(&self.mail().form(), &checked);
        if moves {
            self.mail_possession(current_code, token)?;
        }
        let before = self.mail().save(form).map_err(|e| match e {
            SaveError::Invalid(i) => invalid(i),
            SaveError::Io(m) => failed(FailReason::Internal, m),
        })?;
        self.audit(
            "mail_settings_changed",
            if moves { "delivery changed" } else { "details" },
        );
        if let Err(err) = self.notifier.mail_settings_changed(&before).await {
            // The change stands; the owner sees the result of the test mail.
            tracing::warn!(%err, "mail settings notification not sent");
        }
        Ok(self.mail_reply())
    }

    /// While an authenticator is enrolled, changing how mail is delivered
    /// needs it. One code covers one save: it works once, and a save can be
    /// the form and its secrets.
    fn mail_possession(
        &self,
        current_code: Option<Sensitive>,
        token: &Option<Sensitive>,
    ) -> Reply<()> {
        if !self.enrolled() || self.token_proved(token) {
            return Ok(());
        }
        let Some(code) = current_code else {
            return Err(failed(
                FailReason::CurrentCodeRequired,
                "enter a code from the current authenticator to change how mail is sent",
            ));
        };
        self.check_code(&code.0, true)?;
        if let Some(token) = token {
            self.step_up.mark_proved(&token.0);
        }
        Ok(())
    }

    async fn mail_secret(
        &self,
        which: SecretKind,
        secret: Option<Secret>,
        current_code: Option<Sensitive>,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        self.gated(&mail_target(), token, caller).await?;
        // A first secret changes nothing that worked before.
        let replaces = self.mail().has_secret(which);
        if replaces {
            self.mail_possession(current_code, token)?;
            if self.mail().configured() {
                // Before the change: without this secret the old settings
                // may no longer reach the old address.
                if let Err(err) = self.notifier.secret_changing().await {
                    tracing::warn!(%err, "mail secret notification not sent");
                }
            }
        }
        self.mail()
            .set_secret(which, secret.as_ref().map(Secret::expose))
            .map_err(|e| failed(FailReason::InvalidRequest, e.to_string()))?;
        self.audit("mail_secret_changed", format!("{which:?}"));
        Ok(self.mail_reply())
    }

    async fn mail_test(&self) -> Reply {
        let now = self.clock.now_millis();
        {
            let mut sent = lock(&self.test_mails);
            sent.retain(|t| *t > now.saturating_sub(3_600_000));
            let interval = TEST_MAIL_INTERVAL.as_millis() as u64;
            if sent.len() >= MAX_TEST_MAILS_PER_HOUR
                || sent.last().is_some_and(|t| now < t + interval)
            {
                return Err(failed(
                    FailReason::RateLimited,
                    "wait before sending another test mail",
                ));
            }
            sent.push(now);
        }
        let sent_to = self
            .notifier
            .send(Notice::Test, None)
            .await
            .map_err(mail_failure)?;
        Ok(SecurityReply::MailSent { sent_to })
    }

    async fn recovery_start(&self, token: &Option<Sensitive>, caller: &Caller) -> Reply {
        self.gated(&GateTarget::new(GatedAction::Recovery, &()), token, caller)
            .await?;
        if !self.enrolled() {
            return Err(failed(FailReason::Conflict, "no authenticator is set up"));
        }
        if !self.mail().configured() {
            return Err(mail_failure(MailError::NotConfigured));
        }
        let (code, expires_at) = self.recovery.issue_code().map_err(recovery_failure)?;
        let minutes = (recovery::CODE_TTL.as_secs() / 60) as u32;
        match self
            .notifier
            .send(Notice::RecoveryCode { code, minutes }, None)
            .await
        {
            Ok(to) => {
                self.audit("recovery_started", "mailed code");
                self.notifier.event(SecurityEvent::RecoveryStarted);
                Ok(SecurityReply::RecoveryCodeSent {
                    sent_to: to.iter().map(|a| mail::mask_address(a)).collect(),
                    expires_at,
                })
            }
            Err(err) => {
                self.recovery.cancel_code();
                Err(mail_failure(err))
            }
        }
    }

    async fn recovery_finish(
        &self,
        code: Option<Sensitive>,
        backup_code: Option<Sensitive>,
        purpose: GrantOp,
        token: &Option<Sensitive>,
        caller: &Caller,
    ) -> Reply {
        self.gated(&GateTarget::new(GatedAction::Recovery, &()), token, caller)
            .await?;
        let (grant_expires_at, proof) = match (code, backup_code) {
            (Some(code), _) => (
                self.recovery
                    .redeem_code(&code.0, purpose)
                    .map_err(recovery_failure)?,
                RecoveryProof::MailedCode,
            ),
            (None, Some(backup)) => {
                self.check_guesses()?;
                match self.totp()?.verify_backup_code(&backup.0) {
                    Ok(()) => (self.recovery.grant(purpose), RecoveryProof::BackupCode),
                    Err(TotpError::Storage) => {
                        return Err(failed(FailReason::Internal, "storage failure"));
                    }
                    Err(err) => {
                        self.wrong_guess();
                        return Err(failed(FailReason::TotpInvalid, err.to_string()));
                    }
                }
            }
            (None, None) => {
                return Err(failed(
                    FailReason::InvalidRequest,
                    "enter the mailed code or a backup code",
                ));
            }
        };
        self.audit("recovery_completed", format!("{proof:?} {purpose:?}"));
        self.notifier.event(SecurityEvent::RecoveryCompleted(proof));
        Ok(SecurityReply::RecoveryGranted { grant_expires_at })
    }

    /// Turns an audit record of the remote transport into a notification.
    pub fn on_audit(&self, event: &AuditEvent) {
        match notification_for(event) {
            Some(AuditNotice::Event(event)) => self.notifier.event(event),
            Some(AuditNotice::Failure(detail)) => self.notifier.auth_failure(&detail),
            None => {}
        }
    }
}

/// What an audit record means for the owner's mailbox.
#[derive(Debug, PartialEq, Eq)]
enum AuditNotice {
    Event(SecurityEvent),
    Failure(String),
}

fn notification_for(event: &AuditEvent) -> Option<AuditNotice> {
    match event {
        AuditEvent::DevicePaired {
            name, client_ip, ..
        } => Some(AuditNotice::Event(SecurityEvent::DevicePaired {
            device: name.clone(),
            ip: Some(client_ip.to_string()),
        })),
        AuditEvent::AuthFailed {
            failure, client_ip, ..
        } => {
            let what = match failure {
                // Normal steps of a correct sign-in, or the server's own state.
                AuthFailure::TotpRequired
                | AuthFailure::TotpNotEnrolled
                | AuthFailure::KillSwitch
                | AuthFailure::Internal => return None,
                AuthFailure::InvalidTotp => "wrong code",
                AuthFailure::InvalidPairingSecret | AuthFailure::PairingAttemptsExhausted => {
                    "bad pairing code"
                }
                AuthFailure::UnknownDevice | AuthFailure::DeviceRevoked => "unknown device",
                AuthFailure::TokenReuse => "reused token",
                _ => "failed sign-in",
            };
            Some(AuditNotice::Failure(format!("{client_ip}: {what}")))
        }
        AuditEvent::RateLimited { .. } => Some(AuditNotice::Failure("rate limit reached".into())),
        AuditEvent::TokenReuseDetected { client_ip, .. } => {
            Some(AuditNotice::Failure(format!("{client_ip}: reused token")))
        }
        _ => None,
    }
}

fn recovery_failure(err: RecoveryError) -> SecurityReply {
    let reason = match err {
        RecoveryError::NoCode | RecoveryError::Invalid => FailReason::CodeInvalid,
        RecoveryError::Expired => FailReason::CodeExpired,
        RecoveryError::AttemptsExceeded => FailReason::AttemptsExceeded,
        RecoveryError::RateLimited => FailReason::RateLimited,
    };
    failed(reason, err.to_string())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
