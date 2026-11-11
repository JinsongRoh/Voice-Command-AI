//! End-to-end tests: real server and client over a localhost WebSocket.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use vca_core::{ClientMsg, Envelope, Input, PROTOCOL_VERSION, ServerMsg, SessionEvent};
use vca_net::audit::AuditEvent;
use vca_net::totp::totp_code_at;
use vca_net::{
    AuditLog, AuthFailure, ClientAuth, ClientConfig, ClientConnection, Clock, CloseReason,
    KillSwitch, ManualClock, MemoryDeviceRegistry, NetError, NetServer, PairingPayload, Scope,
    ServerConfig, ServerConnection, StaticKeypair, TokenService, TotpManager,
};
use zeroize::Zeroizing;

const T0: u64 = 1_700_000_000_000;

struct TestServer {
    server: NetServer,
    addr: SocketAddr,
    clock: Arc<ManualClock>,
    totp_secret: Vec<u8>,
    conns: mpsc::Receiver<ServerConnection>,
}

impl TestServer {
    async fn start(mut config: ServerConfig) -> Self {
        let clock = ManualClock::new(T0);
        let tokens = Arc::new(
            TokenService::new(
                Zeroizing::new(vec![42u8; 32]),
                Default::default(),
                clock.clone(),
            )
            .unwrap(),
        );
        let totp = Arc::new(TotpManager::in_memory(clock.clone()));
        let pending = totp.begin_enrollment("VCA", "owner");
        let totp_secret = data_encoding::BASE32_NOPAD
            .decode(pending.secret_base32().as_bytes())
            .unwrap();
        totp.confirm_enrollment(&pending, &totp_code_at(&totp_secret, clock.now_secs()))
            .unwrap();
        // The enrollment code consumed the current step.
        clock.advance_secs(30);

        config.server_name = "Test Desk".into();
        let server = NetServer::builder(
            StaticKeypair::generate(),
            Arc::new(MemoryDeviceRegistry::new()),
            tokens,
        )
        .config(config)
        .clock(clock.clone())
        .totp(totp)
        .audit(Arc::new(AuditLog::memory(clock.clone())))
        .kill_switch(KillSwitch::in_memory())
        .build();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, conns) = mpsc::channel(16);
        let s = server.clone();
        tokio::spawn(async move { s.serve(listener, tx).await });
        Self {
            server,
            addr,
            clock,
            totp_secret,
            conns,
        }
    }

    fn url(&self) -> String {
        format!("ws://{}/v1/ws", self.addr)
    }

    fn totp_now(&self) -> String {
        totp_code_at(&self.totp_secret, self.clock.now_secs())
    }

    fn qr(&self, scope: Scope) -> PairingPayload {
        let payload = self.server.create_pairing(scope, vec![self.url()]).unwrap();
        // Go through the textual QR form, as a phone would.
        PairingPayload::parse(&payload.to_uri()).unwrap()
    }

    fn config(&self, device: &StaticKeypair) -> ClientConfig {
        ClientConfig::new(self.url(), *self.server.public_key(), device.clone())
    }

    async fn next_conn(&mut self) -> ServerConnection {
        timeout(Duration::from_secs(5), self.conns.recv())
            .await
            .unwrap()
            .unwrap()
    }

    fn audit_contains(&self, pred: impl Fn(&AuditEvent) -> bool) -> bool {
        self.server
            .audit()
            .recent(1000)
            .iter()
            .any(|r| pred(&r.event))
    }

    /// Pairs a new device and returns it with its first connection.
    async fn pair(&mut self, scope: Scope) -> (StaticKeypair, ClientConnection, ServerConnection) {
        let qr = self.qr(scope);
        let device = StaticKeypair::generate();
        let config = ClientConfig::from_pairing(&qr, 0, device.clone()).unwrap();
        let client = ClientConnection::connect(
            config,
            ClientAuth::Pair {
                secret: qr.secret.clone(),
                device_name: "Test Phone".into(),
                platform: "test".into(),
                totp: Some(self.totp_now()),
            },
        )
        .await
        .unwrap();
        let server_side = self.next_conn().await;
        // Use a fresh TOTP step for any later login.
        self.clock.advance_secs(30);
        (device, client, server_side)
    }
}

fn auth_err(r: Result<ClientConnection, NetError>) -> AuthFailure {
    match r {
        Err(NetError::Auth(f)) => f,
        Err(e) => panic!("expected auth failure, got {e}"),
        Ok(_) => panic!("expected auth failure, got success"),
    }
}

#[tokio::test]
async fn pairing_reconnect_and_envelope_exchange() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let (device, mut client, mut server_side) = ts.pair(Scope::Command).await;

    assert_eq!(client.session.scope, Scope::Command);
    assert_eq!(client.session.server_name, "Test Desk");
    assert_eq!(server_side.device.device_id, client.session.device_id);
    assert_eq!(server_side.device.public_key, *device.public());
    assert_eq!(server_side.device.device_name, "Test Phone");
    let tokens = client
        .session
        .tokens
        .clone()
        .expect("tokens issued after pairing");

    // Client -> server.
    client
        .send(&Envelope {
            id: 1,
            body: ClientMsg::Hello {
                protocol_version: PROTOCOL_VERSION,
                app_version: "test".into(),
            },
        })
        .await
        .unwrap();
    let got = server_side.recv().await.unwrap().unwrap();
    assert_eq!(got.id, 1);
    assert!(matches!(got.body, ClientMsg::Hello { .. }));
    assert!(server_side.device.scope.permits(&got.body));

    // Server -> client.
    server_side
        .send(&Envelope {
            id: 1,
            body: ServerMsg::Welcome {
                protocol_version: PROTOCOL_VERSION,
                server_version: "0".into(),
                server_name: "Test Desk".into(),
                features: Vec::new(),
            },
        })
        .await
        .unwrap();
    let got = client.recv().await.unwrap().unwrap();
    assert!(matches!(got.body, ServerMsg::Welcome { .. }));

    assert_eq!(ts.server.active_connections().len(), 1);
    assert_eq!(client.close().await, CloseReason::Normal);
    assert_eq!(
        timeout(Duration::from_secs(2), server_side.closed())
            .await
            .unwrap(),
        CloseReason::PeerClosed
    );

    // Reconnect with the access token (no TOTP needed).
    let c2 = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::AccessToken(tokens.access_token.clone()),
    )
    .await
    .unwrap();
    assert!(c2.session.tokens.is_none());
    let s2 = ts.next_conn().await;
    assert_eq!(s2.device.device_id, client.session.device_id);
    drop(c2);

    // Reconnect with the refresh token after the access token expired.
    ts.clock.advance_secs(16 * 60);
    let expired = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::AccessToken(tokens.access_token.clone()),
    )
    .await;
    assert_eq!(auth_err(expired), AuthFailure::InvalidToken);
    let c3 = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::RefreshToken(tokens.refresh_token.clone()),
    )
    .await
    .unwrap();
    let new_tokens = c3.session.tokens.clone().unwrap();
    assert_ne!(new_tokens.refresh_token, tokens.refresh_token);

    // Registry recorded the device and last-seen time.
    let rec = ts
        .server
        .registry()
        .get(&client.session.device_id)
        .unwrap()
        .unwrap();
    assert_eq!(rec.scope, Scope::Command);
    assert!(rec.last_seen.is_some());
    assert!(ts.audit_contains(|e| matches!(e, AuditEvent::DevicePaired { .. })));
}

#[tokio::test]
async fn totp_login_and_replay_rejected() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let (device, _c, _s) = ts.pair(Scope::View).await;

    let code = ts.totp_now();
    let ok = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::Login {
            totp: Some(code.clone()),
            backup_code: None,
        },
    )
    .await
    .unwrap();
    assert!(ok.session.tokens.is_some());

    // Same code again: replay.
    let replay = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::Login {
            totp: Some(code),
            backup_code: None,
        },
    )
    .await;
    assert_eq!(auth_err(replay), AuthFailure::InvalidTotp);
    assert!(ts.audit_contains(
        |e| matches!(e, AuditEvent::AuthFailed { detail, .. } if detail.contains("already used"))
    ));

    // Missing and wrong codes.
    let missing = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::Login {
            totp: None,
            backup_code: None,
        },
    )
    .await;
    assert_eq!(auth_err(missing), AuthFailure::TotpRequired);
}

#[tokio::test]
async fn pairing_requires_totp_when_enrolled() {
    let ts = TestServer::start(ServerConfig::default()).await;
    let qr = ts.qr(Scope::Command);
    let device = StaticKeypair::generate();
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, device).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "x".into(),
            platform: "test".into(),
            totp: Some("000000".into()),
        },
    )
    .await;
    // `000000` could be valid by chance only with probability ~3e-6.
    assert_eq!(auth_err(r), AuthFailure::InvalidTotp);
    // Pairing is atomic with sign-in: nothing was registered.
    assert!(ts.server.registry().list().unwrap().is_empty());
    assert!(!ts.audit_contains(|e| matches!(e, AuditEvent::DevicePaired { .. })));
}

/// One pairing attempt with a fresh device key, as the phone app makes it.
async fn try_pair(qr: &PairingPayload, totp: Option<String>) -> Result<ClientConnection, NetError> {
    try_pair_as(qr, totp, StaticKeypair::generate()).await
}

/// A pairing try with the device key `key` (a retry of the same link keeps
/// its key).
async fn try_pair_as(
    qr: &PairingPayload,
    totp: Option<String>,
    key: StaticKeypair,
) -> Result<ClientConnection, NetError> {
    ClientConnection::connect(
        ClientConfig::from_pairing(qr, 0, key).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "Beam Pro".into(),
            platform: "android".into(),
            totp,
        },
    )
    .await
}

#[tokio::test]
async fn pairing_link_carries_the_totp_hint() {
    let ts = TestServer::start(ServerConfig::default()).await;
    assert!(ts.qr(Scope::View).totp_required);
    let off = TestServer::start(ServerConfig {
        require_totp: false,
        ..Default::default()
    })
    .await;
    assert!(!off.qr(Scope::View).totp_required);
}

#[tokio::test]
async fn missing_totp_does_not_spend_the_pairing_secret() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let qr = ts.qr(Scope::Command);
    // The phone first tries without the code (field left empty).
    let r = try_pair(&qr, None).await;
    assert_eq!(auth_err(r), AuthFailure::TotpRequired);
    assert!(ts.server.registry().list().unwrap().is_empty());
    // And with a mistyped one.
    let r = try_pair(&qr, Some("000000".into())).await;
    assert_eq!(auth_err(r), AuthFailure::InvalidTotp);
    assert!(ts.server.registry().list().unwrap().is_empty());
    // The same link with the right code still pairs.
    let ok = try_pair(&qr, Some(ts.totp_now())).await.unwrap();
    let _ = ts.next_conn().await;
    let devices = ts.server.registry().list().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, ok.session.device_id);
    assert!(ts.audit_contains(|e| matches!(e, AuditEvent::DevicePaired { .. })));
    // Now the secret is spent, even with a fresh valid code.
    ts.clock.advance_secs(30);
    let again = try_pair(&qr, Some(ts.totp_now())).await;
    assert_eq!(auth_err(again), AuthFailure::InvalidPairingSecret);
    assert_eq!(ts.server.registry().list().unwrap().len(), 1);
}

#[tokio::test]
async fn presented_secret_survives_the_qr_expiry_for_totp_retries() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let qr = ts.qr(Scope::Command);
    ts.clock.advance_secs(50);
    let phone = StaticKeypair::generate();
    // Presented in time, but the code is missing.
    let r = try_pair_as(&qr, None, phone.clone()).await;
    assert_eq!(auth_err(r), AuthFailure::TotpRequired);
    // The QR code's 60 s are over; the attempt started in time may finish
    // within 2 minutes of the first presentation, by the same device only.
    ts.clock.advance_secs(60);
    assert!(qr.is_expired(ts.clock.now_secs()));
    let stranger = try_pair(&qr, Some(ts.totp_now())).await;
    assert_eq!(auth_err(stranger), AuthFailure::InvalidPairingSecret);
    let _ok = try_pair_as(&qr, Some(ts.totp_now()), phone).await.unwrap();
    let _ = ts.next_conn().await;
    assert_eq!(ts.server.registry().list().unwrap().len(), 1);

    // A link never presented before its expiry is not re-opened.
    let stale = ts.qr(Scope::Command);
    ts.clock.advance_secs(61);
    let r = try_pair(&stale, Some(ts.totp_now())).await;
    assert_eq!(auth_err(r), AuthFailure::InvalidPairingSecret);
}

#[tokio::test]
async fn wrong_totp_codes_revoke_the_pairing_secret() {
    // A generous per-IP limit, so the per-secret limit is what triggers.
    let config = ServerConfig {
        auth_rate_limit: vca_net::RateLimitConfig {
            max_failures: 100,
            window_ms: 60_000,
            lockout_ms: 60_000,
        },
        ..Default::default()
    };
    let ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::Command);
    // Missing codes are not guesses and do not count.
    for _ in 0..3 {
        let r = try_pair(&qr, None).await;
        assert_eq!(auth_err(r), AuthFailure::TotpRequired);
    }
    for _ in 1..vca_net::MAX_PAIRING_CODE_ATTEMPTS {
        let r = try_pair(&qr, Some("000000".into())).await;
        assert_eq!(auth_err(r), AuthFailure::InvalidTotp);
    }
    let r = try_pair(&qr, Some("000000".into())).await;
    assert_eq!(auth_err(r), AuthFailure::PairingAttemptsExhausted);
    assert!(ts.audit_contains(|e| matches!(
        e,
        AuditEvent::AuthFailed {
            failure: AuthFailure::PairingAttemptsExhausted,
            ..
        }
    )));
    // Revoked: even the right code no longer pairs with this link.
    let r = try_pair(&qr, Some(ts.totp_now())).await;
    assert_eq!(auth_err(r), AuthFailure::InvalidPairingSecret);
    // No device was left behind by any of the failures.
    assert!(ts.server.registry().list().unwrap().is_empty());
    assert!(!ts.audit_contains(|e| matches!(e, AuditEvent::DevicePaired { .. })));
}

#[tokio::test]
async fn a_missing_code_does_not_count_against_the_ip() {
    let config = ServerConfig {
        auth_rate_limit: vca_net::RateLimitConfig {
            max_failures: 5,
            window_ms: 60_000,
            lockout_ms: 60_000,
        },
        ..Default::default()
    };
    let ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::Command);
    // The app's first try has no code: that is not a guess.
    for _ in 0..3 {
        let r = try_pair(&qr, None).await;
        assert_eq!(auth_err(r), AuthFailure::TotpRequired);
    }
    // Four wrong codes used to lock the IP after one blind attempt. Here the
    // per-secret limit is still what ends this link.
    for _ in 0..4 {
        let r = try_pair(&qr, Some("000000".into())).await;
        assert_eq!(auth_err(r), AuthFailure::InvalidTotp);
    }
    assert!(!ts.audit_contains(|e| matches!(e, AuditEvent::RateLimited { .. })));
    let r = try_pair(&qr, Some(ts.totp_now())).await;
    assert!(r.is_ok(), "the owner still gets in");
    let _ = ts;
}

#[tokio::test]
async fn pairing_without_totp_policy_spends_the_secret_once() {
    let mut ts = TestServer::start(ServerConfig {
        require_totp: false,
        ..Default::default()
    })
    .await;
    let qr = ts.qr(Scope::View);
    let _ok = try_pair(&qr, None).await.unwrap();
    let _ = ts.next_conn().await;
    let again = try_pair(&qr, None).await;
    assert_eq!(auth_err(again), AuthFailure::InvalidPairingSecret);
    assert_eq!(ts.server.registry().list().unwrap().len(), 1);
}

#[tokio::test]
async fn unknown_device_rejected() {
    let ts = TestServer::start(ServerConfig::default()).await;
    let stranger = StaticKeypair::generate();
    let r = ClientConnection::connect(
        ts.config(&stranger),
        ClientAuth::Login {
            totp: Some(ts.totp_now()),
            backup_code: None,
        },
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::UnknownDevice);
    let r = ClientConnection::connect(
        ts.config(&stranger),
        ClientAuth::AccessToken("vca1.x.y".into()),
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::UnknownDevice);
    assert!(ts.audit_contains(|e| matches!(
        e,
        AuditEvent::AuthFailed {
            failure: AuthFailure::UnknownDevice,
            ..
        }
    )));
}

#[tokio::test]
async fn token_of_another_device_rejected() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let (_a, ca, _sa) = ts.pair(Scope::Command).await;
    let (b, _cb, _sb) = ts.pair(Scope::Command).await;
    let stolen = ca.session.tokens.clone().unwrap();
    let r = ClientConnection::connect(ts.config(&b), ClientAuth::AccessToken(stolen.access_token))
        .await;
    assert_eq!(auth_err(r), AuthFailure::InvalidToken);
}

#[tokio::test]
async fn wrong_pairing_secret_rejected() {
    let ts = TestServer::start(ServerConfig::default()).await;
    let qr = ts.qr(Scope::Command);
    let mut wrong = qr.secret.to_vec();
    wrong[0] ^= 0xff;
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: Zeroizing::new(wrong),
            device_name: "x".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::InvalidPairingSecret);
    assert!(ts.server.registry().list().unwrap().is_empty());
    assert!(ts.audit_contains(
        |e| matches!(e, AuditEvent::AuthFailed { detail, .. } if detail.contains("wrong"))
    ));
}

#[tokio::test]
async fn expired_pairing_secret_rejected() {
    let ts = TestServer::start(ServerConfig::default()).await;
    let qr = ts.qr(Scope::Command);
    ts.clock.advance_secs(5 * 60 + 1);
    assert!(qr.is_expired(ts.clock.now_secs()));
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "x".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::InvalidPairingSecret);
    assert!(ts.audit_contains(
        |e| matches!(e, AuditEvent::AuthFailed { detail, .. } if detail.contains("expired"))
    ));
}

#[tokio::test]
async fn pairing_secret_is_single_use_and_key_cannot_pair_twice() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let qr = ts.qr(Scope::View);
    let device = StaticKeypair::generate();
    let ok = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, device.clone()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "a".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert!(ok.is_ok());
    let _ = ts.next_conn().await;
    let again = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "b".into(),
            platform: "test".into(),
            totp: None,
        },
    )
    .await;
    assert_eq!(auth_err(again), AuthFailure::InvalidPairingSecret);
    let qr2 = ts.qr(Scope::Admin);
    let twice = ClientConnection::connect(
        ClientConfig::from_pairing(&qr2, 0, device).unwrap(),
        ClientAuth::Pair {
            secret: qr2.secret.clone(),
            device_name: "a".into(),
            platform: "test".into(),
            totp: None,
        },
    )
    .await;
    assert_eq!(auth_err(twice), AuthFailure::AlreadyPaired);
}

#[tokio::test]
async fn refresh_token_reuse_revokes_family() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let (device, c, _s) = ts.pair(Scope::Command).await;
    let t1 = c.session.tokens.clone().unwrap();

    let c2 = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::RefreshToken(t1.refresh_token.clone()),
    )
    .await
    .unwrap();
    let t2 = c2.session.tokens.clone().unwrap();

    // An attacker replays the old refresh token.
    let reuse = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::RefreshToken(t1.refresh_token.clone()),
    )
    .await;
    assert_eq!(auth_err(reuse), AuthFailure::TokenReuse);
    assert!(ts.audit_contains(|e| matches!(e, AuditEvent::TokenReuseDetected { .. })));

    // The legitimate newest tokens are revoked too.
    let r = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::RefreshToken(t2.refresh_token.clone()),
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::InvalidToken);
    let r = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::AccessToken(t2.access_token.clone()),
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::InvalidToken);

    // The device itself is still paired: TOTP login starts a new family.
    let fresh = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::Login {
            totp: Some(ts.totp_now()),
            backup_code: None,
        },
    )
    .await;
    assert!(fresh.is_ok());
}

#[tokio::test]
async fn kill_switch_drops_connections_and_blocks_new_ones() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let (device, client, server_side) = ts.pair(Scope::Admin).await;
    let access = client.session.tokens.clone().unwrap().access_token;

    let started = std::time::Instant::now();
    ts.server.trip_kill_switch("test").unwrap();
    let client_reason = timeout(Duration::from_secs(1), client.closed())
        .await
        .expect("closed within 1 s");
    let server_reason = timeout(Duration::from_secs(1), server_side.closed())
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(client_reason, CloseReason::KillSwitch);
    assert_eq!(server_reason, CloseReason::KillSwitch);
    assert!(
        client
            .send(&Envelope {
                id: 2,
                body: ClientMsg::ListSessions
            })
            .await
            .is_err()
    );

    // New connections are refused while tripped.
    let r = ClientConnection::connect(ts.config(&device), ClientAuth::AccessToken(access.clone()))
        .await;
    assert!(r.is_err());
    assert!(ts.audit_contains(|e| matches!(e, AuditEvent::ConnectionRejected { .. })));

    ts.server.reset_kill_switch("test").unwrap();
    let ok = ClientConnection::connect(ts.config(&device), ClientAuth::AccessToken(access)).await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn revoking_a_device_closes_it_and_blocks_reconnect() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let (device, client, _server_side) = ts.pair(Scope::Command).await;
    let tokens = client.session.tokens.clone().unwrap();
    assert!(ts.server.revoke_device(&client.session.device_id).unwrap());
    let reason = timeout(Duration::from_secs(1), client.closed())
        .await
        .unwrap();
    assert_eq!(reason, CloseReason::DeviceRevoked);
    let r = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::AccessToken(tokens.access_token),
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::DeviceRevoked);
    let r = ClientConnection::connect(
        ts.config(&device),
        ClientAuth::Login {
            totp: Some(ts.totp_now()),
            backup_code: None,
        },
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::DeviceRevoked);
    assert!(ts.server.active_connections().is_empty());
}

#[tokio::test]
async fn large_messages_are_chunked() {
    let mut ts = TestServer::start(ServerConfig::default()).await;
    let (_device, mut client, mut server_side) = ts.pair(Scope::Command).await;

    let data: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 253) as u8).collect();
    let msg = Envelope {
        id: 0,
        body: ServerMsg::Event {
            session_id: "s1".into(),
            event: SessionEvent::Output {
                seq: 1,
                at: 0,
                data: data.clone(),
            },
        },
    };
    server_side.send(&msg).await.unwrap();
    let got = timeout(Duration::from_secs(10), client.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(got, msg);

    let text = "x".repeat(1024 * 1024);
    let up = Envelope {
        id: 5,
        body: ClientMsg::SendInput {
            session_id: "s1".into(),
            input: Input::Text {
                text,
                submit: true,
                at_prompt: false,
            },
        },
    };
    client.send(&up).await.unwrap();
    let got = timeout(Duration::from_secs(10), server_side.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(got, up);
}

#[tokio::test]
async fn oversized_message_is_refused_by_sender() {
    let config = ServerConfig {
        max_message_size: 64 * 1024,
        ..Default::default()
    };
    let mut ts = TestServer::start(config).await;
    let (_d, _client, server_side) = ts.pair(Scope::Command).await;
    let msg = Envelope {
        id: 0,
        body: ServerMsg::Event {
            session_id: "s1".into(),
            event: SessionEvent::Output {
                seq: 1,
                at: 0,
                data: vec![0; 100 * 1024],
            },
        },
    };
    assert!(matches!(
        server_side.send(&msg).await,
        Err(NetError::MessageTooLarge { .. })
    ));
}

#[tokio::test]
async fn heartbeats_keep_idle_connection_alive() {
    let config = ServerConfig {
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    let mut ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::View);
    let mut cc = ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap();
    cc.heartbeat_interval = Duration::from_millis(100);
    let mut client = ClientConnection::connect(
        cc,
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await
    .unwrap();
    let server_side = ts.next_conn().await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(client.close_reason().is_none());
    assert!(server_side.close_reason().is_none());
    server_side
        .send(&Envelope {
            id: 0,
            body: ServerMsg::Ok,
        })
        .await
        .unwrap();
    assert_eq!(client.recv().await.unwrap().unwrap().body, ServerMsg::Ok);
}

#[tokio::test]
async fn client_rejects_wrong_server_key() {
    let ts = TestServer::start(ServerConfig::default()).await;
    let mut qr = ts.qr(Scope::View);
    qr.server_key = *StaticKeypair::generate().public();
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: None,
        },
    )
    .await;
    assert!(matches!(r, Err(NetError::ServerKeyMismatch { .. })));
    // The secret was never presented, so it is still valid... but only to the real server.
    assert!(ts.server.registry().list().unwrap().is_empty());
}

#[tokio::test]
async fn failed_attempts_are_rate_limited() {
    let config = ServerConfig {
        auth_rate_limit: vca_net::RateLimitConfig {
            max_failures: 3,
            window_ms: 60_000,
            // Shorter than the 60 s pairing TTL, so the QR code outlives it.
            lockout_ms: 30_000,
        },
        ..Default::default()
    };
    let ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::View);
    for _ in 0..3 {
        let r = ClientConnection::connect(
            ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
            ClientAuth::Pair {
                secret: Zeroizing::new(vec![0; 18]),
                device_name: "p".into(),
                platform: "test".into(),
                totp: None,
            },
        )
        .await;
        assert_eq!(auth_err(r), AuthFailure::InvalidPairingSecret);
    }
    // Even the right secret is refused while locked out.
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::RateLimited);
    assert!(ts.audit_contains(|e| matches!(e, AuditEvent::RateLimited { .. })));
    // After the lockout the genuine secret works.
    ts.clock.advance_secs(31);
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert!(r.is_ok());
}

/// Failures from an address must not lock out a device that proved its key:
/// anyone can fail from a shared address, and behind a tunnel everybody
/// shares one.
#[tokio::test]
async fn a_lockout_of_the_address_leaves_paired_devices_alone() {
    let config = ServerConfig {
        auth_rate_limit: vca_net::RateLimitConfig {
            max_failures: 3,
            window_ms: 600_000,
            lockout_ms: 600_000,
        },
        ..Default::default()
    };
    let ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::View);
    let device = StaticKeypair::generate();
    let mut paired = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, device.clone()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await
    .unwrap();
    let tokens = paired.session.tokens.clone().unwrap();
    paired.close().await;

    let qr = ts.qr(Scope::View);
    for _ in 0..3 {
        let r = ClientConnection::connect(
            ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
            ClientAuth::Pair {
                secret: Zeroizing::new(vec![0; 18]),
                device_name: "x".into(),
                platform: "test".into(),
                totp: None,
            },
        )
        .await;
        assert_eq!(auth_err(r), AuthFailure::InvalidPairingSecret);
    }
    // The address is locked for strangers...
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "x".into(),
            platform: "test".into(),
            totp: None,
        },
    )
    .await;
    assert_eq!(auth_err(r), AuthFailure::RateLimited);
    // ...and not for the device, by token or by key.
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, device.clone()).unwrap(),
        ClientAuth::AccessToken(tokens.access_token.clone()),
    )
    .await;
    assert!(r.is_ok(), "the token flow stays open");
    r.unwrap().close().await;
    // A code of a new time step: the one used for pairing cannot repeat.
    ts.clock.advance_secs(30);
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, device).unwrap(),
        ClientAuth::Login {
            totp: Some(ts.totp_now()),
            backup_code: None,
        },
    )
    .await;
    assert!(r.is_ok(), "the device key flow stays open");
}

/// A peer that connects and never starts the upgrade gives its slot back.
#[tokio::test]
async fn a_silent_peer_gives_its_slot_back() {
    let config = ServerConfig {
        max_pre_auth: 1,
        upgrade_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::View);
    let url = url::Url::parse(&qr.endpoints[0]).unwrap();
    let addr = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
    let _silent = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert!(r.is_ok());
}

/// Junk must not stop the owner's real code: the secret is what counts.
#[tokio::test]
async fn wrong_secrets_do_not_lock_out_the_right_one() {
    let config = ServerConfig {
        pairing_rate_limit: vca_net::RateLimitConfig {
            max_failures: 2,
            window_ms: 600_000,
            lockout_ms: 600_000,
        },
        ..Default::default()
    };
    let ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::View);
    let mut failures = Vec::new();
    for _ in 0..3 {
        let r = ClientConnection::connect(
            ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
            ClientAuth::Pair {
                secret: Zeroizing::new(vec![0; 18]),
                device_name: "x".into(),
                platform: "test".into(),
                totp: None,
            },
        )
        .await;
        failures.push(auth_err(r));
    }
    assert_eq!(failures[2], AuthFailure::RateLimited);
    ts.clock.advance_secs(1);
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert!(r.is_ok(), "the owner's code still works");
}

/// Connections that never sign in cannot fill the slots of devices.
#[tokio::test]
async fn anonymous_connections_do_not_take_the_slots_of_devices() {
    let config = ServerConfig {
        max_connections: 1,
        max_pre_auth: 1,
        ..Default::default()
    };
    let ts = TestServer::start(config).await;
    let url = ts.qr(Scope::View).endpoints[0].clone();
    // One connection from this address sits in the handshake...
    let (_idle, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // ...and with the pool this small a second one is turned away.
    let qr = ts.qr(Scope::View);
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert!(r.is_err(), "one source holds at most its own share");
    drop(_idle);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let r = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, StaticKeypair::generate()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: Some(ts.totp_now()),
        },
    )
    .await;
    assert!(r.is_ok(), "the only slot was not taken by the stranger");
}

#[tokio::test]
async fn audit_uses_forwarded_for_from_loopback_only() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let ts = TestServer::start(ServerConfig::default()).await;
    let mut req = ts.url().into_client_request().unwrap();
    req.headers_mut()
        .insert("X-Forwarded-For", "203.0.113.50".parse().unwrap());
    req.headers_mut()
        .insert("X-Real-IP", "198.51.100.66".parse().unwrap());
    req.headers_mut()
        .insert("X-DNSPilot-Client-Id", "client-123".parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    drop(ws);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let records = ts.server.audit().recent(100);
    let attempt = records
        .iter()
        .find_map(|r| match &r.event {
            AuditEvent::ConnectionAttempt {
                client_ip,
                forwarded,
                provider_client_id,
                ..
            } => Some((*client_ip, *forwarded, provider_client_id.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(attempt.0.to_string(), "203.0.113.50");
    assert!(attempt.1);
    assert_eq!(attempt.2.as_deref(), Some("client-123"));
    assert!(
        !records
            .iter()
            .any(|r| serde_json::to_string(r).unwrap().contains("198.51.100.66"))
    );
}

#[tokio::test]
async fn login_without_totp_when_policy_allows() {
    let config = ServerConfig {
        require_totp: false,
        ..Default::default()
    };
    let mut ts = TestServer::start(config).await;
    let qr = ts.qr(Scope::View);
    let device = StaticKeypair::generate();
    let c = ClientConnection::connect(
        ClientConfig::from_pairing(&qr, 0, device.clone()).unwrap(),
        ClientAuth::Pair {
            secret: qr.secret.clone(),
            device_name: "p".into(),
            platform: "test".into(),
            totp: None,
        },
    )
    .await
    .unwrap();
    let s = ts.next_conn().await;
    assert_eq!(s.device.method, vca_net::AuthMethod::Pairing);
    assert_eq!(c.session.scope, Scope::View);
    assert!(!s.device.scope.permits(&ClientMsg::Launch {
        request: vca_core::LaunchRequest {
            agent: vca_core::AgentKind::ClaudeCode,
            project_dir: "/tmp".into(),
            create_dir: false,
            profile: None,
            args: vec![],
            display_name: None,
            fresh: false,
        }
    }));
}
