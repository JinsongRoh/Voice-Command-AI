//! DNSPilot on the device side: the TLS client certificate against a local
//! stand-in for the DNSPilot Agent (a TLS terminator that admits only a
//! pinned client key, in front of a real in-process vcad), and the setup
//! steps against a wiremock console. No live DNSPilot calls.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rustls::DistinguishedName;
use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::json;
use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope as ContainScope};
use vca_client::dnspilot::{
    CLIENT_CERT_NAME, CLIENT_KEY_NAME, CreateTunnelRequest, DnsPilotEnrollment, EnrollLink,
    OAUTH_TOKENS_NAME, PairingInvite, Registration, TUNNELS_FILE, app_token_name,
};
use vca_client::{ClientError, ClientHub, ClientOptions};
use vca_net::{KeyStore, MemoryKeyStore};
use vca_providers::dnspilot::oauth::OAuthConfig;
use vca_providers::dnspilot::spki_der_from_pem;
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};
use vca_server::daemon::{Options, Server};
use wiremock::matchers::{body_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
            r"\\.\pipe\vca-test-{}-dnspilot-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        let id = uuid::Uuid::new_v4().simple().to_string();
        dir.join(format!("c{}.sock", &id[..8]))
    }
}

async fn start_vcad(dir: &Path) -> (Server, u16) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config {
        workspace_roots: vec![workspace],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.bind = format!("127.0.0.1:{port}");
    config.remote.require_totp = false;
    config.broker.policy_path = Some(dir.join("policies.toml"));
    let server = Server::start(
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
    .expect("vcad failed to start");
    (server, port)
}

async fn pairing_uri(server: &Server, endpoint: &str) -> String {
    let mut c = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let request = |id, step_up| LocalRequest::Pair {
        id,
        step_up,
        scope: "command".into(),
        endpoints: vec![endpoint.to_owned()],
    };
    let id = c.next_id();
    let LocalResponse::StepUpRequired { challenge, .. } = c.call(&request(id, None)).await.unwrap()
    else {
        panic!("no challenge");
    };
    let step_up = test_step_up(server, challenge).await;
    let id = c.next_id();
    let resp = c.call(&request(id, step_up)).await.unwrap();
    let LocalResponse::Pairing { uri, .. } = resp else {
        panic!("pairing failed: {resp:?}");
    };
    uri
}

/// Admits only a client certificate that carries the pinned
/// SubjectPublicKeyInfo, like the DNSPilot Agent in app mode (the
/// signature check proves possession of the key).
#[derive(Debug)]
struct PinnedClientKey {
    spki_der: Vec<u8>,
    provider: Arc<CryptoProvider>,
    denied: Arc<AtomicUsize>,
}

impl ClientCertVerifier for PinnedClientKey {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        if end_entity
            .as_ref()
            .windows(self.spki_der.len())
            .any(|w| w == self.spki_der.as_slice())
        {
            Ok(ClientCertVerified::assertion())
        } else {
            self.denied.fetch_add(1, Ordering::SeqCst);
            Err(rustls::Error::General("unknown client key".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Starts the Agent stand-in on 127.0.0.1; returns its port, the server
/// certificate (to trust) and the denied-handshake counter.
async fn start_agent_standin(
    pinned_spki: Vec<u8>,
    vcad_port: u16,
) -> (u16, Vec<u8>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let cert_der = cert.der().to_vec();
    let denied = Arc::new(AtomicUsize::new(0));
    let admitted = Arc::new(AtomicUsize::new(0));
    let config = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(Arc::new(PinnedClientKey {
            spki_der: pinned_spki,
            provider,
            denied: Arc::clone(&denied),
        }))
        .with_single_cert(
            vec![CertificateDer::from(cert_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let admitted2 = Arc::clone(&admitted);
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let admitted = Arc::clone(&admitted2);
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                admitted.fetch_add(1, Ordering::SeqCst);
                let Ok(mut upstream) =
                    tokio::net::TcpStream::connect(("127.0.0.1", vcad_port)).await
                else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut upstream).await;
            });
        }
    });
    (port, cert_der, denied, admitted)
}

fn options(extra_root: Vec<u8>, console: Option<&str>) -> ClientOptions {
    let mut o = ClientOptions {
        connect_timeout: Duration::from_secs(5),
        extra_root_certs: vec![extra_root],
        ..ClientOptions::default()
    };
    if let Some(url) = console {
        o.dnspilot = OAuthConfig {
            console_url: url.to_owned(),
            interval_unit: Duration::from_millis(5),
            ..OAuthConfig::default()
        };
    }
    o
}

#[tokio::test(flavor = "multi_thread")]
async fn pairs_through_an_app_mode_tunnel_with_the_client_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let (server, vcad_port) = start_vcad(dir.path()).await;

    // The app's identity, created in the key store.
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    let hub = ClientHub::open_with(
        dir.path().join("phone"),
        Arc::clone(&keys),
        ClientOptions::default(),
    )
    .unwrap();
    let identity = hub.dnspilot().identity().unwrap();
    assert!(keys.load(CLIENT_KEY_NAME).unwrap().is_some());
    assert!(keys.load(CLIENT_CERT_NAME).unwrap().is_some());
    drop(hub);

    let spki = spki_der_from_pem(&identity.public_key_pem).unwrap();
    let (port, server_cert, denied, admitted) = start_agent_standin(spki, vcad_port).await;
    let endpoint = format!("wss://127.0.0.1:{port}/v1/ws");

    // The tunnel host is known: the certificate is presented and pairing
    // works through the "Agent".
    let hub = ClientHub::open_with(
        dir.path().join("phone"),
        Arc::clone(&keys),
        options(server_cert.clone(), None),
    )
    .unwrap();
    hub.dnspilot()
        .add_tunnel("127.0.0.1", 443, Some(1))
        .unwrap();
    let machine = hub
        .pair(
            &pairing_uri(&server, &endpoint).await,
            "phone",
            "android",
            None,
        )
        .await
        .expect("pairing through the tunnel");
    assert_eq!(machine.endpoints, vec![endpoint.clone()]);
    assert!(admitted.load(Ordering::SeqCst) >= 1);
    assert_eq!(denied.load(Ordering::SeqCst), 0);

    // Another install (different key) is turned away during the TLS
    // handshake, before vcad sees anything.
    let other_keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    let other = ClientHub::open_with(
        dir.path().join("other"),
        other_keys,
        options(server_cert.clone(), None),
    )
    .unwrap();
    other.dnspilot().add_tunnel("127.0.0.1", 443, None).unwrap();
    let err = other
        .pair(
            &pairing_uri(&server, &endpoint).await,
            "intruder",
            "android",
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Unreachable(_)), "{err:?}");
    assert_eq!(denied.load(Ordering::SeqCst), 1);

    // A host that is not a known tunnel never gets the certificate.
    let plain = ClientHub::open_with(
        dir.path().join("plain"),
        Arc::new(MemoryKeyStore::default()),
        options(server_cert, None),
    )
    .unwrap();
    assert!(
        plain
            .pair(
                &pairing_uri(&server, &endpoint).await,
                "plain",
                "android",
                None
            )
            .await
            .is_err()
    );
    // Nothing secret is in the metadata files.
    let meta = std::fs::read_to_string(dir.path().join("phone").join(TUNNELS_FILE)).unwrap();
    assert!(!meta.contains("PRIVATE") && meta.contains("127.0.0.1"));
    server.shutdown().await;
}

/// A tunnel object as the console returns it (production shape).
fn tunnel_json(id: i64, hostname: &str, device_id: i64) -> serde_json::Value {
    json!({
        "id": id, "hostname": hostname, "zone_id": 7, "name": hostname.split('.').next(),
        "device_id": device_id, "device": "Desk", "device_status": "online", "local_port": 47800,
        "local_scheme": "http", "access_mode": "app", "public_port": 443, "status": "active",
        "suspend_reason": null, "suspended_by": null, "suspended_at": null,
        "certificate_id": 4, "key_mode": "agent", "url": format!("https://{hostname}"),
        "session_hours": 12, "session_hours_set": false, "app_token_allowed": false,
        "free_host_id": null, "created_by": "owner@example.com",
        "created_at": "2026-10-30T00:00:00Z", "last_conn_at": null, "month_bytes": 0,
        "month_conns": 0
    })
}

fn client_json(id: i64, tunnel_id: i64, spki: &str) -> serde_json::Value {
    json!({"id": id, "tunnel_id": tunnel_id, "spki_sha256": spki, "label": "Voice Command AI app",
        "source": "oauth", "created_by": "owner@example.com", "created_at": "2026-10-30T00:00:00Z",
        "revoked_at": null, "last_seen_at": null, "status": "active"})
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_dnspilot_flow_against_a_mock_console() {
    let console = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .and(body_string_contains("client_id=vca"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc", "user_code": "BCDF-GHJK",
            "verification_uri": format!("{}/oauth/activate", console.uri()),
            "verification_uri_complete": format!("{}/oauth/activate?user_code=BCDF-GHJK", console.uri()),
            "expires_in": 600, "interval": 1
        })))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("device_code=dc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dpo_at_1", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "dpo_rt_1", "scope": "tunnels:read tunnels:write devices:read"
        })))
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/agent-devices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": 3, "name": "Desk", "hostname": "desk", "os": "windows", "agent_version": "1.4.0",
                 "flavor": "windows", "status": "active", "online": true,
                 "last_seen_at": "2026-10-30T00:00:00Z", "tunnel_create_allowed": false},
                {"id": 4, "name": "Laptop", "os": "linux", "status": "active", "online": false},
                {"id": 5, "name": "Old", "status": "revoked", "online": false}
            ],
            "limit": 5, "used": 3, "flavors": ["windows", "linux"]
        })))
        .mount(&console)
        .await;
    // No app-mode tunnel yet while the tunnel is created.
    Mock::given(method("GET"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [], "paid": true})))
        .up_to_n_times(3)
        .mount(&console)
        .await;
    // The zones call is not live yet: 404 → typed-in zone id.
    Mock::given(method("POST"))
        .and(path("/api/tunnels"))
        .and(header("authorization", "Bearer dpo_at_1"))
        .and(body_string_contains("\"device_id\":3"))
        .respond_with(ResponseTemplate::new(201).set_body_json(tunnel_json(
            12,
            "vca-new.example.com",
            3,
        )))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels"))
        .and(body_string_contains("\"device_id\":4"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"detail": "Device not allowed for this app."})),
        )
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .up_to_n_times(1)
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(201).set_body_json(client_json(5, 12, "x")))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                tunnel_json(12, "vca-new.example.com", 3),
                tunnel_json(13, "vca-manual.example.com", 3),
                {"id": 14, "hostname": "shop.example.com", "access_mode": "password", "status": "active"}
            ],
            "paid": true, "public_allowed": false, "limit": 10, "used": 3, "quota": {},
            "public_ports": [443, 9443], "relay_ready": true
        })))
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/13"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tunnel_json(
            13,
            "vca-manual.example.com",
            3,
        )))
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/13/app-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/13/app-clients"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({"detail": "Not this app's tunnel."})),
        )
        .mount(&console)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/tunnels/12/app-token-allowed"))
        .and(body_json(json!({"allowed": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token_allowed": true})))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-tokens"))
        .and(body_json(
            json!({"expires_days": 7, "label": "Voice Command AI app"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 8, "tunnel_id": 12, "label": "Voice Command AI app", "prefix": "dpt_app_x",
            "expires_at": "2026-11-06T00:00:00Z", "revoked_at": null,
            "created_by": "owner@example.com", "created_at": "2026-10-30T00:00:00Z",
            "last_used_at": null, "status": "active", "token": "dpt_app_secret"
        })))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(body_string_contains("token=dpo_rt_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&console)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    let hub = ClientHub::open_with(
        dir.path().join("phone"),
        Arc::clone(&keys),
        options(Vec::new(), Some(&console.uri())),
    )
    .unwrap();
    let dp = hub.dnspilot();
    assert!(!dp.status().unwrap().connected);
    let req = CreateTunnelRequest {
        device_id: 3,
        zone_id: 7,
        local_port: None,
    };
    // Creating a tunnel needs the authorization first.
    assert_eq!(
        dp.create_tunnel(&req).await.unwrap_err().code(),
        "dnspilot_not_connected"
    );

    let prompt = dp.start_authorization().await.unwrap();
    assert_eq!(prompt.user_code, "BCDF-GHJK");
    assert_eq!(
        dp.activation_url().as_deref(),
        Some(format!("{}/oauth/activate?user_code=BCDF-GHJK", console.uri()).as_str())
    );
    let status = dp.wait_for_authorization().await.unwrap();
    assert!(status.connected);
    assert!(!status.pending);
    let stored =
        String::from_utf8(keys.load(OAUTH_TOKENS_NAME).unwrap().unwrap().to_vec()).unwrap();
    assert!(stored.contains("dpo_rt_1"));

    // Device picker: active devices only.
    let devices = dp.devices().await.unwrap();
    assert_eq!(
        devices.iter().map(|d| (d.id, d.online)).collect::<Vec<_>>(),
        vec![(3, true), (4, false)]
    );
    assert_eq!(devices[0].name, "Desk");
    assert_eq!(devices[0].os.as_deref(), Some("windows"));
    assert_eq!(dp.zones().await.unwrap(), None);

    // A PC that was not picked on approval.
    let refused = dp
        .create_tunnel(&CreateTunnelRequest {
            device_id: 4,
            ..req.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "dnspilot_device_not_allowed");
    assert!(refused.to_string().contains("select it"), "{refused}");

    // Create: id and hostname come from the response, and the key is
    // registered right away.
    let tunnel = dp.create_tunnel(&req).await.unwrap();
    assert_eq!(tunnel.hostname, "vca-new.example.com");
    assert_eq!(tunnel.tunnel_id, Some(12));
    assert_eq!(tunnel.status.as_deref(), Some("active"));
    assert!(tunnel.created_by_app && tunnel.client_registered);
    let reqs = console.received_requests().await.unwrap();
    let create = reqs
        .iter()
        .find(|r| r.url.path() == "/api/tunnels" && r.method.as_str() == "POST")
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(body["access_mode"], "app");
    assert_eq!(body["local_port"], 47800);
    assert_eq!(body["local_scheme"], "http");
    assert_eq!(body["zone_id"], 7);
    assert!(body["name"].as_str().unwrap().starts_with("vca-"));
    let posted = reqs
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/api/tunnels/12/app-clients")
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&posted.body).unwrap();
    assert_eq!(body["spki_sha256"], dp.identity().unwrap().spki_sha256);

    // Existing app-mode tunnels can be reused; password tunnels are hidden.
    let remote = dp.remote_tunnels().await.unwrap();
    assert_eq!(
        remote.iter().map(|t| (t.id, t.known)).collect::<Vec<_>>(),
        vec![(12, true), (13, false)]
    );
    assert_eq!(remote[1].device_name.as_deref(), Some("Desk"));
    // Tunnel 13 was made in the console: the app may read it but not change it.
    match dp.use_tunnel(13).await.unwrap() {
        Registration::NeedsConsole {
            spki_sha256,
            tunnel_id,
            reason,
        } => {
            assert_eq!(spki_sha256.len(), 64);
            assert_eq!(tunnel_id, Some(13));
            assert_eq!(reason, "not_created_by_app");
        }
        other => panic!("{other:?}"),
    }
    let status = dp.refresh().await.unwrap();
    assert_eq!(status.tunnels.len(), 2);
    assert!(
        status
            .tunnels
            .iter()
            .all(|t| t.status.as_deref() == Some("active"))
    );

    // Advanced fallback: an app token, kept in the key store only.
    let t = dp.issue_app_token(&tunnel.hostname, 7).await.unwrap();
    assert!(t.has_app_token);
    assert_eq!(
        keys.load(&app_token_name(&tunnel.hostname))
            .unwrap()
            .unwrap()
            .as_slice(),
        b"dpt_app_secret"
    );
    let meta = std::fs::read_to_string(dir.path().join("phone").join(TUNNELS_FILE)).unwrap();
    assert!(!meta.contains("dpt_app_secret") && !meta.contains("dpo_"));

    dp.disconnect().await.unwrap();
    assert!(keys.load(OAUTH_TOKENS_NAME).unwrap().is_none());
    assert!(!dp.status().unwrap().connected);
}

#[tokio::test(flavor = "multi_thread")]
async fn zones_are_listed_when_the_console_offers_them() {
    let console = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": 7, "name": "example.com"}, {"id": 8, "name": "example.net"}
        ]})))
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    keys.save(
        OAUTH_TOKENS_NAME,
        json!({"access_token": "dpo_at_1", "refresh_token": "dpo_rt_1", "token_type": "Bearer",
            "expires_at": 4_000_000_000u64, "scope": "tunnels:read"})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let hub = ClientHub::open_with(
        dir.path().join("phone"),
        keys,
        options(Vec::new(), Some(&console.uri())),
    )
    .unwrap();
    let zones = hub.dnspilot().zones().await.unwrap().unwrap();
    assert_eq!(zones.len(), 2);
    assert_eq!(zones[0].name, "example.com");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_devices_allowed_on_approval_are_offered_and_used() {
    let console = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/agent-devices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": 3, "name": "Desk", "status": "active", "online": true},
            {"id": 4, "name": "Laptop", "status": "active", "online": true}
        ]})))
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "paid": true, "oauth_allowed_device_ids": [3]
        })))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    keys.save(
        OAUTH_TOKENS_NAME,
        json!({"access_token": "dpo_at_1", "refresh_token": "dpo_rt_1", "token_type": "Bearer",
            "expires_at": 4_000_000_000u64, "scope": "tunnels:read tunnels:write devices:read"})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let hub = ClientHub::open_with(
        dir.path().join("phone"),
        keys,
        options(Vec::new(), Some(&console.uri())),
    )
    .unwrap();
    let dp = hub.dnspilot();
    let devices = dp.devices().await.unwrap();
    assert_eq!(devices.iter().map(|d| d.id).collect::<Vec<_>>(), vec![3]);
    // A device outside the approval is refused before anything is created.
    let refused = dp
        .create_tunnel(&CreateTunnelRequest {
            device_id: 4,
            zone_id: 7,
            local_port: None,
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "dnspilot_device_not_allowed");
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_authorization_is_reported() {
    let console = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc", "user_code": "U", "verification_uri": "https://evil.example/oauth/activate",
            "expires_in": 600, "interval": 1
        })))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "access_denied"})))
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let hub = ClientHub::open_with(
        dir.path().join("phone"),
        Arc::new(MemoryKeyStore::default()),
        options(Vec::new(), Some(&console.uri())),
    )
    .unwrap();
    hub.dnspilot().start_authorization().await.unwrap();
    // Only URLs on the configured console are opened natively.
    assert_eq!(hub.dnspilot().activation_url(), None);
    assert_eq!(
        hub.dnspilot()
            .wait_for_authorization()
            .await
            .unwrap_err()
            .code(),
        "dnspilot_denied"
    );
}

// --- Tunnel creation rules and invite codes ---

const SCOPE: &str = "tunnels:read tunnels:write devices:read";

fn invite_code() -> String {
    format!("dpt_inv_{}", "aB3_-".repeat(8) + "xyz")
}

fn invite_link(hostname: &str, tunnel_id: i64) -> EnrollLink {
    EnrollLink::new(hostname, tunnel_id, invite_code().as_str().into()).unwrap()
}

/// A hub that is signed in to the mock console.
fn signed_in_hub(console: &MockServer, dir: &Path) -> (ClientHub, Arc<dyn KeyStore>) {
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    keys.save(
        OAUTH_TOKENS_NAME,
        json!({"access_token": "dpo_at_1", "refresh_token": "dpo_rt_1", "token_type": "Bearer",
            "expires_at": 4_000_000_000u64, "scope": SCOPE})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let hub = ClientHub::open_with(
        dir.join("hub"),
        Arc::clone(&keys),
        options(Vec::new(), Some(&console.uri())),
    )
    .unwrap();
    (hub, keys)
}

/// A phone: no DNSPilot login at all.
fn phone_hub(console: &MockServer, dir: &Path) -> ClientHub {
    ClientHub::open_with(
        dir.join("phone"),
        Arc::new(MemoryKeyStore::default()),
        options(Vec::new(), Some(&console.uri())),
    )
    .unwrap()
}

async fn mock_tunnels(console: &MockServer, items: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/api/tunnels"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items": items, "paid": true})),
        )
        .mount(console)
        .await;
}

async fn mock_no_clients(console: &MockServer, tunnel: i64) {
    Mock::given(method("GET"))
        .and(path(format!("/api/tunnels/{tunnel}/app-clients")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(console)
        .await;
}

const CREATE: CreateTunnelRequest = CreateTunnelRequest {
    device_id: 3,
    zone_id: 7,
    local_port: None,
};

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_tunnel_is_adopted_instead_of_creating_another() {
    let console = MockServer::start().await;
    mock_tunnels(
        &console,
        json!([
            tunnel_json(11, "vca-other-pc.example.com", 4),
            {"id": 12, "hostname": "shop.example.com", "access_mode": "password",
             "device_id": 3, "local_port": 47800, "status": "active"},
            tunnel_json(13, "vca-existing.example.com", 3),
        ]),
    )
    .await;
    mock_no_clients(&console, 13).await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/13/app-clients"))
        .respond_with(ResponseTemplate::new(201).set_body_json(client_json(5, 13, "x")))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    let dp = hub.dnspilot();
    let adopted = dp.create_tunnel(&CREATE).await.unwrap();
    assert_eq!(adopted.hostname, "vca-existing.example.com");
    assert_eq!(adopted.tunnel_id, Some(13));
    assert!(!adopted.created_by_app, "the app did not create this one");
    assert!(adopted.client_registered);
}

#[tokio::test(flavor = "multi_thread")]
async fn other_ports_and_modes_do_not_count_as_duplicates() {
    let console = MockServer::start().await;
    let mut other_port = tunnel_json(13, "vca-other-port.example.com", 3);
    other_port["local_port"] = json!(8080);
    mock_tunnels(
        &console,
        json!([other_port, {"id": 14, "hostname": "shop.example.com",
            "access_mode": "password", "device_id": 3, "local_port": 47800}]),
    )
    .await;
    mock_no_clients(&console, 12).await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(201).set_body_json(tunnel_json(
            12,
            "vca-new.example.com",
            3,
        )))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(201).set_body_json(client_json(5, 12, "x")))
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    assert!(
        hub.dnspilot()
            .create_tunnel(&CREATE)
            .await
            .unwrap()
            .created_by_app
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_key_registration_is_an_error_and_keeps_the_tunnel() {
    let console = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .up_to_n_times(1)
        .mount(&console)
        .await;
    mock_tunnels(&console, json!([tunnel_json(12, "vca-new.example.com", 3)])).await;
    mock_no_clients(&console, 12).await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(201).set_body_json(tunnel_json(
            12,
            "vca-new.example.com",
            3,
        )))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    let dp = hub.dnspilot();
    let err = dp.create_tunnel(&CREATE).await.unwrap_err();
    assert_eq!(err.code(), "dnspilot_key_registration_failed");
    assert!(err.to_string().contains("vca-new.example.com"), "{err}");
    // The tunnel stays recorded, as not registered.
    let tunnels = dp.tunnels();
    assert_eq!(tunnels.len(), 1);
    assert!(tunnels[0].created_by_app && !tunnels[0].client_registered);
    // A second press reaches the same tunnel: still one POST.
    assert_eq!(
        dp.create_tunnel(&CREATE).await.unwrap_err().code(),
        "dnspilot_key_registration_failed"
    );
    assert_eq!(dp.tunnels().len(), 1);
}

fn invite_json(id: i64, status: &str) -> serde_json::Value {
    json!({"id": id, "tunnel_id": 12, "label": "VCA phone", "status": status,
        "created_at": "2026-10-31T00:00:00Z", "created_by": "owner@example.com",
        "expires_at": "2026-10-31T00:10:00Z", "revoked_at": null, "used_at": null,
        "used_client_id": null})
}

fn invite_created_json(id: i64, hostname: &str) -> serde_json::Value {
    let mut v = invite_json(id, "active");
    v["code"] = json!(invite_code());
    v["hostname"] = json!(hostname);
    v["expires_in"] = json!(600);
    v["enroll_url"] = json!(format!(
        "https://dns.dnspilot.kr/enroll#v=1&h={hostname}&t=12&c={}",
        invite_code()
    ));
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn invites_are_created_listed_and_revoked() {
    let console = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-invites"))
        .and(header("authorization", "Bearer dpo_at_1"))
        .and(body_json(json!({"label": "Tablet", "ttl_sec": 900})))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(invite_created_json(9, "vca-new.example.com")),
        )
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/12/app-invites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [invite_json(9, "active"), invite_json(8, "used")], "access_mode": "app"
        })))
        .mount(&console)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/tunnels/12/app-invites/9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(invite_json(9, "revoked")))
        .expect(1)
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, keys) = signed_in_hub(&console, dir.path());
    let dp = hub.dnspilot();

    let link = dp
        .create_invite(12, Some("Tablet"), Some(900))
        .await
        .unwrap();
    assert_eq!(link.hostname, "vca-new.example.com");
    assert_eq!(link.tunnel_id, 12);
    assert_eq!(link.code.expose(), invite_code());
    assert!(!format!("{link:?}").contains(&invite_code()));
    assert_eq!(
        dp.create_invite(12, None, Some(30))
            .await
            .unwrap_err()
            .code(),
        "dnspilot_invalid"
    );

    let listed = dp.invites(12).await.unwrap();
    assert_eq!(listed.iter().map(|i| i.id).collect::<Vec<_>>(), vec![9, 8]);
    assert_eq!(listed[1].status.as_deref(), Some("used"));
    dp.revoke_invite(12, 9).await.unwrap();

    // The code is kept nowhere.
    let meta = std::fs::read_to_string(dir.path().join("hub").join(TUNNELS_FILE));
    assert!(!meta.unwrap_or_default().contains("dpt_inv_"));
    let stored = keys.load(OAUTH_TOKENS_NAME).unwrap().unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("dpt_inv_"));
}

#[tokio::test(flavor = "multi_thread")]
async fn enrolling_signs_as_documented_and_records_the_tunnel() {
    let console = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let hub = phone_hub(&console, dir.path());
    let dp = hub.dnspilot();
    let identity = dp.identity().unwrap();
    Mock::given(method("POST"))
        .and(path("/api/public/tunnel-app-enroll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tunnel_id": 12, "hostname": "VCA-new.example.com", "client_id": 41,
            "spki_sha256": identity.spki_sha256, "ready_within_sec": 30
        })))
        .expect(1)
        .mount(&console)
        .await;

    let enrollment = dp
        .enroll(&invite_link("vca-new.example.com", 12))
        .await
        .unwrap();
    assert_eq!(
        enrollment,
        DnsPilotEnrollment {
            hostname: "vca-new.example.com".into(),
            tunnel_id: 12,
            client_id: Some(41),
            ready_within_sec: 30,
        }
    );

    let reqs = console.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    // Public call: no credentials.
    assert!(!reqs[0].headers.contains_key("authorization"));
    assert_eq!(body["code"], invite_code());

    // The certificate carries the key that pins this app.
    let spki = spki_der_from_pem(&identity.public_key_pem).unwrap();
    let pem = body["cert_pem"].as_str().unwrap();
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let cert = data_encoding::BASE64.decode(b64.as_bytes()).unwrap();
    assert!(cert.windows(spki.len()).any(|w| w == spki.as_slice()));
    assert_eq!(cert, keys_cert(&hub));

    // A real ECDSA P-256/SHA-256 check over the documented message.
    let sig = data_encoding::BASE64
        .decode(body["sig"].as_str().unwrap().as_bytes())
        .unwrap();
    assert_eq!(sig[0], 0x30, "DER SEQUENCE");
    let message = format!("dpt-app-enroll:v1:{}", invite_code());
    let provider = rustls::crypto::ring::default_provider();
    let (_, verifiers) = provider
        .signature_verification_algorithms
        .mapping
        .iter()
        .find(|(scheme, _)| *scheme == SignatureScheme::ECDSA_NISTP256_SHA256)
        .unwrap();
    // SubjectPublicKeyInfo ends with the uncompressed P-256 point.
    let point = &spki[spki.len() - 65..];
    verifiers[0]
        .verify_signature(point, message.as_bytes(), &sig)
        .expect("the signature verifies with the certificate key");
    assert!(
        verifiers[0]
            .verify_signature(point, b"dpt-app-enroll:v1:other", &sig)
            .is_err()
    );

    // Recorded as registered; the certificate now goes to this host only.
    let tunnels = dp.tunnels();
    assert_eq!(tunnels.len(), 1);
    assert_eq!(tunnels[0].hostname, "vca-new.example.com");
    assert_eq!(tunnels[0].tunnel_id, Some(12));
    assert_eq!(tunnels[0].public_port, 443);
    assert!(tunnels[0].client_registered && !tunnels[0].created_by_app);
    let meta = std::fs::read_to_string(dir.path().join("phone").join(TUNNELS_FILE)).unwrap();
    assert!(!meta.contains("dpt_inv_"));
}

/// The stored certificate of a hub (through its public transport data).
fn keys_cert(hub: &ClientHub) -> Vec<u8> {
    hub.dnspilot().client_certificate().unwrap()
}

async fn mock_enroll(console: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/api/public/tunnel-app-enroll"))
        .respond_with(response)
        .mount(console)
        .await;
}

fn enroll_ok(spki: &str, ready: Option<u32>) -> ResponseTemplate {
    let mut body = json!({"tunnel_id": 12, "hostname": "vca-new.example.com", "client_id": 41,
        "spki_sha256": spki});
    if let Some(ready) = ready {
        body["ready_within_sec"] = json!(ready);
    }
    ResponseTemplate::new(200).set_body_json(body)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wait_comes_from_the_console_and_defaults_to_30_seconds() {
    for (ready, expected) in [(Some(30), 30), (Some(0), 0), (None, 30)] {
        let console = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let hub = phone_hub(&console, dir.path());
        let spki = hub.dnspilot().identity().unwrap().spki_sha256;
        mock_enroll(&console, enroll_ok(&spki, ready)).await;
        let enrollment = hub
            .dnspilot()
            .enroll(&invite_link("vca-new.example.com", 12))
            .await
            .unwrap();
        assert_eq!(enrollment.ready_within_sec, expected, "{ready:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn enrolling_again_is_idempotent() {
    let console = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let hub = phone_hub(&console, dir.path());
    let dp = hub.dnspilot();
    let spki = dp.identity().unwrap().spki_sha256;
    let link = invite_link("vca-new.example.com", 12);
    // A new code for a key that is already registered.
    Mock::given(method("POST"))
        .and(path("/api/public/tunnel-app-enroll"))
        .respond_with(enroll_ok(&spki, Some(0)))
        .up_to_n_times(2)
        .mount(&console)
        .await;
    assert_eq!(dp.enroll(&link).await.unwrap().ready_within_sec, 0);
    assert_eq!(dp.enroll(&link).await.unwrap().ready_within_sec, 0);
    assert_eq!(dp.tunnels().len(), 1);
    // The same code again: the console says it is used. The tunnel is known
    // as registered, so this succeeds; how far the Agent is stays unknown.
    mock_enroll(
        &console,
        ResponseTemplate::new(404)
            .set_body_json(json!({"detail": {"message": "No", "code": "invite_invalid"}})),
    )
    .await;
    let again = dp.enroll(&link).await.unwrap();
    assert_eq!(again.client_id, None);
    assert_eq!(again.ready_within_sec, 30);
    assert_eq!(dp.tunnels().len(), 1);
    assert!(dp.tunnels()[0].client_registered);
    // Another tunnel's used code is still an error.
    assert_eq!(
        dp.enroll(&invite_link("vca-other.example.com", 13))
            .await
            .unwrap_err()
            .code(),
        "dnspilot_invite_invalid"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_documented_enroll_error_has_a_stable_code() {
    for (status, code, expected) in [
        (404, "invite_invalid", "dnspilot_invite_invalid"),
        (400, "enroll_bad_signature", "dnspilot_enroll_bad_signature"),
        (400, "enroll_bad_cert", "dnspilot_enroll_bad_cert"),
        (400, "bad_request", "dnspilot_bad_request"),
        (409, "tunnel_not_app_mode", "dnspilot_tunnel_not_app_mode"),
        (409, "client_limit", "dnspilot_client_limit"),
    ] {
        let console = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let hub = phone_hub(&console, dir.path());
        mock_enroll(
            &console,
            ResponseTemplate::new(status).set_body_json(json!({
                "detail": {"message": format!("refused {}", invite_code()), "code": code,
                    "message_src": "x"}
            })),
        )
        .await;
        let err = hub
            .dnspilot()
            .enroll(&invite_link("vca-new.example.com", 12))
            .await
            .unwrap_err();
        assert_eq!(err.code(), expected);
        assert!(!err.to_string().contains(&invite_code()), "{err}");
        assert!(!format!("{err:?}").contains(&invite_code()), "{err:?}");
        assert!(hub.dnspilot().tunnels().is_empty(), "nothing recorded");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limits_carry_retry_after() {
    let console = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let hub = phone_hub(&console, dir.path());
    mock_enroll(
        &console,
        ResponseTemplate::new(429)
            .insert_header("Retry-After", "120")
            .set_body_json(json!({"detail": {"message": "Slow", "code": "rate_limited"}})),
    )
    .await;
    let err = hub
        .dnspilot()
        .enroll(&invite_link("vca-new.example.com", 12))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "dnspilot_rate_limited");
    assert!(
        matches!(
            err,
            ClientError::DnsPilotRateLimited {
                retry_after_secs: Some(120)
            }
        ),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_for_another_key_or_tunnel_is_refused() {
    let console = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let hub = phone_hub(&console, dir.path());
    let dp = hub.dnspilot();
    let spki = dp.identity().unwrap().spki_sha256;
    mock_enroll(&console, enroll_ok(&"0".repeat(64), Some(30))).await;
    assert_eq!(
        dp.enroll(&invite_link("vca-new.example.com", 12))
            .await
            .unwrap_err()
            .code(),
        "dnspilot_invalid"
    );
    console.reset().await;
    mock_enroll(&console, enroll_ok(&spki, Some(30))).await;
    // The link names another host than the console answers with.
    assert_eq!(
        dp.enroll(&invite_link("vca-evil.example.com", 12))
            .await
            .unwrap_err()
            .code(),
        "dnspilot_invalid"
    );
    assert!(dp.tunnels().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_never_changes_where_a_code_goes() {
    let console = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let hub = phone_hub(&console, dir.path());
    let dp = hub.dnspilot();
    let spki = dp.identity().unwrap().spki_sha256;
    mock_enroll(&console, enroll_ok(&spki, Some(0))).await;
    let code = invite_code();
    for link in [
        format!("https://evil.example/enroll#v=1&h=a.example.com&t=12&c={code}"),
        format!("http://dns.dnspilot.kr/enroll#v=1&h=a.example.com&t=12&c={code}"),
        format!("https://dns.dnspilot.kr.evil.example/enroll#v=1&h=a.example.com&t=12&c={code}"),
        format!("https://dns.dnspilot.kr/enroll#v=2&h=a.example.com&t=12&c={code}"),
        format!("https://dns.dnspilot.kr/enroll#v=1&h=a.example.com&t=12&c=dpt_app_{code}"),
        "vca://pair?v=1&k=a&h=a.example.com&t=12".to_owned(),
    ] {
        let err = dp.enroll_link(&link).await.unwrap_err();
        assert_eq!(err.code(), "dnspilot_bad_link", "{link}");
        assert!(!err.to_string().contains(&code), "{err}");
    }
    assert!(console.received_requests().await.unwrap().is_empty());

    // Valid links go to the configured console (the mock), not to their host.
    let good = format!("https://dns.dnspilot.kr/enroll#v=1&h=vca-new.example.com&t=12&c={code}");
    assert_eq!(dp.enroll_link(&good).await.unwrap().unwrap().tunnel_id, 12);
    let pairing = format!("vca://pair?v=1&k=a&h=vca-new.example.com&t=12&c={code}");
    assert!(dp.enroll_link(&pairing).await.unwrap().is_some());
    assert_eq!(console.received_requests().await.unwrap().len(), 2);
    assert_eq!(dp.enroll_link("vca://pair?v=1&k=a").await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn enrollment_only_goes_to_a_secure_console() {
    let dir = tempfile::tempdir().unwrap();
    let hub = ClientHub::open_with(
        dir.path().join("phone"),
        Arc::new(MemoryKeyStore::default()),
        options(Vec::new(), Some("http://console.example.com")),
    )
    .unwrap();
    assert_eq!(
        hub.dnspilot()
            .enroll(&invite_link("vca-new.example.com", 12))
            .await
            .unwrap_err()
            .code(),
        "dnspilot_invalid"
    );
}

/// Captures every log line of the current thread, at every level.
#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invite_codes_never_reach_the_logs() {
    let console = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-invites"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(invite_created_json(9, "vca-new.example.com")),
        )
        .mount(&console)
        .await;
    mock_enroll(
        &console,
        ResponseTemplate::new(400).set_body_json(json!({
            "detail": {"message": format!("bad {}", invite_code()), "code": "enroll_bad_cert"}
        })),
    )
    .await;
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    let endpoints = ["wss://vca-new.example.com/v1/ws".to_owned()];
    hub.dnspilot()
        .add_tunnel("vca-new.example.com", 443, Some(12))
        .unwrap();
    let PairingInvite::Included(link) = hub.dnspilot().pairing_invite(&endpoints).await else {
        panic!("no invite");
    };
    tracing::info!("the link is {link:?}");
    let err = hub.dnspilot().enroll(&link).await.unwrap_err();
    tracing::info!("the error is {err} / {err:?}");

    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(text.contains("the link is"), "the capture works");
    assert!(!text.contains(&invite_code()), "{text}");
}

fn pairing_endpoints() -> Vec<String> {
    vec!["wss://vca-new.example.com/v1/ws".to_owned()]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pairing_code_gets_a_fresh_invite_and_the_previous_one_is_revoked() {
    let console = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-invites"))
        .and(body_json(json!({"label": "VCA phone", "ttl_sec": 600})))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(invite_created_json(9, "vca-new.example.com")),
        )
        .expect(2)
        .mount(&console)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/tunnels/12/app-invites/9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(invite_json(9, "revoked")))
        .expect(1)
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    let dp = hub.dnspilot();
    dp.add_tunnel("vca-new.example.com", 443, Some(12)).unwrap();
    for _ in 0..2 {
        match dp.pairing_invite(&pairing_endpoints()).await {
            PairingInvite::Included(link) => assert_eq!(link.tunnel_id, 12),
            other => panic!("{other:?}"),
        }
    }
    // A code that does not go through a tunnel needs no invite.
    assert_eq!(
        dp.pairing_invite(&["ws://192.168.0.10:47800/v1/ws".to_owned()])
            .await,
        PairingInvite::NotApplicable
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tunnel_unknown_to_the_app_is_found_by_host_name() {
    let console = MockServer::start().await;
    mock_tunnels(&console, json!([tunnel_json(12, "vca-new.example.com", 3)])).await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-invites"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(invite_created_json(9, "vca-new.example.com")),
        )
        .expect(1)
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    assert!(matches!(
        hub.dnspilot().pairing_invite(&pairing_endpoints()).await,
        PairingInvite::Included(_)
    ));
    assert_eq!(
        hub.dnspilot()
            .pairing_invite(&["wss://vca-unknown.example.com/v1/ws".to_owned()])
            .await,
        PairingInvite::Skipped {
            reason: "not_owned"
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_invite_the_pairing_code_says_why() {
    // Not connected to DNSPilot: no token, nothing is sent.
    let console = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let hub = phone_hub(&console, dir.path());
    assert_eq!(
        hub.dnspilot().pairing_invite(&pairing_endpoints()).await,
        PairingInvite::Skipped {
            reason: "not_connected"
        }
    );
    assert!(console.received_requests().await.unwrap().is_empty());

    for (response, reason) in [
        (
            ResponseTemplate::new(403).set_body_json(json!({"detail": "Not this app's tunnel."})),
            "not_owned",
        ),
        // DNSPilot 1.27.6+ gives the same refusal a stable code.
        (
            ResponseTemplate::new(403).set_body_json(json!({"detail": {
                "message": "This OAuth grant may not change this tunnel.",
                "code": "oauth_tunnel_not_owned"
            }})),
            "not_owned",
        ),
        (
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "60")
                .set_body_json(json!({"detail": {"message": "Slow", "code": "rate_limited"}})),
            "rate_limited",
        ),
        (
            ResponseTemplate::new(409)
                .set_body_json(json!({"detail": {"message": "Full", "code": "invite_limit"}})),
            "invite_limit",
        ),
        (ResponseTemplate::new(500), "failed"),
    ] {
        let console = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/tunnels/12/app-invites"))
            .respond_with(response)
            .mount(&console)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let (hub, _) = signed_in_hub(&console, dir.path());
        hub.dnspilot()
            .add_tunnel("vca-new.example.com", 443, Some(12))
            .unwrap();
        assert_eq!(
            hub.dnspilot().pairing_invite(&pairing_endpoints()).await,
            PairingInvite::Skipped { reason }
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_coded_not_owned_refusal_asks_for_the_console_like_a_plain_one() {
    let console = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&console)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"detail": {
            "message": "This OAuth grant may not change this tunnel.",
            "code": "oauth_tunnel_not_owned"
        }})))
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    let dp = hub.dnspilot();
    dp.add_tunnel("vca-new.example.com", 443, Some(12)).unwrap();
    match dp.register_client("vca-new.example.com").await.unwrap() {
        Registration::NeedsConsole { reason, .. } => assert_eq!(reason, "not_created_by_app"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn app_clients_are_listed_and_revoked() {
    let console = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (hub, _) = signed_in_hub(&console, dir.path());
    let dp = hub.dnspilot();
    let spki = dp.identity().unwrap().spki_sha256;
    let mut phone = client_json(7, 12, &"a".repeat(64));
    phone["source"] = json!("invite");
    phone["label"] = json!("VCA phone");
    let mut gone = client_json(6, 12, &"b".repeat(64));
    gone["status"] = json!("revoked");
    gone["revoked_at"] = json!("2026-10-30T00:00:00Z");
    Mock::given(method("GET"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [phone, gone, client_json(5, 12, &spki)]
        })))
        .mount(&console)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/tunnels/12/app-clients/7"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&console)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/tunnels/12/app-clients/5"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&console)
        .await;
    dp.add_tunnel("vca-new.example.com", 443, Some(12)).unwrap();
    dp.mark_registered("vca-new.example.com").unwrap();

    let clients = dp.app_clients(12).await.unwrap();
    assert_eq!(
        clients.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![7, 5],
        "revoked clients are hidden"
    );
    assert_eq!(clients[0].source.as_deref(), Some("invite"));
    assert_eq!(clients[0].label.as_deref(), Some("VCA phone"));
    assert!(!clients[0].this_app && clients[1].this_app);

    dp.revoke_app_client(12, 7).await.unwrap();
    assert!(dp.tunnels()[0].client_registered, "another device's key");
    dp.revoke_app_client(12, 5).await.unwrap();
    assert!(!dp.tunnels()[0].client_registered, "this app's own key");
}

/// Strict rotation like the console: one valid refresh token at a time; a
/// used one presented again revokes the grant.
struct RotatingGrant {
    state: std::sync::Mutex<(String, Vec<String>, u32)>,
    refreshes: AtomicUsize,
    reused: AtomicUsize,
}

struct Rotating(Arc<RotatingGrant>);

impl wiremock::Respond for Rotating {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        let this = &self.0;
        this.refreshes.fetch_add(1, Ordering::SeqCst);
        let body = String::from_utf8_lossy(&req.body).into_owned();
        let mut s = this.state.lock().unwrap();
        if body.contains(&format!("refresh_token={}&", s.0)) || body.ends_with(&s.0) {
            s.2 += 1;
            let next = format!("dpo_rt_{}", s.2);
            let old = std::mem::replace(&mut s.0, next.clone());
            s.1.push(old);
            return ResponseTemplate::new(200)
                .set_body_json(json!({
                    "access_token": format!("dpo_at_{}", s.2), "token_type": "Bearer",
                    "expires_in": 3600, "refresh_token": next,
                    "scope": "tunnels:read tunnels:write devices:read"
                }))
                .set_delay(Duration::from_millis(200));
        }
        if s.1
            .iter()
            .any(|u| body.contains(&format!("refresh_token={u}")))
        {
            this.reused.fetch_add(1, Ordering::SeqCst);
        }
        ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"}))
    }
}

/// The incident: the DNSPilot screen loads devices, zones and tunnels at
/// once with an expired access token. Before the fix each call refreshed
/// with the same stored refresh token and DNSPilot revoked the grant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_setup_calls_refresh_the_grant_once() {
    let console = MockServer::start().await;
    let grant = Arc::new(RotatingGrant {
        state: std::sync::Mutex::new(("dpo_rt_0".into(), Vec::new(), 0)),
        refreshes: AtomicUsize::new(0),
        reused: AtomicUsize::new(0),
    });
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(Rotating(Arc::clone(&grant)))
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/agent-devices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": 3, "name": "Desk", "status": "active", "online": true}
        ]})))
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [], "paid": true})))
        .mount(&console)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": 7, "name": "x.com"}])))
        .mount(&console)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::default());
    keys.save(
        OAUTH_TOKENS_NAME,
        json!({"access_token": "dpo_at_0", "refresh_token": "dpo_rt_0", "token_type": "Bearer",
            "expires_at": 1u64, "scope": "tunnels:read tunnels:write devices:read"})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let hub = ClientHub::open_with(
        dir.path().join("pc"),
        Arc::clone(&keys),
        options(Vec::new(), Some(&console.uri())),
    )
    .unwrap();
    let dp = hub.dnspilot();
    let (devices, zones, remote, tunnels) =
        tokio::join!(dp.devices(), dp.zones(), dp.remote_tunnels(), dp.refresh());
    devices.unwrap();
    zones.unwrap();
    remote.unwrap();
    tunnels.unwrap();
    assert_eq!(grant.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(grant.reused.load(Ordering::SeqCst), 0);
    let stored: serde_json::Value =
        serde_json::from_slice(&keys.load(OAUTH_TOKENS_NAME).unwrap().unwrap()).unwrap();
    assert_eq!(stored["refresh_token"], "dpo_rt_1");
    assert!(stored.get("refresh_in_flight").is_none());
    assert!(!dp.status().unwrap().needs_reauthorization);
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
