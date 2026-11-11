//! Remote shell service tests with an injectable clock and an in-memory
//! TOTP enrollment. Real shells are spawned (cmd on Windows, sh elsewhere;
//! ConPTY, no window) and always stopped at the end.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use vca_broker::contain::Scope;
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ControlTier, DangerLevel, Error, Input, ServerMsg, SessionId, SessionInfo,
    SessionOrigin, SessionState, ShellRequest,
};
use vca_net::{Clock, KillSwitch, ManualClock, TotpManager};

use super::audit::{AUDIT_FILE, ShellAudit};
use super::*;
use crate::events::LogLimits;
use crate::sessions::{ManagerConfig, Observed, ObservedIdentity, SessionManager};
use crate::workspace::{WorkspaceRoots, without_verbatim_prefix};

const TEST_SHELL: &str = if cfg!(windows) { "cmd" } else { "sh" };
const WAIT: Duration = Duration::from_secs(20);

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: PathBuf,
    audit_path: PathBuf,
    clock: Arc<ManualClock>,
    secret: Vec<u8>,
    backups: Vec<String>,
    shells: Shells,
    mgr: SessionManager,
    notices: broadcast::Receiver<ServerMsg>,
}

fn settings(idle: u64, life: u64, step_up: u64) -> ShellSettings {
    ShellSettings {
        enabled: true,
        idle_timeout: Duration::from_secs(idle),
        max_lifetime: Duration::from_secs(life),
        step_up_window: Duration::from_secs(step_up),
    }
}

fn fixture(settings: ShellSettings) -> Fixture {
    let dir = short_tempdir();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(workspace.join("project")).unwrap();
    // The server reports canonical paths; on Windows the temp dir can come
    // as an 8.3 short name (`RUNNER~1`) that canonicalizing expands.
    let workspace = without_verbatim_prefix(std::fs::canonicalize(&workspace).unwrap());
    let mgr = SessionManager::new(
        ManagerConfig {
            limits: LogLimits::default(),
            exited_retention: Duration::from_secs(60),
            pause_scope: Scope::ChildrenOnly,
            heuristics_interval: Duration::from_secs(1),
            control_endpoint: PathBuf::from("unused"),
            attach_dir: Some(dir.path().to_owned()),
            workspace_roots: WorkspaceRoots::new(vec![workspace.clone()]),
            session_names: None,
            allow_generic_launch: false,
        },
        None,
    );
    // The clock starts at the real time so that codes look ordinary.
    let clock = ManualClock::new(vca_core::now_millis());
    let totp = Arc::new(TotpManager::in_memory(clock.clone()));
    let pending = totp.begin_enrollment("VCA test", "owner");
    let secret = data_encoding::BASE32_NOPAD
        .decode(pending.secret_base32().trim_end_matches('=').as_bytes())
        .unwrap();
    let code = vca_net::totp::totp_code_at(&secret, clock.now_secs());
    let backups = totp.confirm_enrollment(&pending, &code).unwrap();
    let audit_path = dir.path().join("data").join(AUDIT_FILE);
    let (tx, notices) = broadcast::channel(64);
    let shells = Shells::new(
        mgr.clone(),
        settings,
        clock.clone(),
        ShellAudit::new(&audit_path, audit::AUDIT_MAX_BYTES),
        tx,
    );
    shells.set_totp(totp);
    Fixture {
        _dir: dir,
        workspace,
        audit_path,
        clock,
        secret,
        backups,
        shells,
        mgr,
        notices,
    }
}

impl Fixture {
    /// A code for the next TOTP step (the previous one is used up).
    fn next_code(&self) -> String {
        self.clock.advance_secs(30);
        self.code_now()
    }

    fn code_now(&self) -> String {
        vca_net::totp::totp_code_at(&self.secret, self.clock.now_secs())
    }

    fn request(&self, shell: &str, totp: &str) -> ShellRequest {
        ShellRequest {
            shell: shell.into(),
            project_dir: self.workspace.join("project"),
            display_name: None,
            totp: totp.into(),
        }
    }

    async fn open(&self) -> SessionInfo {
        let req = self.request(TEST_SHELL, &self.next_code());
        self.shells.open(&phone(), req).await.expect("open shell")
    }

    fn audit_text(&self) -> String {
        std::fs::read_to_string(&self.audit_path).unwrap_or_default()
    }

    async fn wait_exited(&self, id: &SessionId) {
        tokio::time::timeout(WAIT, async {
            loop {
                let exited = self
                    .mgr
                    .get(id)
                    .is_none_or(|s| matches!(s.info().state, SessionState::Exited { .. }));
                if exited {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the shell process ends");
    }

    /// Stops every shell the test left open.
    async fn cleanup(&self) {
        self.shells.close_all(CloseReason::Shutdown).await;
        self.mgr.stop_all(Duration::from_secs(5)).await;
    }

    fn next_notice(&mut self) -> ServerMsg {
        loop {
            match self.notices.try_recv() {
                Ok(msg) => return msg,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(e) => panic!("no notice: {e:?}"),
            }
        }
    }
}

fn phone() -> Actor {
    Actor {
        device_id: "dev_phone".into(),
        device_name: "Phone".into(),
    }
}

fn code(err: Error) -> ErrorCode {
    err.code()
}

#[tokio::test]
async fn open_needs_a_fresh_totp_and_rejects_a_replayed_code() {
    let mut f = fixture(settings(3600, 7200, 300));
    // No code, a wrong code.
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, ""))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpRequired);
    let wrong = vca_net::totp::totp_code_at(&f.secret, f.clock.now_secs() - 3600);
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &wrong))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpInvalid);

    // A fresh code opens the shell.
    let good = f.next_code();
    let info = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &good))
        .await
        .unwrap();
    assert!(
        matches!(
            &info.origin,
            SessionOrigin::Local { agent: AgentKind::Shell(name), .. } if name == TEST_SHELL
        ),
        "{:?}",
        info.origin
    );
    assert_eq!(info.control, ControlTier::ManagedPty);
    let shell = info.shell.clone().expect("shell info");
    let now = f.clock.now_millis();
    assert_eq!(shell.idle_timeout_secs, 3600);
    assert_eq!(shell.idle_deadline, now + 3_600_000);
    assert_eq!(shell.hard_deadline, now + 7_200_000);
    assert_eq!(shell.opened_by, "Phone");
    assert!(f.shells.in_step_up("dev_phone"));

    // The same code again is a replay.
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &good))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpInvalid);

    match f.next_notice() {
        ServerMsg::ShellNotice {
            session_id,
            event,
            device_name,
            reason,
        } => {
            assert_eq!(session_id, info.id);
            assert_eq!(event, "opened");
            assert_eq!(device_name, "Phone");
            assert_eq!(reason, None);
        }
        other => panic!("{other:?}"),
    }
    assert!(f.shells.close(&info.id, CloseReason::User, None).await);
    match f.next_notice() {
        ServerMsg::ShellNotice { event, reason, .. } => {
            assert_eq!(event, "closed");
            assert_eq!(reason.as_deref(), Some("user"));
        }
        other => panic!("{other:?}"),
    }
    f.wait_exited(&info.id).await;

    // Audit: failures and successes, never a code.
    let text = f.audit_text();
    for event in ["step_up_failed", "step_up_ok", "shell_open", "shell_close"] {
        assert!(text.contains(event), "{event} in {text}");
    }
    assert!(text.contains("reason=replayed"), "{text}");
    assert!(text.contains("reason=user"), "{text}");
    assert!(!text.contains(&good) && !text.contains(&wrong), "{text}");
    f.cleanup().await;
}

#[tokio::test]
async fn only_allow_listed_shells_and_workspace_folders_open() {
    let f = fixture(settings(3600, 7200, 300));
    let code = f.next_code();
    let foreign = if cfg!(windows) { "bash" } else { "cmd" };
    for bad in [
        "fish",
        foreign,
        "/bin/sh",
        r"C:\Windows\System32\cmd.exe",
        "cmd.exe",
        "../sh",
        "powershell; calc",
        "sh -c id",
        "",
    ] {
        // A valid code is offered each time; it must not be used up.
        let err = f
            .shells
            .open(&phone(), f.request(bad, &code))
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest, "{bad:?}");
    }
    let outside = tempfile::tempdir().unwrap();
    let mut req = f.request(TEST_SHELL, &code);
    req.project_dir = outside.path().to_owned();
    let err = f.shells.open(&phone(), req).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    let mut req = f.request(TEST_SHELL, &code);
    req.project_dir = f.workspace.join("project").join("..").join("..");
    let err = f.shells.open(&phone(), req).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    let mut req = f.request(TEST_SHELL, &code);
    req.project_dir = PathBuf::from("relative/dir");
    let err = f.shells.open(&phone(), req).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    let mut req = f.request(TEST_SHELL, &code);
    req.project_dir = f.workspace.join("missing");
    let err = f.shells.open(&phone(), req).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
    // None of the refusals used the current code.
    assert!(!f.audit_text().contains("step_up"));
    let info = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &code))
        .await
        .unwrap();
    // `default` resolves to an allowed name.
    let default = resolve_shell("default").unwrap();
    assert!(allowed_shells().contains(&default.name));
    assert_eq!(resolve_shell(" CMD ").is_ok(), cfg!(windows));
    f.shells.close(&info.id, CloseReason::User, None).await;
    f.cleanup().await;
}

#[test]
fn shells_do_not_inherit_agent_or_vca_variables() {
    for key in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "ANTHROPIC_API_KEY",
        "VCA_CONTROL",
        "vca_session_id",
    ] {
        assert!(stripped_env(&OsString::from(key)), "{key}");
    }
    for key in ["PATH", "HOME", "USERPROFILE", "MY_CLAUDE"] {
        assert!(!stripped_env(&OsString::from(key)), "{key}");
    }
}

/// Inserts an agent session (not a shell) the way discovery does.
fn observed_agent(mgr: &SessionManager, dir: &Path) -> SessionId {
    let info = SessionInfo {
        id: SessionId::new(),
        origin: SessionOrigin::Local {
            agent: AgentKind::ClaudeCode,
            project_dir: dir.to_owned(),
            pid: None,
        },
        display_name: "agent".into(),
        state: SessionState::Running,
        control: ControlTier::ObserveOnly,
        profile: None,
        created_at: 1,
        last_activity: 1,
        unread: 0,
        preview: None,
        auto_name: None,
        custom_name: false,
        shell: None,
        pending_decision: None,
        subagents: 0,
        conductor: false,
        terminal_prompt: None,
        question: None,
        progress: None,
        problem: None,
    };
    let observed = Observed {
        terminal: None,
        identity: ObservedIdentity::default(),
    };
    mgr.insert_observed(info, observed, None).id.clone()
}

#[tokio::test]
async fn shell_run_is_only_for_open_shells() {
    let f = fixture(settings(3600, 7200, 300));
    let agent = observed_agent(&f.mgr, &f.workspace);
    let err = f
        .shells
        .run(&phone(), &agent, "ls", None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::InvalidRequest);
    let err = f
        .shells
        .run(&phone(), &SessionId::from("nope"), "ls", None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::NotFound);

    let info = f.open().await;
    for bad in ["", "   ", "echo \u{1b}[2J", "echo a\u{0}b"] {
        let err = f
            .shells
            .run(&phone(), &info.id, bad, None, false)
            .await
            .unwrap_err();
        assert_eq!(code(err), ErrorCode::InvalidRequest, "{bad:?}");
    }
    let long = "x".repeat(MAX_COMMAND_BYTES + 1);
    let err = f
        .shells
        .run(&phone(), &info.id, &long, None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::InvalidRequest);
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, "echo vca-shell-ok", None, false)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Safe)
    );
    // A closed shell takes no commands.
    f.shells.close(&info.id, CloseReason::User, None).await;
    let err = f
        .shells
        .run(&phone(), &info.id, "echo again", None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Conflict);
    f.cleanup().await;
}

#[tokio::test]
async fn dangerous_commands_need_the_step_up_window() {
    let f = fixture(settings(3600, 7200, 300));
    let info = f.open().await;
    // Harmless to run, but classified dangerous (names a secret file).
    let dangerous = "echo .env";
    assert_eq!(danger::classify(dangerous).level, DangerLevel::Dangerous);

    // Right after opening, the step-up window covers it.
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, dangerous, None, false)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Dangerous)
    );
    // After the window: a code is required.
    f.clock.advance_secs(301);
    assert!(!f.shells.in_step_up("dev_phone"));
    let err = f
        .shells
        .run(&phone(), &info.id, dangerous, None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpRequired);
    // Raw text input is held to the same rule; keys are not.
    let err = f
        .shells
        .check_input(
            &phone(),
            &info.id,
            &Input::Text {
                at_prompt: false,
                text: "rm -rf /".into(),
                submit: true,
            },
        )
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpRequired);
    f.shells
        .check_input(
            &phone(),
            &info.id,
            &Input::Key {
                key: vca_core::ControlKey::Interrupt,
            },
        )
        .unwrap();
    // Safe and caution commands never need a code.
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, "echo hello", None, false)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Safe)
    );
    // A wrong code is refused, a good one runs and reopens the window.
    let err = f
        .shells
        .run(&phone(), &info.id, dangerous, Some("000000x"), false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpInvalid);
    let good = f.next_code();
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, dangerous, Some(&good), false)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Dangerous)
    );
    // Within 5 minutes: no code needed; another device has its own window.
    f.clock.advance_secs(240);
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, dangerous, None, false)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Dangerous)
    );
    let laptop = Actor {
        device_id: "dev_laptop".into(),
        device_name: "Laptop".into(),
    };
    let err = f
        .shells
        .run(&laptop, &info.id, dangerous, None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpRequired);

    // The audit log has the runs with their level, redacted text and no
    // codes.
    f.shells
        .run(
            &phone(),
            &info.id,
            "echo token=supersecretvalue123",
            None,
            false,
        )
        .await
        .unwrap();
    let text = f.audit_text();
    assert!(
        text.contains("danger=dangerous command=echo .env"),
        "{text}"
    );
    assert!(text.contains("danger=safe command=echo hello"), "{text}");
    assert!(!text.contains("supersecretvalue123"), "{text}");
    assert!(!text.contains(&good), "{text}");
    let entries = f.shells.list_audit(2);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].event, "shell_run");
    assert!(entries[0].ts >= entries[1].ts);
    f.cleanup().await;
}

#[tokio::test]
async fn too_many_wrong_codes_lock_the_device_out() {
    let f = fixture(settings(3600, 7200, 300));
    for _ in 0..MAX_TOTP_FAILURES {
        let err = f
            .shells
            .open(&phone(), f.request(TEST_SHELL, "999999x"))
            .await
            .unwrap_err();
        assert_eq!(code(err), ErrorCode::TotpInvalid);
    }
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &f.next_code()))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Forbidden);
    // The lockout ends after the failure window.
    f.clock.advance_secs(FAILURE_WINDOW_MS / 1000 + 1);
    let info = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &f.code_now()))
        .await
        .unwrap();
    f.shells.close(&info.id, CloseReason::User, None).await;
    f.cleanup().await;
}

/// Codes checked at the same time count against the limit before they
/// are tested: a burst of guesses gets no more tries than one by one.
#[tokio::test]
async fn concurrent_wrong_codes_cannot_pass_the_lockout() {
    let f = fixture(settings(3600, 7200, 300));
    let start = std::sync::Barrier::new(32);
    let results: Vec<ErrorCode> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..32)
            .map(|_| {
                s.spawn(|| {
                    start.wait();
                    f.shells
                        .verify_totp(&phone(), Some("999999x"), "open_shell", None)
                        .unwrap_err()
                        .code()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let tested = results
        .iter()
        .filter(|c| **c == ErrorCode::TotpInvalid)
        .count();
    assert_eq!(tested, MAX_TOTP_FAILURES, "{results:?}");
    assert!(
        results
            .iter()
            .all(|c| matches!(c, ErrorCode::TotpInvalid | ErrorCode::Forbidden))
    );
}

/// An attempt that never tested a code (none enrolled) does not count.
#[tokio::test]
async fn an_untested_code_does_not_count_as_a_failure() {
    let f = fixture(settings(3600, 7200, 300));
    let bare = Shells::new(
        f.mgr.clone(),
        settings(3600, 7200, 300),
        f.clock.clone(),
        ShellAudit::new(&f.audit_path, audit::AUDIT_MAX_BYTES),
        broadcast::channel(4).0,
    );
    bare.set_totp(Arc::new(TotpManager::in_memory(f.clock.clone())));
    for _ in 0..MAX_TOTP_FAILURES + 2 {
        let err = bare
            .verify_totp(&phone(), Some("123456"), "open_shell", None)
            .unwrap_err();
        assert_ne!(err.code(), ErrorCode::Forbidden);
    }
}

/// "No limit" in the config (a huge timeout) must not wrap the deadlines
/// into the past (closing the shell at once) or overflow.
#[tokio::test]
async fn endless_timeouts_do_not_wrap() {
    let f = fixture(settings(u64::MAX, u64::MAX, u64::MAX));
    f.shells
        .require_step_up(&phone(), Some(&f.next_code()), "test")
        .unwrap();
    assert!(f.shells.in_step_up("dev_phone"));
    let info = f.open().await;
    f.shells.touch(&info.id);
    f.shells.reap().await;
    assert!(f.shells.is_open(&info.id));
    let shell = f.mgr.get(&info.id).unwrap().info().shell.unwrap();
    assert!(shell.idle_deadline > vca_core::now_millis());
    f.shells.close(&info.id, CloseReason::User, None).await;
    f.cleanup().await;
}

/// The owner turning remote shells off also closes the open ones, and
/// typing into a shell is refused from then on.
#[tokio::test]
async fn turning_remote_shells_off_closes_open_shells() {
    let f = fixture(settings(3600, 7200, 300));
    let info = f.open().await;
    f.shells.set_owner_enabled(false);
    let text = Input::Text {
        text: "echo hi".into(),
        submit: true,
        at_prompt: false,
    };
    let err = f.shells.check_input(&phone(), &info.id, &text).unwrap_err();
    assert_eq!(code(err), ErrorCode::Unsupported);
    tokio::time::timeout(WAIT, async {
        while f.shells.is_open(&info.id) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the shell closes");
    f.wait_exited(&info.id).await;
    f.cleanup().await;
}

#[tokio::test]
async fn idle_and_lifetime_expiry_stop_the_shell() {
    // Idle: 2 s without input.
    let mut f = fixture(settings(2, 3600, 300));
    let info = f.open().await;
    let _ = f.next_notice();
    f.clock.advance_millis(1500);
    f.shells.touch(&info.id);
    f.clock.advance_millis(1500);
    f.shells.reap().await;
    assert!(f.shells.is_open(&info.id), "input moved the idle deadline");
    // Half a second left (the warning comes at most half the timeout ahead).
    match f.next_notice() {
        ServerMsg::ShellClosingSoon {
            session_id,
            seconds_left,
            reason,
        } => {
            assert_eq!(session_id, info.id);
            assert_eq!(reason, "idle");
            assert_eq!(seconds_left, 1);
        }
        other => panic!("{other:?}"),
    }
    // The new deadline is announced on the card.
    let deadline = f
        .mgr
        .get(&info.id)
        .unwrap()
        .info()
        .shell
        .unwrap()
        .idle_deadline;
    assert!(deadline > info.shell.as_ref().unwrap().idle_deadline);
    f.clock.advance_millis(600);
    f.shells.reap().await;
    assert!(!f.shells.is_open(&info.id));
    f.wait_exited(&info.id).await;
    match f.next_notice() {
        ServerMsg::ShellNotice { event, reason, .. } => {
            assert_eq!(event, "closed");
            assert_eq!(reason.as_deref(), Some("idle"));
        }
        other => panic!("{other:?}"),
    }
    assert!(f.audit_text().contains("reason=idle"));
    f.cleanup().await;

    // Lifetime: 5 s, whatever the input.
    let f = fixture(settings(3600, 5, 300));
    let info = f.open().await;
    let shell = info.shell.clone().unwrap();
    assert_eq!(
        shell.idle_deadline, shell.hard_deadline,
        "capped by lifetime"
    );
    for _ in 0..3 {
        f.clock.advance_secs(2);
        f.shells.touch(&info.id);
    }
    f.shells.reap().await;
    assert!(!f.shells.is_open(&info.id));
    f.wait_exited(&info.id).await;
    let text = f.audit_text();
    assert!(text.contains("reason=lifetime"), "{text}");
    f.cleanup().await;
}

fn no_notice(f: &mut Fixture) {
    match f.notices.try_recv() {
        Err(broadcast::error::TryRecvError::Empty) => {}
        other => panic!("unexpected notice: {other:?}"),
    }
}

fn expect_notice(f: &mut Fixture, which: &str) -> (Option<String>, Option<u32>) {
    match f.next_notice() {
        ServerMsg::ShellNotice { event, reason, .. } if event == which => (reason, None),
        ServerMsg::ShellClosingSoon {
            seconds_left,
            reason,
            ..
        } if which == "closing_soon" => (Some(reason), Some(seconds_left)),
        other => panic!("expected {which}: {other:?}"),
    }
}

#[tokio::test]
async fn closing_soon_is_announced_once_and_keep_alive_never_passes_the_lifetime() {
    // Idle 120 s, lifetime 300 s: the warning comes 60 s ahead.
    let mut f = fixture(settings(120, 300, 300));
    let info = f.open().await;
    let shell = info.shell.clone().unwrap();
    assert_eq!(shell.shell, TEST_SHELL);
    assert_eq!(shell.project_dir, f.workspace.join("project"));
    let _ = f.next_notice();
    f.clock.advance_secs(59);
    f.shells.reap().await;
    no_notice(&mut f);
    f.clock.advance_secs(2);
    f.shells.reap().await;
    assert_eq!(
        expect_notice(&mut f, "closing_soon"),
        (Some("idle".into()), Some(59))
    );
    // Once per deadline.
    f.shells.reap().await;
    no_notice(&mut f);

    // Kept open: the deadline moves and every client hears it.
    let kept = f.shells.keep_alive(&phone(), &info.id).unwrap();
    assert_eq!(kept, shell.hard_deadline - 300_000 + 61_000 + 120_000);
    assert_eq!(expect_notice(&mut f, "kept_open"), (None, None));
    let card = f.mgr.get(&info.id).unwrap().info().shell.unwrap();
    assert_eq!(card.idle_deadline, kept);

    // Up to the lifetime and no further.
    f.clock.advance_secs(100);
    f.shells.keep_alive(&phone(), &info.id).unwrap();
    no_notice(&mut f);
    f.clock.advance_secs(100);
    let capped = f.shells.keep_alive(&phone(), &info.id).unwrap();
    assert_eq!(capped, shell.hard_deadline);
    f.shells.reap().await;
    assert_eq!(
        expect_notice(&mut f, "closing_soon"),
        (Some("lifetime".into()), Some(39))
    );
    // Nothing extends the lifetime: no `kept_open`, no second warning.
    assert_eq!(
        f.shells.keep_alive(&phone(), &info.id).unwrap(),
        shell.hard_deadline
    );
    f.shells.reap().await;
    no_notice(&mut f);
    f.clock.advance_secs(40);
    f.shells.reap().await;
    assert_eq!(
        expect_notice(&mut f, "closed"),
        (Some("lifetime".into()), None)
    );
    f.wait_exited(&info.id).await;
    // A closed shell cannot be kept open.
    let err = f.shells.keep_alive(&phone(), &info.id).unwrap_err();
    assert_eq!(code(err), ErrorCode::Conflict);
    let err = f
        .shells
        .keep_alive(&phone(), &SessionId::from("nope"))
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::NotFound);
    f.cleanup().await;
}

#[tokio::test]
async fn an_ended_shell_can_be_dismissed_but_not_a_running_one() {
    let f = fixture(settings(3600, 7200, 300));
    let info = f.open().await;
    let err = f.mgr.dismiss(&info.id).unwrap_err();
    assert_eq!(code(err), ErrorCode::Conflict);
    assert!(f.mgr.get(&info.id).is_some());

    f.shells.close(&info.id, CloseReason::Idle, None).await;
    f.wait_exited(&info.id).await;
    let mut updates = f.mgr.subscribe_updates();
    // What a new shell "here" needs stays on the ended card.
    let ended = f.mgr.get(&info.id).unwrap().info();
    let shell = ended.shell.clone().unwrap();
    assert_eq!(
        (shell.shell.as_str(), shell.project_dir.clone()),
        (TEST_SHELL, f.workspace.join("project"))
    );
    let gone = f.mgr.dismiss(&info.id).unwrap();
    assert_eq!(gone.id, info.id);
    assert!(f.mgr.get(&info.id).is_none());
    let removed = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(crate::sessions::Update::Removed(id)) = updates.recv().await {
                return id;
            }
        }
    })
    .await
    .expect("removal announced");
    assert_eq!(removed, info.id);
    let err = f.mgr.dismiss(&info.id).unwrap_err();
    assert_eq!(code(err), ErrorCode::NotFound);

    // The same shell kind and folder open a new one.
    let again = f.request(&shell.shell, &f.next_code());
    let again = ShellRequest {
        project_dir: shell.project_dir.clone(),
        ..again
    };
    let reopened = f.shells.open(&phone(), again).await.unwrap();
    assert_ne!(reopened.id, info.id);
    assert_eq!(reopened.shell.unwrap().project_dir, shell.project_dir);
    f.cleanup().await;
}

#[tokio::test]
async fn the_reaper_notices_a_shell_that_exited() {
    let f = fixture(settings(3600, 7200, 300));
    f.shells.start();
    let info = f.open().await;
    f.shells
        .run(&phone(), &info.id, "exit", None, false)
        .await
        .unwrap();
    f.wait_exited(&info.id).await;
    tokio::time::timeout(WAIT, async {
        while f.shells.is_open(&info.id) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the shell is closed");
    assert!(f.audit_text().contains("reason=exit"));
    f.cleanup().await;
}

#[tokio::test]
async fn the_kill_switch_stops_shells_and_refuses_new_ones() {
    let mut f = fixture(settings(3600, 7200, 300));
    let kill = KillSwitch::in_memory();
    f.shells.set_kill_switch(kill.clone());
    let watch = f.shells.watch_kill_switch().unwrap();
    let a = f.open().await;
    let b = f.open().await;
    let _ = (f.next_notice(), f.next_notice());
    kill.trip().unwrap();
    tokio::time::timeout(WAIT, async {
        while f.shells.is_open(&a.id) || f.shells.is_open(&b.id) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("shells closed by the kill switch");
    f.wait_exited(&a.id).await;
    f.wait_exited(&b.id).await;
    for _ in 0..2 {
        match f.next_notice() {
            ServerMsg::ShellNotice { reason, .. } => {
                assert_eq!(reason.as_deref(), Some("kill_switch"));
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(!f.shells.in_step_up("dev_phone"), "step-up ends too");
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &f.next_code()))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Forbidden);
    assert!(f.audit_text().contains("reason=kill_switch"));
    watch.abort();
    f.cleanup().await;
}

#[tokio::test]
async fn a_backup_code_opens_a_shell_once() {
    let f = fixture(settings(3600, 7200, 0));
    let backup = f.backups[2].clone();
    let info = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &backup))
        .await
        .expect("a backup code is accepted in the code field");
    let audit = f.audit_text();
    assert!(audit.contains("method=backup_code"), "{audit}");
    assert!(!audit.contains(&backup), "backup codes are never logged");
    // Single use, even with other spelling.
    let again = backup.to_lowercase().replace('-', " ");
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &again))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpInvalid);
    // A backup code also confirms a dangerous command.
    // Harmless to run, but classified dangerous (names a secret file).
    let danger = "echo .env";
    let err = f
        .shells
        .run(&phone(), &info.id, danger, None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpRequired);
    f.shells
        .run(&phone(), &info.id, danger, Some(&f.backups[3]), false)
        .await
        .expect("the backup code confirms the command");
    f.cleanup().await;
}

#[tokio::test]
async fn without_an_enrollment_no_code_is_asked_for() {
    let f = fixture(settings(3600, 7200, 0));
    f.shells.inner.totp.get().unwrap().disable().unwrap();
    // Asking without a code tells the app that nothing is enrolled.
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, ""))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not set up"), "{err}");
    assert_eq!(code(err), ErrorCode::Unsupported);
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, "123456"))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Unsupported);
}

#[tokio::test]
async fn disabled_shells_refuse_everything() {
    let mut s = settings(3600, 7200, 300);
    s.enabled = false;
    let f = fixture(s);
    let err = f
        .shells
        .open(&phone(), f.request(TEST_SHELL, &f.next_code()))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Unsupported);
    let err = f
        .shells
        .run(&phone(), &SessionId::from("x"), "ls", None, false)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Unsupported);
}

#[tokio::test]
async fn without_totp_no_shell_opens() {
    let dir = short_tempdir();
    let workspace = dir.path().join("w");
    std::fs::create_dir_all(&workspace).unwrap();
    let mgr = SessionManager::new(
        ManagerConfig {
            limits: LogLimits::default(),
            exited_retention: Duration::from_secs(60),
            pause_scope: Scope::ChildrenOnly,
            heuristics_interval: Duration::from_secs(1),
            control_endpoint: PathBuf::from("unused"),
            attach_dir: Some(dir.path().to_owned()),
            workspace_roots: WorkspaceRoots::new(vec![workspace.clone()]),
            session_names: None,
            allow_generic_launch: false,
        },
        None,
    );
    let shells = Shells::new(
        mgr,
        settings(60, 60, 60),
        vca_net::system_clock(),
        ShellAudit::new(dir.path().join(AUDIT_FILE), 1024),
        broadcast::channel(4).0,
    );
    let req = ShellRequest {
        shell: TEST_SHELL.into(),
        project_dir: workspace,
        display_name: None,
        totp: "123456".into(),
    };
    let err = shells.open(&Actor::local(), req).await.unwrap_err();
    assert_eq!(code(err), ErrorCode::Unsupported);
}

fn laptop() -> Actor {
    Actor {
        device_id: "dev_laptop".into(),
        device_name: "Laptop".into(),
    }
}

fn text(t: &str) -> Input {
    Input::Text {
        text: t.into(),
        submit: false,
        at_prompt: true,
    }
}

/// The refusal for a device that has not joined the shell.
fn is_join_required(err: &Error) -> bool {
    matches!(err, Error::Api { code: ErrorCode::TotpRequired, message } if message.contains("join"))
}

#[test]
fn submissions_are_split_into_lines() {
    let count = |t: &str| submission_lines(t).len();
    // One command, with or without its Enter; a lone Enter key.
    for one in ["ls", "ls\n", "ls\r\n", "ls\r", "\r", "\n", ""] {
        assert_eq!(count(one), 1, "{one:?}");
    }
    assert_eq!(submission_lines("ls -la\n"), ["ls -la"]);
    // Pasted scripts, continuations, heredocs, blank lines.
    assert_eq!(submission_lines("echo a\necho b"), ["echo a", "echo b"]);
    assert_eq!(submission_lines("a\r\nb\r\n"), ["a", "b"]);
    assert_eq!(submission_lines("a\rb"), ["a", "b"]);
    assert_eq!(count("echo a\n\n"), 2);
    assert_eq!(count("ls \\\n  -la"), 2);
    assert_eq!(count("cat <<EOF\nhello\nEOF\n"), 3);
}

#[tokio::test]
async fn multi_line_input_waits_for_a_confirmation() {
    let f = fixture(settings(3600, 7200, 300));
    let info = f.open().await;
    let script = "echo vca-line-1\necho vca-line-2\n";

    // Not typed without `confirm`: the full text comes back.
    let out = f
        .shells
        .run(&phone(), &info.id, script, None, false)
        .await
        .unwrap();
    assert_eq!(
        out,
        RunOutcome::Confirm {
            command: script.into(),
            lines: 2
        }
    );
    assert!(!f.audit_text().contains("shell_run"), "{}", f.audit_text());

    // One command with its Enter is not multi-line.
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, "echo one\n", None, false)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Safe)
    );
    // Confirmed: typed, and the audit says how many lines.
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, script, None, true)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Safe)
    );
    assert!(
        f.audit_text().contains("danger=safe lines=2"),
        "{}",
        f.audit_text()
    );

    // Every line is classified on confirm: a dangerous second line needs
    // the step-up window like a dangerous single command.
    f.clock.advance_secs(301);
    let risky = "echo fine\necho .env";
    let err = f
        .shells
        .run(&phone(), &info.id, risky, None, true)
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpRequired);
    let good = f.next_code();
    assert_eq!(
        f.shells
            .run(&phone(), &info.id, risky, Some(&good), true)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Dangerous)
    );

    // Raw text input is held the same way; keys and single lines pass.
    assert_eq!(
        f.shells
            .check_input(&phone(), &info.id, &text("echo a\r\necho b"))
            .unwrap(),
        InputCheck::Confirm {
            text: "echo a\r\necho b".into(),
            lines: 2
        }
    );
    for single in ["echo a\r", "\r", "x"] {
        assert_eq!(
            f.shells
                .check_input(&phone(), &info.id, &text(single))
                .unwrap(),
            InputCheck::Pass,
            "{single:?}"
        );
    }
    assert_eq!(
        f.shells
            .check_input(
                &phone(),
                &info.id,
                &Input::Key {
                    key: vca_core::ControlKey::Interrupt
                }
            )
            .unwrap(),
        InputCheck::Pass
    );
    f.cleanup().await;
}

#[tokio::test]
async fn another_admin_device_joins_a_shell_with_a_code() {
    let mut f = fixture(settings(3600, 7200, 300));
    let info = f.open().await;
    expect_notice(&mut f, "opened");

    // The opener needs nothing more.
    f.shells.check_access(&phone(), &info.id).unwrap();
    // Another device is refused everywhere until it joins.
    assert!(is_join_required(
        &f.shells.check_access(&laptop(), &info.id).unwrap_err()
    ));
    let err = f
        .shells
        .run(&laptop(), &info.id, "echo hi", None, false)
        .await
        .unwrap_err();
    assert!(is_join_required(&err), "{err:?}");
    let err = f
        .shells
        .check_input(&laptop(), &info.id, &text("x"))
        .unwrap_err();
    assert!(is_join_required(&err), "{err:?}");
    let err = f.shells.keep_alive(&laptop(), &info.id).unwrap_err();
    assert!(is_join_required(&err), "{err:?}");

    // A step-up window from elsewhere does not replace the code.
    f.shells
        .require_step_up(&laptop(), Some(&f.next_code()), "restore_run")
        .unwrap();
    let err = f.shells.join(&laptop(), &info.id, "").unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpRequired);
    // A wrong code is refused and counted.
    let err = f.shells.join(&laptop(), &info.id, "000000x").unwrap_err();
    assert_eq!(code(err), ErrorCode::TotpInvalid);
    assert!(f.audit_text().contains("purpose=shell_join reason=invalid"));
    // A good code grants this shell to this device.
    let good = f.next_code();
    f.shells.join(&laptop(), &info.id, &good).unwrap();
    match f.next_notice() {
        ServerMsg::ShellNotice {
            event, device_name, ..
        } => {
            assert_eq!(event, "joined");
            assert_eq!(device_name, "Laptop");
        }
        other => panic!("{other:?}"),
    }
    let audit = f.audit_text();
    assert!(audit.contains("\"shell_join\""), "{audit}");
    assert!(audit.contains("opened_by=Phone"), "{audit}");
    assert!(!audit.contains(&good), "{audit}");
    f.shells.check_access(&laptop(), &info.id).unwrap();
    assert_eq!(
        f.shells
            .run(&laptop(), &info.id, "echo joined", None, false)
            .await
            .unwrap(),
        RunOutcome::Ran(DangerLevel::Safe)
    );
    // Joining again uses no code.
    f.shells.join(&laptop(), &info.id, "").unwrap();
    // Per device: a third device still has to join.
    let tablet = Actor {
        device_id: "dev_tablet".into(),
        device_name: "Tablet".into(),
    };
    assert!(is_join_required(
        &f.shells.check_access(&tablet, &info.id).unwrap_err()
    ));

    // The grant ends with the shell; the opener still reads it.
    f.shells.close(&info.id, CloseReason::User, None).await;
    assert!(is_join_required(
        &f.shells.check_access(&laptop(), &info.id).unwrap_err()
    ));
    f.shells.check_access(&phone(), &info.id).unwrap();
    let err = f
        .shells
        .join(&laptop(), &info.id, &f.next_code())
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Conflict);
    f.cleanup().await;
}

#[tokio::test]
async fn turning_shells_off_ends_join_grants_at_once() {
    let f = fixture(settings(3600, 7200, 300));
    let info = f.open().await;
    f.shells.join(&laptop(), &info.id, &f.next_code()).unwrap();
    f.shells.check_access(&laptop(), &info.id).unwrap();
    f.shells.set_owner_enabled(false);
    // Before the shell is even closed, the grant is gone.
    assert!(is_join_required(
        &f.shells.check_access(&laptop(), &info.id).unwrap_err()
    ));
    tokio::time::timeout(WAIT, async {
        while f.shells.is_open(&info.id) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the shell closes");
    f.cleanup().await;
}

#[tokio::test]
async fn a_revoked_device_loses_its_join_grant() {
    let f = fixture(settings(3600, 7200, 300));
    let info = f.open().await;
    f.shells.join(&laptop(), &info.id, &f.next_code()).unwrap();
    f.shells.forget_device("dev_laptop");
    assert!(is_join_required(
        &f.shells.check_access(&laptop(), &info.id).unwrap_err()
    ));
    f.cleanup().await;
}

/// A temporary folder with a short path. The attach sockets live in it,
/// and a Unix socket path is limited to about 100 bytes: macOS's own
/// temporary folder (`/var/folders/...`) is already too long for one.
fn short_tempdir() -> tempfile::TempDir {
    if cfg!(unix) {
        tempfile::tempdir_in("/tmp").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    }
}
