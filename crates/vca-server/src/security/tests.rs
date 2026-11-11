//! The security service end to end, with a fake verifier and a memory
//! mail transport.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use vca_net::totp::totp_code_at;
use vca_net::{Clock, ManualClock, MemoryKeyStore, TotpManager};

use super::mail::transport::{MailKind, MemoryTransport};
use super::mail::{MailForm, MailMode, SecretKind, TlsMode};
use super::verifier::{FakeVerifier, VerifyOutcome};
use super::*;

struct Fixture {
    clock: Arc<ManualClock>,
    security: Arc<Security>,
    verifier: Arc<FakeVerifier>,
    transport: Arc<MemoryTransport>,
}

fn fixture_with(verifier: Arc<FakeVerifier>) -> Fixture {
    let clock = ManualClock::new(1_793_631_900_000);
    let security = Security::new(SecurityOptions {
        data_dir: None,
        config_dir: None,
        server_name: "work-pc".into(),
        clock: clock.clone(),
        verifier: verifier.clone(),
        secrets: Arc::new(MemoryKeyStore::new()),
    })
    .unwrap();
    security.attach_totp(Arc::new(TotpManager::in_memory(clock.clone())), None);
    let transport = Arc::new(MemoryTransport::default());
    security.mail().set_transport_override(transport.clone());
    Fixture {
        clock,
        security: Arc::new(security),
        verifier,
        transport,
    }
}

fn fixture() -> Fixture {
    fixture_with(FakeVerifier::available())
}

fn reason(reply: &SecurityReply) -> FailReason {
    match reply {
        SecurityReply::Failed { reason, .. } => *reason,
        SecurityReply::StepUpRequired { .. } => FailReason::StepUpRequired,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn nonce_of(reply: &SecurityReply) -> Sensitive {
    match reply {
        SecurityReply::StepUpRequired { challenge } => Sensitive(challenge.nonce.clone()),
        other => panic!("expected a challenge, got {other:?}"),
    }
}

fn step_up(nonce: &Sensitive) -> SecurityCall {
    SecurityCall::StepUp {
        nonce: nonce.clone(),
        window: None,
        totp: None,
        presence: None,
        language: None,
    }
}

fn step_up_with(nonce: &Sensitive, totp: Option<&str>) -> SecurityCall {
    SecurityCall::StepUp {
        nonce: nonce.clone(),
        window: None,
        totp: totp.map(Sensitive::from),
        presence: None,
        language: None,
    }
}

fn mail_form() -> MailForm {
    MailForm {
        mode: Some(MailMode::Smtp),
        smtp_host: Some("smtp.example.com".into()),
        smtp_tls: TlsMode::Starttls,
        smtp_username: Some("owner@example.com".into()),
        sender_address: Some("vca@example.com".into()),
        recipients: vec!["owner@example.com".into()],
        language: "ko".into(),
        ..MailForm::default()
    }
}

fn begin(replace: bool, step_up: Option<Sensitive>) -> SecurityCall {
    SecurityCall::TotpBegin {
        account: "owner".into(),
        replace,
        step_up,
    }
}

fn confirm(code: &str, current: Option<&str>, step_up: Option<Sensitive>) -> SecurityCall {
    SecurityCall::TotpConfirm {
        code: code.into(),
        current_code: current.map(Sensitive::from),
        step_up,
    }
}

fn recovery_finish(
    code: Option<&str>,
    backup: Option<&str>,
    purpose: GrantOp,
    step_up: Option<Sensitive>,
) -> SecurityCall {
    SecurityCall::RecoveryFinish {
        code: code.map(Sensitive::from),
        backup_code: backup.map(Sensitive::from),
        purpose,
        step_up,
    }
}

impl Fixture {
    /// Runs `build(None)`, answers its challenge, and returns the token.
    async fn verified(
        &self,
        build: impl Fn(Option<Sensitive>) -> SecurityCall,
    ) -> Option<Sensitive> {
        let reply = self.security.handle(build(None)).await;
        match self.security.handle(step_up(&nonce_of(&reply))).await {
            SecurityReply::StepUp { token, .. } => Some(token),
            other => panic!("{other:?}"),
        }
    }

    /// Runs a gated request through the whole two-phase flow.
    async fn approved(&self, build: impl Fn(Option<Sensitive>) -> SecurityCall) -> SecurityReply {
        let token = self.verified(&build).await;
        self.security.handle(build(token)).await
    }

    fn secret_of(reply: &SecurityReply) -> Vec<u8> {
        let SecurityReply::TotpPending { secret_base32, .. } = reply else {
            panic!("begin failed: {reply:?}");
        };
        data_encoding::BASE32_NOPAD
            .decode(secret_base32.0.as_bytes())
            .unwrap()
    }

    async fn enroll(&self) -> (Vec<u8>, Vec<String>) {
        let token = self.verified(|t| begin(false, t)).await;
        let reply = self.security.handle(begin(false, token.clone())).await;
        let secret = Self::secret_of(&reply);
        let code = totp_code_at(&secret, self.clock.now_secs());
        let SecurityReply::TotpEnrolled { backup_codes } =
            self.security.handle(confirm(&code, None, token)).await
        else {
            panic!("confirm failed");
        };
        (secret, backup_codes.into_iter().map(|c| c.0).collect())
    }

    /// The first mail setup, before any authenticator exists.
    async fn configure_mail(&self) {
        let build = |step_up| SecurityCall::MailSet {
            settings: Box::new(mail_form()),
            current_code: None,
            step_up,
        };
        let token = self.verified(build).await;
        assert!(matches!(
            self.security.handle(build(token.clone())).await,
            SecurityReply::Mail { .. }
        ));
        self.security
            .handle(SecurityCall::MailSecret {
                which: SecretKind::SmtpPassword,
                secret: Some(Secret::new("pw-1")),
                current_code: None,
                step_up: token,
            })
            .await;
    }

    fn mailed_code(&self) -> String {
        let sent = self.transport.sent();
        let mail = sent
            .iter()
            .rfind(|m| m.kind == MailKind::RecoveryCode)
            .expect("a recovery mail");
        mail.text
            .lines()
            .map(str::trim)
            .find(|l| l.len() == 6 && l.bytes().all(|b| b.is_ascii_digit()))
            .expect("a code line")
            .to_owned()
    }

    fn code_now(&self, secret: &[u8]) -> String {
        totp_code_at(secret, self.clock.now_secs())
    }

    fn enrolled_secret(&self, secret: &[u8]) -> bool {
        let code = self.code_now(secret);
        self.security.totp().unwrap().verify(&code).is_ok()
    }
}

fn pair_target() -> GateTarget {
    GateTarget::new(GatedAction::PairingCreate, &"view")
}

const ME: Caller = Caller {
    pid: None,
    in_session: false,
    in_remote_shell: false,
};

#[tokio::test]
async fn gate_rejects_remote_and_needs_a_live_token_locally() {
    let f = fixture();
    let s = &f.security;
    let target = pair_target();
    assert_eq!(
        s.gate(Origin::Remote, &target, None, &ME).await,
        Err(Refusal::Remote)
    );
    // The local answer is a challenge for exactly this request.
    let Err(Refusal::Required(challenge)) = s.gate(Origin::Local, &target, None, &ME).await else {
        panic!("expected a challenge");
    };
    assert_eq!(challenge.action, GatedAction::PairingCreate);
    let reply = s.handle(step_up(&Sensitive(challenge.nonce))).await;
    let SecurityReply::StepUp { token, .. } = reply else {
        panic!("{reply:?}");
    };
    // A valid token does not help a remote caller.
    assert_eq!(
        s.gate(Origin::Remote, &target, Some(&token.0), &ME).await,
        Err(Refusal::Remote)
    );
    assert_eq!(
        s.gate(Origin::Local, &target, Some(&token.0), &ME).await,
        Ok(())
    );
    assert!(
        matches!(
            s.gate(Origin::Local, &target, Some(&token.0), &ME).await,
            Err(Refusal::Required(_))
        ),
        "single use"
    );
}

#[tokio::test]
async fn a_token_expires_with_the_grace_window() {
    let f = fixture();
    let target = GateTarget::new(GatedAction::Recovery, &());
    let s = &f.security;
    let Err(Refusal::Required(c)) = s.gate(Origin::Local, &target, None, &ME).await else {
        panic!("expected a challenge");
    };
    let SecurityReply::StepUp { token, .. } = s.handle(step_up(&Sensitive(c.nonce))).await else {
        panic!("step-up failed");
    };
    f.clock.advance_secs(120);
    assert!(matches!(
        s.gate(Origin::Local, &target, Some(&token.0), &ME).await,
        Err(Refusal::Required(_))
    ));
}

/// The server decides what is being approved: the verification takes no
/// action from the caller at all.
#[tokio::test]
async fn the_os_prompt_comes_from_the_gated_request() {
    let f = fixture();
    let reply = f
        .security
        .handle(SecurityCall::TotpDisable {
            current_code: None,
            step_up: None,
        })
        .await;
    f.security.handle(step_up(&nonce_of(&reply))).await;
    assert_eq!(f.verifier.prompts(), ["Turn off 2-step verification"]);
    let forged = r#"{"type":"step_up","action":"mail_settings"}"#;
    assert!(serde_json::from_str::<SecurityCall>(forged).is_err());
}

#[tokio::test]
async fn the_prompt_names_the_new_mail_recipients() {
    let f = fixture();
    let reply = f
        .security
        .handle(SecurityCall::MailSet {
            settings: Box::new(mail_form()),
            current_code: None,
            step_up: None,
        })
        .await;
    f.security.handle(step_up(&nonce_of(&reply))).await;
    assert_eq!(
        f.verifier.prompts(),
        ["Change mail settings: owner@example.com"]
    );
}

#[tokio::test]
async fn the_prompt_follows_the_language_of_the_app_not_of_the_mail() {
    let f = fixture();
    f.configure_mail().await; // Mail language: Korean.
    let target = GateTarget::new(GatedAction::TotpDisable, &());
    for (language, text) in [
        (Some("en-US"), "Turn off 2-step verification"),
        (Some("ja"), "2段階認証をオフにする"),
        (Some("es"), "Desactivar la verificación en dos pasos"),
        (Some("ko"), "2단계 인증 끄기"),
        // The CLI sends none.
        (None, "2단계 인증 끄기"),
    ] {
        let Err(Refusal::Required(c)) = f.security.gate(Origin::Local, &target, None, &ME).await
        else {
            panic!("expected a challenge");
        };
        f.security
            .handle(SecurityCall::StepUp {
                nonce: Sensitive(c.nonce),
                window: None,
                totp: None,
                presence: None,
                language: language.map(str::to_owned),
            })
            .await;
        assert_eq!(f.verifier.prompts().last().unwrap(), text);
    }
}

#[test]
fn every_action_has_a_prompt_in_every_language() {
    use GatedAction as A;
    for action in [
        A::PairingCreate,
        A::TotpSetup,
        A::TotpReset,
        A::TotpDisable,
        A::DeviceApprove,
        A::RemoteShellEnable,
        A::SecuritySettings,
        A::MailSettings,
        A::BackupCodesExport,
        A::Recovery,
        A::ConductorSetup,
        A::ConductorDecide,
        A::TaskSettings,
        A::EngineUrl,
    ] {
        let texts = ["ko", "ja", "es", "en"].map(|l| action_text(action, l));
        for (i, text) in texts.iter().enumerate() {
            assert!(!text.is_empty());
            assert!(!texts[..i].contains(text), "{action:?}");
        }
    }
}

#[tokio::test]
async fn a_token_for_one_request_does_not_approve_another() {
    let f = fixture();
    let s = &f.security;
    let build = |grace| {
        move |step_up| SecurityCall::Settings {
            grace_secs: Some(grace),
            remote_shell: None,
            step_up,
        }
    };
    let token = f.verified(build(60)).await;
    // The same action with other values is another request.
    assert_eq!(
        reason(&s.handle(build(300)(token.clone())).await),
        FailReason::StepUpRequired
    );
    assert_eq!(
        reason(
            &s.handle(SecurityCall::BackupCodes {
                current_code: None,
                step_up: token.clone(),
            })
            .await
        ),
        FailReason::StepUpRequired
    );
    assert!(matches!(
        s.handle(build(60)(token.clone())).await,
        SecurityReply::Status { .. }
    ));
    assert_eq!(
        reason(&s.handle(build(60)(token)).await),
        FailReason::StepUpRequired,
        "spent"
    );
}

#[tokio::test]
async fn a_nonce_and_token_belong_to_the_process_that_got_them() {
    let f = fixture();
    let s = &f.security;
    let me = Caller {
        pid: Some(100),
        ..ME
    };
    let them = Caller {
        pid: Some(200),
        ..ME
    };
    let build = |step_up| SecurityCall::Settings {
        grace_secs: Some(60),
        remote_shell: None,
        step_up,
    };
    let reply = s.handle_from(build(None), &me).await;
    let nonce = nonce_of(&reply);
    assert_eq!(
        reason(&s.handle_from(step_up(&nonce), &them).await),
        FailReason::StepUpRequired
    );
    assert!(f.verifier.prompts().is_empty(), "no prompt for a stranger");
    let SecurityReply::StepUp { token, .. } = s.handle_from(step_up(&nonce), &me).await else {
        panic!("step-up failed");
    };
    assert_eq!(
        reason(&s.handle_from(build(Some(token.clone())), &them).await),
        FailReason::StepUpRequired
    );
    assert!(matches!(
        s.handle_from(build(Some(token)), &me).await,
        SecurityReply::Status { .. }
    ));
}

#[tokio::test]
async fn a_remote_shell_can_neither_verify_nor_use_a_token() {
    let f = fixture();
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    let shell = Caller {
        pid: Some(7),
        in_session: false,
        in_remote_shell: true,
    };
    let build = |step_up| SecurityCall::Settings {
        grace_secs: Some(60),
        remote_shell: None,
        step_up,
    };
    // A nonce the shell somehow holds.
    let reply = s.handle(build(None)).await;
    let nonce = nonce_of(&reply);
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    let prompts = f.verifier.prompts().len();
    assert_eq!(
        reason(
            &s.handle_from(step_up_with(&nonce, Some(&code)), &shell)
                .await
        ),
        FailReason::RemoteForbidden
    );
    assert_eq!(f.verifier.prompts().len(), prompts);
    assert_eq!(
        reason(&s.handle_from(build(None), &shell).await),
        FailReason::RemoteForbidden
    );
    assert_eq!(
        reason(
            &s.handle_from(
                SecurityCall::StepUpTest {
                    window: None,
                    totp: None,
                    language: None,
                },
                &shell
            )
            .await
        ),
        FailReason::RemoteForbidden
    );
    assert_eq!(
        reason(
            &s.handle_from(
                SecurityCall::AppUnlock {
                    window: None,
                    totp: None,
                    language: None,
                },
                &shell
            )
            .await
        ),
        FailReason::RemoteForbidden
    );
}

#[tokio::test]
async fn unlocking_the_app_says_so_and_approves_nothing() {
    let f = fixture();
    for (language, text) in [
        ("en", "Unlock Voice Command AI"),
        ("ko-KR", "Voice Command AI 잠금 해제"),
        ("ja", "Voice Command AI のロックを解除"),
        ("es", "Desbloquear Voice Command AI"),
    ] {
        let reply = f
            .security
            .handle(SecurityCall::AppUnlock {
                window: Some(7),
                totp: None,
                language: Some(language.into()),
            })
            .await;
        // No token: the same answer as the test, nothing to present later.
        assert_eq!(reply, SecurityReply::Done);
        assert_eq!(f.verifier.prompts().last().map(String::as_str), Some(text));
    }
    assert_eq!(f.verifier.prompts().len(), 4);
}

#[tokio::test]
async fn testing_the_verification_approves_nothing() {
    let f = fixture();
    let reply = f
        .security
        .handle(SecurityCall::StepUpTest {
            window: None,
            totp: None,
            language: Some("en".into()),
        })
        .await;
    assert_eq!(reply, SecurityReply::Done);
    assert_eq!(
        f.verifier.prompts(),
        ["Test the verification (nothing is approved)"]
    );
}

#[tokio::test]
async fn canceled_and_failed_prompts_issue_no_token() {
    let f = fixture();
    let reply = f
        .security
        .handle(SecurityCall::MailSet {
            settings: Box::new(mail_form()),
            current_code: None,
            step_up: None,
        })
        .await;
    let nonce = nonce_of(&reply);
    f.verifier.push(VerifyOutcome::Canceled);
    assert_eq!(
        reason(&f.security.handle(step_up(&nonce)).await),
        FailReason::VerificationCanceled
    );
    f.verifier.push(VerifyOutcome::Failed("no".into()));
    assert_eq!(
        reason(&f.security.handle(step_up(&nonce)).await),
        FailReason::VerificationFailed
    );
    // The prompt can be tried again, with the same nonce.
    assert!(matches!(
        f.security.handle(step_up(&nonce)).await,
        SecurityReply::StepUp { .. }
    ));
    // Then it is spent.
    assert_eq!(
        reason(&f.security.handle(step_up(&nonce)).await),
        FailReason::StepUpRequired
    );
}

#[tokio::test]
async fn gated_calls_fail_without_a_token_and_change_nothing() {
    let f = fixture();
    let reply = f
        .security
        .handle(SecurityCall::MailSet {
            settings: Box::new(mail_form()),
            current_code: None,
            step_up: None,
        })
        .await;
    assert_eq!(reason(&reply), FailReason::StepUpRequired);
    assert!(!f.security.mail().configured());
    assert_eq!(f.security.mail().form().mode, None);
    let reply = f
        .security
        .handle(SecurityCall::MailSecret {
            which: SecretKind::RelayKey,
            secret: Some(Secret::new("k")),
            current_code: None,
            step_up: Some("forged".into()),
        })
        .await;
    assert_eq!(reason(&reply), FailReason::StepUpRequired);
    assert!(!f.security.mail().view().relay_key_set);
    for call in [
        SecurityCall::BackupCodes {
            current_code: None,
            step_up: None,
        },
        SecurityCall::TotpDisable {
            current_code: None,
            step_up: None,
        },
        SecurityCall::RecoveryStart { step_up: None },
        SecurityCall::Settings {
            grace_secs: Some(300),
            remote_shell: None,
            step_up: None,
        },
        SecurityCall::Settings {
            grace_secs: None,
            remote_shell: Some(true),
            step_up: None,
        },
    ] {
        assert_eq!(
            reason(&f.security.handle(call).await),
            FailReason::StepUpRequired
        );
    }
}

#[tokio::test]
async fn turning_remote_shells_off_needs_no_verification() {
    let f = fixture();
    let reply = f
        .security
        .handle(SecurityCall::Settings {
            grace_secs: None,
            remote_shell: Some(false),
            step_up: None,
        })
        .await;
    assert!(matches!(reply, SecurityReply::Status { .. }), "{reply:?}");
}

/// Enrolls on a computer with no OS verifier, the terminal way.
async fn bootstrap(f: &Fixture) -> Vec<u8> {
    let s = &f.security;
    let reply = s.handle(begin(false, None)).await;
    let nonce = nonce_of(&reply);
    let reply = s
        .handle(SecurityCall::StepUp {
            nonce,
            window: None,
            totp: None,
            presence: Some(Presence::Terminal),
            language: None,
        })
        .await;
    let SecurityReply::StepUp { token, method, .. } = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(method, StepUpMethod::Presence);
    let token = Some(token);
    let reply = s.handle(begin(false, token.clone())).await;
    let secret = Fixture::secret_of(&reply);
    let code = f.code_now(&secret);
    assert!(matches!(
        s.handle(confirm(&code, None, token)).await,
        SecurityReply::TotpEnrolled { .. }
    ));
    secret
}

#[tokio::test]
async fn headless_computers_verify_with_the_authenticator() {
    let f = fixture_with(FakeVerifier::unavailable());
    let s = &f.security;
    let build = |step_up| SecurityCall::Settings {
        grace_secs: Some(60),
        remote_shell: None,
        step_up,
    };
    let nonce = nonce_of(&s.handle(build(None)).await);
    assert_eq!(
        reason(&s.handle(step_up(&nonce)).await),
        FailReason::VerificationUnavailable
    );
    let secret = bootstrap(&f).await;
    // Now a code is needed.
    let nonce = nonce_of(&s.handle(build(None)).await);
    assert_eq!(
        reason(&s.handle(step_up(&nonce)).await),
        FailReason::TotpRequired
    );
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    let reply = s.handle(step_up_with(&nonce, Some(&code))).await;
    assert!(
        matches!(
            reply,
            SecurityReply::StepUp {
                method: StepUpMethod::Totp,
                ..
            }
        ),
        "{reply:?}"
    );
    // The same code cannot be replayed, and guessing is limited.
    let nonce = nonce_of(&s.handle(build(None)).await);
    assert_eq!(
        reason(&s.handle(step_up_with(&nonce, Some(&code))).await),
        FailReason::TotpReplayed
    );
    for _ in 0..5 {
        s.handle(step_up_with(&nonce, Some("000000"))).await;
    }
    assert_eq!(
        reason(&s.handle(step_up_with(&nonce, Some("000000"))).await),
        FailReason::RateLimited
    );
}

#[tokio::test]
async fn the_first_enrollment_needs_a_person() {
    let f = fixture_with(FakeVerifier::unavailable());
    let s = &f.security;
    // Nothing passes without a token any more.
    assert_eq!(
        reason(&s.handle(begin(false, None)).await),
        FailReason::StepUpRequired
    );
    let nonce = nonce_of(&s.handle(begin(false, None)).await);
    assert_eq!(
        reason(&s.handle(step_up(&nonce)).await),
        FailReason::PresenceRequired
    );
    // Nor from inside a session, whatever it claims.
    let agent = Caller {
        pid: Some(5),
        in_session: true,
        in_remote_shell: false,
    };
    for presence in [Presence::Terminal, Presence::DesktopApp] {
        let reply = s
            .handle_from(
                SecurityCall::StepUp {
                    nonce: nonce.clone(),
                    window: None,
                    totp: None,
                    presence: Some(presence),
                    language: None,
                },
                &Caller { pid: None, ..agent },
            )
            .await;
        assert_eq!(reason(&reply), FailReason::PresenceRequired);
    }
    let reply = s
        .handle(SecurityCall::StepUp {
            nonce: nonce.clone(),
            window: None,
            totp: None,
            presence: Some(Presence::DesktopApp),
            language: None,
        })
        .await;
    assert!(matches!(reply, SecurityReply::StepUp { .. }), "{reply:?}");
}

#[tokio::test]
async fn presence_opens_only_the_first_enrollment() {
    let f = fixture_with(FakeVerifier::unavailable());
    let s = &f.security;
    let other = s
        .handle(SecurityCall::Settings {
            grace_secs: Some(60),
            remote_shell: None,
            step_up: None,
        })
        .await;
    let reply = s
        .handle(SecurityCall::StepUp {
            nonce: nonce_of(&other),
            window: None,
            totp: None,
            presence: Some(Presence::Terminal),
            language: None,
        })
        .await;
    assert_eq!(reason(&reply), FailReason::VerificationUnavailable);
    // And is closed once an authenticator exists.
    bootstrap(&f).await;
    let reply = s.handle(begin(true, None)).await;
    let reply = s
        .handle(SecurityCall::StepUp {
            nonce: nonce_of(&reply),
            window: None,
            totp: None,
            presence: Some(Presence::Terminal),
            language: None,
        })
        .await;
    assert_eq!(reason(&reply), FailReason::TotpRequired);
}

#[tokio::test]
async fn replacing_the_authenticator_needs_the_old_one_or_recovery() {
    let f = fixture();
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    assert_eq!(
        reason(&s.handle(begin(false, None)).await),
        FailReason::Conflict
    );
    let token = f.verified(|t| begin(true, t)).await;
    let reply = s.handle(begin(true, token.clone())).await;
    let new_secret = Fixture::secret_of(&reply);
    f.clock.advance_secs(30);
    let new_code = f.code_now(&new_secret);
    // Windows Hello alone is not enough.
    assert_eq!(
        reason(&s.handle(confirm(&new_code, None, token.clone())).await),
        FailReason::CurrentCodeRequired
    );
    let old_code = f.code_now(&secret);
    assert!(matches!(
        s.handle(confirm(&new_code, Some(&old_code), token)).await,
        SecurityReply::TotpEnrolled { .. }
    ));
    assert!(!s.recovery.granted(), "the grant ends with the replacement");
}

/// One Hello approval must not rebuild the authenticator: a mistyped new
/// code spends nothing and opens nothing.
#[tokio::test]
async fn a_mistyped_new_code_leaves_nothing_open() {
    let f = fixture();
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    let token = f.verified(|t| begin(true, t)).await;
    let reply = s.handle(begin(true, token.clone())).await;
    let new_secret = Fixture::secret_of(&reply);
    f.clock.advance_secs(30);
    let old_code = f.code_now(&secret);
    let wrong = if f.code_now(&new_secret) == "000000" {
        "111111"
    } else {
        "000000"
    };
    assert_eq!(
        reason(
            &s.handle(confirm(wrong, Some(&old_code), token.clone()))
                .await
        ),
        FailReason::TotpInvalid
    );
    assert!(!s.recovery.granted(), "no grant after a mistyped code");
    // The old code was not spent by the mistake.
    let new_code = f.code_now(&new_secret);
    assert!(matches!(
        s.handle(confirm(&new_code, Some(&old_code), token)).await,
        SecurityReply::TotpEnrolled { .. }
    ));
}

#[tokio::test]
async fn new_backup_codes_need_the_authenticator() {
    let f = fixture();
    let (secret, old_backups) = f.enroll().await;
    let s = &f.security;
    let build = |code: Option<&str>| {
        let code = code.map(Sensitive::from);
        move |step_up| SecurityCall::BackupCodes {
            current_code: code.clone(),
            step_up,
        }
    };
    // The approval alone is not enough, and is not spent by that.
    let token = f.verified(build(None)).await;
    assert_eq!(
        reason(&s.handle(build(None)(token.clone())).await),
        FailReason::CurrentCodeRequired
    );
    assert_eq!(
        s.totp().unwrap().remaining_backup_codes(),
        old_backups.len()
    );
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    let reply = s.handle(build(Some(&code))(token.clone())).await;
    assert!(
        matches!(reply, SecurityReply::TotpEnrolled { .. }),
        "{reply:?}"
    );
    assert_eq!(
        reason(&s.handle(build(None)(token)).await),
        FailReason::StepUpRequired,
        "the token is spent"
    );
}

#[tokio::test]
async fn a_recovery_grant_opens_only_its_own_operation() {
    let f = fixture();
    let (secret, backups) = f.enroll().await;
    let s = &f.security;
    let finish = recovery_finish(None, Some(&backups[0]), GrantOp::Replace, None);
    let reply = s.handle(finish).await;
    let SecurityReply::StepUp { token, .. } = s.handle(step_up(&nonce_of(&reply))).await else {
        panic!("step-up failed");
    };
    assert!(matches!(
        s.handle(recovery_finish(
            None,
            Some(&backups[0]),
            GrantOp::Replace,
            Some(token)
        ))
        .await,
        SecurityReply::RecoveryGranted { .. }
    ));
    // A grant for the new authenticator does not turn the old one off.
    let disable = |step_up| SecurityCall::TotpDisable {
        current_code: None,
        step_up,
    };
    assert_eq!(
        reason(&f.approved(disable).await),
        FailReason::CurrentCodeRequired
    );
    // Nor does it make backup codes.
    let codes = |step_up| SecurityCall::BackupCodes {
        current_code: None,
        step_up,
    };
    assert_eq!(
        reason(&f.approved(codes).await),
        FailReason::CurrentCodeRequired
    );
    f.clock.advance_secs(30);
    assert!(f.enrolled_secret(&secret));
}

#[tokio::test]
async fn wrong_new_codes_close_a_recovery_grant() {
    let f = fixture();
    let (_, backups) = f.enroll().await;
    let s = &f.security;
    let build = |step_up| recovery_finish(None, Some(&backups[0]), GrantOp::Replace, step_up);
    assert!(matches!(
        f.approved(build).await,
        SecurityReply::RecoveryGranted { .. }
    ));
    let token = f.verified(|t| begin(true, t)).await;
    let reply = s.handle(begin(true, token.clone())).await;
    let new_secret = Fixture::secret_of(&reply);
    for _ in 0..recovery::MAX_ATTEMPTS {
        let wrong = if f.code_now(&new_secret) == "000000" {
            "111111"
        } else {
            "000000"
        };
        s.handle(confirm(wrong, None, token.clone())).await;
        f.clock.advance_secs(3);
    }
    assert!(!s.recovery.granted());
}

#[tokio::test]
async fn a_grant_ends_with_a_new_enrollment_it_did_not_start() {
    let f = fixture();
    let (_, backups) = f.enroll().await;
    let s = &f.security;
    let build = |step_up| recovery_finish(None, Some(&backups[0]), GrantOp::Replace, step_up);
    f.approved(build).await;
    let token = f.verified(|t| begin(true, t)).await;
    let first = s.handle(begin(true, token.clone())).await;
    let first_secret = Fixture::secret_of(&first);
    let bound = s.pending_totp.lock().unwrap().as_ref().unwrap().0;
    assert!(s.recovery.granted_for(GrantOp::Replace, Some(bound)));
    assert!(!s.recovery.granted_for(GrantOp::Replace, Some(bound + 1)));
    // The confirmation still works for the enrollment it is bound to.
    let code = f.code_now(&first_secret);
    assert!(matches!(
        s.handle(confirm(&code, None, token)).await,
        SecurityReply::TotpEnrolled { .. }
    ));
}

#[tokio::test]
async fn recovery_needs_local_verification_and_the_mailed_code() {
    let f = fixture();
    f.configure_mail().await;
    f.enroll().await;
    let s = &f.security;
    let start = |step_up| SecurityCall::RecoveryStart { step_up };
    assert_eq!(
        reason(&s.handle(start(None)).await),
        FailReason::StepUpRequired
    );
    let token = f.verified(start).await;
    let SecurityReply::RecoveryCodeSent { sent_to, .. } = s.handle(start(token.clone())).await
    else {
        panic!("start failed");
    };
    assert_eq!(sent_to, ["o***@example.com"]);
    let code = f.mailed_code();

    // The code alone does not pass the gate.
    assert_eq!(
        reason(
            &s.handle(recovery_finish(Some(&code), None, GrantOp::Replace, None))
                .await
        ),
        FailReason::StepUpRequired
    );
    assert!(matches!(
        s.handle(recovery_finish(
            Some(&code),
            None,
            GrantOp::Replace,
            token.clone()
        ))
        .await,
        SecurityReply::RecoveryGranted { .. }
    ));
    // Single use.
    assert_eq!(
        reason(
            &s.handle(recovery_finish(
                Some(&code),
                None,
                GrantOp::Replace,
                token.clone()
            ))
            .await
        ),
        FailReason::CodeInvalid
    );

    // A new authenticator goes in without a code from the lost one.
    let token = f.verified(|t| begin(true, t)).await;
    let reply = s.handle(begin(true, token.clone())).await;
    let new_secret = Fixture::secret_of(&reply);
    f.clock.advance_secs(30);
    let new_code = f.code_now(&new_secret);
    assert!(matches!(
        s.handle(confirm(&new_code, None, token)).await,
        SecurityReply::TotpEnrolled { .. }
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let kinds: Vec<_> = f.transport.sent().iter().map(|m| m.kind).collect();
    assert!(kinds.contains(&MailKind::RecoveryStarted), "{kinds:?}");
    assert!(kinds.contains(&MailKind::RecoveryCompleted), "{kinds:?}");
    assert!(kinds.contains(&MailKind::TotpChanged), "{kinds:?}");
}

#[tokio::test]
async fn recovery_without_mail_uses_a_backup_code() {
    let f = fixture();
    let (_, backups) = f.enroll().await;
    let s = &f.security;
    let token = f
        .verified(|t| SecurityCall::RecoveryStart { step_up: t })
        .await;
    let reply = s
        .handle(SecurityCall::Status {
            step_up: token.clone(),
        })
        .await;
    let SecurityReply::Status { status } = reply else {
        panic!("status failed");
    };
    assert_eq!(status.recovery, RecoveryPath::BackupCodeAndLocal);
    assert!(status.step_up.active);
    assert!(status.setup_complete);
    assert_eq!(
        reason(
            &s.handle(SecurityCall::RecoveryStart {
                step_up: token.clone(),
            })
            .await
        ),
        FailReason::MailNotConfigured
    );
    assert_eq!(
        reason(
            &s.handle(recovery_finish(
                None,
                Some("AAAA-BBBB-CCCC-DDDD"),
                GrantOp::Disable,
                token.clone()
            ))
            .await
        ),
        FailReason::TotpInvalid
    );
    assert!(matches!(
        s.handle(recovery_finish(
            None,
            Some(&backups[0]),
            GrantOp::Disable,
            token
        ))
        .await,
        SecurityReply::RecoveryGranted { .. }
    ));
    let disable = |step_up| SecurityCall::TotpDisable {
        current_code: None,
        step_up,
    };
    assert!(matches!(
        f.approved(disable).await,
        SecurityReply::Status { .. }
    ));
    assert!(!s.enrolled());
}

/// A computer with no OS verifier and a lost authenticator: a backup code
/// verifies, for recovery only, and the second proof is a separate one.
#[tokio::test]
async fn recovery_works_with_no_verifier_and_needs_two_backup_codes() {
    let f = fixture_with(FakeVerifier::unavailable());
    let s = &f.security;
    let reply = s.handle(begin(false, None)).await;
    let SecurityReply::StepUp { token, .. } = s
        .handle(SecurityCall::StepUp {
            nonce: nonce_of(&reply),
            window: None,
            totp: None,
            presence: Some(Presence::Terminal),
            language: None,
        })
        .await
    else {
        panic!("presence refused");
    };
    let token = Some(token);
    let secret = Fixture::secret_of(&s.handle(begin(false, token.clone())).await);
    let code = f.code_now(&secret);
    let SecurityReply::TotpEnrolled { backup_codes } = s.handle(confirm(&code, None, token)).await
    else {
        panic!("confirm failed");
    };
    let first = backup_codes[0].0.clone();
    let second = backup_codes[1].0.clone();

    // A backup code is no general way in.
    let other = s
        .handle(SecurityCall::Settings {
            grace_secs: Some(60),
            remote_shell: None,
            step_up: None,
        })
        .await;
    assert_eq!(
        reason(
            &s.handle(step_up_with(&nonce_of(&other), Some(&first)))
                .await
        ),
        FailReason::TotpInvalid
    );

    let finish = |backup: &str, step_up: Option<Sensitive>| {
        recovery_finish(None, Some(backup), GrantOp::Replace, step_up)
    };
    let reply = s.handle(finish(&second, None)).await;
    let verified = s
        .handle(step_up_with(&nonce_of(&reply), Some(&first)))
        .await;
    let SecurityReply::StepUp { token, method, .. } = verified else {
        panic!("{verified:?}");
    };
    assert_eq!(method, StepUpMethod::BackupCode);
    let token = Some(token);
    // The code that verified cannot also be the second proof.
    assert_eq!(
        reason(&s.handle(finish(&first, token.clone())).await),
        FailReason::TotpInvalid
    );
    assert!(matches!(
        s.handle(finish(&second, token)).await,
        SecurityReply::RecoveryGranted { .. }
    ));

    // The way out: the lost authenticator is no longer asked for, neither
    // for the step-up nor as a current code.
    let reply = s.handle(begin(true, None)).await;
    let new_secret = Fixture::secret_of(&reply);
    let code = f.code_now(&new_secret);
    assert!(matches!(
        s.handle(confirm(&code, None, None)).await,
        SecurityReply::TotpEnrolled { .. }
    ));
    assert!(!s.recovery.granted(), "the grant is spent");
    // A step-up is needed again right away.
    assert_eq!(
        reason(&s.handle(begin(true, None)).await),
        FailReason::StepUpRequired
    );
}

async fn recovered(purpose: GrantOp) -> Fixture {
    let f = fixture_with(FakeVerifier::unavailable());
    bootstrap(&f).await;
    let s = &f.security;
    s.recovery.grant(purpose);
    f
}

#[tokio::test]
async fn a_grant_stands_in_for_its_own_step_up_only() {
    let disable = || SecurityCall::TotpDisable {
        current_code: None,
        step_up: None,
    };
    let codes = || SecurityCall::BackupCodes {
        current_code: None,
        step_up: None,
    };
    // Each operation with its own grant.
    let f = recovered(GrantOp::Disable).await;
    assert!(matches!(
        f.security.handle(disable()).await,
        SecurityReply::Status { .. }
    ));
    let f = recovered(GrantOp::BackupCodes).await;
    assert!(matches!(
        f.security.handle(codes()).await,
        SecurityReply::TotpEnrolled { .. }
    ));
    // Another grant opens nothing.
    let f = recovered(GrantOp::Replace).await;
    assert_eq!(
        reason(&f.security.handle(disable()).await),
        FailReason::StepUpRequired
    );
    assert_eq!(
        reason(&f.security.handle(codes()).await),
        FailReason::StepUpRequired
    );
}

#[tokio::test]
async fn a_grant_does_not_stand_in_for_a_session_or_next_to_a_remote_shell() {
    let f = recovered(GrantOp::Replace).await;
    let s = &f.security;
    let agent = Caller {
        pid: None,
        in_session: true,
        in_remote_shell: false,
    };
    // A process inside a session never steps up at all.
    assert_eq!(
        reason(&s.handle_from(begin(true, None), &agent).await),
        FailReason::RemoteForbidden
    );
    s.test_shell_open.store(true, Ordering::SeqCst);
    assert_eq!(
        reason(&s.handle(begin(true, None)).await),
        FailReason::StepUpRequired
    );
}

#[tokio::test]
async fn an_os_prompt_still_asks_after_a_recovery() {
    let f = fixture();
    f.enroll().await;
    f.security.recovery.grant(GrantOp::Replace);
    assert_eq!(
        reason(&f.security.handle(begin(true, None)).await),
        FailReason::StepUpRequired
    );
}

/// With no OS prompt the code that verified is the only proof there is:
/// asking for it again in the same 30 seconds could not work.
#[tokio::test]
async fn the_code_of_the_step_up_counts_as_the_current_code() {
    let f = fixture_with(FakeVerifier::unavailable());
    let secret = bootstrap(&f).await;
    let s = &f.security;
    f.clock.advance_secs(30);
    let old = f.code_now(&secret);
    let nonce = nonce_of(&s.handle(begin(true, None)).await);
    let SecurityReply::StepUp { token, .. } = s.handle(step_up_with(&nonce, Some(&old))).await
    else {
        panic!("step-up failed");
    };
    let token = Some(token);
    let new_secret = Fixture::secret_of(&s.handle(begin(true, token.clone())).await);
    let new_code = f.code_now(&new_secret);
    assert!(matches!(
        s.handle(confirm(&new_code, None, token)).await,
        SecurityReply::TotpEnrolled { .. }
    ));
}

#[tokio::test]
async fn a_replayed_code_is_no_wrong_guess() {
    let f = fixture();
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    s.check_code(&code, false).unwrap();
    for _ in 0..20 {
        assert_eq!(
            reason(&s.check_code(&code, false).unwrap_err()),
            FailReason::TotpReplayed
        );
    }
    assert_eq!(lock(&s.codes).failures, 0);
}

#[tokio::test]
async fn a_code_refused_while_a_remote_shell_is_open() {
    let f = fixture_with(FakeVerifier::unavailable());
    let secret = bootstrap(&f).await;
    let s = &f.security;
    let build = |step_up| SecurityCall::Settings {
        grace_secs: Some(60),
        remote_shell: None,
        step_up,
    };
    let nonce = nonce_of(&s.handle(build(None)).await);
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    s.test_shell_open.store(true, Ordering::SeqCst);
    assert_eq!(
        reason(&s.handle(step_up_with(&nonce, Some(&code))).await),
        FailReason::RemoteShellOpen
    );
    s.test_shell_open.store(false, Ordering::SeqCst);
    assert!(matches!(
        s.handle(step_up_with(&nonce, Some(&code))).await,
        SecurityReply::StepUp { .. }
    ));
}

#[tokio::test]
async fn the_prompt_of_a_mail_save_approves_only_what_it_showed() {
    let f = fixture();
    f.configure_mail().await;
    let s = &f.security;
    let save = |to: &str, step_up| {
        let mut form = mail_form();
        form.recipients = vec![to.to_owned()];
        SecurityCall::MailSet {
            settings: Box::new(form),
            current_code: None,
            step_up,
        }
    };
    let token = f.verified(|t| save("shown@example.com", t)).await;
    assert_eq!(
        reason(&s.handle(save("other@example.com", token.clone())).await),
        FailReason::StepUpRequired
    );
    assert!(matches!(
        s.handle(save("shown@example.com", token)).await,
        SecurityReply::Mail { .. }
    ));
}

#[tokio::test]
async fn replacing_a_sending_secret_needs_the_authenticator_and_tells_the_owner() {
    let f = fixture();
    f.configure_mail().await;
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    let build = |code: Option<&str>, value: Option<&str>| {
        let code = code.map(Sensitive::from);
        let value = value.map(Secret::new);
        move |step_up| SecurityCall::MailSecret {
            which: SecretKind::SmtpPassword,
            secret: value.clone(),
            current_code: code.clone(),
            step_up,
        }
    };
    let token = f.verified(build(None, Some("pw-2"))).await;
    assert_eq!(
        reason(&s.handle(build(None, Some("pw-2"))(token.clone())).await),
        FailReason::CurrentCodeRequired
    );
    let notices = |f: &Fixture| {
        f.transport
            .sent()
            .iter()
            .filter(|m| m.kind == MailKind::MailSettingsChanged)
            .count()
    };
    let before = notices(&f);
    // Deleting goes the same way, and the owner hears it while mail works.
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    assert!(matches!(
        s.handle(build(Some(&code), None)(token)).await,
        SecurityReply::Mail { .. }
    ));
    assert_eq!(notices(&f), before + 1);
    assert!(!s.mail().has_secret(SecretKind::SmtpPassword));
}

#[tokio::test]
async fn one_code_covers_the_secrets_of_one_save() {
    let f = fixture();
    f.configure_mail().await;
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    let mut moved = mail_form();
    moved.recipients = vec!["new@example.com".into()];
    let save = |code: Option<Sensitive>, step_up| SecurityCall::MailSet {
        settings: Box::new(moved.clone()),
        current_code: code,
        step_up,
    };
    let token = f.verified(|t| save(None, t)).await;
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    assert!(matches!(
        s.handle(save(Some(code.as_str().into()), token.clone()))
            .await,
        SecurityReply::Mail { .. }
    ));
    assert!(matches!(
        s.handle(SecurityCall::MailSecret {
            which: SecretKind::SmtpPassword,
            secret: Some(Secret::new("pw-2")),
            current_code: None,
            step_up: token,
        })
        .await,
        SecurityReply::Mail { .. }
    ));
}

#[tokio::test]
async fn the_mail_account_and_key_header_count_as_where_mail_goes() {
    let before = mail_form();
    for change in [
        MailForm {
            smtp_username: Some("attacker".into()),
            ..before.clone()
        },
        MailForm {
            api_key_header: Some("X-Other".into()),
            ..before.clone()
        },
    ] {
        assert!(mail::delivery_changed(&before, &change));
    }
}

#[tokio::test]
async fn a_failed_recovery_mail_leaves_no_usable_code() {
    let f = fixture();
    f.configure_mail().await;
    f.enroll().await;
    f.transport
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let start = |step_up| SecurityCall::RecoveryStart { step_up };
    let token = f.verified(start).await;
    assert_eq!(
        reason(&f.security.handle(start(token.clone())).await),
        FailReason::MailFailed
    );
    assert_eq!(
        reason(
            &f.security
                .handle(recovery_finish(
                    Some("123456"),
                    None,
                    GrantOp::Replace,
                    token
                ))
                .await
        ),
        FailReason::CodeInvalid
    );
}

#[tokio::test]
async fn mail_changes_notify_and_secrets_stay_out_of_replies() {
    let f = fixture();
    f.configure_mail().await;
    let s = &f.security;
    let view = s.handle(SecurityCall::MailGet).await;
    let json = serde_json::to_string(&view).unwrap();
    assert!(json.contains("\"password_set\":true"), "{json}");
    assert!(!json.contains("pw-1"));

    let mut form = mail_form();
    form.recipients = vec!["new@example.com".into()];
    f.approved(|step_up| SecurityCall::MailSet {
        settings: Box::new(form.clone()),
        current_code: None,
        step_up,
    })
    .await;
    let sent = f.transport.sent();
    let changes: Vec<_> = sent
        .iter()
        .filter(|m| m.kind == MailKind::MailSettingsChanged)
        .collect();
    let to: Vec<_> = changes.iter().map(|m| m.to.clone()).collect();
    assert!(to.contains(&vec!["owner@example.com".to_owned()]), "{to:?}");
    assert!(to.contains(&vec!["new@example.com".to_owned()]), "{to:?}");

    let reply = s.handle(SecurityCall::MailTest).await;
    assert_eq!(
        reply,
        SecurityReply::MailSent {
            sent_to: vec!["new@example.com".into()]
        }
    );
    assert_eq!(
        reason(&s.handle(SecurityCall::MailTest).await),
        FailReason::RateLimited
    );
}

#[tokio::test]
async fn test_mails_are_capped_per_hour() {
    let f = fixture();
    f.configure_mail().await;
    let s = &f.security;
    for _ in 0..MAX_TEST_MAILS_PER_HOUR {
        assert!(matches!(
            s.handle(SecurityCall::MailTest).await,
            SecurityReply::MailSent { .. }
        ));
        f.clock.advance_secs(60);
    }
    assert_eq!(
        reason(&s.handle(SecurityCall::MailTest).await),
        FailReason::RateLimited
    );
    f.clock.advance_secs(3600);
    assert!(matches!(
        s.handle(SecurityCall::MailTest).await,
        SecurityReply::MailSent { .. }
    ));
}

#[tokio::test]
async fn moving_mail_needs_the_authenticator() {
    let f = fixture();
    f.configure_mail().await;
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    let mut moved = mail_form();
    moved.recipients = vec!["attacker@example.com".into()];
    let build = |code: Option<&str>| {
        let code = code.map(Sensitive::from);
        let form = moved.clone();
        move |step_up| SecurityCall::MailSet {
            settings: Box::new(form.clone()),
            current_code: code.clone(),
            step_up,
        }
    };
    let token = f.verified(build(None)).await;
    assert_eq!(
        reason(&s.handle(build(None)(token.clone())).await),
        FailReason::CurrentCodeRequired
    );
    assert_eq!(s.mail().form().recipients, ["owner@example.com"]);
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    assert!(matches!(
        s.handle(build(Some(&code))(token)).await,
        SecurityReply::Mail { .. }
    ));
    assert_eq!(s.mail().form().recipients, ["attacker@example.com"]);
}

#[tokio::test]
async fn moving_the_delivery_server_counts_as_moving_mail() {
    let f = fixture();
    f.configure_mail().await;
    f.enroll().await;
    let mut moved = mail_form();
    moved.smtp_host = Some("smtp.attacker.example".into());
    let reply = f
        .approved(|step_up| SecurityCall::MailSet {
            settings: Box::new(moved.clone()),
            current_code: None,
            step_up,
        })
        .await;
    assert_eq!(reason(&reply), FailReason::CurrentCodeRequired);
    // Details that keep the delivery need the approval only.
    let mut renamed = mail_form();
    renamed.sender_name = Some("New name".into());
    let reply = f
        .approved(|step_up| SecurityCall::MailSet {
            settings: Box::new(renamed.clone()),
            current_code: None,
            step_up,
        })
        .await;
    assert!(matches!(reply, SecurityReply::Mail { .. }), "{reply:?}");
}

/// The old address hears about a change over the old settings, even when
/// the new transport is the attacker's and notifications were turned off.
#[tokio::test]
async fn the_old_address_is_told_over_the_old_transport() {
    use super::mail::transport::{MailError as TransportError, MailTransport, OutgoingMail};

    /// Delivers only what the old server would: this stands in for the
    /// attacker's server, which keeps everything to itself.
    struct OnlyTo {
        sent: Mutex<Vec<OutgoingMail>>,
    }
    #[async_trait]
    impl MailTransport for OnlyTo {
        async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
            self.sent.lock().unwrap().push(mail.clone());
            Ok(())
        }
    }

    let f = fixture();
    f.configure_mail().await;
    let (secret, _) = f.enroll().await;
    let s = &f.security;
    let old = Arc::new(OnlyTo {
        sent: Mutex::new(Vec::new()),
    });
    s.mail().set_transport_override(old.clone());
    let mut moved = mail_form();
    moved.smtp_host = Some("smtp.attacker.example".into());
    moved.recipients = vec!["attacker@example.com".into()];
    moved.notifications.enabled = false;
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    let reply = f
        .approved(|step_up| SecurityCall::MailSet {
            settings: Box::new(moved.clone()),
            current_code: Some(code.as_str().into()),
            step_up,
        })
        .await;
    assert!(matches!(reply, SecurityReply::Mail { .. }), "{reply:?}");
    let sent = old.sent.lock().unwrap();
    assert_eq!(sent[0].kind, MailKind::MailSettingsChanged);
    assert_eq!(sent[0].to, ["owner@example.com"]);
}

#[tokio::test]
async fn wizard_state_and_grace_persist() {
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        Security::new(SecurityOptions {
            data_dir: Some(dir.path().to_owned()),
            config_dir: None,
            server_name: "pc".into(),
            clock: ManualClock::new(1_000),
            verifier: FakeVerifier::available(),
            secrets: Arc::new(MemoryKeyStore::new()),
        })
        .unwrap()
    };
    let s = open();
    s.handle(SecurityCall::Wizard {
        state: WizardState::Skipped,
    })
    .await;
    let build = |grace_secs, step_up| SecurityCall::Settings {
        grace_secs: Some(grace_secs),
        remote_shell: None,
        step_up,
    };
    // A value out of range is refused before anything is asked or spent.
    assert_eq!(
        reason(&s.handle(build(5, None)).await),
        FailReason::InvalidRequest
    );
    assert_eq!(
        reason(&s.handle(build(301, None)).await),
        FailReason::InvalidRequest
    );
    let reply = s.handle(build(300, None)).await;
    let SecurityReply::StepUp { token, .. } = s.handle(step_up(&nonce_of(&reply))).await else {
        panic!("step-up failed");
    };
    s.handle(build(300, Some(token))).await;
    let again = open();
    let SecurityReply::Status { status } =
        again.handle(SecurityCall::Status { step_up: None }).await
    else {
        panic!("status failed");
    };
    assert_eq!(status.wizard.state, WizardState::Skipped);
    assert_eq!(status.step_up.grace_secs, 300);
    assert!(!status.step_up.active, "tokens do not survive a restart");
}

#[tokio::test]
async fn corrupt_state_turns_remote_shells_off() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("security");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(STATE_FILE), "{ not json").unwrap();
    let s = Security::new(SecurityOptions {
        data_dir: Some(dir.path().to_owned()),
        config_dir: None,
        server_name: "pc".into(),
        clock: ManualClock::new(1_000),
        verifier: FakeVerifier::available(),
        secrets: Arc::new(MemoryKeyStore::new()),
    })
    .unwrap();
    assert_eq!(lock(&s.state).remote_shell, Some(false));
}

/// A saved grace window below the minimum cannot lock the owner out.
#[tokio::test]
async fn a_saved_zero_grace_window_is_raised_to_the_minimum() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("security");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(STATE_FILE), r#"{"grace_secs":0}"#).unwrap();
    let s = Security::new(SecurityOptions {
        data_dir: Some(dir.path().to_owned()),
        config_dir: None,
        server_name: "pc".into(),
        clock: ManualClock::new(1_000),
        verifier: FakeVerifier::available(),
        secrets: Arc::new(MemoryKeyStore::new()),
    })
    .unwrap();
    assert_eq!(s.step_up.grace(), Duration::from_secs(MIN_GRACE_SECS));
}

#[test]
fn audit_records_become_notifications() {
    use std::net::{IpAddr, Ipv4Addr};
    use vca_net::Scope;
    let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));
    let paired = AuditEvent::DevicePaired {
        device_id: "d1".into(),
        name: "Phone".into(),
        scope: Scope::Command,
        key_fingerprint: "ab".into(),
        client_ip: ip,
    };
    assert_eq!(
        notification_for(&paired),
        Some(AuditNotice::Event(SecurityEvent::DevicePaired {
            device: "Phone".into(),
            ip: Some("203.0.113.5".into()),
        }))
    );
    let failure = |failure| AuditEvent::AuthFailed {
        conn_id: 1,
        device_id: None,
        key_fingerprint: "ab".into(),
        failure,
        detail: "x".into(),
        client_ip: ip,
    };
    assert_eq!(
        notification_for(&failure(AuthFailure::InvalidTotp)),
        Some(AuditNotice::Failure("203.0.113.5: wrong code".into()))
    );
    // Asking for the code is a normal step of signing in.
    assert_eq!(notification_for(&failure(AuthFailure::TotpRequired)), None);
}

#[tokio::test]
async fn security_steps_reach_the_audit_log() {
    let f = fixture();
    let clock = f.clock.clone();
    let audit = Arc::new(AuditLog::memory(clock.clone()));
    let security = Security::new(SecurityOptions {
        data_dir: None,
        config_dir: None,
        server_name: "pc".into(),
        clock: clock.clone(),
        verifier: FakeVerifier::available(),
        secrets: Arc::new(MemoryKeyStore::new()),
    })
    .unwrap();
    security.attach_totp(
        Arc::new(TotpManager::in_memory(clock.clone())),
        Some(audit.clone()),
    );
    security
        .mail()
        .set_transport_override(Arc::new(MemoryTransport::default()));
    let g = Fixture {
        clock,
        security: Arc::new(security),
        verifier: f.verifier.clone(),
        transport: f.transport.clone(),
    };
    g.configure_mail().await;
    let (_, backups) = g.enroll().await;
    let finish = |step_up| recovery_finish(None, Some(&backups[0]), GrantOp::Disable, step_up);
    g.approved(finish).await;
    g.approved(|step_up| SecurityCall::TotpDisable {
        current_code: None,
        step_up,
    })
    .await;
    let lines: Vec<String> = audit
        .memory_records()
        .iter()
        .filter_map(|r| match &r.event {
            AuditEvent::LocalSecurity { what, .. } => Some(what.clone()),
            _ => None,
        })
        .collect();
    for what in [
        "step_up_verified",
        "mail_settings_changed",
        "mail_secret_changed",
        "recovery_completed",
        "totp_disabled",
    ] {
        assert!(lines.iter().any(|l| l == what), "{what}: {lines:?}");
    }
}

#[test]
fn debug_output_hides_tokens_and_codes() {
    let call = SecurityCall::TotpConfirm {
        code: "123456".into(),
        current_code: Some("654321".into()),
        step_up: Some("token-value".into()),
    };
    let text = format!("{call:?}");
    assert!(!text.contains("123456"));
    assert!(!text.contains("654321"));
    assert!(!text.contains("token-value"));
    let json = serde_json::to_value(&call).unwrap();
    assert_eq!(json["type"], "totp_confirm");
    assert_eq!(serde_json::from_value::<SecurityCall>(json).unwrap(), call);
}

/// The exact lines the desktop app sends (`apps/shell/src/host/security.rs`
/// builds them as JSON, without linking this crate).
#[test]
fn requests_from_the_desktop_app_decode() {
    use crate::control::LocalRequest;
    let lines = [
        r#"{"op":"security","id":1,"call":{"type":"status","step_up":null}}"#,
        r#"{"op":"security","id":1,"call":{"type":"step_up","nonce":"n","window":123,"totp":null,"presence":"desktop_app","language":"ko"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"step_up_test","window":123,"totp":null,"language":"ko"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"app_unlock","window":123,"totp":null,"language":"ko"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"step_up_end","step_up":"t"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"totp_begin","account":"owner","replace":true,"step_up":"t"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"totp_confirm","code":"123456","current_code":null,"step_up":null}}"#,
        r#"{"op":"security","id":1,"call":{"type":"backup_codes","current_code":"1","step_up":null}}"#,
        r#"{"op":"security","id":1,"call":{"type":"totp_disable","current_code":"1","step_up":"t"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"settings","grace_secs":120,"remote_shell":null,"step_up":null}}"#,
        r#"{"op":"security","id":1,"call":{"type":"wizard","state":"skipped"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"mail_get"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"mail_secret","which":"relay_key","secret":null,"step_up":null}}"#,
        r#"{"op":"security","id":1,"call":{"type":"mail_test"}}"#,
        r#"{"op":"security","id":1,"call":{"type":"recovery_start","step_up":null}}"#,
        r#"{"op":"security","id":1,"call":{"type":"recovery_finish","code":"123456","backup_code":null,"step_up":"t"}}"#,
    ];
    for line in lines {
        let req: LocalRequest = serde_json::from_str(line).expect(line);
        assert_eq!(req.id(), 1);
    }
    // The settings form comes back from the screen with the view's extras.
    let view = serde_json::to_value(fixture().security.mail().view()).unwrap();
    let call = serde_json::json!({
        "op": "security", "id": 1,
        "call": { "type": "mail_set", "settings": view, "current_code": null, "step_up": null },
    });
    serde_json::from_value::<LocalRequest>(call).unwrap();
}

/// A code proves the phone, not who sits here: with an OS verifier present,
/// only the OS prompt counts, even with a correct code.
#[tokio::test]
async fn an_authenticator_code_cannot_replace_the_os_prompt() {
    let f = fixture();
    let (secret, _) = f.enroll().await;
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    f.verifier.push(VerifyOutcome::Canceled);
    let target = pair_target();
    let Err(Refusal::Required(c)) = f.security.gate(Origin::Local, &target, None, &ME).await else {
        panic!("expected a challenge");
    };
    let reply = f
        .security
        .handle(step_up_with(&Sensitive(c.nonce), Some(&code)))
        .await;
    assert_eq!(reason(&reply), FailReason::VerificationCanceled);
    assert!(
        f.verifier
            .prompts()
            .contains(&"Create a pairing code".to_owned())
    );
}

#[tokio::test]
async fn repeated_prompts_are_limited_but_never_locked_out() {
    let f = fixture();
    let target = pair_target();
    let Err(Refusal::Required(c)) = f.security.gate(Origin::Local, &target, None, &ME).await else {
        panic!("expected a challenge");
    };
    let nonce = Sensitive(c.nonce);
    for _ in 0..5 {
        f.verifier.push(VerifyOutcome::Canceled);
        assert_eq!(
            reason(&f.security.handle(step_up(&nonce)).await),
            FailReason::VerificationCanceled
        );
    }
    let shown = f.verifier.prompts().len();
    assert_eq!(
        reason(&f.security.handle(step_up(&nonce)).await),
        FailReason::RateLimited
    );
    assert_eq!(f.verifier.prompts().len(), shown, "no sixth prompt");
    // The wait is short and bounded: anyone local can cause failures, and
    // the owner must still get in.
    f.clock.advance_secs(2);
    assert!(matches!(
        f.security.handle(step_up(&nonce)).await,
        SecurityReply::StepUp { .. }
    ));
}

#[test]
fn the_wait_after_failures_grows_and_stops_at_the_bound() {
    assert_eq!(Backoff::wait(FREE_TRIES), 0);
    assert_eq!(Backoff::wait(FREE_TRIES + 1), 2_000);
    assert_eq!(Backoff::wait(FREE_TRIES + 2), 4_000);
    assert_eq!(Backoff::wait(10_000), MAX_BACKOFF.as_millis() as u64);
    let mut b = Backoff::default();
    for i in 0..1000 {
        b.failed(1_000 + i);
    }
    assert!(b.remaining(1_999) <= MAX_BACKOFF.as_millis() as u64);
    assert_eq!(b.remaining(1_999 + MAX_BACKOFF.as_millis() as u64), 0);
    // A quiet period forgets.
    assert_eq!(b.remaining(1_999 + BACKOFF_RESET.as_millis() as u64), 0);
    assert_eq!(b.failures, 0);
}

/// Spamming one kind of failure must not hold back the other.
#[tokio::test]
async fn wrong_codes_do_not_hold_back_the_os_prompt() {
    let f = fixture();
    for _ in 0..50 {
        f.security.wrong_guess();
    }
    let target = pair_target();
    let Err(Refusal::Required(c)) = f.security.gate(Origin::Local, &target, None, &ME).await else {
        panic!("expected a challenge");
    };
    assert!(matches!(
        f.security.handle(step_up(&Sensitive(c.nonce))).await,
        SecurityReply::StepUp { .. }
    ));
}

/// A prompt nobody answers ends, and frees the one prompt slot.
#[tokio::test]
async fn an_unanswered_prompt_times_out() {
    struct Hangs;
    #[async_trait]
    impl LocalVerifier for Hangs {
        fn kind(&self) -> VerifierKind {
            VerifierKind::WindowsHello
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn verify(&self, _reason: &str, _window: Option<u64>) -> VerifyOutcome {
            std::future::pending().await
        }
    }
    let clock = ManualClock::new(1_000);
    let mut security = Security::new(SecurityOptions {
        data_dir: None,
        config_dir: None,
        server_name: "pc".into(),
        clock,
        verifier: Arc::new(Hangs),
        secrets: Arc::new(MemoryKeyStore::new()),
    })
    .unwrap();
    security.prompt_timeout = Duration::from_millis(20);
    let target = pair_target();
    let Err(Refusal::Required(c)) = security.gate(Origin::Local, &target, None, &ME).await else {
        panic!("expected a challenge");
    };
    let reply = security.handle(step_up(&Sensitive(c.nonce))).await;
    assert_eq!(reason(&reply), FailReason::VerificationCanceled);
    assert!(!security.prompting.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_broken_preset_keeps_the_gates_working() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(mail::PRESET_FILE), "not_a_key = 1\n").unwrap();
    let s = Security::new(SecurityOptions {
        data_dir: None,
        config_dir: Some(dir.path().to_owned()),
        server_name: "pc".into(),
        clock: ManualClock::new(1_000),
        verifier: FakeVerifier::available(),
        secrets: Arc::new(MemoryKeyStore::new()),
    })
    .unwrap();
    assert_eq!(s.mail().form().mode, None);
    assert!(matches!(
        s.gate(Origin::Local, &pair_target(), None, &ME).await,
        Err(Refusal::Required(_))
    ));
}

#[tokio::test]
async fn preset_values_meet_the_rules_of_a_saved_form() {
    for bad in [
        "mode = \"relay\"\nrelay_url = \"http://relay.example.com\"\n",
        "mode = \"smtp\"\nsmtp_host = \"smtp.example.com\"\nsmtp_tls = \"none\"\n",
        "recipients = [\"not an address\"]\n",
        "relay_url = \"https://relay.example.com\"\napi_key_header = \"Bad Header\"\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(mail::PRESET_FILE), bad).unwrap();
        let s = Security::new(SecurityOptions {
            data_dir: None,
            config_dir: Some(dir.path().to_owned()),
            server_name: "pc".into(),
            clock: ManualClock::new(1_000),
            verifier: FakeVerifier::available(),
            secrets: Arc::new(MemoryKeyStore::new()),
        })
        .unwrap();
        assert_eq!(s.mail().form().mode, None, "{bad}");
        assert!(s.mail().form().recipients.is_empty(), "{bad}");
    }
    // A partial preset that is right stays.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(mail::PRESET_FILE),
        "mode = \"relay\"\nrelay_url = \"https://relay.example.com\"\n",
    )
    .unwrap();
    let s = Security::new(SecurityOptions {
        data_dir: None,
        config_dir: Some(dir.path().to_owned()),
        server_name: "pc".into(),
        clock: ManualClock::new(1_000),
        verifier: FakeVerifier::available(),
        secrets: Arc::new(MemoryKeyStore::new()),
    })
    .unwrap();
    assert_eq!(s.mail().form().mode, Some(MailMode::Relay));
}

/// The conductor kill switch from a phone: the owner's code, checked like
/// a local one.
#[tokio::test]
async fn a_paired_device_proves_the_owner_with_a_code() {
    use vca_core::error::ErrorCode;
    let f = fixture();
    let not_set_up = f
        .security
        .verify_remote_code(Some("123456"), "conductor_kill");
    assert_eq!(not_set_up.unwrap_err().code(), ErrorCode::Unsupported);
    let (secret, _) = f.enroll().await;
    f.clock.advance_secs(30);
    let missing = f.security.verify_remote_code(None, "conductor_kill");
    assert_eq!(missing.unwrap_err().code(), ErrorCode::TotpRequired);
    let blank = f.security.verify_remote_code(Some("  "), "conductor_kill");
    assert_eq!(blank.unwrap_err().code(), ErrorCode::TotpRequired);
    let mut wrong = f.code_now(&secret).into_bytes();
    wrong[0] = if wrong[0] == b'9' { b'0' } else { wrong[0] + 1 };
    let wrong = String::from_utf8(wrong).unwrap();
    let refused = f
        .security
        .verify_remote_code(Some(&wrong), "conductor_kill");
    assert_eq!(refused.unwrap_err().code(), ErrorCode::TotpInvalid);
    let code = f.code_now(&secret);
    assert!(
        f.security
            .verify_remote_code(Some(&code), "conductor_kill")
            .is_ok()
    );
    // A code works once.
    let again = f.security.verify_remote_code(Some(&code), "conductor_kill");
    assert_eq!(again.unwrap_err().code(), ErrorCode::TotpInvalid);
}

/// The app lock, the Command Center, step-up and a paired device all check
/// the same authenticator: a code that unlocked the app once is spent for
/// every one of them, an earlier code too.
#[tokio::test]
async fn one_authenticator_code_unlocks_once_across_every_check() {
    use vca_core::error::ErrorCode;
    let f = fixture_with(FakeVerifier::unavailable());
    let secret = bootstrap(&f).await;
    let s = &f.security;
    f.clock.advance_secs(30);
    let code = f.code_now(&secret);
    let earlier = totp_code_at(&secret, f.clock.now_secs() - 30);
    let unlock = |totp: &str| SecurityCall::AppUnlock {
        window: None,
        totp: Some(Sensitive::from(totp)),
        language: None,
    };
    assert_eq!(s.handle(unlock(&code)).await, SecurityReply::Done);
    // A second lock screen, or the Command Center gate, with the same code.
    assert_eq!(
        reason(&s.handle(unlock(&code)).await),
        FailReason::TotpReplayed
    );
    assert_eq!(
        reason(&s.handle(unlock(&earlier)).await),
        FailReason::TotpReplayed
    );
    let test = s
        .handle(SecurityCall::StepUpTest {
            window: None,
            totp: Some(Sensitive::from(code.as_str())),
            language: None,
        })
        .await;
    assert_eq!(reason(&test), FailReason::TotpReplayed);
    let build = |step_up| SecurityCall::Settings {
        grace_secs: Some(60),
        remote_shell: None,
        step_up,
    };
    let nonce = nonce_of(&s.handle(build(None)).await);
    assert_eq!(
        reason(&s.handle(step_up_with(&nonce, Some(&code))).await),
        FailReason::TotpReplayed
    );
    let remote = s.verify_remote_code(Some(&code), "conductor_kill");
    assert_eq!(remote.unwrap_err().code(), ErrorCode::TotpInvalid);
    // The next code works, once.
    f.clock.advance_secs(30);
    let next = f.code_now(&secret);
    assert_eq!(s.handle(unlock(&next)).await, SecurityReply::Done);
    assert_eq!(
        reason(&s.handle(unlock(&next)).await),
        FailReason::TotpReplayed
    );
}
