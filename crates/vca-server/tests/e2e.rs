//! End-to-end test of vcad over the local control channel, driving
//! `vca-fake-agent` sessions (handoff §6).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use vca_adapters::HookDecision;
use vca_adapters::claude::hook_event::ClaudeHookPayload;
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ClientMsg, DeliveryStatus, Input, LaunchRequest, PROTOCOL_VERSION, ServerMsg,
    SessionAction, SessionEvent, SessionId, SessionState,
};
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse, RunRequest};
use vca_server::daemon::{Options, Server};

const TIMEOUT: Duration = Duration::from_secs(30);

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
    config.sessions.heuristics_interval_ms = 100;
    config.sessions.exited_retention_secs = 60;
    config.broker.policy_path = Some(policy);
    config.shutdown.grace_secs = 2;
    Server::start(
        Options {
            config_file: Some(dir.join("config.toml")),
            control: Some(control_endpoint(dir, name)),
            data_dir: Some(dir.join("data")),
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
    client: &mut ControlClient,
    session_id: Option<SessionId>,
    json: serde_json::Value,
) -> HookDecision {
    let id = client.next_id();
    let resp = tokio::time::timeout(
        TIMEOUT,
        client.call(&LocalRequest::Hook {
            id,
            session_id,
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
        &mut hooks,
        Some(sid.clone()),
        pre_tool_use("a1", "cargo build"),
    )
    .await;
    assert_eq!(first, HookDecision::NoOpinion);
    let light = hook(&mut hooks, None, pre_tool_use("b1", "ls -la")).await;
    assert_eq!(light, HookDecision::NoOpinion);
    let second = hook(
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
        hook(&mut hooks, Some(sid.clone()), post).await,
        HookDecision::NoOpinion
    );
    let third = hook(
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
    hook(&mut hooks, Some(sid.clone()), notification).await;
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

    for msg in [ClientMsg::KillSwitch, ClientMsg::ListDevices] {
        let resp = client.api(msg).await.unwrap();
        assert!(
            matches!(
                resp,
                ServerMsg::Error {
                    code: ErrorCode::Unsupported,
                    ..
                }
            ),
            "{resp:?}"
        );
    }
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
async fn second_server_on_same_endpoint_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "single").await;
    let mut config = Config::default();
    config.discovery.enabled = false;
    let second = Server::start(
        Options {
            control: Some(server.control_endpoint.clone()),
            config_file: Some(dir.path().join("none.toml")),
            data_dir: None,
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
