//! End-to-end tests of session restore over real Noise WebSocket
//! connections: scope, TOTP step-up, audit and profile editing, on a fake
//! desktop. No window is opened, moved or read.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ArrangeCell, ArrangeEdit, ArrangeMonitorPlan, ClientMsg, Envelope, RestoreEdit,
    RestoreEntryDraft, RestoreItemStatus, RestoreJob, RestoreMonitor, RestoreRunRequest, ServerMsg,
    WindowRect,
};
use vca_net::{ClientAuth, ClientConfig, ClientConnection, PairingPayload, StaticKeypair};
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse, RestoreLocalAction};
use vca_server::daemon::{Options, Server};
use vca_server::restore::{FakeBackend, FakeCall, LiveWindow};

const TIMEOUT: Duration = Duration::from_secs(30);

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
            r"\\.\pipe\vca-test-{}-restore-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join("control.sock")
    }
}

async fn start(dir: &Path) -> (Server, Arc<FakeBackend>) {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(workspace.join("project")).unwrap();
    std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
    let mut config = Config {
        workspace_roots: vec![workspace],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.bind = "127.0.0.1:0".into();
    config.remote.require_totp = false;
    config.remote.heartbeat_secs = 20;
    config.broker.policy_path = Some(dir.join("policies.toml"));
    let backend = FakeBackend::new();
    let server = Server::start(
        Options {
            verifier: Some(vca_server::security::verifier::FakeVerifier::available()),
            config_file: Some(dir.join("config.toml")),
            control: Some(control_endpoint(dir)),
            data_dir: Some(dir.join("data")),
            process_control: Some(Arc::new(NoProcessControl)),
            restore_backend: Some(backend.clone()),
        },
        Some(config),
    )
    .await
    .expect("vcad failed to start");
    (server, backend)
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
    assert!(
        matches!(resp, LocalResponse::TotpEnrolled { .. }),
        "{resp:?}"
    );
    secret
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A code for the next time step: the enrollment used the current one and
/// codes are single use.
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
        panic!("{resp:?}");
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

/// Sends a request and returns the reply with the same id.
async fn call(conn: &mut ClientConnection, id: u64, body: ClientMsg) -> ServerMsg {
    conn.send(&Envelope { id, body }).await.unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match conn.recv().await {
                Some(Ok(env)) if env.id == id => return env.body,
                Some(Ok(_)) => {}
                other => panic!("connection ended: {other:?}"),
            }
        }
    })
    .await
    .expect("no reply")
}

fn is_error(resp: &ServerMsg, expected: ErrorCode) -> bool {
    matches!(resp, ServerMsg::Error { code, .. } if *code == expected)
}

fn add_window(profile: &str, title: &str, folder: &Path) -> ClientMsg {
    ClientMsg::RestoreEdit {
        edit: RestoreEdit::EntryAdd {
            profile: profile.into(),
            entry: RestoreEntryDraft {
                title: title.into(),
                agent: AgentKind::ClaudeCode,
                folder: folder.to_owned(),
                session_id: None,
                rect: Some(WindowRect {
                    x: -100,
                    y: 10,
                    w: 800,
                    h: 600,
                }),
                maximized: false,
                enabled: true,
            },
        },
    }
}

fn run_with(totp: Option<String>) -> ClientMsg {
    ClientMsg::RestoreRun {
        request: RestoreRunRequest {
            totp,
            ..RestoreRunRequest::default()
        },
    }
}

/// Asks until the current run is done; returns it.
async fn wait_finished(conn: &mut ClientConnection) -> RestoreJob {
    tokio::time::timeout(TIMEOUT, async {
        let mut id = 1000;
        loop {
            id += 1;
            if let ServerMsg::RestoreList { overview } =
                call(conn, id, ClientMsg::RestoreList).await
            {
                if let Some(job) = overview.job.filter(|j| j.finished_at.is_some()) {
                    return job;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the run did not finish")
}

#[tokio::test(flavor = "multi_thread")]
async fn scope_gates_viewing_editing_and_running() {
    let dir = tempfile::tempdir().unwrap();
    let (server, backend) = start(dir.path()).await;
    let mut view = device(&server, "view", "viewer").await;
    let mut command = device(&server, "command", "commander").await;
    let mut admin = device(&server, "admin", "owner phone").await;
    let project = dir.path().join("workspace").join("project");

    // Everyone may look.
    let list = call(&mut view, 1, ClientMsg::RestoreList).await;
    assert!(matches!(list, ServerMsg::RestoreList { .. }), "{list:?}");

    let edit = add_window("default", "API", &project);
    for conn in [&mut view, &mut command] {
        for msg in [
            edit.clone(),
            run_with(None),
            ClientMsg::RestoreCapture { profile: None },
            ClientMsg::RestoreImport {
                layout: "{}".into(),
                profile: None,
            },
        ] {
            let resp = call(conn, 2, msg).await;
            assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
        }
    }
    assert!(backend.calls().is_empty());

    // Admins edit without a code.
    let resp = call(&mut admin, 3, edit).await;
    let ServerMsg::RestoreList { overview } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(overview.profiles[0].entries[0].title, "API");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_run_needs_a_fresh_code_and_is_audited() {
    let dir = tempfile::tempdir().unwrap();
    let (server, backend) = start(dir.path()).await;
    let secret = enroll_totp(&server).await;
    let mut admin = device(&server, "admin", "owner phone").await;
    let project = dir.path().join("workspace").join("project");
    let outside = dir.path().join("elsewhere");
    call(&mut admin, 1, add_window("default", "Inside", &project)).await;
    call(&mut admin, 2, add_window("default", "Outside", &outside)).await;
    let resp = call(
        &mut admin,
        3,
        ClientMsg::RestoreEdit {
            edit: RestoreEdit::ProfileSettings {
                profile: "default".into(),
                delay_secs: 0,
                auto_prompt: None,
            },
        },
    )
    .await;
    assert!(matches!(resp, ServerMsg::RestoreList { .. }), "{resp:?}");

    // A dry run needs no code and opens nothing.
    let plan = call(
        &mut admin,
        4,
        ClientMsg::RestoreRun {
            request: RestoreRunRequest {
                dry_run: true,
                ..RestoreRunRequest::default()
            },
        },
    )
    .await;
    let ServerMsg::RestoreProgress { job } = plan else {
        panic!("{plan:?}");
    };
    assert!(job.dry_run);
    assert_eq!(job.items[1].status, RestoreItemStatus::SkippedOutside);
    assert!(backend.calls().is_empty());

    let resp = call(&mut admin, 5, run_with(None)).await;
    assert!(is_error(&resp, ErrorCode::TotpRequired), "{resp:?}");
    let resp = call(&mut admin, 6, run_with(Some("000000".into()))).await;
    assert!(is_error(&resp, ErrorCode::TotpInvalid), "{resp:?}");
    assert!(backend.calls().is_empty());

    let resp = call(&mut admin, 7, run_with(Some(next_code(&secret)))).await;
    assert!(
        matches!(resp, ServerMsg::RestoreProgress { .. }),
        "{resp:?}"
    );
    let job = wait_finished(&mut admin).await;
    assert_eq!(job.by, "owner phone");
    assert_eq!(job.items[0].status, RestoreItemStatus::Placed);
    assert_eq!(job.items[1].status, RestoreItemStatus::SkippedOutside);
    assert_eq!(backend.opened_titles(), ["Inside"]);

    // Inside the step-up window the next run needs no code. The window is
    // open now, so nothing new opens.
    let resp = call(&mut admin, 8, run_with(None)).await;
    assert!(
        matches!(resp, ServerMsg::RestoreProgress { .. }),
        "{resp:?}"
    );
    wait_finished(&mut admin).await;
    assert_eq!(backend.opened_titles(), ["Inside"]);

    let resp = call(&mut admin, 9, ClientMsg::ListAudit { limit: 100 }).await;
    let ServerMsg::Audit { entries } = resp else {
        panic!("{resp:?}");
    };
    let events: Vec<&str> = entries.iter().map(|e| e.event.as_str()).collect();
    assert!(events.contains(&"restore_run"), "{events:?}");
    assert!(events.contains(&"restore_edit"), "{events:?}");
    assert!(events.contains(&"step_up_ok"), "{events:?}");
    assert!(events.contains(&"step_up_failed"), "{events:?}");
    let run = entries.iter().find(|e| e.event == "restore_run").unwrap();
    assert_eq!(run.device_name, "owner phone");
    assert!(run.detail.contains("remote=true"), "{}", run.detail);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_on_the_computer_needs_no_code() {
    let dir = tempfile::tempdir().unwrap();
    let (server, backend) = start(dir.path()).await;
    let outside = dir.path().join("elsewhere");
    let mut local = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let resp = local
        .api(add_window("default", "Mine", &outside))
        .await
        .unwrap();
    assert!(matches!(resp, ServerMsg::RestoreList { .. }), "{resp:?}");
    let resp = local.api(run_with(None)).await.unwrap();
    assert!(
        matches!(resp, ServerMsg::RestoreProgress { .. }),
        "{resp:?}"
    );
    tokio::time::timeout(TIMEOUT, async {
        while backend.opened_titles().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("nothing opened");
    assert_eq!(backend.opened_titles(), ["Mine"]);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_message_after_restore_cannot_be_set_remotely() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _) = start(dir.path()).await;
    let mut admin = device(&server, "admin", "owner phone").await;
    let set = |prompt: &str| ClientMsg::RestoreEdit {
        edit: RestoreEdit::ProfileSettings {
            profile: "default".into(),
            delay_secs: 6,
            auto_prompt: Some(prompt.into()),
        },
    };
    let resp = call(&mut admin, 1, set("start working")).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    let mut local = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let resp = local.api(set("start working")).await.unwrap();
    let ServerMsg::RestoreList { overview } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(
        overview.profiles[0].auto_prompt.as_deref(),
        Some("start working")
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_are_managed_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _) = start(dir.path()).await;
    let mut admin = device(&server, "admin", "owner phone").await;
    let project = dir.path().join("workspace").join("project");
    let resp = call(
        &mut admin,
        1,
        ClientMsg::RestoreEdit {
            edit: RestoreEdit::ProfileAdd {
                name: "Home".into(),
            },
        },
    )
    .await;
    let ServerMsg::RestoreList { overview } = resp else {
        panic!("{resp:?}");
    };
    let home = overview.profiles[1].id.clone();
    let resp = call(&mut admin, 2, add_window(&home, "Evening", &project)).await;
    let ServerMsg::RestoreList { overview } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(overview.profiles[1].entries.len(), 1);
    assert!(overview.profiles[0].entries.is_empty());
    let entry = overview.profiles[1].entries[0].id.clone();
    let resp = call(
        &mut admin,
        3,
        ClientMsg::RestoreEdit {
            edit: RestoreEdit::EntryDelete {
                profile: home.clone(),
                entry_id: entry,
            },
        },
    )
    .await;
    assert!(matches!(resp, ServerMsg::RestoreList { .. }), "{resp:?}");
    let resp = call(
        &mut admin,
        4,
        ClientMsg::RestoreEdit {
            edit: RestoreEdit::ProfileDelete { profile: home },
        },
    )
    .await;
    let ServerMsg::RestoreList { overview } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(overview.profiles.len(), 1);
    // A generic program cannot be added.
    let mut bad = add_window("default", "evil", &project);
    if let ClientMsg::RestoreEdit {
        edit: RestoreEdit::EntryAdd { entry, .. },
    } = &mut bad
    {
        entry.agent = AgentKind::Generic("powershell".into());
    }
    let resp = call(&mut admin, 5, bad).await;
    assert!(is_error(&resp, ErrorCode::InvalidRequest), "{resp:?}");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bringing_a_terminal_forward_is_refused_from_a_paired_device() {
    let dir = tempfile::tempdir().unwrap();
    let (server, backend) = start(dir.path()).await;
    let focus = || ClientMsg::FocusTerminal {
        session_id: "vca-anything".into(),
    };
    // Even an admin phone cannot move the focus on this computer.
    let mut admin = device(&server, "admin", "owner phone").await;
    let resp = call(&mut admin, 1, focus()).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    let mut viewer = device(&server, "view", "tablet").await;
    let resp = call(&mut viewer, 1, focus()).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    // The owner's own app gets an answer: no such session runs here.
    let mut local = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let resp = local.api(focus()).await.unwrap();
    assert!(is_error(&resp, ErrorCode::NotFound), "{resp:?}");
    // Nothing was focused, opened or moved.
    assert!(backend.calls().is_empty());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn arranging_windows_needs_admin_and_is_audited() {
    let dir = tempfile::tempdir().unwrap();
    let (server, backend) = start(dir.path()).await;
    let secret = enroll_totp(&server).await;
    let rect = WindowRect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };
    backend.set_monitors(vec![RestoreMonitor {
        id: "A".into(),
        rect,
        work: WindowRect { h: 1040, ..rect },
        dpi: 96,
    }]);
    backend.set_windows(vec![LiveWindow {
        id: 7,
        title: "API".into(),
        rect,
        maximized: false,
        minimized: false,
    }]);
    let plan = || {
        vec![ArrangeMonitorPlan {
            monitor: "A".into(),
            rows: 1,
            cols: 2,
            cells: vec![
                ArrangeCell {
                    cell: 1,
                    window: "7".into(),
                },
                ArrangeCell {
                    cell: 0,
                    window: "99".into(),
                },
            ],
        }]
    };
    let mut view = device(&server, "view", "viewer").await;
    let mut command = device(&server, "command", "commander").await;
    let mut admin = device(&server, "admin", "owner phone").await;

    // Everyone may look. Window 7 hosts no AI session, so it is not offered,
    // yet an arrangement naming it still moves it, and undoes it.
    let resp = call(&mut view, 1, ClientMsg::ArrangeList).await;
    let ServerMsg::ArrangeList { overview } = resp else {
        panic!("{resp:?}");
    };
    assert!(overview.windows.is_empty());
    assert_eq!(overview.monitors.len(), 1);

    for conn in [&mut view, &mut command] {
        for msg in [
            ClientMsg::ArrangeApply {
                monitors: plan(),
                totp: None,
            },
            ClientMsg::ArrangeApplyLayout {
                layout: "any".into(),
                totp: None,
            },
            ClientMsg::ArrangeUndo { totp: None },
            ClientMsg::ArrangeEdit {
                edit: ArrangeEdit::LayoutDelete {
                    layout: "any".into(),
                },
            },
        ] {
            let resp = call(conn, 2, msg).await;
            assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
        }
    }
    assert!(backend.calls().is_empty());

    // Moving windows from a paired admin device needs a code, like a
    // restore run; without one nothing moves.
    let apply = |totp: Option<String>| ClientMsg::ArrangeApply {
        monitors: plan(),
        totp,
    };
    let resp = call(&mut admin, 3, apply(None)).await;
    assert!(is_error(&resp, ErrorCode::TotpRequired), "{resp:?}");
    let resp = call(&mut admin, 3, apply(Some("000000".into()))).await;
    assert!(is_error(&resp, ErrorCode::TotpInvalid), "{resp:?}");
    for msg in [
        ClientMsg::ArrangeApplyLayout {
            layout: "any".into(),
            totp: None,
        },
        ClientMsg::ArrangeUndo { totp: None },
    ] {
        let resp = call(&mut admin, 3, msg).await;
        assert!(is_error(&resp, ErrorCode::TotpRequired), "{resp:?}");
    }
    assert!(backend.calls().is_empty());

    let resp = call(&mut admin, 3, apply(Some(next_code(&secret)))).await;
    let ServerMsg::ArrangeDone { result, overview } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!((result.moved, result.skipped), (1, 1));
    assert_eq!(overview.undo_windows, 1);
    assert_eq!(
        backend.calls(),
        [FakeCall::Place {
            id: 7,
            rect: WindowRect {
                x: 960,
                y: 0,
                w: 960,
                h: 1040,
            },
            maximized: false,
        }]
    );
    // Inside the step-up window the undo needs no new code.
    let resp = call(&mut admin, 4, ClientMsg::ArrangeUndo { totp: None }).await;
    let ServerMsg::ArrangeDone { result, overview } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(result.moved, 1);
    assert_eq!(overview.undo_windows, 0);
    assert!(overview.windows.is_empty());

    let resp = call(&mut admin, 5, ClientMsg::ListAudit { limit: 100 }).await;
    let ServerMsg::Audit { entries } = resp else {
        panic!("{resp:?}");
    };
    let arranged = entries
        .iter()
        .find(|e| e.event == "window_arrange" && e.detail.starts_with("op=apply"))
        .unwrap();
    assert_eq!(arranged.device_name, "owner phone");
    assert!(
        arranged
            .detail
            .contains("moved=1 skipped=1 failed=0 remote=true"),
        "{}",
        arranged.detail
    );
    let events: Vec<&str> = entries.iter().map(|e| e.event.as_str()).collect();
    assert!(events.contains(&"step_up_ok"), "{events:?}");
    assert!(events.contains(&"step_up_failed"), "{events:?}");
    let ok = entries.iter().find(|e| e.event == "step_up_ok").unwrap();
    assert!(
        ok.detail.contains("purpose=window_arrange"),
        "{}",
        ok.detail
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_on_the_computer_arranges_without_a_code() {
    let dir = tempfile::tempdir().unwrap();
    let (server, backend) = start(dir.path()).await;
    // A code exists, yet the owner's own app needs none.
    enroll_totp(&server).await;
    let rect = WindowRect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };
    backend.set_monitors(vec![RestoreMonitor {
        id: "A".into(),
        rect,
        work: rect,
        dpi: 96,
    }]);
    backend.set_windows(vec![LiveWindow {
        id: 7,
        title: "API".into(),
        rect,
        maximized: false,
        minimized: false,
    }]);
    let mut local = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let resp = local
        .api(ClientMsg::ArrangeApply {
            monitors: vec![ArrangeMonitorPlan {
                monitor: "A".into(),
                rows: 1,
                cols: 2,
                cells: vec![ArrangeCell {
                    cell: 0,
                    window: "7".into(),
                }],
            }],
            totp: None,
        })
        .await
        .unwrap();
    let ServerMsg::ArrangeDone { result, .. } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(result.moved, 1);
    let resp = local
        .api(ClientMsg::ArrangeUndo { totp: None })
        .await
        .unwrap();
    assert!(matches!(resp, ServerMsg::ArrangeDone { .. }), "{resp:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shortcuts_are_only_on_the_local_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _) = start(dir.path()).await;
    // Reading the state changes nothing and creates no shortcut.
    let resp = local_call(&server, |id| LocalRequest::RestoreLocal {
        id,
        action: RestoreLocalAction::Status,
    })
    .await;
    let LocalResponse::RestoreLocal {
        sign_in_delay_secs, ..
    } = resp
    else {
        panic!("{resp:?}");
    };
    assert_eq!(sign_in_delay_secs, 60);
    server.shutdown().await;
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
