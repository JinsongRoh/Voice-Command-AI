//! Integration tests of the client hub against a real in-process vcad
//! (remote transport on loopback; no external network).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope as ContainScope};
use vca_client::{
    Backoff, ClientError, ClientHub, ClientOptions, LinkSink, LinkState, LinkStatus, MACHINES_FILE,
    device_key_name, refresh_token_name,
};
use vca_core::{ClientMsg, Envelope, PROTOCOL_VERSION, ServerMsg};
use vca_net::{FileKeyStore, KeyStore, MemoryKeyStore};
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};
use vca_server::daemon::{Options, Server};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Never touches a real process.
struct NoProcessControl;

impl ProcessControl for NoProcessControl {
    fn set_priority(&self, root: u32, _: ContainScope, _: Priority) -> ContainResult {
        Ok(vec![root])
    }
    fn suspend(&self, root: u32, _: ContainScope) -> ContainResult {
        Ok(vec![root])
    }
    fn resume(&self, root: u32, _: ContainScope) -> ContainResult {
        Ok(vec![root])
    }
    fn terminate(&self, root: u32, _: ContainScope, _: Duration) -> ContainResult {
        panic!("tests must never terminate processes through the broker ({root})")
    }
}

fn control_endpoint(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\vca-test-{}-client-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join(format!("control-{}.sock", uuid::Uuid::new_v4().simple()))
    }
}

async fn start(dir: &Path, require_totp: bool, bind: &str) -> Server {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config {
        workspace_roots: vec![workspace],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.bind = bind.into();
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

async fn local_call(server: &Server, make: impl FnOnce(u64) -> LocalRequest) -> LocalResponse {
    let mut c = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let id = c.next_id();
    c.call(&make(id)).await.unwrap()
}

async fn pairing_uri(server: &Server) -> String {
    let resp = local_call(server, |id| LocalRequest::Pair {
        id,
        scope: "command".into(),
        endpoints: Vec::new(),
    })
    .await;
    let LocalResponse::Pairing { uri, .. } = resp else {
        panic!("pairing failed: {resp:?}");
    };
    uri
}

/// TOTP code for the current step plus `steps`.
fn totp_code(secret_base32: &str, steps: i64) -> String {
    let secret = data_encoding::BASE32_NOPAD
        .decode(secret_base32.trim_end_matches('=').as_bytes())
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    vca_net::totp::totp_code_at(&secret, (now + steps * 30) as u64)
}

/// Enrolls TOTP; returns the base32 secret and the backup codes.
async fn enroll_totp(server: &Server) -> (String, Vec<String>) {
    let resp = local_call(server, |id| LocalRequest::TotpBegin {
        id,
        account: "owner".into(),
    })
    .await;
    let LocalResponse::TotpPending { secret_base32, .. } = resp else {
        panic!("{resp:?}");
    };
    // Confirm with the previous step's code (accepted as clock skew), so
    // the current and next steps stay available for pairing and login.
    let code = totp_code(&secret_base32, -1);
    let resp = local_call(server, |id| LocalRequest::TotpConfirm { id, code }).await;
    let LocalResponse::TotpEnrolled { backup_codes, .. } = resp else {
        panic!("{resp:?}");
    };
    (secret_base32, backup_codes)
}

fn options() -> ClientOptions {
    ClientOptions {
        connect_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(1),
        backoff: Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(1),
        },
    }
}

/// Records sink events into channels.
struct TestSink {
    status: mpsc::UnboundedSender<LinkStatus>,
    messages: mpsc::UnboundedSender<Vec<u8>>,
}

struct SinkRx {
    status: mpsc::UnboundedReceiver<LinkStatus>,
    messages: mpsc::UnboundedReceiver<Vec<u8>>,
}

fn sink() -> (Arc<dyn LinkSink>, SinkRx) {
    let (st, sr) = mpsc::unbounded_channel();
    let (mt, mr) = mpsc::unbounded_channel();
    (
        Arc::new(TestSink {
            status: st,
            messages: mt,
        }),
        SinkRx {
            status: sr,
            messages: mr,
        },
    )
}

impl LinkSink for TestSink {
    fn status(&self, status: LinkStatus) {
        let _ = self.status.send(status);
    }
    fn message(&self, msgpack: Vec<u8>) {
        let _ = self.messages.send(msgpack);
    }
}

impl SinkRx {
    /// Waits for a status with `state`; returns it.
    async fn wait_state(&mut self, state: LinkState) -> LinkStatus {
        let mut seen = Vec::new();
        let r = tokio::time::timeout(TIMEOUT, async {
            loop {
                let s = self.status.recv().await.expect("sink dropped");
                seen.push(s.clone());
                if s.state == state {
                    return s;
                }
            }
        })
        .await;
        r.unwrap_or_else(|_| panic!("no {state:?} status; seen {seen:?}"))
    }

    /// Waits for the server envelope with `id`.
    async fn wait_reply(&mut self, id: u64) -> (Vec<u8>, Envelope<ServerMsg>) {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let bytes = self.messages.recv().await.expect("sink dropped");
                let env: Envelope<ServerMsg> = rmp_serde::from_slice(&bytes).unwrap();
                if env.id == id {
                    return (bytes, env);
                }
            }
        })
        .await
        .expect("no reply")
    }
}

fn hello(id: u64) -> Vec<u8> {
    rmp_serde::to_vec_named(&Envelope {
        id,
        body: ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: "test".into(),
        },
    })
    .unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn load_text(keys: &dyn KeyStore, name: &str) -> Option<String> {
    keys.load(name)
        .unwrap()
        .map(|b| String::from_utf8(b.to_vec()).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_persist_connect_send_and_rotate() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(&dir.path().join("server"), false, "127.0.0.1:0").await;
    let client_dir = dir.path().join("client");
    let keys: Arc<FileKeyStore> = Arc::new(FileKeyStore::new(dir.path().join("keys")));

    let hub = ClientHub::open_with(&client_dir, keys.clone(), options()).unwrap();
    assert!(hub.machines().is_empty());
    let bad = hub.pair("vca://pair?v=1", "phone", "test", None).await;
    assert!(
        matches!(bad, Err(ClientError::InvalidPairing(_))),
        "{bad:?}"
    );

    let uri = pairing_uri(&server).await;
    let info = hub.pair(&uri, "phone", "test", None).await.unwrap();
    assert_eq!(hub.machines(), vec![info.clone()]);
    assert_eq!(info.scope, vca_client::Scope::Command);
    assert!(!info.device_id.is_empty());
    // The pairing code is single use.
    let again = hub.pair(&uri, "phone", "test", None).await;
    assert_eq!(again.unwrap_err().code(), "invalid_pairing");
    assert_eq!(hub.machines(), vec![info.clone()]);

    // Metadata survives a reopen.
    drop(hub);
    let hub = ClientHub::open_with(&client_dir, keys.clone(), options()).unwrap();
    assert_eq!(hub.machines(), vec![info.clone()]);
    assert_eq!(hub.status(&info.id), LinkStatus::disconnected());

    // Secrets are in the key store only, never in machines.json.
    let rt1 = load_text(keys.as_ref(), &refresh_token_name(&info.id)).unwrap();
    let dev_key = keys.load(&device_key_name(&info.id)).unwrap().unwrap();
    let dev_key_file = std::fs::read_to_string(
        keys.dir()
            .join(format!("{}.key", device_key_name(&info.id))),
    )
    .unwrap();
    let json = std::fs::read_to_string(client_dir.join(MACHINES_FILE)).unwrap();
    assert!(
        !json.contains(&rt1),
        "refresh token leaked into machines.json"
    );
    assert!(!json.contains(dev_key_file.trim()), "device key leaked");
    assert_eq!(dev_key.len(), 32);
    assert!(!json.to_lowercase().contains("secret"));
    assert!(!json.to_lowercase().contains("token"));

    // Connect and exchange messages.
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    rx.wait_state(LinkState::Connected).await;
    assert_eq!(hub.status(&info.id).state, LinkState::Connected);
    let rt2 = load_text(keys.as_ref(), &refresh_token_name(&info.id)).unwrap();
    assert_ne!(rt1, rt2, "the rotated refresh token must be stored");
    let json = std::fs::read_to_string(client_dir.join(MACHINES_FILE)).unwrap();
    assert!(!json.contains(&rt2));

    hub.send(&info.id, &hello(7)).await.unwrap();
    let (bytes, reply) = rx.wait_reply(7).await;
    assert!(
        matches!(reply.body, ServerMsg::Welcome { .. }),
        "{:?}",
        reply.body
    );
    // Named msgpack: the envelope is a map (fixmap marker), not an array.
    assert_eq!(bytes[0] & 0xf0, 0x80, "envelope must be a msgpack map");

    // Invalid input and unknown machines.
    let err = hub.send(&info.id, b"\xc1garbage").await.unwrap_err();
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    assert_eq!(err.code(), "protocol");
    let not_client = rmp_serde::to_vec_named(&Envelope {
        id: 1,
        body: ServerMsg::Ok,
    })
    .unwrap();
    assert_eq!(
        hub.send(&info.id, &not_client).await.unwrap_err().code(),
        "protocol"
    );
    let err = hub.send("0123456789abcdef", &hello(8)).await.unwrap_err();
    assert!(matches!(err, ClientError::NotFound(_)), "{err:?}");
    let (s2, _rx2) = sink();
    assert_eq!(
        hub.connect("0123456789abcdef", s2).unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        hub.status("nope").code.as_deref(),
        Some("not_found"),
        "unknown machines report not_found"
    );
    assert_eq!(
        hub.login("0123456789abcdef", None, None)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );

    // Disconnect, then connect again with the stored rotated token.
    hub.disconnect(&info.id);
    rx.wait_state(LinkState::Disconnected).await;
    assert_eq!(hub.status(&info.id), LinkStatus::disconnected());
    assert_eq!(
        hub.send(&info.id, &hello(9)).await.unwrap_err().code(),
        "not_connected"
    );
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    rx.wait_state(LinkState::Connected).await;
    let rt3 = load_text(keys.as_ref(), &refresh_token_name(&info.id)).unwrap();
    assert_ne!(rt2, rt3);
    hub.send(&info.id, &hello(10)).await.unwrap();
    rx.wait_reply(10).await;

    // Forget removes everything.
    hub.forget(&info.id).await.unwrap();
    rx.wait_state(LinkState::Disconnected).await;
    assert!(hub.machines().is_empty());
    assert!(keys.load(&refresh_token_name(&info.id)).unwrap().is_none());
    assert!(keys.load(&device_key_name(&info.id)).unwrap().is_none());
    assert_eq!(hub.forget(&info.id).await.unwrap_err().code(), "not_found");
    let hub = ClientHub::open_with(&client_dir, keys, options()).unwrap();
    assert!(hub.machines().is_empty());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_after_server_restart() {
    let dir = tempfile::tempdir().unwrap();
    let server_dir = dir.path().join("server");
    let bind = format!("127.0.0.1:{}", free_port());
    let server = start(&server_dir, false, &bind).await;
    let hub = ClientHub::open_with(
        dir.path().join("client"),
        Arc::new(MemoryKeyStore::new()),
        options(),
    )
    .unwrap();
    let info = hub
        .pair(&pairing_uri(&server).await, "phone", "test", None)
        .await
        .unwrap();
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    rx.wait_state(LinkState::Connected).await;

    server.shutdown().await;
    let status = rx.wait_state(LinkState::Reconnecting).await;
    assert_eq!(status.code.as_deref(), Some("unreachable"), "{status:?}");
    // Still retrying while the server is down.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(hub.status(&info.id).state, LinkState::Reconnecting);

    let server = start(&server_dir, false, &bind).await;
    rx.wait_state(LinkState::Connected).await;
    hub.send(&info.id, &hello(1)).await.unwrap();
    let (_, reply) = rx.wait_reply(1).await;
    assert!(matches!(reply.body, ServerMsg::Welcome { .. }));
    hub.disconnect(&info.id);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_device_stops_with_revoked_code() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(&dir.path().join("server"), false, "127.0.0.1:0").await;
    let hub = ClientHub::open_with(
        dir.path().join("client"),
        Arc::new(MemoryKeyStore::new()),
        options(),
    )
    .unwrap();
    let info = hub
        .pair(&pairing_uri(&server).await, "phone", "test", None)
        .await
        .unwrap();
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    rx.wait_state(LinkState::Connected).await;

    let resp = local_call(&server, |id| LocalRequest::Api {
        id,
        body: ClientMsg::RevokeDevice {
            device_id: info.device_id.clone(),
        },
    })
    .await;
    assert!(
        matches!(
            resp,
            LocalResponse::Api {
                body: ServerMsg::Ok,
                ..
            }
        ),
        "{resp:?}"
    );
    // A live connection is closed with reason `device_revoked`.
    let status = rx.wait_state(LinkState::Disconnected).await;
    assert_eq!(status.code.as_deref(), Some("revoked"), "{status:?}");
    assert_eq!(hub.status(&info.id), status);

    // A new attempt is refused by the server (`device_revoked`), and the
    // supervisor stops instead of retrying.
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    let status = rx.wait_state(LinkState::Disconnected).await;
    assert_eq!(status.code.as_deref(), Some("revoked"), "{status:?}");
    let err = hub.login(&info.id, None, None).await.unwrap_err();
    assert_eq!(err.code(), "revoked", "{err:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_switch_stops_with_kill_switch_code() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(&dir.path().join("server"), false, "127.0.0.1:0").await;
    let hub = ClientHub::open_with(
        dir.path().join("client"),
        Arc::new(MemoryKeyStore::new()),
        options(),
    )
    .unwrap();
    let info = hub
        .pair(&pairing_uri(&server).await, "phone", "test", None)
        .await
        .unwrap();
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    rx.wait_state(LinkState::Connected).await;
    let _ = local_call(&server, |id| LocalRequest::Api {
        id,
        body: ClientMsg::KillSwitch,
    })
    .await;
    let status = rx.wait_state(LinkState::Disconnected).await;
    assert_eq!(status.code.as_deref(), Some("kill_switch"), "{status:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn totp_server_requires_login_after_token_loss() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(&dir.path().join("server"), true, "127.0.0.1:0").await;
    let (secret, backup_codes) = enroll_totp(&server).await;
    let keys = Arc::new(MemoryKeyStore::new());
    let hub = ClientHub::open_with(dir.path().join("client"), keys.clone(), options()).unwrap();
    let uri = pairing_uri(&server).await;

    // Pairing needs the TOTP code on this server.
    let err = hub.pair(&uri, "phone", "test", None).await.unwrap_err();
    assert_eq!(err.code(), "totp_required", "{err:?}");
    assert!(hub.machines().is_empty());
    let uri = pairing_uri(&server).await;
    // The enrollment used the previous step; pair with the current one.
    let info = hub
        .pair(&uri, "phone", "test", Some(totp_code(&secret, 0)))
        .await
        .unwrap();

    // Refresh-token reconnects do not need TOTP.
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    rx.wait_state(LinkState::Connected).await;
    hub.disconnect(&info.id);
    rx.wait_state(LinkState::Disconnected).await;

    // An invalid refresh token needs a new sign-in.
    keys.save(&refresh_token_name(&info.id), b"not-a-token")
        .unwrap();
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    let status = rx.wait_state(LinkState::LoginRequired).await;
    assert_eq!(status.code.as_deref(), Some("auth_failed"), "{status:?}");
    assert_eq!(hub.status(&info.id).state, LinkState::LoginRequired);

    let err = hub.login(&info.id, None, None).await.unwrap_err();
    assert_eq!(err.code(), "totp_required", "{err:?}");
    // A backup code resumes the requested connection.
    hub.login(&info.id, None, Some(backup_codes[0].clone()))
        .await
        .unwrap();
    rx.wait_state(LinkState::Connected).await;
    hub.send(&info.id, &hello(3)).await.unwrap();
    rx.wait_reply(3).await;

    // A missing refresh token needs a new sign-in as well; TOTP works.
    hub.disconnect(&info.id);
    keys.delete(&refresh_token_name(&info.id)).unwrap();
    let (s, mut rx) = sink();
    hub.connect(&info.id, s).unwrap();
    rx.wait_state(LinkState::LoginRequired).await;
    // The pairing used the current step, so the next one is still unused.
    hub.login(&info.id, Some(totp_code(&secret, 1)), None)
        .await
        .unwrap();
    rx.wait_state(LinkState::Connected).await;
    hub.disconnect(&info.id);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wss_endpoints_have_a_tls_provider() {
    // Opening a hub installs the rustls provider; a `wss://` attempt must
    // fail cleanly (here: the peer drops the TCP connection) instead of
    // panicking for lack of a crypto provider.
    let dir = tempfile::tempdir().unwrap();
    let _hub = ClientHub::open(dir.path(), Arc::new(MemoryKeyStore::new())).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let server_key = *vca_net::StaticKeypair::generate().public();
    let mut config = vca_net::ClientConfig::new(
        format!("wss://127.0.0.1:{port}/v1/ws"),
        server_key,
        vca_net::StaticKeypair::generate(),
    );
    config.connect_timeout = Duration::from_secs(5);
    let result = vca_net::ClientConnection::connect(
        config,
        vca_net::ClientAuth::RefreshToken("unused".into()),
    )
    .await;
    assert!(result.is_err());
}
