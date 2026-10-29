//! Cloud sessions through the API: launch, card, events, follow-up and
//! stop against a local `wiremock` server standing in for the Cursor Cloud
//! Agents API. No real provider is ever contacted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{
    ClientMsg, CloudLaunchRequest, CloudProvider, ControlTier, DeliveryStatus, Envelope, Input,
    MessageRole, ServerMsg, SessionAction, SessionEvent, SessionId, SessionOrigin, SessionState,
};
use vca_net::{ClientAuth, ClientConfig, ClientConnection, PairingPayload, StaticKeypair};
use vca_server::cloud::{CloudAccountConfig, Secret};
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};
use vca_server::daemon::{Options, Server};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TIMEOUT: Duration = Duration::from_secs(30);
const SECRET: &str = "cur_test_secret_4711";
const REMOTE_ID: &str = "bc_cloud1";

/// Cloud sessions must never reach the broker's process control.
struct NoProcessControl;

impl ProcessControl for NoProcessControl {
    fn set_priority(&self, root: u32, _: Scope, _: Priority) -> ContainResult {
        panic!("cloud tests must not touch processes ({root})")
    }
    fn suspend(&self, root: u32, _: Scope) -> ContainResult {
        panic!("cloud tests must not touch processes ({root})")
    }
    fn resume(&self, root: u32, _: Scope) -> ContainResult {
        panic!("cloud tests must not touch processes ({root})")
    }
    fn terminate(&self, root: u32, _: Scope, _: Duration) -> ContainResult {
        panic!("cloud tests must not touch processes ({root})")
    }
}

fn control_endpoint(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\vca-test-{}-cloud-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join("control.sock")
    }
}

async fn start(dir: &Path, provider_url: &str) -> Server {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config {
        workspace_roots: vec![workspace],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.bind = "127.0.0.1:0".into();
    config.remote.require_totp = false;
    config.remote.heartbeat_secs = 1;
    config.broker.policy_path = Some(dir.join("policies.toml"));
    config.cloud.poll_min_ms = 50;
    config.cloud.poll_max_ms = 200;
    config.cloud.accounts = vec![CloudAccountConfig {
        base_url: Some(provider_url.into()),
        ..CloudAccountConfig::new(CloudProvider::CursorCloud, "default")
    }];
    Server::start(
        Options {
            config_file: Some(dir.join("config.toml")),
            control: Some(control_endpoint(dir)),
            data_dir: Some(dir.join("data")),
            process_control: Some(Arc::new(NoProcessControl)),
        },
        Some(config),
    )
    .await
    .expect("vcad failed to start")
}

/// Basic auth as Cursor documents it: the key as user name, no password.
fn basic_auth() -> String {
    format!(
        "Basic {}",
        data_encoding::BASE64.encode(format!("{SECRET}:").as_bytes())
    )
}

/// Serves the agent and its conversation; lower `priority` values win.
async fn mount_agent(server: &MockServer, status: &str, priority: u8) {
    Mock::given(method("GET"))
        .and(path(format!("/v0/agents/{REMOTE_ID}")))
        .and(header("Authorization", basic_auth().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": REMOTE_ID, "name": "Fix the flaky test", "status": status,
            "target": {"url": "https://cursor.com/agents?id=bc_cloud1"}
        })))
        .with_priority(priority)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/agents/{REMOTE_ID}/conversation")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": REMOTE_ID,
            "messages": [
                {"id": "m1", "type": "user_message", "text": "Fix the flaky test"},
                {"id": "m2", "type": "assistant_message", "text": "Looking at the test now"}
            ]
        })))
        .with_priority(priority)
        .mount(server)
        .await;
}

async fn mount_provider(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v0/agents"))
        .and(header("Authorization", basic_auth().as_str()))
        .and(body_json(json!({
            "prompt": {"text": "Fix the flaky test"},
            "source": {"repository": "https://github.com/octo/hello"},
            "target": {"autoCreatePr": false}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": REMOTE_ID, "status": "CREATING",
            "source": {"repository": "https://github.com/octo/hello"},
            "target": {"url": "https://cursor.com/agents?id=bc_cloud1"},
            "createdAt": "2026-10-29T00:00:00Z"
        })))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/agents/{REMOTE_ID}/followup")))
        .and(body_json(
            json!({"prompt": {"text": "also update the docs"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": REMOTE_ID})))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/agents/{REMOTE_ID}/stop")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": REMOTE_ID})))
        .expect(1)
        .mount(server)
        .await;
    mount_agent(server, "RUNNING", 5).await;
}

async fn local(server: &Server) -> ControlClient {
    ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap()
}

async fn local_call(server: &Server, make: impl FnOnce(u64) -> LocalRequest) -> LocalResponse {
    let mut c = local(server).await;
    let id = c.next_id();
    c.call(&make(id)).await.unwrap()
}

fn launch_request() -> ClientMsg {
    ClientMsg::LaunchCloud {
        request: CloudLaunchRequest {
            provider: CloudProvider::CursorCloud,
            repository: "octo/hello".into(),
            prompt: "Fix the flaky test".into(),
            display_name: Some("flaky test".into()),
        },
    }
}

/// Reads pushes until `pred` matches one, returning it.
async fn wait_for(
    c: &mut ControlClient,
    what: &str,
    mut pred: impl FnMut(&ServerMsg) -> bool,
) -> ServerMsg {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match c.recv().await.unwrap() {
                Some(LocalResponse::Api { body, .. }) if pred(&body) => return body,
                Some(_) => continue,
                None => panic!("vcad closed the connection while waiting for {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn providers(msg: ServerMsg) -> Vec<vca_core::CloudProviderInfo> {
    match msg {
        ServerMsg::CloudProviders { providers } => providers,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cloud_session_lifecycle_through_the_api() {
    let provider = MockServer::start().await;
    mount_provider(&provider).await;
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), &provider.uri()).await;
    let mut c = local(&server).await;

    // Configured account, but no credential yet.
    let list = providers(c.api(ClientMsg::ListCloudProviders).await.unwrap());
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].provider, CloudProvider::CursorCloud);
    assert_eq!(list[0].account, "default");
    assert!(!list[0].configured);
    assert!(list[0].follow_up && list[0].stop && !list[0].status_only);
    match c.api(launch_request()).await.unwrap() {
        ServerMsg::Error { code, .. } => assert_eq!(code, ErrorCode::Unauthorized),
        other => panic!("{other:?}"),
    }

    // `vca cloud login` over the local channel.
    let resp = local_call(&server, |id| LocalRequest::CloudCredentialSet {
        id,
        provider: CloudProvider::CursorCloud,
        account: "default".into(),
        secret: Secret::new(SECRET),
    })
    .await;
    assert_eq!(resp, LocalResponse::Done { id: 1 });
    let reply = c.api(ClientMsg::ListCloudProviders).await.unwrap();
    assert!(!serde_json::to_string(&reply).unwrap().contains(SECRET));
    assert!(providers(reply)[0].configured);

    // Launch: the card has a cloud origin and the cloud control tier.
    let ServerMsg::Launched { session } = c.api(launch_request()).await.unwrap() else {
        panic!("launch failed");
    };
    assert_eq!(session.id, SessionId::from("cloud:cursor:bc_cloud1"));
    assert_eq!(session.control, ControlTier::Cloud);
    assert_eq!(session.display_name, "flaky test");
    match &session.origin {
        SessionOrigin::Cloud {
            provider,
            remote_id,
            url,
        } => {
            assert_eq!(*provider, CloudProvider::CursorCloud);
            assert_eq!(remote_id, REMOTE_ID);
            assert_eq!(
                url.as_deref(),
                Some("https://cursor.com/agents?id=bc_cloud1")
            );
        }
        other => panic!("{other:?}"),
    }
    let sessions = match c.api(ClientMsg::ListSessions).await.unwrap() {
        ServerMsg::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    };
    assert!(sessions.iter().any(|s| s.id == session.id));

    // Polled activity arrives as session events.
    let sid = session.id.clone();
    assert_eq!(
        c.api(ClientMsg::Subscribe {
            session_id: sid.clone(),
            from_seq: Some(0),
        })
        .await
        .unwrap(),
        ServerMsg::Ok
    );
    wait_for(&mut c, "the assistant message", |m| {
        matches!(m, ServerMsg::Event {
            event: SessionEvent::Message { role: MessageRole::Assistant, text, .. },
            ..
        } if text == "Looking at the test now")
    })
    .await;
    wait_for(&mut c, "the running state", |m| {
        matches!(
            m,
            ServerMsg::Event {
                event: SessionEvent::StateChanged {
                    state: SessionState::Running,
                    ..
                },
                ..
            }
        )
    })
    .await;
    let card = server.api.sessions().get(&sid).unwrap().info();
    assert_eq!(card.state, SessionState::Running);
    assert_eq!(card.preview.as_deref(), Some("Looking at the test now"));
    assert!(card.unread >= 1);

    // Follow-up message: queued, then delivered to the provider.
    let id = c.next_id();
    c.send(&LocalRequest::Api {
        id,
        body: ClientMsg::SendInput {
            session_id: sid.clone(),
            input: Input::Text {
                text: "also update the docs".into(),
                submit: true,
            },
        },
    })
    .await
    .unwrap();
    wait_for(&mut c, "the delivery", |m| match m {
        ServerMsg::InputAck { status, .. } => *status == DeliveryStatus::Delivered,
        ServerMsg::Error { message, .. } => panic!("follow-up failed: {message}"),
        _ => false,
    })
    .await;

    // Keys and local controls make no sense for a cloud agent.
    for action in [
        SessionAction::Pause,
        SessionAction::Resume,
        SessionAction::Restart,
    ] {
        match c
            .api(ClientMsg::Control {
                session_id: sid.clone(),
                action,
            })
            .await
            .unwrap()
        {
            ServerMsg::Error { code, .. } => assert_eq!(code, ErrorCode::Unsupported),
            other => panic!("{action:?}: {other:?}"),
        }
    }

    // Stop reaches the provider; the poller then sees the final state.
    assert_eq!(
        c.api(ClientMsg::Control {
            session_id: sid.clone(),
            action: SessionAction::Stop,
        })
        .await
        .unwrap(),
        ServerMsg::Ok
    );
    mount_agent(&provider, "FINISHED", 1).await;
    wait_for(&mut c, "the exited state", |m| {
        matches!(
            m,
            ServerMsg::Event {
                event: SessionEvent::StateChanged {
                    state: SessionState::Exited { .. },
                    ..
                },
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        server.api.sessions().get(&sid).unwrap().info().state,
        SessionState::Exited { .. }
    ));

    // `vca cloud logout`.
    let logout = |id| LocalRequest::CloudCredentialDelete {
        id,
        provider: CloudProvider::CursorCloud,
        account: "default".into(),
    };
    assert!(matches!(
        local_call(&server, logout).await,
        LocalResponse::Done { .. }
    ));
    match local_call(&server, logout).await {
        LocalResponse::Error { code, .. } => assert_eq!(code, ErrorCode::NotFound),
        other => panic!("{other:?}"),
    }
    drop(c);
    server.shutdown().await;
}

async fn paired_admin(server: &Server) -> ClientConnection {
    let resp = local_call(server, |id| LocalRequest::Pair {
        id,
        scope: "admin".into(),
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
            device_name: "phone".into(),
            platform: "test".into(),
            totp: None,
        },
    )
    .await
    .unwrap()
}

async fn remote_call(conn: &mut ClientConnection, id: u64, body: ClientMsg) -> ServerMsg {
    conn.send(&Envelope { id, body }).await.unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match conn.recv().await {
                Some(Ok(env)) if env.id == id => return env.body,
                Some(Ok(_)) => continue,
                other => panic!("connection ended: {other:?}"),
            }
        }
    })
    .await
    .expect("no reply")
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_devices_cannot_reach_cloud_credentials() {
    // No mocks: any provider request would fail the launch below.
    let provider = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), &provider.uri()).await;
    let mut phone = paired_admin(&server).await;

    // Even an admin device only speaks the `ClientMsg` API, which has no
    // credential request: the local-channel request does not decode as one.
    let local_req = serde_json::to_value(LocalRequest::CloudCredentialSet {
        id: 1,
        provider: CloudProvider::CursorCloud,
        account: "default".into(),
        secret: Secret::new(SECRET),
    })
    .unwrap();
    let mut as_api = local_req.clone();
    as_api["type"] = as_api["op"].clone();
    assert!(serde_json::from_value::<ClientMsg>(as_api).is_err());
    assert!(
        serde_json::from_value::<Envelope<ClientMsg>>(json!({"id": 1, "body": local_req})).is_err()
    );

    let list = providers(remote_call(&mut phone, 1, ClientMsg::ListCloudProviders).await);
    assert!(!list[0].configured);
    match remote_call(&mut phone, 2, launch_request()).await {
        ServerMsg::Error { code, .. } => assert_eq!(code, ErrorCode::Unauthorized),
        other => panic!("{other:?}"),
    }
    assert!(
        provider
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );

    // After a local login the device sees the account as configured, but
    // never the secret, and the secret is in no configuration or log file.
    let resp = local_call(&server, |id| LocalRequest::CloudCredentialSet {
        id,
        provider: CloudProvider::CursorCloud,
        account: "default".into(),
        secret: Secret::new(SECRET),
    })
    .await;
    assert!(matches!(resp, LocalResponse::Done { .. }), "{resp:?}");
    let reply = remote_call(&mut phone, 3, ClientMsg::ListCloudProviders).await;
    assert!(!format!("{reply:?}").contains(SECRET));
    assert!(providers(reply)[0].configured);
    let stored = dir
        .path()
        .join("data")
        .join("cloud")
        .join("cursor_default.key");
    assert!(stored.exists());
    for entry in walk(dir.path()) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(SECRET),
            "secret found in plain text in {}",
            entry.display()
        );
    }
    drop(phone);
    server.shutdown().await;
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
