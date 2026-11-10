//! End-to-end test of vcad over the local control channel, driving
//! `vca-fake-agent` sessions (handoff §6).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use vca_adapters::HookDecision;
use vca_adapters::claude::hook_event::ClaudeHookPayload;
use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ClientMsg, DeliveryStatus, Input, LaunchRequest, PROTOCOL_VERSION, ServerMsg,
    SessionAction, SessionEvent, SessionId, SessionState,
};
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse, RunRequest};
use vca_server::daemon::{Options, Server};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Process control that never touches a real process. The end-to-end tests
/// run on developer machines with other work in progress, so the broker
/// must not be able to suspend or terminate anything there.
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
        panic!("the end-to-end tests must never terminate processes through the broker ({root})")
    }
}

fn no_process_control() -> Option<Arc<dyn ProcessControl>> {
    Some(Arc::new(NoProcessControl))
}

fn fake_agent() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir.parent().and_then(Path::parent).unwrap();
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "vca-fake-agent"])
            .current_dir(workspace)
            .status()
            .expect("failed to run cargo build for vca-fake-agent");
        assert!(status.success(), "building vca-fake-agent failed");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target"));
        target
            .join("debug")
            .join(format!("vca-fake-agent{}", std::env::consts::EXE_SUFFIX))
    })
    .clone()
}

fn control_endpoint(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\vca-test-{}-{name}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join(format!("{name}.sock"))
    }
}

/// A server with temporary directories, no discovery and a broker policy
/// with one heavy slot and a short queue timeout.
async fn start_server(dir: &Path, name: &str) -> Server {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let policy = dir.join("policies.toml");
    std::fs::write(
        &policy,
        "[admission]\nmax_heavy_jobs = 1\nqueue_timeout_secs = 1\n\
         cpu_threshold_percent = 100\nmemory_headroom_percent = 1\ndeny_memory_percent = 100\n",
    )
    .unwrap();
    let mut config = Config {
        workspace_roots: vec![workspace],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.bind = "127.0.0.1:0".into();
    config.sessions.heuristics_interval_ms = 100;
    config.sessions.exited_retention_secs = 60;
    config.sessions.allow_generic_launch = true;
    config.broker.policy_path = Some(policy);
    config.shutdown.grace_secs = 2;
    Server::start(
        Options {
            config_file: Some(dir.join("config.toml")),
            control: Some(control_endpoint(dir, name)),
            data_dir: Some(dir.join("data")),
            process_control: no_process_control(),
            restore_backend: None,
            verifier: Some(vca_server::security::verifier::FakeVerifier::available()),
        },
        Some(config),
    )
    .await
    .expect("vcad failed to start")
}

async fn connect(server: &Server) -> ControlClient {
    let mut client = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let welcome = client
        .api(ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: "test".into(),
        })
        .await
        .unwrap();
    assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");
    client
}

/// Reads pushes until `pred` returns a value.
async fn wait_push<T>(
    client: &mut ControlClient,
    what: &str,
    mut pred: impl FnMut(&ServerMsg) -> Option<T>,
) -> T {
    let result = tokio::time::timeout(TIMEOUT, async {
        loop {
            match client.recv().await.unwrap() {
                Some(LocalResponse::Api { body, .. }) => {
                    if let Some(v) = pred(&body) {
                        return v;
                    }
                }
                Some(_) => {}
                None => panic!("vcad closed the connection while waiting for {what}"),
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Collects session output until it contains `needle`.
async fn wait_output(client: &mut ControlClient, sid: &SessionId, needle: &str) -> String {
    let mut text = String::new();
    wait_push(client, needle, |msg| {
        if let ServerMsg::Event {
            session_id,
            event: SessionEvent::Output { data, .. },
        } = msg
        {
            if session_id == sid {
                text.push_str(&String::from_utf8_lossy(data));
            }
        }
        text.contains(needle).then_some(())
    })
    .await;
    text
}

fn hook_event(json: serde_json::Value) -> Box<vca_adapters::AgentHookEvent> {
    Box::new(
        ClaudeHookPayload::parse(&json.to_string())
            .unwrap()
            .to_agent_event(),
    )
}

async fn hook(
    server: &Server,
    client: &mut ControlClient,
    session_id: Option<SessionId>,
    json: serde_json::Value,
) -> HookDecision {
    let id = client.next_id();
    // What `vca hook` reads from the session's environment.
    let hook_token = session_id
        .as_ref()
        .and_then(|sid| server.api.sessions().hook_token(sid))
        .map(vca_server::security::Sensitive);
    let resp = tokio::time::timeout(
        TIMEOUT,
        client.call(&LocalRequest::Hook {
            id,
            session_id,
            hook_token,
            event: hook_event(json),
        }),
    )
    .await
    .expect("hook timed out")
    .unwrap();
    match resp {
        LocalResponse::Hook { decision, .. } => decision,
        other => panic!("unexpected hook response {other:?}"),
    }
}

fn pre_tool_use(session: &str, command: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "tool_use_id": "t1",
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_session_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "e2e").await;
    let mut client = connect(&server).await;
    let workspace = dunce_canonical(&dir.path().join("workspace"));

    // Launch through the API.
    let launched = client
        .api(ClientMsg::Launch {
            request: LaunchRequest {
                agent: AgentKind::Generic(fake_agent().to_string_lossy().into_owned()),
                project_dir: workspace.join("project"),
                create_dir: true,
                profile: None,
                args: Vec::new(),
                display_name: Some("fake".into()),
                fresh: false,
            },
        })
        .await
        .unwrap();
    let ServerMsg::Launched { session } = launched else {
        panic!("launch failed: {launched:?}");
    };
    let sid = session.id.clone();
    assert!(workspace.join("project").is_dir());

    let sub = client
        .api(ClientMsg::Subscribe {
            session_id: sid.clone(),
            from_seq: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(sub, ServerMsg::Ok);
    wait_output(&mut client, &sid, "> ").await;

    // Input without focus, with delivery confirmation.
    let ack = client
        .api(ClientMsg::SendInput {
            session_id: sid.clone(),
            input: Input::Text {
                text: "hello vcad".into(),
                submit: true,
                at_prompt: false,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        ack,
        ServerMsg::InputAck {
            session_id: sid.clone(),
            status: DeliveryStatus::Queued
        }
    );
    wait_push(&mut client, "delivered ack", |m| {
        matches!(
            m,
            ServerMsg::InputAck { status: DeliveryStatus::Delivered, session_id } if *session_id == sid
        )
        .then_some(())
    })
    .await;
    wait_output(&mut client, &sid, "Echo: hello vcad").await;

    // A permission prompt becomes a decision that can be answered.
    client
        .api(ClientMsg::SendInput {
            session_id: sid.clone(),
            input: Input::Text {
                text: "please run tests".into(),
                submit: true,
                at_prompt: false,
            },
        })
        .await
        .unwrap();
    let decision = wait_push(&mut client, "decision", |m| match m {
        ServerMsg::Event {
            session_id,
            event: SessionEvent::DecisionRequested { decision, .. },
        } if *session_id == sid => Some(decision.clone()),
        _ => None,
    })
    .await;
    let yes = decision
        .options
        .iter()
        .find(|o| o.id.eq_ignore_ascii_case("y") || o.id.eq_ignore_ascii_case("yes"))
        .unwrap_or_else(|| panic!("no yes option in {decision:?}"))
        .id
        .clone();
    let ack = client
        .api(ClientMsg::SendInput {
            session_id: sid.clone(),
            input: Input::Choose {
                decision_id: decision.id.clone(),
                option_id: yes,
            },
        })
        .await
        .unwrap();
    assert!(matches!(ack, ServerMsg::InputAck { .. }), "{ack:?}");
    wait_output(&mut client, &sid, "Running `cargo test`... ok").await;

    // Hooks: admission control for heavy commands. The first build gets the
    // only slot; a second one from another agent session waits and is
    // denied after the queue timeout; PostToolUse frees the slot.
    let mut hooks = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let first = hook(
        &server,
        &mut hooks,
        Some(sid.clone()),
        pre_tool_use("a1", "cargo build"),
    )
    .await;
    assert_eq!(first, HookDecision::NoOpinion);
    let light = hook(&server, &mut hooks, None, pre_tool_use("b1", "ls -la")).await;
    assert_eq!(light, HookDecision::NoOpinion);
    let second = hook(
        &server,
        &mut hooks,
        None,
        pre_tool_use("b1", "cargo build --release"),
    )
    .await;
    assert!(matches!(second, HookDecision::Deny { .. }), "{second:?}");
    let post = serde_json::json!({
        "session_id": "a1",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "cargo build" },
    });
    assert_eq!(
        hook(&server, &mut hooks, Some(sid.clone()), post).await,
        HookDecision::NoOpinion
    );
    let third = hook(
        &server,
        &mut hooks,
        None,
        pre_tool_use("b1", "cargo build --release"),
    )
    .await;
    assert_eq!(third, HookDecision::NoOpinion);

    // Hook notifications reach the session.
    let notification = serde_json::json!({
        "session_id": "a1",
        "hook_event_name": "Notification",
        "message": "Claude needs your permission to use Bash",
        "notification_type": "permission_prompt",
    });
    hook(&server, &mut hooks, Some(sid.clone()), notification).await;
    wait_push(&mut client, "hook notification", |m| match m {
        ServerMsg::Event {
            session_id,
            event: SessionEvent::Notification { body, .. },
        } if *session_id == sid => body.contains("permission").then_some(()),
        _ => None,
    })
    .await;

    // Stop.
    let stopped = client
        .api(ClientMsg::Control {
            session_id: sid.clone(),
            action: SessionAction::Stop,
        })
        .await
        .unwrap();
    assert_eq!(stopped, ServerMsg::Ok);
    wait_push(&mut client, "exit", |m| match m {
        ServerMsg::Event {
            session_id,
            event:
                SessionEvent::StateChanged {
                    state: SessionState::Exited { .. },
                    ..
                },
        } if *session_id == sid => Some(()),
        _ => None,
    })
    .await;

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_outside_workspace_roots_is_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "roots").await;
    let mut client = connect(&server).await;
    for project_dir in [
        outside.path().to_owned(),
        dir.path().join("workspace").join("..").join("escape"),
    ] {
        let resp = client
            .api(ClientMsg::Launch {
                request: LaunchRequest {
                    agent: AgentKind::Generic(fake_agent().to_string_lossy().into_owned()),
                    project_dir,
                    create_dir: true,
                    profile: None,
                    args: Vec::new(),
                    display_name: None,
                    fresh: false,
                },
            })
            .await
            .unwrap();
        assert!(
            matches!(
                resp,
                ServerMsg::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{resp:?}"
        );
    }
    assert!(!dir.path().join("escape").exists());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_attach_lookup_and_unsupported_messages() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "run").await;
    let mut client = connect(&server).await;

    let id = client.next_id();
    let resp = client
        .call(&LocalRequest::Run {
            id,
            request: RunRequest {
                program: fake_agent().to_string_lossy().into_owned(),
                args: Vec::new(),
                cwd: dir.path().to_owned(),
                env: vec![("VCA_TEST_MARKER".into(), "42".into())],
                size: None,
                agent: None,
                display_name: None,
            },
        })
        .await
        .unwrap();
    let LocalResponse::Run {
        session, endpoint, ..
    } = resp
    else {
        panic!("run failed: {resp:?}");
    };
    // The attach endpoint serves the session's terminal.
    let attach = vca_pty::AttachClient::connect(&endpoint, vca_pty::AttachOptions::default())
        .await
        .unwrap();
    let (mut reader, mut writer) = attach.split();
    writer.input(b"/env VCA_TEST_MARKER\r").await.unwrap();
    let mut seen = String::new();
    tokio::time::timeout(TIMEOUT, async {
        while !seen.contains("VCA_TEST_MARKER=42") {
            match reader.recv().await.unwrap() {
                Some(
                    vca_pty::attach::ServerFrame::Output { data, .. }
                    | vca_pty::attach::ServerFrame::Redraw(data),
                ) => seen.push_str(&String::from_utf8_lossy(&data)),
                Some(_) => {}
                None => break,
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("env not applied: {seen:?}"));

    let id = client.next_id();
    let resp = client
        .call(&LocalRequest::Attach {
            id,
            session_id: session.id.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(resp, LocalResponse::Attach { endpoint: e, .. } if e == endpoint));

    let resp = client
        .api(ClientMsg::LaunchCloud {
            request: vca_core::CloudLaunchRequest {
                provider: vca_core::CloudProvider::Jules,
                repository: "o/r".into(),
                prompt: "p".into(),
                display_name: None,
            },
        })
        .await
        .unwrap();
    // No cloud account is configured for the provider.
    assert!(
        matches!(
            resp,
            ServerMsg::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ),
        "{resp:?}"
    );
    let devices = client.api(ClientMsg::ListDevices).await.unwrap();
    assert_eq!(devices, ServerMsg::Devices { devices: vec![] });
    let resources = client.api(ClientMsg::GetResources).await.unwrap();
    assert!(
        matches!(resources, ServerMsg::Resources { .. }),
        "{resources:?}"
    );

    let list = client.api(ClientMsg::ListSessions).await.unwrap();
    let ServerMsg::Sessions { sessions } = list else {
        panic!("{list:?}")
    };
    assert!(sessions.iter().any(|s| s.id == session.id));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_session_pushes_and_persists_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "rename").await;
    let mut client = connect(&server).await;
    let mut watcher = connect(&server).await;

    let id = client.next_id();
    let resp = client
        .call(&LocalRequest::Run {
            id,
            request: RunRequest {
                program: fake_agent().to_string_lossy().into_owned(),
                args: Vec::new(),
                cwd: dir.path().to_owned(),
                env: Vec::new(),
                size: None,
                agent: None,
                display_name: None,
            },
        })
        .await
        .unwrap();
    let LocalResponse::Run { session, .. } = resp else {
        panic!("run failed: {resp:?}");
    };
    let auto = session.display_name.clone();
    assert_eq!(session.auto_name.as_deref(), Some(auto.as_str()));
    assert!(!session.custom_name);

    let rename = |name: Option<&str>| ClientMsg::RenameSession {
        session_id: session.id.clone(),
        name: name.map(str::to_owned),
    };
    let resp = client.api(rename(Some("  API work "))).await.unwrap();
    assert_eq!(resp, ServerMsg::Ok);
    // Every client hears about the new name.
    let updated = wait_push(&mut watcher, "renamed session", |msg| match msg {
        ServerMsg::SessionUpdated { session: s } if s.id == session.id && s.custom_name => {
            Some(s.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(updated.display_name, "API work");
    assert_eq!(updated.auto_name.as_deref(), Some(auto.as_str()));
    let names =
        std::fs::read_to_string(dir.path().join("data").join(vca_server::names::NAMES_FILE))
            .unwrap();
    assert!(names.contains("API work"), "{names}");

    for bad in ["two\nlines", &"x".repeat(81)] {
        let resp = client.api(rename(Some(bad))).await.unwrap();
        assert!(
            matches!(
                resp,
                ServerMsg::Error {
                    code: ErrorCode::InvalidRequest,
                    ..
                }
            ),
            "{resp:?}"
        );
    }
    let resp = client
        .api(ClientMsg::RenameSession {
            session_id: "missing".into(),
            name: Some("x".into()),
        })
        .await
        .unwrap();
    assert!(
        matches!(
            resp,
            ServerMsg::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ),
        "{resp:?}"
    );

    assert_eq!(client.api(rename(None)).await.unwrap(), ServerMsg::Ok);
    let reset = wait_push(&mut watcher, "reset session", |msg| match msg {
        ServerMsg::SessionUpdated { session: s } if s.id == session.id && !s.custom_name => {
            Some(s.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(reset.display_name, auto);
    let list = client.api(ClientMsg::ListSessions).await.unwrap();
    let ServerMsg::Sessions { sessions } = list else {
        panic!("{list:?}")
    };
    let listed = sessions.iter().find(|s| s.id == session.id).unwrap();
    assert_eq!(listed.display_name, auto);
    assert!(!listed.custom_name);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_order_is_shared_and_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "order").await;
    let mut client = connect(&server).await;
    let mut watcher = connect(&server).await;

    let mut ids = Vec::new();
    for name in ["first", "second", "third"] {
        let id = client.next_id();
        let resp = client
            .call(&LocalRequest::Run {
                id,
                request: RunRequest {
                    program: fake_agent().to_string_lossy().into_owned(),
                    args: Vec::new(),
                    cwd: dir.path().to_owned(),
                    env: Vec::new(),
                    size: None,
                    agent: None,
                    display_name: Some(name.into()),
                },
            })
            .await
            .unwrap();
        let LocalResponse::Run { session, .. } = resp else {
            panic!("run failed: {resp:?}");
        };
        ids.push(session.id);
    }
    let resp = client.api(ClientMsg::GetSessionOrder).await.unwrap();
    assert_eq!(
        resp,
        ServerMsg::SessionOrder {
            session_ids: ids.clone(),
            custom: false
        }
    );

    let wanted = vec![ids[2].clone(), ids[0].clone(), ids[1].clone()];
    let resp = client
        .api(ClientMsg::SetSessionOrder {
            session_ids: wanted.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        resp,
        ServerMsg::SessionOrder {
            session_ids: wanted.clone(),
            custom: true
        }
    );
    // The other client is told, and the list comes in the new order.
    let pushed = wait_push(&mut watcher, "session order", |msg| match msg {
        ServerMsg::SessionOrder {
            session_ids,
            custom: true,
        } => Some(session_ids.clone()),
        _ => None,
    })
    .await;
    assert_eq!(pushed, wanted);
    let list = watcher.api(ClientMsg::ListSessions).await.unwrap();
    let ServerMsg::Sessions { sessions } = list else {
        panic!("{list:?}")
    };
    let listed: Vec<SessionId> = sessions.into_iter().map(|s| s.id).collect();
    assert_eq!(listed, wanted);
    let file = std::fs::read_to_string(dir.path().join("data").join(vca_server::order::ORDER_FILE))
        .unwrap();
    assert!(file.contains("third"), "{file}");

    let resp = client.api(ClientMsg::ResetSessionOrder).await.unwrap();
    assert_eq!(
        resp,
        ServerMsg::SessionOrder {
            session_ids: ids.clone(),
            custom: false
        }
    );

    server.shutdown().await;
}

/// "Remove from VCA" on a running session VCA started: one request stops
/// the agent, waits for its exit and removes the card; the exit is
/// announced before the removal, so nothing brings the card back.
#[tokio::test(flavor = "multi_thread")]
async fn remove_from_vca_stops_a_running_agent_and_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "remove").await;
    let mut client = connect(&server).await;
    let workspace = dunce_canonical(&dir.path().join("workspace"));
    let launched = client
        .api(ClientMsg::Launch {
            request: LaunchRequest {
                agent: AgentKind::Generic(fake_agent().to_string_lossy().into_owned()),
                project_dir: workspace.join("project"),
                create_dir: true,
                profile: None,
                args: Vec::new(),
                display_name: Some("to remove".into()),
                fresh: false,
            },
        })
        .await
        .unwrap();
    let ServerMsg::Launched { session } = launched else {
        panic!("launch failed: {launched:?}");
    };
    let sid = session.id.clone();
    let sub = client
        .api(ClientMsg::Subscribe {
            session_id: sid.clone(),
            from_seq: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(sub, ServerMsg::Ok);
    wait_output(&mut client, &sid, "> ").await;

    // Without `stop` a running session is refused, as before.
    let plain = client
        .api(ClientMsg::DismissSession {
            session_id: sid.clone(),
            stop: false,
        })
        .await
        .unwrap();
    assert!(
        matches!(
            plain,
            ServerMsg::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ),
        "{plain:?}"
    );
    let removed = client
        .api(ClientMsg::DismissSession {
            session_id: sid.clone(),
            stop: true,
        })
        .await
        .unwrap();
    assert_eq!(removed, ServerMsg::Ok);
    assert!(server.api.sessions().get(&sid).is_none());
    let list = client.api(ClientMsg::ListSessions).await.unwrap();
    let ServerMsg::Sessions { sessions } = list else {
        panic!("{list:?}");
    };
    assert!(sessions.iter().all(|s| s.id != sid), "{sessions:?}");
    // Its name does not outlive it either.
    let names = std::fs::read_to_string(dir.path().join("data").join("session-names.json"))
        .unwrap_or_default();
    assert!(!names.contains("to remove"), "{names}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn second_server_on_same_endpoint_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "single").await;
    let mut config = Config::default();
    config.discovery.enabled = false;
    config.remote.enabled = false;
    let second = Server::start(
        Options {
            control: Some(server.control_endpoint.clone()),
            config_file: Some(dir.path().join("none.toml")),
            data_dir: None,
            process_control: no_process_control(),
            restore_backend: None,
            verifier: Some(vca_server::security::verifier::FakeVerifier::available()),
        },
        Some(config),
    )
    .await;
    assert!(second.is_err());
    server.shutdown().await;
}

/// Canonical path without the `\\?\` prefix Windows adds, so that it can
/// be compared with paths the server returns.
fn dunce_canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap();
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    }
}

/// Stopping a managed session on Windows ends the processes the agent
/// started (through the session's Job Object). Terminates real processes,
/// so it only runs with `VCA_PROCESS_CONTROL_TESTS=1` (CI's disposable VMs).
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_processes_the_agent_started() {
    if std::env::var("VCA_PROCESS_CONTROL_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set VCA_PROCESS_CONTROL_TESTS=1 (isolated VMs only)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "job").await;
    let mut client = connect(&server).await;
    let id = client.next_id();
    let resp = client
        .call(&LocalRequest::Run {
            id,
            request: RunRequest {
                program: "cmd".into(),
                args: vec!["/c".into(), "ping -n 60 127.0.0.1 >NUL".into()],
                cwd: dir.path().to_owned(),
                env: Vec::new(),
                size: None,
                agent: None,
                display_name: None,
            },
        })
        .await
        .unwrap();
    let LocalResponse::Run { session, .. } = resp else {
        panic!("run failed: {resp:?}");
    };
    let vca_core::SessionOrigin::Local {
        pid: Some(root), ..
    } = session.origin
    else {
        panic!("no pid");
    };
    let deadline = std::time::Instant::now() + TIMEOUT;
    let ping = loop {
        let procs = vca_adapters::ProcessTable::snapshot();
        if let Some(p) = procs.descendants(root).into_iter().find(|p| {
            procs
                .get(*p)
                .is_some_and(|i| i.name.eq_ignore_ascii_case("PING.EXE"))
        }) {
            break p;
        }
        assert!(std::time::Instant::now() < deadline, "ping did not start");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let stopped = client
        .api(ClientMsg::Control {
            session_id: session.id.clone(),
            action: SessionAction::Stop,
        })
        .await
        .unwrap();
    assert_eq!(stopped, ServerMsg::Ok);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while vca_adapters::ProcessTable::snapshot().get(ping).is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "ping {ping} survived Stop"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    server.shutdown().await;
}

/// A turn that ends in a question, and a permission prompt the agent
/// reports on screen, reach every client as card updates: the question with
/// its excerpt, the decision at once (no grace period), and the question
/// gone once the person writes again.
#[tokio::test(flavor = "multi_thread")]
async fn questions_and_reported_prompts_reach_clients() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "asks").await;
    let mut client = connect(&server).await;
    let mut hooks = connect(&server).await;
    let mut watcher = connect(&server).await;

    let id = client.next_id();
    let resp = client
        .call(&LocalRequest::Run {
            id,
            request: RunRequest {
                program: fake_agent().to_string_lossy().into_owned(),
                args: Vec::new(),
                cwd: dir.path().to_owned(),
                env: Vec::new(),
                size: None,
                agent: None,
                display_name: None,
            },
        })
        .await
        .unwrap();
    let LocalResponse::Run { session, .. } = resp else {
        panic!("run failed: {resp:?}");
    };
    let sid = session.id.clone();

    let stop = serde_json::json!({
        "session_id": "q1",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "Fixed the parser and the tests pass.\n\n두 가지 방법이 있습니다.\n\n1. 지금 배포\n2. 내일 배포\n\n어떻게 할까요?",
    });
    hook(&server, &mut hooks, Some(sid.clone()), stop.clone()).await;
    let asked = wait_push(&mut watcher, "question on the card", |msg| match msg {
        ServerMsg::SessionUpdated { session: s } if s.id == sid => s.question.clone(),
        _ => None,
    })
    .await;
    assert_eq!(asked.text, "어떻게 할까요?");

    // One question per turn: the same Stop again changes nothing.
    hook(&server, &mut hooks, Some(sid.clone()), stop).await;

    // The person writes: the question is gone.
    let prompt = serde_json::json!({
        "session_id": "q1",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "지금 배포",
    });
    hook(&server, &mut hooks, Some(sid.clone()), prompt).await;
    wait_push(&mut watcher, "question cleared", |msg| match msg {
        ServerMsg::SessionUpdated { session: s } if s.id == sid && s.question.is_none() => Some(()),
        _ => None,
    })
    .await;

    // A prompt in auto mode is held until the agent reports it on screen,
    // then shown at once.
    let request = serde_json::json!({
        "session_id": "q1",
        "hook_event_name": "PermissionRequest",
        "permission_mode": "auto",
        "tool_name": "Bash",
        "tool_input": { "command": "git push" },
    });
    hook(&server, &mut hooks, Some(sid.clone()), request).await;
    let notice = serde_json::json!({
        "session_id": "q1",
        "hook_event_name": "Notification",
        "message": "Claude needs your permission to use Bash",
        "notification_type": "permission_prompt",
    });
    hook(&server, &mut hooks, Some(sid.clone()), notice).await;
    let decision = tokio::time::timeout(
        Duration::from_secs(2),
        wait_push(&mut watcher, "decision on the card", |msg| match msg {
            ServerMsg::SessionUpdated { session: s }
                if s.id == sid && s.state == SessionState::AwaitingDecision =>
            {
                s.pending_decision.clone()
            }
            _ => None,
        }),
    )
    .await
    .expect("the reported prompt is shown without a grace period");
    assert_eq!(decision.decision.prompt, "Allow Bash: git push?");

    server.shutdown().await;
}
