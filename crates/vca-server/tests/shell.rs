//! End-to-end tests of remote shells over real Noise WebSocket connections:
//! scope enforcement, TOTP (fresh and replayed codes), commands, notices to
//! every client, the audit logs and the kill switch. Shells are real (cmd on
//! Windows, sh elsewhere; ConPTY, no window) and stopped by every test.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ClientMsg, DangerLevel, Envelope, LaunchRequest, ServerMsg, SessionAction,
    SessionEvent, SessionId, SessionOrigin, ShellRequest,
};
use vca_net::{ClientAuth, ClientConfig, ClientConnection, PairingPayload, StaticKeypair};
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};
use vca_server::daemon::{Options, Server};

const TIMEOUT: Duration = Duration::from_secs(30);
const TEST_SHELL: &str = if cfg!(windows) { "cmd" } else { "sh" };

/// Never touches a real process through the broker.
struct NoProcessControl;

impl ProcessControl for NoProcessControl {
    fn set_priority(&self, root: u32, _: Scope, _: Priority) -> ContainResult {
        Ok(vec![root])
    }
    fn suspend(&self, root: u32, _: Scope) -> ContainResult {
        Ok(vec![root])
    }
    fn resume(&self, root: u32, _: Scope) -> ContainResult {
        Ok(vec![root])
    }
    fn terminate(&self, root: u32, _: Scope, _: Duration) -> ContainResult {
        panic!("tests must never terminate processes through the broker ({root})")
    }
}

fn control_endpoint(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\vca-test-{}-shell-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join("control.sock")
    }
}

async fn start(dir: &Path) -> Server {
    start_with(dir, |_| {}).await
}

async fn start_with(dir: &Path, change: impl FnOnce(&mut Config)) -> Server {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(workspace.join("project")).unwrap();
    let mut config = Config {
        workspace_roots: vec![workspace],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.bind = "127.0.0.1:0".into();
    // Sign-in without TOTP keeps the tests short; shells still need it.
    config.remote.require_totp = false;
    config.remote.heartbeat_secs = 1;
    config.broker.policy_path = Some(dir.join("policies.toml"));
    change(&mut config);
    Server::start(
        Options {
            verifier: Some(vca_server::security::verifier::FakeVerifier::available()),
            config_file: Some(dir.join("config.toml")),
            control: Some(control_endpoint(dir)),
            data_dir: Some(dir.join("data")),
            process_control: Some(Arc::new(NoProcessControl)),
            restore_backend: None,
        },
        Some(config),
    )
    .await
    .expect("vcad failed to start")
}

async fn local_call(server: &Server, make: impl FnOnce(u64) -> LocalRequest) -> LocalResponse {
    let mut c = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let id = c.next_id();
    c.call(&make(id)).await.unwrap()
}

/// Enrolls TOTP on the server; returns the secret.
async fn enroll_totp(server: &Server) -> Vec<u8> {
    enroll_totp_with_backups(server).await.0
}

/// Enrolls TOTP on the server; returns the secret and the backup codes.
async fn enroll_totp_with_backups(server: &Server) -> (Vec<u8>, Vec<String>) {
    let resp = local_verified(server, |id, step_up| LocalRequest::TotpBegin {
        id,
        step_up,
        account: "owner".into(),
    })
    .await;
    let LocalResponse::TotpPending { secret_base32, .. } = resp else {
        panic!("{resp:?}");
    };
    let secret = data_encoding::BASE32_NOPAD
        .decode(secret_base32.trim_end_matches('=').as_bytes())
        .unwrap();
    let code = vca_net::totp::totp_code_at(&secret, now_secs());
    let resp = local_verified(server, |id, step_up| LocalRequest::TotpConfirm {
        id,
        code: code.clone(),
        current_code: None,
        step_up,
    })
    .await;
    let LocalResponse::TotpEnrolled { backup_codes, .. } = resp else {
        panic!("{resp:?}");
    };
    (secret, backup_codes)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A code of the next TOTP step: newer than the one enrollment used up and
/// still inside the verifier's one-step tolerance.
fn next_code(secret: &[u8]) -> String {
    vca_net::totp::totp_code_at(secret, now_secs() + 30)
}

async fn device(server: &Server, scope: &str, name: &str) -> ClientConnection {
    let resp = local_verified(server, |id, step_up| LocalRequest::Pair {
        id,
        step_up,
        scope: scope.into(),
        endpoints: Vec::new(),
    })
    .await;
    let LocalResponse::Pairing { uri, .. } = resp else {
        panic!("pairing failed: {resp:?}");
    };
    let payload = PairingPayload::parse(&uri).unwrap();
    let config = ClientConfig::from_pairing(&payload, 0, StaticKeypair::generate()).unwrap();
    ClientConnection::connect(
        config,
        ClientAuth::Pair {
            secret: payload.secret.clone(),
            device_name: name.into(),
            platform: "test".into(),
            totp: None,
        },
    )
    .await
    .unwrap()
}

/// Sends a request and returns the reply with the same id; pushes
/// (`id = 0`) are kept in `pushes`.
async fn call(
    conn: &mut ClientConnection,
    id: u64,
    body: ClientMsg,
    pushes: &mut Vec<ServerMsg>,
) -> ServerMsg {
    conn.send(&Envelope { id, body }).await.unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match conn.recv().await {
                Some(Ok(env)) if env.id == id => return env.body,
                Some(Ok(env)) => pushes.push(env.body),
                other => panic!("connection ended: {other:?}"),
            }
        }
    })
    .await
    .expect("no reply")
}

/// Waits for a push matching `pred` (looking at earlier pushes first).
async fn wait_push(
    conn: &mut ClientConnection,
    pushes: &mut Vec<ServerMsg>,
    pred: impl Fn(&ServerMsg) -> bool,
) -> ServerMsg {
    if let Some(i) = pushes.iter().position(&pred) {
        return pushes.remove(i);
    }
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match conn.recv().await {
                Some(Ok(env)) if pred(&env.body) => return env.body,
                Some(Ok(env)) => pushes.push(env.body),
                other => panic!("connection ended: {other:?}"),
            }
        }
    })
    .await
    .expect("push not received")
}

fn is_error(resp: &ServerMsg, expected: ErrorCode) -> bool {
    matches!(resp, ServerMsg::Error { code, .. } if *code == expected)
}

fn open_shell(dir: &Path, totp: &str) -> ClientMsg {
    ClientMsg::OpenShell {
        request: ShellRequest {
            shell: TEST_SHELL.into(),
            project_dir: dir.join("workspace").join("project"),
            display_name: Some("Ops shell".into()),
            totp: totp.into(),
        },
    }
}

fn is_notice(msg: &ServerMsg, which: &str) -> bool {
    matches!(msg, ServerMsg::ShellNotice { event, .. } if event == which)
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_messages_need_admin_scope() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;
    let mut pushes = Vec::new();
    let mut viewer = device(&server, "view", "tablet").await;
    let mut phone = device(&server, "command", "phone").await;
    let mut laptop = device(&server, "admin", "laptop").await;
    let shell_msgs = || {
        vec![
            open_shell(dir.path(), "123456"),
            ClientMsg::ShellRun {
                session_id: "s".into(),
                command: "ls".into(),
                totp: None,
                confirm: false,
            },
            ClientMsg::ListAudit { limit: 10 },
        ]
    };
    for conn in [&mut viewer, &mut phone] {
        for (i, msg) in shell_msgs().into_iter().enumerate() {
            let resp = call(conn, 10 + i as u64, msg, &mut pushes).await;
            assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
        }
    }
    // An admin gets past the scope check: asked without a code, this
    // server (no authenticator enrolled) says so instead of asking for
    // one; then no such session.
    let resp = call(&mut laptop, 1, open_shell(dir.path(), ""), &mut pushes).await;
    assert!(is_error(&resp, ErrorCode::Unsupported), "{resp:?}");
    assert!(
        matches!(&resp, ServerMsg::Error { message, .. } if message.contains("not set up")),
        "{resp:?}"
    );
    let run = shell_msgs().remove(1);
    let resp = call(&mut laptop, 2, run, &mut pushes).await;
    assert!(is_error(&resp, ErrorCode::NotFound), "{resp:?}");
    let resp = call(
        &mut laptop,
        3,
        ClientMsg::ListAudit { limit: 10 },
        &mut pushes,
    )
    .await;
    assert!(matches!(resp, ServerMsg::Audit { .. }), "{resp:?}");
    // Without TOTP enrollment, no shell opens.
    let resp = call(
        &mut laptop,
        4,
        open_shell(dir.path(), "123456"),
        &mut pushes,
    )
    .await;
    assert!(is_error(&resp, ErrorCode::Unsupported), "{resp:?}");
    // Anyone may classify a command.
    let resp = call(
        &mut viewer,
        5,
        ClientMsg::ClassifyCommand {
            command: "sudo rm -rf /".into(),
        },
        &mut pushes,
    )
    .await;
    let ServerMsg::CommandDanger {
        level,
        reasons,
        spans,
        voice_ok,
    } = resp
    else {
        panic!("{resp:?}");
    };
    assert_eq!(level, DangerLevel::Dangerous);
    assert_eq!(reasons, ["elevation", "recursive_delete"]);
    assert_eq!(spans, vec![[0, 11]]);
    assert!(!voice_ok);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_opens_runs_and_closes_a_shell_and_everyone_is_told() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;
    let secret = enroll_totp(&server).await;
    let mut laptop_pushes = Vec::new();
    let mut phone_pushes = Vec::new();
    let mut laptop = device(&server, "admin", "laptop").await;
    let mut phone = device(&server, "command", "phone").await;
    // A local client (the desktop app) gets the notices too.
    let (_local, mut local_rx) = server.api.connect("test");

    let code = next_code(&secret);
    let resp = call(
        &mut laptop,
        1,
        open_shell(dir.path(), &code),
        &mut laptop_pushes,
    )
    .await;
    let ServerMsg::Launched { session } = resp else {
        panic!("{resp:?}");
    };
    assert!(matches!(
        &session.origin,
        SessionOrigin::Local { agent: AgentKind::Shell(name), .. } if name == TEST_SHELL
    ));
    assert_eq!(session.display_name, "Ops shell");
    let shell = session.shell.clone().expect("shell info");
    assert_eq!(shell.opened_by, "laptop");
    assert_eq!(shell.idle_timeout_secs, 15 * 60);
    assert!(shell.hard_deadline > shell.idle_deadline);

    // The replayed code is refused.
    let resp = call(
        &mut laptop,
        2,
        open_shell(dir.path(), &code),
        &mut laptop_pushes,
    )
    .await;
    assert!(is_error(&resp, ErrorCode::TotpInvalid), "{resp:?}");

    // Every client hears about it.
    let notice = wait_push(&mut phone, &mut phone_pushes, |m| is_notice(m, "opened")).await;
    let ServerMsg::ShellNotice {
        session_id,
        device_name,
        ..
    } = notice
    else {
        unreachable!()
    };
    assert_eq!(session_id, session.id);
    assert_eq!(device_name, "laptop");
    let local_notice = tokio::time::timeout(TIMEOUT, async {
        loop {
            let env = local_rx.recv().await.expect("local client open");
            if is_notice(&env.body, "opened") {
                return env.body;
            }
        }
    })
    .await
    .expect("local notice");
    assert!(is_notice(&local_notice, "opened"));

    // A command device may neither watch nor type into the shell.
    let sub = ClientMsg::Subscribe {
        session_id: session.id.clone(),
        from_seq: Some(0),
    };
    let resp = call(&mut phone, 1, sub.clone(), &mut phone_pushes).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    // Every message that reads a session, not only the live stream.
    for read in [
        ClientMsg::History {
            session_id: session.id.clone(),
            before_seq: None,
            limit: 100,
        },
        ClientMsg::Summarize {
            session_id: session.id.clone(),
            language: None,
            number: None,
        },
    ] {
        let resp = call(&mut phone, 1, read, &mut phone_pushes).await;
        assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    }
    let typing = ClientMsg::SendInput {
        session_id: session.id.clone(),
        input: vca_core::Input::Text {
            text: "echo hi".into(),
            submit: true,
            at_prompt: false,
        },
    };
    let resp = call(&mut phone, 2, typing, &mut phone_pushes).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    let restart = ClientMsg::Control {
        session_id: session.id.clone(),
        action: SessionAction::Restart,
    };
    let resp = call(&mut laptop, 3, restart, &mut laptop_pushes).await;
    assert!(is_error(&resp, ErrorCode::Unsupported), "{resp:?}");

    // The admin watches and runs a command.
    assert_eq!(
        call(&mut laptop, 4, sub, &mut laptop_pushes).await,
        ServerMsg::Ok
    );
    let run = ClientMsg::ShellRun {
        session_id: session.id.clone(),
        command: "echo vca-shell-ok".into(),
        totp: None,
        confirm: false,
    };
    let resp = call(&mut laptop, 5, run, &mut laptop_pushes).await;
    assert_eq!(
        resp,
        ServerMsg::ShellRan {
            session_id: session.id.clone(),
            danger: DangerLevel::Safe
        }
    );
    let mut text = String::new();
    for m in &laptop_pushes {
        if let ServerMsg::Event {
            event: SessionEvent::Output { data, .. },
            ..
        } = m
        {
            text.push_str(&String::from_utf8_lossy(data));
        }
    }
    tokio::time::timeout(TIMEOUT, async {
        // The echoed command line also contains the text; wait for the
        // output line after it.
        while text.matches("vca-shell-ok").count() < 2 {
            if let Some(Ok(env)) = laptop.recv().await
                && let ServerMsg::Event {
                    event: SessionEvent::Output { data, .. },
                    ..
                } = env.body
            {
                text.push_str(&String::from_utf8_lossy(&data));
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no output: {text:?}"));

    // Shells are never started with Launch.
    let launch = ClientMsg::Launch {
        request: LaunchRequest {
            agent: AgentKind::Shell(TEST_SHELL.into()),
            project_dir: dir.path().join("workspace").join("project"),
            create_dir: false,
            profile: None,
            args: Vec::new(),
            display_name: None,
            fresh: false,
        },
    };
    let resp = call(&mut laptop, 6, launch, &mut laptop_pushes).await;
    assert!(is_error(&resp, ErrorCode::InvalidRequest), "{resp:?}");

    // Closing from the phone (Stop needs command scope) is announced with
    // the reason.
    let stop = ClientMsg::Control {
        session_id: session.id.clone(),
        action: SessionAction::Stop,
    };
    assert_eq!(
        call(&mut phone, 3, stop, &mut phone_pushes).await,
        ServerMsg::Ok
    );
    let closed = wait_push(&mut laptop, &mut laptop_pushes, |m| is_notice(m, "closed")).await;
    assert!(
        matches!(&closed, ServerMsg::ShellNotice { reason: Some(r), .. } if r == "user"),
        "{closed:?}"
    );

    // The shell audit log, newest first.
    let resp = call(
        &mut laptop,
        7,
        ClientMsg::ListAudit { limit: 0 },
        &mut laptop_pushes,
    )
    .await;
    let ServerMsg::Audit { entries } = resp else {
        panic!("{resp:?}");
    };
    let events: Vec<&str> = entries.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(
        events,
        [
            "shell_close",
            "shell_run",
            "step_up_failed",
            "shell_open",
            "step_up_ok"
        ],
        "{entries:?}"
    );
    assert_eq!(entries[0].device_name, "phone");
    assert_eq!(entries[0].detail, "reason=user");
    assert_eq!(entries[3].device_name, "laptop");
    assert_eq!(entries[3].session_id.as_ref(), Some(&session.id));
    let jsonl = std::fs::read_to_string(dir.path().join("data").join("audit.jsonl")).unwrap();
    assert!(!jsonl.contains(&format!("\"{code}\"")), "{jsonl}");
    assert!(!jsonl.contains(&format!("={code}")), "{jsonl}");
    // The transport's own audit log records the request without the code.
    let net_audit = std::fs::read_to_string(dir.path().join("data/remote/audit.log")).unwrap();
    assert!(net_audit.contains("open_shell"), "{net_audit}");
    assert!(
        !net_audit.contains(&format!("\\\"{code}\\\"")),
        "{net_audit}"
    );

    // The ended card goes at once on a dismiss (command scope), for every
    // client, and the audit log says who did it.
    wait_exited(&server, &session.id).await;
    let dismiss = || ClientMsg::DismissSession {
        session_id: session.id.clone(),
        stop: false,
    };
    assert_eq!(
        call(&mut phone, 4, dismiss(), &mut phone_pushes).await,
        ServerMsg::Ok
    );
    wait_push(
        &mut laptop,
        &mut laptop_pushes,
        |m| matches!(m, ServerMsg::SessionRemoved { session_id } if *session_id == session.id),
    )
    .await;
    let again = call(&mut phone, 5, dismiss(), &mut phone_pushes).await;
    assert!(is_error(&again, ErrorCode::NotFound), "{again:?}");
    let resp = call(
        &mut laptop,
        8,
        ClientMsg::ListAudit { limit: 1 },
        &mut laptop_pushes,
    )
    .await;
    let ServerMsg::Audit { entries } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(entries[0].event, "session_dismiss", "{entries:?}");
    assert_eq!(entries[0].device_name, "phone");
    assert_eq!(entries[0].session_id.as_ref(), Some(&session.id));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn misheard_words_need_admin_to_save_and_are_audited() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_with(dir.path(), |config| {
        config.speech.engine = vca_server::config::SpeechEngineKind::Openai;
        config.speech.language = Some("en".into());
        config.speech.openai.base_url = "http://127.0.0.1:1/v1".into();
        config.speech.openai.api_key_env = None;
    })
    .await;
    let words = vec![vca_core::PersonalTerm {
        term: "Zentorix".into(),
        variants: vec!["zen torix".into()],
    }];
    let save = ClientMsg::SavePersonalTerms {
        terms: words.clone(),
    };
    let mut pushes = Vec::new();

    let mut viewer = device(&server, "view", "viewer").await;
    let hello = ClientMsg::Hello {
        protocol_version: vca_core::PROTOCOL_VERSION,
        app_version: "test".into(),
    };
    let welcome = call(&mut viewer, 1, hello, &mut pushes).await;
    let ServerMsg::Welcome { features, .. } = welcome else {
        panic!("{welcome:?}");
    };
    assert!(
        features
            .iter()
            .any(|f| f == vca_core::FEATURE_PERSONAL_TERMS)
    );
    let list = call(&mut viewer, 2, ClientMsg::ListPersonalTerms, &mut pushes).await;
    assert_eq!(list, ServerMsg::PersonalTerms { terms: Vec::new() });
    let denied = call(&mut viewer, 3, save.clone(), &mut pushes).await;
    assert!(is_error(&denied, ErrorCode::Forbidden), "{denied:?}");

    let mut commander = device(&server, "command", "commander").await;
    let denied = call(&mut commander, 1, save.clone(), &mut pushes).await;
    assert!(is_error(&denied, ErrorCode::Forbidden), "{denied:?}");

    let mut admin = device(&server, "admin", "laptop").await;
    let saved = call(&mut admin, 1, save, &mut pushes).await;
    assert_eq!(
        saved,
        ServerMsg::PersonalTerms {
            terms: words.clone()
        }
    );
    let list = call(&mut viewer, 4, ClientMsg::ListPersonalTerms, &mut pushes).await;
    assert_eq!(list, ServerMsg::PersonalTerms { terms: words });

    // A refused list is not audited.
    let bad = ClientMsg::SavePersonalTerms {
        terms: vec![vca_core::PersonalTerm {
            term: "two\nlines".into(),
            variants: Vec::new(),
        }],
    };
    let refused = call(&mut admin, 2, bad, &mut pushes).await;
    assert!(is_error(&refused, ErrorCode::InvalidRequest), "{refused:?}");

    let audit = call(
        &mut admin,
        3,
        ClientMsg::ListAudit { limit: 5 },
        &mut pushes,
    )
    .await;
    let ServerMsg::Audit { entries } = audit else {
        panic!("{audit:?}");
    };
    assert_eq!(entries[0].event, "personal_terms_save", "{entries:?}");
    assert_eq!(entries[0].device_name, "laptop");
    assert_eq!(entries[0].detail, "terms=1 variants=1");
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.event == "personal_terms_save")
            .count(),
        1
    );
    let jsonl = std::fs::read_to_string(dir.path().join("data").join("audit.jsonl")).unwrap();
    assert!(!jsonl.contains("Zentorix"), "{jsonl}");
    assert!(
        dir.path()
            .join("data")
            .join("speech")
            .join("personal.json")
            .exists()
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_speech_the_misheard_words_are_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;
    let mut pushes = Vec::new();
    let mut admin = device(&server, "admin", "laptop").await;
    let hello = ClientMsg::Hello {
        protocol_version: vca_core::PROTOCOL_VERSION,
        app_version: "test".into(),
    };
    let welcome = call(&mut admin, 1, hello, &mut pushes).await;
    let ServerMsg::Welcome { features, .. } = welcome else {
        panic!("{welcome:?}");
    };
    assert!(
        !features
            .iter()
            .any(|f| f == vca_core::FEATURE_PERSONAL_TERMS)
    );
    let list = call(&mut admin, 2, ClientMsg::ListPersonalTerms, &mut pushes).await;
    assert!(is_error(&list, ErrorCode::Unsupported), "{list:?}");
    let save = ClientMsg::SavePersonalTerms { terms: Vec::new() };
    let save = call(&mut admin, 3, save, &mut pushes).await;
    assert!(is_error(&save, ErrorCode::Unsupported), "{save:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_session_cannot_be_dismissed_and_keep_alive_needs_admin() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;
    let secret = enroll_totp(&server).await;
    let mut pushes = Vec::new();
    let mut laptop = device(&server, "admin", "laptop").await;
    let mut phone = device(&server, "command", "phone").await;
    let resp = call(
        &mut laptop,
        1,
        open_shell(dir.path(), &next_code(&secret)),
        &mut pushes,
    )
    .await;
    let ServerMsg::Launched { session } = resp else {
        panic!("{resp:?}");
    };
    let shell = session.shell.clone().unwrap();
    assert_eq!(shell.shell, TEST_SHELL);
    assert_eq!(
        shell.project_dir,
        dunce_canonical(&dir.path().join("workspace").join("project"))
    );
    let dismiss = ClientMsg::DismissSession {
        session_id: session.id.clone(),
        stop: false,
    };
    let resp = call(&mut phone, 2, dismiss, &mut pushes).await;
    assert!(is_error(&resp, ErrorCode::Conflict), "{resp:?}");
    let keep = || ClientMsg::ShellKeepAlive {
        session_id: session.id.clone(),
    };
    let resp = call(&mut phone, 3, keep(), &mut pushes).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    assert_eq!(
        call(&mut laptop, 4, keep(), &mut pushes).await,
        ServerMsg::Ok
    );
    let stop = ClientMsg::Control {
        session_id: session.id.clone(),
        action: SessionAction::Stop,
    };
    assert_eq!(call(&mut laptop, 5, stop, &mut pushes).await, ServerMsg::Ok);
    wait_exited(&server, &session.id).await;
    server.shutdown().await;
}

/// "Remove from VCA" (`dismiss_session` with `stop`): a command device
/// stops a running session VCA started and removes it in one request; a
/// view-only device may not. Both the transport's audit log and the
/// session audit record it.
#[tokio::test(flavor = "multi_thread")]
async fn remove_from_vca_stops_and_removes_in_one_request() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;
    let secret = enroll_totp(&server).await;
    let mut pushes = Vec::new();
    let mut laptop_pushes = Vec::new();
    let mut laptop = device(&server, "admin", "laptop").await;
    let mut phone = device(&server, "command", "phone").await;
    let mut tablet = device(&server, "view", "tablet").await;
    let welcome = call(
        &mut tablet,
        1,
        ClientMsg::Hello {
            protocol_version: vca_core::PROTOCOL_VERSION,
            app_version: "test".into(),
        },
        &mut pushes,
    )
    .await;
    let ServerMsg::Welcome { features, .. } = welcome else {
        panic!("{welcome:?}");
    };
    assert!(features.iter().any(|f| f == "dismiss_stop"), "{features:?}");
    let resp = call(
        &mut laptop,
        1,
        open_shell(dir.path(), &next_code(&secret)),
        &mut laptop_pushes,
    )
    .await;
    let ServerMsg::Launched { session } = resp else {
        panic!("{resp:?}");
    };
    let remove = || ClientMsg::DismissSession {
        session_id: session.id.clone(),
        stop: true,
    };
    // View only: refused, and the session keeps running.
    let resp = call(&mut tablet, 2, remove(), &mut pushes).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    assert!(server.api.sessions().get(&session.id).is_some());
    // Command: stopped and removed, for every client.
    assert_eq!(
        call(&mut phone, 2, remove(), &mut pushes).await,
        ServerMsg::Ok
    );
    assert!(server.api.sessions().get(&session.id).is_none());
    wait_push(
        &mut laptop,
        &mut laptop_pushes,
        |m| matches!(m, ServerMsg::SessionRemoved { session_id } if *session_id == session.id),
    )
    .await;
    // Nothing announces the removed session again afterwards.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let resp = call(&mut laptop, 3, ClientMsg::ListSessions, &mut laptop_pushes).await;
    let ServerMsg::Sessions { sessions } = resp else {
        panic!("{resp:?}");
    };
    assert!(sessions.iter().all(|s| s.id != session.id), "{sessions:?}");
    let after = laptop_pushes.iter().position(
        |m| matches!(m, ServerMsg::SessionRemoved { session_id } if *session_id == session.id),
    );
    if let Some(at) = after {
        assert!(
            !laptop_pushes[at..].iter().any(
                |m| matches!(m, ServerMsg::SessionUpdated { session: s } if s.id == session.id)
            ),
            "{laptop_pushes:?}"
        );
    }
    let again = call(&mut phone, 4, remove(), &mut pushes).await;
    assert!(is_error(&again, ErrorCode::NotFound), "{again:?}");
    let net_audit = std::fs::read_to_string(dir.path().join("data/remote/audit.log")).unwrap();
    assert!(net_audit.contains("dismiss_session"), "{net_audit}");
    assert!(net_audit.contains("stop"), "{net_audit}");
    let jsonl = std::fs::read_to_string(dir.path().join("data").join("audit.jsonl")).unwrap();
    assert!(jsonl.contains("stopped=true"), "{jsonl}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_kill_switch_stops_remote_shells() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;
    let secret = enroll_totp(&server).await;
    let mut pushes = Vec::new();
    let mut laptop = device(&server, "admin", "laptop").await;
    let (_local, mut local_rx) = server.api.connect("test");
    let resp = call(
        &mut laptop,
        1,
        open_shell(dir.path(), &next_code(&secret)),
        &mut pushes,
    )
    .await;
    let ServerMsg::Launched { session } = resp else {
        panic!("{resp:?}");
    };
    laptop
        .send(&Envelope {
            id: 2,
            body: ClientMsg::KillSwitch,
        })
        .await
        .unwrap();
    let closed = tokio::time::timeout(TIMEOUT, async {
        loop {
            let env = local_rx.recv().await.expect("local client open");
            if is_notice(&env.body, "closed") {
                return env.body;
            }
        }
    })
    .await
    .expect("closed notice");
    assert!(
        matches!(&closed, ServerMsg::ShellNotice { reason: Some(r), session_id, .. }
            if r == "kill_switch" && *session_id == session.id),
        "{closed:?}"
    );
    wait_exited(&server, &session.id).await;
    // New shells are refused while the switch is on (local client: the
    // remote ones were dropped).
    let (local, mut rx) = server.api.connect("test");
    server
        .api
        .handle(
            &local,
            Envelope {
                id: 9,
                body: open_shell(dir.path(), &next_code(&secret)),
            },
        )
        .await;
    let reply = tokio::time::timeout(TIMEOUT, async {
        loop {
            let env = rx.recv().await.expect("local client open");
            if env.id == 9 {
                return env.body;
            }
        }
    })
    .await
    .expect("reply");
    assert!(is_error(&reply, ErrorCode::Forbidden), "{reply:?}");
    let audit = std::fs::read_to_string(dir.path().join("data").join("audit.jsonl")).unwrap();
    assert!(audit.contains("reason=kill_switch"), "{audit}");
    server.shutdown().await;
}

fn is_join_required(resp: &ServerMsg) -> bool {
    matches!(resp, ServerMsg::Error { code: ErrorCode::TotpRequired, message }
        if message.contains("join"))
}

#[tokio::test(flavor = "multi_thread")]
async fn another_admin_device_joins_with_a_code_and_multi_line_input_is_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;
    let (secret, backups) = enroll_totp_with_backups(&server).await;
    let mut laptop_pushes = Vec::new();
    let mut desk_pushes = Vec::new();
    let mut laptop = device(&server, "admin", "laptop").await;
    let mut desk = device(&server, "admin", "desk").await;

    let resp = call(
        &mut laptop,
        1,
        open_shell(dir.path(), &next_code(&secret)),
        &mut laptop_pushes,
    )
    .await;
    let ServerMsg::Launched { session } = resp else {
        panic!("{resp:?}");
    };
    let sid = session.id.clone();

    // Another admin device neither watches nor types before joining.
    let sub = ClientMsg::Subscribe {
        session_id: sid.clone(),
        from_seq: Some(0),
    };
    let resp = call(&mut desk, 1, sub.clone(), &mut desk_pushes).await;
    assert!(is_join_required(&resp), "{resp:?}");
    let history = ClientMsg::History {
        session_id: sid.clone(),
        before_seq: None,
        limit: 100,
    };
    let resp = call(&mut desk, 2, history, &mut desk_pushes).await;
    assert!(is_join_required(&resp), "{resp:?}");
    let run = |command: &str, confirm: bool| ClientMsg::ShellRun {
        session_id: sid.clone(),
        command: command.into(),
        totp: None,
        confirm,
    };
    let resp = call(&mut desk, 3, run("echo hi", false), &mut desk_pushes).await;
    assert!(is_join_required(&resp), "{resp:?}");
    let typing = |text: &str| ClientMsg::SendInput {
        session_id: sid.clone(),
        input: vca_core::Input::Text {
            text: text.into(),
            submit: true,
            at_prompt: true,
        },
    };
    let resp = call(&mut desk, 4, typing("echo hi"), &mut desk_pushes).await;
    assert!(is_join_required(&resp), "{resp:?}");

    // Joining with a wrong code fails; a good code (here a backup code)
    // grants the shell to this device, and everyone is told.
    let join = |totp: &str| ClientMsg::ShellJoin {
        session_id: sid.clone(),
        totp: totp.into(),
    };
    let resp = call(&mut desk, 5, join("000000"), &mut desk_pushes).await;
    assert!(is_error(&resp, ErrorCode::TotpInvalid), "{resp:?}");
    let resp = call(&mut desk, 6, join(&backups[0]), &mut desk_pushes).await;
    assert_eq!(resp, ServerMsg::Ok);
    let joined = wait_push(&mut laptop, &mut laptop_pushes, |m| is_notice(m, "joined")).await;
    assert!(
        matches!(&joined, ServerMsg::ShellNotice { device_name, .. } if device_name == "desk"),
        "{joined:?}"
    );
    assert_eq!(
        call(&mut desk, 7, sub, &mut desk_pushes).await,
        ServerMsg::Ok
    );
    assert_eq!(
        call(&mut desk, 8, run("echo hi", false), &mut desk_pushes).await,
        ServerMsg::ShellRan {
            session_id: sid.clone(),
            danger: DangerLevel::Safe
        }
    );

    // Two lines are not typed without a confirmation, whichever way they
    // come; the answer carries the full text.
    let script = "echo vca-a\necho vca-b";
    let ask = ServerMsg::ShellConfirm {
        session_id: sid.clone(),
        command: script.into(),
        lines: 2,
    };
    assert_eq!(
        call(&mut laptop, 2, run(script, false), &mut laptop_pushes).await,
        ask
    );
    assert_eq!(
        call(&mut desk, 9, typing(script), &mut desk_pushes).await,
        ask
    );
    // A single command with its Enter is typed as usual.
    let resp = call(&mut laptop, 3, typing("echo one\n"), &mut laptop_pushes).await;
    assert!(matches!(resp, ServerMsg::InputAck { .. }), "{resp:?}");
    assert_eq!(
        call(&mut laptop, 4, run(script, true), &mut laptop_pushes).await,
        ServerMsg::ShellRan {
            session_id: sid.clone(),
            danger: DangerLevel::Safe
        }
    );

    let resp = call(
        &mut laptop,
        5,
        ClientMsg::ListAudit { limit: 0 },
        &mut laptop_pushes,
    )
    .await;
    let ServerMsg::Audit { entries } = resp else {
        panic!("{resp:?}");
    };
    let join_entry = entries
        .iter()
        .find(|e| e.event == "shell_join")
        .expect("join audited");
    assert_eq!(join_entry.device_name, "desk");
    assert_eq!(join_entry.session_id.as_ref(), Some(&sid));
    assert!(
        entries
            .iter()
            .any(|e| e.event == "shell_run" && e.detail.contains("lines=2")),
        "{entries:?}"
    );
    let jsonl = std::fs::read_to_string(dir.path().join("data").join("audit.jsonl")).unwrap();
    assert!(!jsonl.contains(&backups[0]), "{jsonl}");
    // The transport's audit log records the join without the code.
    let net_audit = std::fs::read_to_string(dir.path().join("data/remote/audit.log")).unwrap();
    assert!(net_audit.contains("shell_join"), "{net_audit}");
    assert!(!net_audit.contains(&backups[0]), "{net_audit}");

    let stop = ClientMsg::Control {
        session_id: sid.clone(),
        action: SessionAction::Stop,
    };
    assert_eq!(
        call(&mut laptop, 6, stop, &mut laptop_pushes).await,
        ServerMsg::Ok
    );
    wait_exited(&server, &sid).await;
    server.shutdown().await;
}

async fn wait_exited(server: &Server, id: &SessionId) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let exited =
                server.api.sessions().get(id).is_none_or(|s| {
                    matches!(s.info().state, vca_core::SessionState::Exited { .. })
                });
            if exited {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the shell process ends");
}

/// Runs a gated request through the whole flow: the challenge, local
/// verification through the fake verifier of this test server, the retry.
async fn local_verified(
    server: &vca_server::daemon::Server,
    make: impl Fn(u64, Option<vca_server::security::Sensitive>) -> LocalRequest,
) -> LocalResponse {
    let first = local_call(server, |id| make(id, None)).await;
    let LocalResponse::StepUpRequired { challenge, .. } = first else {
        return first;
    };
    let token = test_step_up(server, challenge).await;
    local_call(server, |id| make(id, token)).await
}

/// Local verification through the fake verifier of this test server.
async fn test_step_up(
    server: &vca_server::daemon::Server,
    challenge: vca_server::security::StepUpChallenge,
) -> Option<vca_server::security::Sensitive> {
    let call = vca_server::security::SecurityCall::StepUp {
        nonce: vca_server::security::Sensitive(challenge.nonce),
        window: None,
        totp: None,
        presence: None,
        language: None,
    };
    // As the process that got the challenge: a step-up bound to a process
    // never answers an unnamed caller.
    let me = vca_server::security::Caller {
        pid: Some(std::process::id()),
        ..Default::default()
    };
    match server
        .api
        .security()
        .expect("security")
        .handle_from(call, &me)
        .await
    {
        vca_server::security::SecurityReply::StepUp { token, .. } => Some(token),
        other => panic!("step-up failed: {other:?}"),
    }
}

/// Canonical path without the `\\?\` prefix Windows adds, so that it can
/// be compared with paths the server returns (Windows temp folders may use
/// short 8.3 names such as `RUNNER~1`).
fn dunce_canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap();
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    }
}
