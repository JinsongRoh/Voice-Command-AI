//! Attachments end to end: a file sent to a managed `vca-fake-agent`
//! session is saved in the project and mentioned in the session without
//! pressing Enter; view-only devices cannot attach.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ClientMsg, ControlKey, Envelope, Input, LaunchRequest, PROTOCOL_VERSION, ServerMsg,
    SessionAction, SessionEvent, SessionId,
};
use vca_server::api::DeviceIdentity;
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalResponse};
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
    start_server_with(dir, name, true).await
}

/// `remote: false` leaves device administration detached, so requests are
/// checked against the scope the connection was opened with.
async fn start_server_with(dir: &Path, name: &str, remote: bool) -> Server {
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
    config.remote.enabled = remote;
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

/// Collects session output until it contains `needle`. On a timeout the
/// output seen so far is part of the failure message.
async fn wait_output(client: &mut ControlClient, sid: &SessionId, needle: &str) -> String {
    let mut text = String::new();
    let found = tokio::time::timeout(TIMEOUT, async {
        loop {
            match client.recv().await.unwrap() {
                Some(LocalResponse::Api {
                    body:
                        ServerMsg::Event {
                            session_id,
                            event: SessionEvent::Output { data, .. },
                        },
                    ..
                }) if &session_id == sid => {
                    text.push_str(&String::from_utf8_lossy(&data));
                    if text.contains(needle) {
                        return;
                    }
                }
                Some(_) => {}
                None => panic!("vcad closed the connection while waiting for {needle}"),
            }
        }
    })
    .await;
    assert!(
        found.is_ok(),
        "timed out waiting for {needle}; output so far: {text:?}"
    );
    text
}

fn dunce_canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap();
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    }
}

async fn launch_fake(client: &mut ControlClient, project: PathBuf) -> SessionId {
    let launched = client
        .api(ClientMsg::Launch {
            request: LaunchRequest {
                agent: AgentKind::Generic(fake_agent().to_string_lossy().into_owned()),
                project_dir: project,
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
    session.id
}

#[tokio::test(flavor = "multi_thread")]
async fn attachment_is_saved_and_mentioned_in_a_managed_session() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "attach").await;
    let mut client = connect(&server).await;
    let workspace = dunce_canonical(&dir.path().join("workspace"));
    let project = workspace.join("project");
    let sid = launch_fake(&mut client, project.clone()).await;
    let sub = client
        .api(ClientMsg::Subscribe {
            session_id: sid.clone(),
            from_seq: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(sub, ServerMsg::Ok);
    wait_output(&mut client, &sid, "> ").await;

    // Chunked upload of a small photo.
    let data: Vec<u8> = b"\x89PNG\r\n\x1a\n"
        .iter()
        .copied()
        .chain((0..3000u32).map(|i| (i % 251) as u8))
        .collect();
    let ready = client
        .api(ClientMsg::AttachBegin {
            session_id: sid.clone(),
            name: "IMG 0001.png".into(),
            mime: "image/png".into(),
            size: data.len() as u64,
            note: None,
            submit: false,
            folder: None,
            batch: None,
        })
        .await
        .unwrap();
    let ServerMsg::AttachReady { upload_id, .. } = ready else {
        panic!("{ready:?}");
    };
    let mut offset = 0;
    for chunk in data.chunks(1000) {
        let r = client
            .api(ClientMsg::AttachChunk {
                upload_id: upload_id.clone(),
                offset,
                data: chunk.to_vec(),
            })
            .await
            .unwrap();
        assert_eq!(r, ServerMsg::Ok);
        offset += chunk.len() as u64;
    }
    let done = client
        .api(ClientMsg::AttachEnd { upload_id })
        .await
        .unwrap();
    let ServerMsg::Attached {
        session_id,
        path,
        size,
        mention,
        typed,
    } = done
    else {
        panic!("{done:?}");
    };
    assert_eq!(session_id, sid);
    assert_eq!(size, data.len() as u64);
    assert!(typed, "managed sessions get the mention typed in");
    assert!(
        path.starts_with(".vca/attachments/") && path.ends_with("-IMG-0001.png"),
        "{path}"
    );
    assert_eq!(mention, format!("Attached file: @{path}"));
    assert_eq!(std::fs::read(project.join(&path)).unwrap(), data);

    // Not submitted: the agent has not answered the line yet.
    let early = tokio::time::timeout(Duration::from_millis(1500), async {
        let mut text = String::new();
        loop {
            if let Ok(Some(LocalResponse::Api {
                body:
                    ServerMsg::Event {
                        event: SessionEvent::Output { data, .. },
                        ..
                    },
                ..
            })) = client.recv().await
            {
                text.push_str(&String::from_utf8_lossy(&data));
                if text.contains("Echo: Attached") {
                    return text;
                }
            }
        }
    })
    .await;
    assert!(early.is_err(), "the mention was submitted: {early:?}");

    // The user adds instructions and sends.
    client
        .api(ClientMsg::SendInput {
            session_id: sid.clone(),
            input: Input::Text {
                text: "describe it".into(),
                submit: false,
                at_prompt: false,
            },
        })
        .await
        .unwrap();
    client
        .api(ClientMsg::SendInput {
            session_id: sid.clone(),
            input: Input::Key {
                key: ControlKey::Enter,
            },
        })
        .await
        .unwrap();
    let echoed = format!("Echo: Attached file: @{path} describe it");
    wait_output(&mut client, &sid, &echoed).await;

    let stopped = client
        .api(ClientMsg::Control {
            session_id: sid.clone(),
            action: SessionAction::Stop,
        })
        .await
        .unwrap();
    assert_eq!(stopped, ServerMsg::Ok);
    server.shutdown().await;
}

/// Several photos picked together: saved into one folder and typed into
/// the agent as ONE mention, then one Enter (not a mention per photo).
#[tokio::test(flavor = "multi_thread")]
async fn batch_is_typed_as_one_mention_in_a_managed_session() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server(dir.path(), "attach-batch").await;
    let mut client = connect(&server).await;
    let workspace = dunce_canonical(&dir.path().join("workspace"));
    let project = workspace.join("project");
    let sid = launch_fake(&mut client, project.clone()).await;
    let sub = client
        .api(ClientMsg::Subscribe {
            session_id: sid.clone(),
            from_seq: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(sub, ServerMsg::Ok);
    wait_output(&mut client, &sid, "> ").await;

    let mut paths = Vec::new();
    for name in ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"] {
        let data = b"\xFF\xD8\xFF photo".to_vec();
        let ready = client
            .api(ClientMsg::AttachBegin {
                session_id: sid.clone(),
                name: name.into(),
                mime: "image/jpeg".into(),
                size: data.len() as u64,
                note: None,
                submit: false,
                folder: None,
                batch: Some("b1".into()),
            })
            .await
            .unwrap();
        let ServerMsg::AttachReady { upload_id, .. } = ready else {
            panic!("{ready:?}");
        };
        let ok = client
            .api(ClientMsg::AttachChunk {
                upload_id: upload_id.clone(),
                offset: 0,
                data,
            })
            .await
            .unwrap();
        assert_eq!(ok, ServerMsg::Ok);
        let done = client
            .api(ClientMsg::AttachEnd { upload_id })
            .await
            .unwrap();
        let ServerMsg::Attached {
            path,
            mention,
            typed,
            ..
        } = done
        else {
            panic!("{done:?}");
        };
        assert!(mention.is_empty() && !typed, "no mention per file");
        paths.push(path);
    }
    let done = client
        .api(ClientMsg::AttachBatchEnd {
            batch_id: "b1".into(),
            note: None,
            submit: true,
        })
        .await
        .unwrap();
    let ServerMsg::Attached { mention, typed, .. } = done else {
        panic!("{done:?}");
    };
    assert!(typed, "managed sessions get the batch mention typed in");
    let list: Vec<String> = paths.iter().map(|p| format!("@{p}")).collect();
    assert_eq!(mention, format!("Attached files (3): {}", list.join(" ")));
    // The agent got one line with every file, submitted once (ConPTY may
    // redraw a long line, so only its start is matched).
    wait_output(&mut client, &sid, "Echo: Attached files (3)").await;
    let more = tokio::time::timeout(
        Duration::from_millis(1500),
        wait_output(&mut client, &sid, "Echo: Attached file:"),
    )
    .await;
    assert!(more.is_err(), "a file was mentioned on its own: {more:?}");

    let stopped = client
        .api(ClientMsg::Control {
            session_id: sid.clone(),
            action: SessionAction::Stop,
        })
        .await
        .unwrap();
    assert_eq!(stopped, ServerMsg::Ok);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn view_only_devices_cannot_attach() {
    let dir = tempfile::tempdir().unwrap();
    // Without remote access no device registry is attached, so the
    // connection's own scope is what the gate checks (the registry path is
    // covered in tests/remote.rs).
    let server = start_server_with(dir.path(), "attach-scope", false).await;
    for (scope, allowed) in [
        (vca_net::Scope::View, false),
        (vca_net::Scope::Command, true),
    ] {
        let (client, mut rx) = server.api.connect_as(
            "test",
            Some(DeviceIdentity {
                device_id: "dev_t".into(),
                scope,
            }),
        );
        server
            .api
            .handle(
                &client,
                Envelope {
                    id: 9,
                    body: ClientMsg::Attach {
                        session_id: "missing".into(),
                        name: "a.txt".into(),
                        mime: "text/plain".into(),
                        data: b"hi".to_vec(),
                        note: None,
                        submit: false,
                    },
                },
            )
            .await;
        let reply = tokio::time::timeout(TIMEOUT, async {
            loop {
                let env = rx.recv().await.unwrap();
                if env.id == 9 {
                    return env.body;
                }
            }
        })
        .await
        .unwrap();
        let ServerMsg::Error { code, .. } = reply else {
            panic!("{reply:?}");
        };
        // Command scope passes the scope check and then fails on the session.
        let expected = if allowed {
            ErrorCode::NotFound
        } else {
            ErrorCode::Forbidden
        };
        assert_eq!(code, expected, "{scope:?}");
    }
    server.shutdown().await;
}

/// Answering a question form is a decision: a view-only device is refused
/// before anything reaches a session, like for permission prompts.
#[tokio::test(flavor = "multi_thread")]
async fn view_only_devices_cannot_answer_questions() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_server_with(dir.path(), "answer-scope", false).await;
    for (scope, expected) in [
        (vca_net::Scope::View, ErrorCode::Forbidden),
        // Passes the scope check, then fails on the session.
        (vca_net::Scope::Command, ErrorCode::NotFound),
    ] {
        let (client, mut rx) = server.api.connect_as(
            "test",
            Some(DeviceIdentity {
                device_id: "dev_t".into(),
                scope,
            }),
        );
        server
            .api
            .handle(
                &client,
                Envelope {
                    id: 7,
                    body: ClientMsg::SendInput {
                        session_id: "missing".into(),
                        input: Input::Answer {
                            decision_id: "claude:abc:toolu_1".into(),
                            answers: vec![vca_core::QuestionAnswer {
                                options: vec![0],
                                text: None,
                            }],
                        },
                    },
                },
            )
            .await;
        let reply = tokio::time::timeout(TIMEOUT, async {
            loop {
                let env = rx.recv().await.unwrap();
                if env.id == 7 {
                    return env.body;
                }
            }
        })
        .await
        .unwrap();
        let ServerMsg::Error { code, .. } = reply else {
            panic!("{reply:?}");
        };
        assert_eq!(code, expected, "{scope:?}");
    }
    server.shutdown().await;
}
