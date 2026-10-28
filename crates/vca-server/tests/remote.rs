//! End-to-end tests of the remote transport: pairing, TOTP, device scopes,
//! device administration and the kill switch, over real Noise WebSocket
//! connections on loopback.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ClientMsg, Envelope, Input, LaunchRequest, PROTOCOL_VERSION, ServerMsg, SessionEvent,
};
use vca_net::{
    ClientAuth, ClientConfig, ClientConnection, NetError, PairingPayload, StaticKeypair,
};
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};
use vca_server::daemon::{Options, Server};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Never touches a real process (see the end-to-end tests).
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

fn control_endpoint(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\vca-test-{}-remote-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join("control.sock")
    }
}

async fn start(dir: &Path, require_totp: bool) -> Server {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config {
        workspace_roots: vec![workspace],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.bind = "127.0.0.1:0".into();
    config.remote.require_totp = require_totp;
    config.remote.heartbeat_secs = 1;
    config.broker.policy_path = Some(dir.join("policies.toml"));
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

async fn pairing(server: &Server, scope: &str) -> PairingPayload {
    let resp = local_call(server, |id| LocalRequest::Pair {
        id,
        scope: scope.into(),
        endpoints: Vec::new(),
    })
    .await;
    let LocalResponse::Pairing { uri, .. } = resp else {
        panic!("pairing failed: {resp:?}");
    };
    PairingPayload::parse(&uri).unwrap()
}

async fn pair(
    payload: &PairingPayload,
    name: &str,
    totp: Option<String>,
) -> Result<ClientConnection, NetError> {
    let config = ClientConfig::from_pairing(payload, 0, StaticKeypair::generate()).unwrap();
    ClientConnection::connect(
        config,
        ClientAuth::Pair {
            secret: payload.secret.clone(),
            device_name: name.into(),
            platform: "test".into(),
            totp,
        },
    )
    .await
}

/// Sends a request and returns the reply with the same id.
async fn call(conn: &mut ClientConnection, id: u64, body: ClientMsg) -> ServerMsg {
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

fn totp_now(secret_base32: &str) -> String {
    let secret = data_encoding::BASE32_NOPAD
        .decode(secret_base32.trim_end_matches('=').as_bytes())
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    vca_net::totp::totp_code_at(&secret, now)
}

#[tokio::test(flavor = "multi_thread")]
async fn paired_device_controls_sessions_over_noise() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    let payload = pairing(&server, "command").await;
    let mut phone = pair(&payload, "phone", None).await.unwrap();

    let welcome = call(
        &mut phone,
        1,
        ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: "test".into(),
        },
    )
    .await;
    assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");

    let workspace = std::fs::canonicalize(dir.path().join("workspace")).unwrap();
    let launched = call(
        &mut phone,
        2,
        ClientMsg::Launch {
            request: LaunchRequest {
                agent: AgentKind::Generic(fake_agent().to_string_lossy().into_owned()),
                project_dir: workspace.join("p"),
                create_dir: true,
                profile: None,
                args: Vec::new(),
                display_name: None,
            },
        },
    )
    .await;
    let ServerMsg::Launched { session } = launched else {
        panic!("{launched:?}");
    };
    let sub = call(
        &mut phone,
        3,
        ClientMsg::Subscribe {
            session_id: session.id.clone(),
            from_seq: Some(0),
        },
    )
    .await;
    assert_eq!(sub, ServerMsg::Ok);
    let ack = call(
        &mut phone,
        4,
        ClientMsg::SendInput {
            session_id: session.id.clone(),
            input: Input::Text {
                text: "over noise".into(),
                submit: true,
            },
        },
    )
    .await;
    assert!(matches!(ack, ServerMsg::InputAck { .. }), "{ack:?}");
    let mut text = String::new();
    tokio::time::timeout(TIMEOUT, async {
        while !text.contains("Echo: over noise") {
            if let Some(Ok(env)) = phone.recv().await {
                if let ServerMsg::Event {
                    event: SessionEvent::Output { data, .. },
                    ..
                } = env.body
                {
                    text.push_str(&String::from_utf8_lossy(&data));
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no echo: {text:?}"));

    let devices = call(&mut phone, 5, ClientMsg::ListDevices).await;
    let ServerMsg::Devices { devices } = devices else {
        panic!("{devices:?}");
    };
    assert_eq!(devices.len(), 1);
    assert!(devices[0].current);
    assert_eq!(devices[0].name, "phone");

    // A pairing code works once.
    assert!(pair(&payload, "again", None).await.is_err());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn view_scope_cannot_send_input_or_administer() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    let payload = pairing(&server, "view").await;
    let mut viewer = pair(&payload, "tablet", None).await.unwrap();
    let list = call(&mut viewer, 1, ClientMsg::ListSessions).await;
    assert!(matches!(list, ServerMsg::Sessions { .. }), "{list:?}");
    for (id, msg) in [
        (
            2,
            ClientMsg::SendInput {
                session_id: "x".into(),
                input: Input::Text {
                    text: "rm -rf /".into(),
                    submit: true,
                },
            },
        ),
        (3, ClientMsg::KillSwitch),
        (
            4,
            ClientMsg::RevokeDevice {
                device_id: "x".into(),
            },
        ),
    ] {
        let resp = call(&mut viewer, id, msg).await;
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
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn totp_is_required_after_enrollment() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), true).await;
    let resp = local_call(&server, |id| LocalRequest::TotpBegin {
        id,
        account: "owner".into(),
    })
    .await;
    let LocalResponse::TotpPending { secret_base32, .. } = resp else {
        panic!("{resp:?}");
    };
    // The pending enrollment survives across control connections.
    let code = totp_now(&secret_base32);
    let resp = local_call(&server, |id| LocalRequest::TotpConfirm { id, code }).await;
    let LocalResponse::TotpEnrolled { backup_codes, .. } = resp else {
        panic!("{resp:?}");
    };
    assert!(!backup_codes.is_empty());

    let payload = pairing(&server, "command").await;
    let wrong = pair(&payload, "phone", Some("000000".into())).await;
    assert!(wrong.is_err(), "a wrong code must be rejected");

    // Codes are single use, so wait for the next TOTP step.
    tokio::time::sleep(Duration::from_secs(
        31 - std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            % 30,
    ))
    .await;
    let payload = pairing(&server, "command").await;
    let ok = pair(&payload, "phone", Some(totp_now(&secret_base32))).await;
    assert!(ok.is_ok(), "{:?}", ok.err());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_and_kill_switch_drop_connections() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    let payload = pairing(&server, "command").await;
    let phone = pair(&payload, "phone", None).await.unwrap();
    let tokens = phone.session.tokens.clone().expect("tokens after pairing");
    let payload2 = pairing(&server, "admin").await;
    let mut laptop = pair(&payload2, "laptop", None).await.unwrap();
    let phone_id = phone.session.device_id.clone();

    // The admin device revokes the phone; the phone is disconnected and its
    // tokens stop working.
    let revoked = call(
        &mut laptop,
        1,
        ClientMsg::RevokeDevice {
            device_id: phone_id.clone(),
        },
    )
    .await;
    assert_eq!(revoked, ServerMsg::Ok);
    let reason = tokio::time::timeout(TIMEOUT, phone.closed()).await.unwrap();
    assert_eq!(reason, vca_net::CloseReason::DeviceRevoked);
    let config = ClientConfig::from_pairing(&payload, 0, StaticKeypair::generate()).unwrap();
    assert!(
        ClientConnection::connect(config, ClientAuth::RefreshToken(tokens.refresh_token))
            .await
            .is_err()
    );

    // The kill switch drops everyone and refuses new connections until the
    // owner resets it locally.
    // The device that trips it is disconnected too, possibly before the
    // reply arrives; the close reason tells it what happened.
    laptop
        .send(&Envelope {
            id: 2,
            body: ClientMsg::KillSwitch,
        })
        .await
        .unwrap();
    let reason = tokio::time::timeout(TIMEOUT, async {
        while let Some(msg) = laptop.recv().await {
            if let Ok(env) = msg {
                assert!(env.id != 2 || env.body == ServerMsg::Ok, "{env:?}");
            }
        }
        laptop.closed().await
    })
    .await
    .unwrap();
    assert_eq!(reason, vca_net::CloseReason::KillSwitch);
    let payload3 = pairing(&server, "command").await;
    assert!(pair(&payload3, "tablet", None).await.is_err());
    let status = local_call(&server, |id| LocalRequest::RemoteStatus { id }).await;
    assert!(
        matches!(
            status,
            LocalResponse::RemoteStatus {
                kill_switch_tripped: true,
                ..
            }
        ),
        "{status:?}"
    );
    let reset = local_call(&server, |id| LocalRequest::KillSwitchReset { id }).await;
    assert!(matches!(reset, LocalResponse::Done { .. }), "{reset:?}");
    let payload4 = pairing(&server, "command").await;
    assert!(pair(&payload4, "tablet", None).await.is_ok());
    server.shutdown().await;
}
