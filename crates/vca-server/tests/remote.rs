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
use vca_server::config::{Config, ProviderKind};
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
    start_with(dir, require_totp, |_| {}).await
}

async fn start_with(dir: &Path, require_totp: bool, edit: impl FnOnce(&mut Config)) -> Server {
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
    config.sessions.allow_generic_launch = true;
    config.broker.policy_path = Some(dir.join("policies.toml"));
    edit(&mut config);
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
                fresh: false,
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
                at_prompt: false,
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
                    at_prompt: false,
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
        (
            5,
            ClientMsg::RenameSession {
                session_id: "x".into(),
                name: Some("mine".into()),
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

/// Only the host mints pairing codes, invites and TOTP enrollments (over the
/// owner-only local channel). A paired client, even with admin scope, has no
/// API for them: a forged request gets a clear error and loses the connection.
#[tokio::test(flavor = "multi_thread")]
async fn remote_admin_cannot_mint_pairing_codes_invites_or_totp() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    let forged = [
        serde_json::json!({ "type": "pair", "scope": "admin", "endpoints": [] }),
        serde_json::json!({ "op": "pair", "id": 9, "scope": "admin", "endpoints": [] }),
        serde_json::json!({ "type": "create_pairing", "scope": "admin" }),
        serde_json::json!({ "type": "pairing_invite" }),
        serde_json::json!({ "type": "dnspilot_invite" }),
        serde_json::json!({ "type": "totp_begin", "account": "owner" }),
        serde_json::json!({ "type": "totp_confirm", "code": "123456" }),
        serde_json::json!({ "type": "totp_backup_codes" }),
        serde_json::json!({ "type": "kill_switch_reset" }),
        serde_json::json!({ "type": "approve_device", "device_id": "dev_x" }),
    ];
    for body in forged {
        assert!(
            serde_json::from_value::<ClientMsg>(body.clone()).is_err(),
            "{body}"
        );
        let payload = pairing(&server, "admin").await;
        let mut admin = pair(&payload, "admin phone", None).await.unwrap();
        assert_eq!(admin.session.scope, vca_net::Scope::Admin);
        admin
            .sender_as::<serde_json::Value>()
            .send(&Envelope {
                id: 9,
                body: body.clone(),
            })
            .await
            .unwrap();
        let reply = tokio::time::timeout(TIMEOUT, admin.recv()).await.unwrap();
        let Some(Ok(Envelope {
            body: ServerMsg::Error { code, message },
            ..
        })) = reply
        else {
            panic!("{body} must be refused: {reply:?}");
        };
        assert_eq!(code, ErrorCode::InvalidRequest);
        assert!(message.contains("host computer"), "{message}");
        let end = tokio::time::timeout(TIMEOUT, admin.recv()).await.unwrap();
        assert!(end.is_none(), "{body}: the connection stays open: {end:?}");
    }
    // Only the devices the host paired exist.
    let resp = local_call(&server, |id| LocalRequest::Api {
        id,
        body: ClientMsg::ListDevices,
    })
    .await;
    let LocalResponse::Api {
        body: ServerMsg::Devices { devices },
        ..
    } = resp
    else {
        panic!("{resp:?}");
    };
    assert_eq!(devices.len(), 10);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn totp_is_required_after_enrollment() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), true).await;
    let resp = local_verified(&server, |id, step_up| LocalRequest::TotpBegin {
        id,
        step_up,
        account: "owner".into(),
    })
    .await;
    let LocalResponse::TotpPending { secret_base32, .. } = resp else {
        panic!("{resp:?}");
    };
    // The pending enrollment survives across control connections.
    let code = totp_now(&secret_base32);
    let resp = local_verified(&server, |id, step_up| LocalRequest::TotpConfirm {
        id,
        code: code.clone(),
        current_code: None,
        step_up,
    })
    .await;
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

async fn totp_status(server: &Server) -> (bool, Option<u64>, Option<String>, usize) {
    let resp = local_call(server, |id| LocalRequest::RemoteStatus { id }).await;
    let LocalResponse::RemoteStatus {
        totp_enrolled,
        totp_enrolled_at,
        totp_issuer,
        backup_codes_remaining,
        ..
    } = resp
    else {
        panic!("{resp:?}");
    };
    (
        totp_enrolled,
        totp_enrolled_at,
        totp_issuer,
        backup_codes_remaining,
    )
}

async fn totp_begin(server: &Server) -> String {
    let resp = local_verified(server, |id, step_up| LocalRequest::TotpBegin {
        id,
        step_up,
        account: "owner".into(),
    })
    .await;
    let LocalResponse::TotpPending {
        secret_base32,
        otpauth_uri,
        ..
    } = resp
    else {
        panic!("{resp:?}");
    };
    // The entry the apps name in their code prompt.
    assert!(
        otpauth_uri.starts_with("otpauth://totp/Voice%20Command%20AI:owner?"),
        "{otpauth_uri}"
    );
    secret_base32
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_sees_the_enrollment_and_can_set_it_up_again() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    assert_eq!(totp_status(&server).await, (false, None, None, 0));
    let resp = local_verified(&server, |id, step_up| LocalRequest::TotpBackupCodes {
        id,
        current_code: None,
        step_up,
    })
    .await;
    assert!(
        matches!(
            resp,
            LocalResponse::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ),
        "{resp:?}"
    );

    let first = totp_begin(&server).await;
    let code = totp_now(&first);
    let resp = local_verified(&server, |id, step_up| LocalRequest::TotpConfirm {
        id,
        code: code.clone(),
        current_code: None,
        step_up,
    })
    .await;
    let LocalResponse::TotpEnrolled { backup_codes, .. } = resp else {
        panic!("{resp:?}");
    };
    let (enrolled, at, issuer, remaining) = totp_status(&server).await;
    assert!(enrolled && at.is_some());
    assert_eq!(issuer.as_deref(), Some("Voice Command AI"));
    assert_eq!(remaining, 10);

    // Setting up again: nothing changes until the new code confirms.
    let second = totp_begin(&server).await;
    assert_ne!(first, second);
    let resp = local_verified(&server, |id, step_up| LocalRequest::TotpConfirm {
        id,
        code: "12345x".into(),
        current_code: Some(vca_server::security::Sensitive(backup_codes[0].clone())),
        step_up,
    })
    .await;
    assert!(
        matches!(
            resp,
            LocalResponse::Error {
                code: ErrorCode::Unauthorized,
                ..
            }
        ),
        "{resp:?}"
    );
    assert_eq!(totp_status(&server).await.1, at);
    // The mistyped code spent nothing: the backup code still proves it.
    let code = totp_now(&second);
    let proof = vca_server::security::Sensitive(backup_codes[0].clone());
    let resp = local_verified(&server, |id, step_up| LocalRequest::TotpConfirm {
        id,
        code: code.clone(),
        current_code: Some(proof.clone()),
        step_up,
    })
    .await;
    let LocalResponse::TotpEnrolled {
        backup_codes: enrolled_codes,
        ..
    } = resp
    else {
        panic!("{resp:?}");
    };

    // New backup codes, shown once.
    let proof = vca_server::security::Sensitive(enrolled_codes[0].clone());
    let resp = local_verified(&server, |id, step_up| LocalRequest::TotpBackupCodes {
        id,
        current_code: Some(proof.clone()),
        step_up,
    })
    .await;
    let LocalResponse::TotpEnrolled { backup_codes, .. } = resp else {
        panic!("{resp:?}");
    };
    assert_eq!(backup_codes.len(), 10);
    assert_eq!(totp_status(&server).await.3, 10);

    // Audited, without secrets or codes.
    let audit = std::fs::read_to_string(dir.path().join("data/remote/audit.log")).unwrap();
    assert!(
        audit.contains(r#""event":"totp_enrolled","by":"local","replaced":false"#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""event":"totp_enrolled","by":"local","replaced":true"#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""event":"backup_codes_regenerated""#),
        "{audit}"
    );
    assert!(!audit.contains(&second) && !audit.contains(&backup_codes[0]));

    // Paired devices only speak the API, which has no TOTP administration:
    // these exist on the owner-only local channel alone.
    for op in ["totp_begin", "totp_confirm", "totp_backup_codes"] {
        let msg = serde_json::json!({ "type": op, "account": "owner", "code": "123456" });
        assert!(serde_json::from_value::<ClientMsg>(msg).is_err(), "{op}");
    }
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
    let reset = local_verified(&server, |id, step_up| LocalRequest::KillSwitchReset {
        id,
        step_up,
    })
    .await;
    assert!(matches!(reset, LocalResponse::Done { .. }), "{reset:?}");
    let payload4 = pairing(&server, "command").await;
    assert!(pair(&payload4, "tablet", None).await.is_ok());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_codes_carry_the_provider_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_with(dir.path(), false, |c| {
        c.remote.provider.kind = ProviderKind::Custom;
        c.remote.provider.url = Some("https://vca-k3x9q2.example.net".into());
    })
    .await;
    // These endpoints leave the LAN, so pairing needs an authenticator.
    enroll(&server).await;
    let payload = pairing(&server, "command").await;
    assert_eq!(
        payload.endpoints,
        vec!["wss://vca-k3x9q2.example.net/v1/ws"]
    );
    let status = local_call(&server, |id| LocalRequest::RemoteStatus { id }).await;
    let LocalResponse::RemoteStatus {
        provider,
        provider_state,
        ..
    } = status
    else {
        panic!("unexpected {status:?}");
    };
    assert_eq!(provider.as_deref(), Some("Custom"));
    assert!(provider_state.unwrap_or_default().starts_with("active"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_providers_refuse_a_loopback_listener() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.discovery.enabled = false;
    config.remote.bind = "127.0.0.1:0".into();
    config.remote.provider.kind = ProviderKind::Lan;
    config.broker.policy_path = Some(dir.path().join("policies.toml"));
    let err = Server::start(
        Options {
            verifier: Some(vca_server::security::verifier::FakeVerifier::available()),
            config_file: Some(dir.path().join("config.toml")),
            control: Some(control_endpoint(dir.path())),
            data_dir: Some(dir.path().join("data")),
            process_control: Some(Arc::new(NoProcessControl)),
            restore_backend: None,
        },
        Some(config),
    )
    .await
    .err()
    .expect("a LAN provider on a loopback listener must not start");
    assert!(format!("{err:#}").contains("remote.bind"), "{err:#}");
}

// --- Paired-device management: rename, scope changes, live enforcement. ---

fn is_error(resp: &ServerMsg, expected: ErrorCode) -> bool {
    matches!(resp, ServerMsg::Error { code, .. } if *code == expected)
}

fn send_text(session: &str) -> ClientMsg {
    ClientMsg::SendInput {
        session_id: session.into(),
        input: Input::Text {
            text: "hi".into(),
            submit: true,
            at_prompt: false,
        },
    }
}

fn rename_msg(id: &str, name: &str) -> ClientMsg {
    ClientMsg::RenameDevice {
        device_id: id.into(),
        name: name.into(),
    }
}

fn scope_msg(id: &str, scope: vca_core::DeviceScope) -> ClientMsg {
    ClientMsg::SetDeviceScope {
        device_id: id.into(),
        scope,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_manages_devices_and_scope_changes_apply_to_live_connections() {
    use vca_core::DeviceScope;
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    let mut phone = pair(&pairing(&server, "command").await, "phone", None)
        .await
        .unwrap();
    let mut tablet = pair(&pairing(&server, "view").await, "tablet", None)
        .await
        .unwrap();
    let mut laptop = pair(&pairing(&server, "admin").await, "laptop", None)
        .await
        .unwrap();
    let phone_id = phone.session.device_id.clone();
    let tablet_id = tablet.session.device_id.clone();
    let laptop_id = laptop.session.device_id.clone();
    let hello = ClientMsg::Hello {
        protocol_version: PROTOCOL_VERSION,
        app_version: "1.2.3".into(),
    };
    assert!(matches!(
        call(&mut laptop, 1, hello).await,
        ServerMsg::Welcome { .. }
    ));

    // View and command devices cannot manage devices.
    let resp = call(&mut phone, 1, rename_msg(&tablet_id, "x")).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    let resp = call(&mut phone, 2, scope_msg(&phone_id, DeviceScope::Admin)).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    let resp = call(&mut tablet, 1, scope_msg(&tablet_id, DeviceScope::Admin)).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");

    // The list carries scope, presence, address and app version.
    let ServerMsg::Devices { devices } = call(&mut laptop, 2, ClientMsg::ListDevices).await else {
        panic!("no device list");
    };
    assert_eq!(devices.len(), 3);
    let laptop_dev = devices.iter().find(|d| d.id == laptop_id).unwrap();
    assert!(laptop_dev.current);
    assert_eq!(laptop_dev.scope, Some(DeviceScope::Admin));
    assert_eq!(laptop_dev.app_version.as_deref(), Some("1.2.3"));
    for d in &devices {
        assert!(d.online, "{d:?}");
        assert_eq!(d.connections, 1);
        assert_eq!(d.last_ip.as_deref(), Some("127.0.0.1"));
    }
    let tablet_dev = devices.iter().find(|d| d.id == tablet_id).unwrap();
    assert_eq!(tablet_dev.scope, Some(DeviceScope::View));

    // An admin renames (trimmed) and gets the updated list back.
    let ServerMsg::Devices { devices } =
        call(&mut laptop, 3, rename_msg(&tablet_id, "  Kitchen tablet ")).await
    else {
        panic!("rename failed");
    };
    let tablet_dev = devices.iter().find(|d| d.id == tablet_id).unwrap();
    assert_eq!(tablet_dev.name, "Kitchen tablet");
    let long = "x".repeat(65);
    for (id, bad) in [(4, "   "), (5, "bell\u{7}"), (6, long.as_str())] {
        let resp = call(&mut laptop, id, rename_msg(&tablet_id, bad)).await;
        assert!(is_error(&resp, ErrorCode::InvalidRequest), "{resp:?}");
    }
    let resp = call(&mut laptop, 7, rename_msg("dev_missing", "x")).await;
    assert!(is_error(&resp, ErrorCode::NotFound), "{resp:?}");

    // Nobody changes their own scope, not even an admin.
    let resp = call(&mut laptop, 8, scope_msg(&laptop_id, DeviceScope::View)).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");

    // Upgrading the tablet applies to its open connection right away: input
    // now reaches the session layer (unknown session) instead of `Forbidden`.
    let resp = call(&mut tablet, 2, send_text("missing")).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    // A remote admin may not do it; the owner at the computer may.
    let resp = call(&mut laptop, 9, scope_msg(&tablet_id, DeviceScope::Command)).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    let widen = |step_up| LocalRequest::DeviceScope {
        id: 1,
        device_id: tablet_id.clone(),
        scope: DeviceScope::Command,
        step_up,
    };
    let mut owner = local(&server).await;
    let LocalResponse::StepUpRequired { challenge, .. } = owner.call(&widen(None)).await.unwrap()
    else {
        panic!("no challenge");
    };
    let step_up = test_step_up(&server, challenge).await;
    let LocalResponse::Api {
        body: ServerMsg::Devices { devices },
        ..
    } = owner.call(&widen(step_up)).await.unwrap()
    else {
        panic!("scope change failed");
    };
    assert_eq!(
        devices.iter().find(|d| d.id == tablet_id).unwrap().scope,
        Some(DeviceScope::Command)
    );
    let resp = call(&mut tablet, 3, send_text("missing")).await;
    assert!(is_error(&resp, ErrorCode::NotFound), "{resp:?}");

    // Downgrading the phone takes effect on its next request; it stays
    // connected with view rights.
    let resp = call(&mut phone, 3, send_text("missing")).await;
    assert!(is_error(&resp, ErrorCode::NotFound), "{resp:?}");
    let resp = call(&mut laptop, 10, scope_msg(&phone_id, DeviceScope::View)).await;
    assert!(matches!(resp, ServerMsg::Devices { .. }), "{resp:?}");
    let resp = call(&mut phone, 4, send_text("missing")).await;
    assert!(is_error(&resp, ErrorCode::Forbidden), "{resp:?}");
    let resp = call(&mut phone, 5, ClientMsg::ListSessions).await;
    assert!(matches!(resp, ServerMsg::Sessions { .. }), "{resp:?}");
    let resp = call(&mut laptop, 11, scope_msg("dev_missing", DeviceScope::View)).await;
    assert!(is_error(&resp, ErrorCode::NotFound), "{resp:?}");

    // Rename, scope changes and revocations are audited with the actor.
    let revoke = ClientMsg::RevokeDevice {
        device_id: phone_id.clone(),
    };
    assert_eq!(call(&mut laptop, 12, revoke).await, ServerMsg::Ok);
    let audit = std::fs::read_to_string(dir.path().join("data/remote/audit.log")).unwrap();
    let records: Vec<serde_json::Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let find = |event: &str, device: &str| {
        records
            .iter()
            .find(|r| r["event"] == event && r["device_id"] == device)
            .cloned()
            .unwrap_or_else(|| panic!("no {event} for {device} in {audit}"))
    };
    let renamed = find("device_renamed", &tablet_id);
    assert_eq!(renamed["by"], laptop_id.as_str());
    assert_eq!(renamed["from"], "tablet");
    assert_eq!(renamed["to"], "Kitchen tablet");
    let scoped = find("device_scope_changed", &phone_id);
    assert_eq!(scoped["by"], laptop_id.as_str());
    assert_eq!(scoped["from"], "command");
    assert_eq!(scoped["to"], "view");
    find("device_revoked", &phone_id);
    server.shutdown().await;
}

/// A fake DNSPilot Agent local API that answers `GET /v1/status` (signed
/// in). Returns the location for `remote.provider.dnspilot.local_api`.
async fn fake_dnspilot_agent(dir: &Path) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn answer<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(mut s: S) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let body = if head.starts_with("GET /v1/token ") {
            r#"{"token":"dpl_test"}"#
        } else if head.starts_with("GET /v1/tunnels ") {
            r#"{"tunnels":[{"id":12,"hostname":"vca-k3x9q2ab4m.example.com","local_port":47800,"local_scheme":"http","access_mode":"app","server_status":"active","paused":false,"cert":"ready","serving":true,"state":"serving","message":""}]}"#
        } else {
            r#"{"version":"1.4.0","logged_in":true,"device_id":3,"device_name":"Desk","relay":"relay-kr-1.dnspilot.kr","tunnels_online":1,"killed":false,"boot_id":"0123456789abcdef"}"#
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(resp.as_bytes()).await;
        let _ = s.shutdown().await;
    }

    #[cfg(unix)]
    {
        let socket = dir.join("dp.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                tokio::spawn(answer(s));
            }
        });
        socket.to_string_lossy().into_owned()
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ServerOptions;
        let _ = dir;
        let name = format!(
            r"\\.\pipe\vca-test-dnspilot-remote-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let pipe = name.clone();
        tokio::spawn(async move {
            while server.connect().await.is_ok() {
                let conn = server;
                server = ServerOptions::new().create(&pipe).unwrap();
                tokio::spawn(answer(conn));
            }
        });
        name
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dnspilot_pairing_codes_carry_the_tunnel_and_vcad_stays_on_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let agent = fake_dnspilot_agent(dir.path()).await;
    let server = start_with(dir.path(), false, |c| {
        c.remote.provider.kind = ProviderKind::Dnspilot;
        c.remote.provider.dnspilot.hostname = Some("vca-k3x9q2ab4m.example.com".into());
        c.remote.provider.dnspilot.local_api = Some(agent);
    })
    .await;
    // These endpoints leave the LAN, so pairing needs an authenticator.
    enroll(&server).await;
    let payload = pairing(&server, "command").await;
    assert_eq!(
        payload.endpoints,
        vec!["wss://vca-k3x9q2ab4m.example.com/v1/ws"]
    );
    let status = local_call(&server, |id| LocalRequest::RemoteStatus { id }).await;
    let LocalResponse::RemoteStatus {
        listen,
        provider,
        provider_state,
        provider_diagnostics,
        ..
    } = status
    else {
        panic!("unexpected {status:?}");
    };
    assert!(listen.unwrap().starts_with("127.0.0.1:"));
    assert_eq!(provider.as_deref(), Some("DNSPilot"));
    assert!(
        provider_state
            .unwrap_or_default()
            .starts_with("active (wss://vca-k3x9q2ab4m.example.com)")
    );
    assert!(
        provider_diagnostics
            .iter()
            .any(|d| d.contains("vca-k3x9q2ab4m.example.com")),
        "{provider_diagnostics:?}"
    );
    // The tunnel state comes from the Agent's tunnel list (on Windows the
    // fake pipe is not the DNSPilotAgent service, so vcad refuses to read it).
    if cfg!(unix) {
        assert!(
            provider_diagnostics
                .iter()
                .any(|d| d.contains("serving (certificate ready)")),
            "{provider_diagnostics:?}"
        );
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dnspilot_without_a_tunnel_explains_what_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    // Never the real Agent of this machine: a fake whose only tunnel goes to
    // another port.
    let agent = fake_dnspilot_agent(dir.path()).await;
    let server = start_with(dir.path(), false, |c| {
        c.remote.provider.kind = ProviderKind::Dnspilot;
        c.remote.provider.dnspilot.local_api = Some(agent);
    })
    .await;
    let resp = local_verified(&server, |id, step_up| LocalRequest::Pair {
        id,
        step_up,
        scope: "command".into(),
        endpoints: Vec::new(),
    })
    .await;
    let text = format!("{resp:?}");
    assert!(text.contains("DNSPilot"), "{text}");
    if cfg!(unix) {
        assert!(text.contains("no DNSPilot app-mode tunnel"), "{text}");
    } else {
        // The fake pipe is not the DNSPilotAgent service: vcad refuses it.
        assert!(text.contains("hostname"), "{text}");
    }
    // (A non-loopback bind is refused: see the `connectivity` unit tests,
    // which check it without opening a listener on every interface.)
    server.shutdown().await;
}

// --- Generic launch gate: arbitrary programs never start from a client. ---

fn generic_launch(dir: &Path, agent: AgentKind) -> ClientMsg {
    let workspace = std::fs::canonicalize(dir.join("workspace")).unwrap();
    ClientMsg::Launch {
        request: LaunchRequest {
            agent,
            project_dir: workspace.join("p"),
            create_dir: true,
            profile: None,
            args: vec!["-c".into(), "whoami".into()],
            display_name: None,
            fresh: false,
        },
    }
}

fn is_generic_refusal(resp: &ServerMsg) -> bool {
    matches!(resp, ServerMsg::Error { code: ErrorCode::InvalidRequest, message }
        if message.contains("vca run"))
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_launch_is_refused_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_with(dir.path(), false, |c| {
        c.sessions.allow_generic_launch = false
    })
    .await;
    let payload = pairing(&server, "command").await;
    let mut phone = pair(&payload, "phone", None).await.unwrap();
    let agent = || AgentKind::Generic("powershell".into());

    // Paired device with only `command` scope.
    let resp = call(&mut phone, 1, generic_launch(dir.path(), agent())).await;
    assert!(is_generic_refusal(&resp), "{resp:?}");

    // Local API client (same manager).
    let mut local_client = local(&server).await;
    let resp = local_client
        .api(generic_launch(dir.path(), agent()))
        .await
        .unwrap();
    assert!(is_generic_refusal(&resp), "{resp:?}");

    // Known agents are not affected by the gate (they may fail to spawn
    // when the CLI is not installed, but never with the refusal).
    let resp = local_client
        .api(generic_launch(dir.path(), AgentKind::ClaudeCode))
        .await
        .unwrap();
    assert!(!is_generic_refusal(&resp), "{resp:?}");

    // `vca run` (local RunRequest) keeps working.
    let run = local_call(&server, |id| LocalRequest::Run {
        id,
        request: vca_server::control::RunRequest {
            program: fake_agent().to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: dir.path().to_owned(),
            env: Vec::new(),
            size: None,
            agent: None,
            display_name: None,
        },
    })
    .await;
    assert!(matches!(run, LocalResponse::Run { .. }), "{run:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_launch_works_when_the_switch_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let server = start_with(dir.path(), false, |c| {
        c.sessions.allow_generic_launch = true
    })
    .await;
    let mut local_client = local(&server).await;
    let resp = local_client
        .api(generic_launch(
            dir.path(),
            AgentKind::Generic(fake_agent().to_string_lossy().into_owned()),
        ))
        .await
        .unwrap();
    assert!(matches!(resp, ServerMsg::Launched { .. }), "{resp:?}");
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

/// The gates sit in the server: any process on the local channel, such as
/// `vca pair` typed into a remote shell, gets nothing without verification.
#[tokio::test(flavor = "multi_thread")]
async fn gated_local_requests_need_verification() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    let needs_step_up = |resp: LocalResponse| {
        assert!(
            matches!(resp, LocalResponse::StepUpRequired { .. }),
            "{resp:?}"
        );
    };
    let forged = || Some(vca_server::security::Sensitive("0".repeat(64)));
    for step_up in [None, forged()] {
        let resp = local_call(&server, |id| LocalRequest::Pair {
            id,
            scope: "admin".into(),
            endpoints: Vec::new(),
            step_up,
        })
        .await;
        needs_step_up(resp);
    }
    needs_step_up(
        local_call(&server, |id| LocalRequest::TotpBegin {
            id,
            account: "owner".into(),
            step_up: None,
        })
        .await,
    );
    needs_step_up(
        local_call(&server, |id| LocalRequest::TotpConfirm {
            id,
            code: "123456".into(),
            current_code: None,
            step_up: None,
        })
        .await,
    );
    needs_step_up(
        local_call(&server, |id| LocalRequest::TotpBackupCodes {
            id,
            current_code: None,
            step_up: None,
        })
        .await,
    );
    needs_step_up(
        local_call(&server, |id| LocalRequest::KillSwitchReset {
            id,
            step_up: None,
        })
        .await,
    );
    // Setup of the conductor and the task inbox over the same channel.
    for body in [
        ClientMsg::ConductorStart,
        ClientMsg::ConductorAllowSession {
            session_id: "s1".into(),
            minutes: 10,
        },
        ClientMsg::ConductorDecide {
            request_id: "r1".into(),
            allow: true,
        },
    ] {
        needs_step_up(local_call(&server, |id| LocalRequest::Api { id, body }).await);
    }
    // Refusing never needs a proof.
    let deny = ClientMsg::ConductorDecide {
        request_id: "r1".into(),
        allow: false,
    };
    let resp = local_call(&server, |id| LocalRequest::Api { id, body: deny }).await;
    assert!(
        !matches!(resp, LocalResponse::StepUpRequired { .. }),
        "{resp:?}"
    );
    // With a verification the API takes it.
    let resp = local_verified(&server, |id, step_up| LocalRequest::GatedApi {
        id,
        body: ClientMsg::ConductorStart,
        step_up,
    })
    .await;
    assert!(matches!(resp, LocalResponse::Api { .. }), "{resp:?}");

    // A word that would end up in the prompt must be a real scope.
    let resp = local_call(&server, |id| LocalRequest::Pair {
        id,
        scope: "view only, nothing to worry about".into(),
        endpoints: Vec::new(),
        step_up: None,
    })
    .await;
    assert!(
        matches!(
            resp,
            LocalResponse::Error {
                code: ErrorCode::InvalidRequest,
                ..
            }
        ),
        "{resp:?}"
    );

    // Older clients send no token at all.
    let mut c = local(&server).await;
    let old = serde_json::json!({ "op": "pair", "id": 1, "scope": "admin", "endpoints": [] });
    let req: LocalRequest = serde_json::from_value(old).unwrap();
    needs_step_up(c.call(&req).await.unwrap());

    // With a verification the same request works.
    let resp = local_verified(&server, |id, step_up| LocalRequest::Pair {
        id,
        scope: "admin".into(),
        endpoints: Vec::new(),
        step_up,
    })
    .await;
    assert!(matches!(resp, LocalResponse::Pairing { .. }), "{resp:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn public_endpoints_need_an_authenticator_to_pair() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), false).await;
    let pair_with = |endpoint: &str| {
        let endpoints = vec![endpoint.to_owned()];
        move |id, step_up| LocalRequest::Pair {
            id,
            scope: "command".into(),
            endpoints: endpoints.clone(),
            step_up,
        }
    };
    let make = pair_with("wss://vca.example.com/v1/ws");
    let resp = local_verified(&server, make).await;
    assert!(
        matches!(
            resp,
            LocalResponse::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ),
        "{resp:?}"
    );
    // On the LAN, an owner who turned require_totp off may still pair.
    let make = pair_with("ws://192.168.0.10:47800/v1/ws");
    let resp = local_verified(&server, make).await;
    assert!(matches!(resp, LocalResponse::Pairing { .. }), "{resp:?}");
    server.shutdown().await;
}

async fn enroll(server: &Server) {
    let code = totp_now(&totp_begin(server).await);
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
}
