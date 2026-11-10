//! DNSPilot provider tests: a wiremock console (OAuth device flow and the
//! public API) and a fake Agent on a Unix socket or named pipe. No live
//! DNSPilot calls.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use vca_providers::dnspilot::console::{ConsoleClient, public_enroll, public_status};
use vca_providers::dnspilot::contract::{
    ACCESS_MODE_APP, AppEnrollRequest, CreateAppInvite, CreateTunnel, IssueAppToken,
};
use vca_providers::dnspilot::oauth::{DeviceFlowClient, OAuthConfig, OAuthError, TokenSet};
use vca_providers::dnspilot::{ClientKey, random_hostname_label};
use vca_providers::{ProviderError, Secret};
use wiremock::matchers::{body_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn oauth_client(server: &MockServer) -> DeviceFlowClient {
    DeviceFlowClient::new(OAuthConfig {
        console_url: server.uri(),
        interval_unit: Duration::from_millis(5),
        ..OAuthConfig::default()
    })
}

fn tokens(access: &str, refresh: &str, expires_at: u64) -> TokenSet {
    serde_json::from_value(json!({
        "access_token": access, "refresh_token": refresh, "token_type": "Bearer",
        "expires_at": expires_at, "scope": "tunnels:read tunnels:write devices:read"
    }))
    .unwrap()
}

/// A tunnel object as the console returns it (production shape).
fn tunnel_json(id: i64, hostname: &str) -> serde_json::Value {
    json!({
        "id": id, "hostname": hostname, "zone_id": 7, "name": hostname.split('.').next(),
        "device_id": 3, "device": "Desk", "device_status": "online", "local_port": 47800,
        "local_scheme": "http", "access_mode": "app", "public_port": 443, "status": "active",
        "suspend_reason": null, "suspended_by": null, "suspended_at": null,
        "certificate_id": 4, "key_mode": "agent", "url": format!("https://{hostname}"),
        "session_hours": 12, "session_hours_set": false, "app_token_allowed": false,
        "free_host_id": null, "created_by": "owner@example.com",
        "created_at": "2026-10-30T00:00:00Z", "last_conn_at": null, "month_bytes": 0,
        "month_conns": 0
    })
}

#[tokio::test]
async fn console_lists_devices_tunnels_and_zones() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/agent-devices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": 3, "name": "Desk", "hostname": "desk", "os": "windows",
                 "agent_version": "1.4.0", "flavor": "windows", "status": "active", "online": true,
                 "last_seen_at": "2026-10-30T00:00:00Z", "tunnel_create_allowed": false},
                {"id": 4, "name": "Old", "status": "revoked", "online": false}
            ],
            "limit": 5, "used": 2, "flavors": ["windows", "linux"]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [tunnel_json(12, "vca-abc.example.com")], "paid": true,
            "public_allowed": false, "limit": 10, "used": 1, "quota": {}, "public_ports": [443, 9443],
            "relay_ready": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/zones"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([{"id": 7, "name": "example.com"}])),
        )
        .mount(&server)
        .await;
    let console = ConsoleClient::new(
        oauth_client(&server),
        tokens("dpo_at_1", "dpo_rt_1", far_future()),
    );
    let devices = console.agent_devices().await.unwrap();
    assert_eq!(devices.len(), 2);
    assert!(devices[0].is_active() && devices[0].online == Some(true));
    assert!(!devices[1].is_active());
    let list = console.tunnels().await.unwrap();
    assert_eq!(list.items[0].id, 12);
    assert_eq!(list.paid, Some(true));
    assert_eq!(
        console.zones().await.unwrap().unwrap()[0].name,
        "example.com"
    );

    // Before DNSPilot ships the zones call: 404 means "not available".
    let older = MockServer::start().await;
    let console = ConsoleClient::new(
        oauth_client(&older),
        tokens("dpo_at_1", "dpo_rt_1", far_future()),
    );
    assert_eq!(console.zones().await.unwrap(), None);
}

fn far_future() -> u64 {
    4_000_000_000
}

#[tokio::test]
async fn device_flow_honors_pending_slow_down_and_rate_limits() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .and(body_string_contains("client_id=vca"))
        .and(body_string_contains(
            "scope=tunnels%3Aread+tunnels%3Awrite+devices%3Aread",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dev-code-secret", "user_code": "BCDF-GHJK",
            "verification_uri": "https://dns.dnspilot.kr/oauth/activate",
            "verification_uri_complete": "https://dns.dnspilot.kr/oauth/activate?user_code=BCDF-GHJK",
            "expires_in": 600, "interval": 1
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"error": "authorization_pending"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "slow_down"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .and(body_string_contains("device_code=dev-code-secret"))
        .and(body_string_contains("client_id=vca"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dpo_at_1", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "dpo_rt_1", "scope": "tunnels:read tunnels:write devices:read"
        })))
        .mount(&server)
        .await;

    let client = oauth_client(&server);
    let auth = client.start().await.unwrap();
    assert_eq!(auth.user_code, "BCDF-GHJK");
    assert_eq!(
        auth.verification_uri,
        "https://dns.dnspilot.kr/oauth/activate"
    );
    assert_eq!(auth.interval_secs, 1);
    let started = std::time::Instant::now();
    let tokens = client.poll(&auth).await.unwrap();
    // 1 + 1 + 2 (Retry-After) + 6 (slow_down) interval units of 5 ms.
    assert!(
        started.elapsed() >= Duration::from_millis(50),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(tokens.access_token.expose(), "dpo_at_1");
    assert_eq!(tokens.refresh_token.expose(), "dpo_rt_1");
    assert!(tokens.has_scope("devices:read"));
    let token_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count();
    assert_eq!(token_calls, 4);
}

#[tokio::test]
async fn device_flow_denied_expired_and_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "d", "user_code": "U", "verification_uri": "https://x/oauth/activate",
            "expires_in": 600, "interval": 1
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "access_denied"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "expired_token"})))
        .mount(&server)
        .await;
    let client = oauth_client(&server);
    let auth = client.start().await.unwrap();
    assert!(matches!(
        client.poll(&auth).await,
        Err(OAuthError::AccessDenied)
    ));
    assert!(matches!(client.poll(&auth).await, Err(OAuthError::Expired)));

    // OAuth disabled on the installation: the endpoints return 404.
    let disabled = MockServer::start().await;
    assert!(matches!(
        oauth_client(&disabled).start().await,
        Err(OAuthError::Disabled)
    ));

    let scoped = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": "invalid_scope", "error_description": "p2p:write"})),
        )
        .mount(&scoped)
        .await;
    assert!(matches!(
        oauth_client(&scoped).start().await,
        Err(OAuthError::Other { code, .. }) if code == "invalid_scope"
    ));
}

#[tokio::test]
async fn refresh_rotates_and_revoke() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=dpo_rt_1"))
        .and(body_string_contains("client_id=vca"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dpo_at_2", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "dpo_rt_2", "scope": "tunnels:read"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("refresh_token=dpo_rt_old"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(body_string_contains("token=dpo_rt_2"))
        .and(body_string_contains("client_id=vca"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let client = oauth_client(&server);
    let t2 = client.refresh(&tokens("a", "dpo_rt_1", 1)).await.unwrap();
    assert_eq!(t2.access_token.expose(), "dpo_at_2");
    assert_eq!(t2.refresh_token.expose(), "dpo_rt_2");
    assert!(matches!(
        client.refresh(&tokens("a", "dpo_rt_old", 1)).await,
        Err(OAuthError::InvalidGrant)
    ));
    client.revoke(&t2.refresh_token).await.unwrap();
    let revoke = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/oauth/revoke")
        .unwrap();
    assert!(!String::from_utf8_lossy(&revoke.body).contains("token_type_hint"));
}

#[tokio::test]
async fn console_creates_app_tunnel_and_registers_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels"))
        .and(header("authorization", "Bearer dpo_at_1"))
        .and(header("accept-language", "en"))
        .and(body_json(json!({
            "device_id": 3, "zone_id": 7, "name": "vca-abc", "local_port": 47800,
            "access_mode": "app", "local_scheme": "http", "public_port": 443
        })))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(tunnel_json(12, "vca-abc.example.com")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-clients"))
        .and(body_json(
            json!({"spki_sha256": "ab".repeat(32), "label": "VCA app"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 5, "tunnel_id": 12, "label": "VCA app", "spki_sha256": "ab".repeat(32),
            "status": "active", "source": "oauth", "created_at": "2026-10-30T00:00:00Z",
            "created_by": "me", "last_seen_at": null, "revoked_at": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/99/app-clients"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({"detail": "Not this app's tunnel."})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/12/app-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": 5, "tunnel_id": 12, "spki_sha256": "ab".repeat(32), "status": "active"},
            {"id": 4, "tunnel_id": 12, "spki_sha256": "cd".repeat(32), "status": "revoked",
             "revoked_at": "2026-10-29T00:00:00Z"}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/tunnels/12/app-token-allowed"))
        .and(body_json(json!({"allowed": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token_allowed": true})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-tokens"))
        .and(body_json(json!({"expires_days": 90, "label": "VCA app"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 8, "tunnel_id": 12, "prefix": "dpt_app_ab", "status": "active",
            "token": "dpt_app_secret"
        })))
        .mount(&server)
        .await;

    let console = ConsoleClient::new(
        oauth_client(&server),
        tokens("dpo_at_1", "dpo_rt_1", far_future()),
    );
    let created = console
        .create_app_tunnel(&CreateTunnel {
            device_id: 3,
            zone_id: 7,
            name: "vca-abc".into(),
            local_port: 47800,
            access_mode: ACCESS_MODE_APP.into(),
            local_scheme: "http".into(),
            public_port: 443,
        })
        .await
        .unwrap();
    assert_eq!(created.id, 12);
    assert_eq!(created.hostname, "vca-abc.example.com");
    assert_eq!(created.url.as_deref(), Some("https://vca-abc.example.com"));
    assert!(created.is_app_mode());
    let bad = CreateTunnel {
        device_id: 3,
        zone_id: 7,
        name: random_hostname_label(),
        local_port: 47800,
        access_mode: "password".into(),
        local_scheme: "http".into(),
        public_port: 443,
    };
    assert!(console.create_app_tunnel(&bad).await.is_err());

    let key = ClientKey::from_hex(&"AB".repeat(32)).unwrap();
    let client = console
        .register_app_client(12, &key.to_request(Some("VCA app".into())))
        .await
        .unwrap();
    assert_eq!(client.id, 5);
    assert!(client.is_active());
    match console
        .register_app_client(99, &key.to_request(Some("VCA app".into())))
        .await
    {
        Err(ProviderError::Forbidden(m)) => assert_eq!(m, "Not this app's tunnel."),
        other => panic!("{other:?}"),
    }
    let clients = console.app_clients(12).await.unwrap();
    assert_eq!(clients.len(), 2);
    assert!(clients[0].is_active() && !clients[1].is_active());

    assert!(console.set_app_token_allowed(12, true).await.unwrap());
    let token = console
        .issue_app_token(
            12,
            &IssueAppToken {
                expires_days: Some(90),
                label: Some("VCA app".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        token.token.as_ref().map(Secret::expose),
        Some("dpt_app_secret")
    );
    assert!(!format!("{token:?}").contains("dpt_app_secret"));
}

#[tokio::test]
async fn console_refreshes_expired_tokens_and_saves_the_rotation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("refresh_token=dpo_rt_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dpo_at_2", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "dpo_rt_2", "scope": "tunnels:read"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("refresh_token=dpo_rt_2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dpo_at_3", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "dpo_rt_3", "scope": "tunnels:read"
        })))
        .expect(1)
        .mount(&server)
        .await;
    // dpo_at_2 is rejected once (revoked early): refresh again and retry.
    Mock::given(method("GET"))
        .and(path("/api/auth/me"))
        .and(header("authorization", "Bearer dpo_at_2"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"detail": "expired"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/auth/me"))
        .and(header("authorization", "Bearer dpo_at_3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user": "x"})))
        .mount(&server)
        .await;
    let saved: Arc<Mutex<Vec<String>>> = Arc::default();
    let saved2 = Arc::clone(&saved);
    let console = ConsoleClient::new(oauth_client(&server), tokens("dpo_at_1", "dpo_rt_1", 1))
        .with_token_saver(Arc::new(move |t: &TokenSet| {
            saved2
                .lock()
                .unwrap()
                .push(t.refresh_token.expose().to_owned());
        }));
    assert_eq!(console.me().await.unwrap(), json!({"user": "x"}));
    assert_eq!(*saved.lock().unwrap(), vec!["dpo_rt_2", "dpo_rt_3"]);
    assert_eq!(
        console.tokens().await.unwrap().refresh_token.expose(),
        "dpo_rt_3"
    );
}

fn enroll_request() -> AppEnrollRequest {
    AppEnrollRequest {
        code: Secret::new(format!("dpt_inv_{}", "a".repeat(43))),
        cert_pem: "-----BEGIN CERTIFICATE-----\n".into(),
        sig: "c2ln".into(),
    }
}

#[tokio::test]
async fn enroll_parses_the_result_and_the_optional_wait() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/tunnel-app-enroll"))
        .and(body_json(json!({
            "code": format!("dpt_inv_{}", "a".repeat(43)),
            "cert_pem": "-----BEGIN CERTIFICATE-----\n", "sig": "c2ln"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tunnel_id": 12, "hostname": "vca.example.com", "client_id": 41,
            "spki_sha256": "ab".repeat(32)
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/public/tunnel-app-enroll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tunnel_id": 12, "hostname": "vca.example.com", "client_id": 41,
            "spki_sha256": "ab".repeat(32), "ready_within_sec": 0
        })))
        .mount(&server)
        .await;
    let first = public_enroll(&server.uri(), &enroll_request())
        .await
        .unwrap();
    assert_eq!(first.ready_within_sec, None);
    assert_eq!(first.ready_within(), 30);
    let second = public_enroll(&server.uri(), &enroll_request())
        .await
        .unwrap();
    assert_eq!(second.ready_within(), 0);
}

#[tokio::test]
async fn enroll_errors_are_coded_and_free_of_the_code() {
    let code = format!("dpt_inv_{}", "a".repeat(43));
    for (status, stable) in [
        (404, "invite_invalid"),
        (400, "enroll_bad_signature"),
        (400, "enroll_bad_cert"),
        (409, "tunnel_not_app_mode"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/public/tunnel-app-enroll"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "detail": {"message": format!("refused {code}"), "code": stable}
            })))
            .mount(&server)
            .await;
        let err = public_enroll(&server.uri(), &enroll_request())
            .await
            .unwrap_err();
        assert!(!err.to_string().contains(&code), "{err}");
        match err {
            ProviderError::Coded {
                status: got,
                code: got_code,
                ..
            } => {
                assert_eq!(got, status);
                assert_eq!(got_code, stable);
            }
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn enroll_rate_limit_carries_retry_after_and_redirects_are_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/tunnel-app-enroll"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "90")
                .set_body_json(json!({"detail": {"message": "Slow", "code": "rate_limited"}})),
        )
        .mount(&server)
        .await;
    assert!(matches!(
        public_enroll(&server.uri(), &enroll_request()).await,
        Err(ProviderError::RateLimited {
            retry_after: Some(d)
        }) if d == Duration::from_secs(90)
    ));

    // A redirect must not carry the code anywhere else.
    let elsewhere = MockServer::start().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/public/tunnel-app-enroll"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("Location", format!("{}/x", elsewhere.uri())),
        )
        .mount(&server)
        .await;
    assert!(
        public_enroll(&server.uri(), &enroll_request())
            .await
            .is_err()
    );
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invite_calls_use_the_documented_paths() {
    let server = MockServer::start().await;
    let code = format!("dpt_inv_{}", "a".repeat(43));
    let invite = json!({
        "id": 9, "tunnel_id": 12, "label": "VCA phone", "status": "active", "created_by": null,
        "created_at": "2026-10-31T00:00:00Z", "expires_at": "2026-10-31T00:10:00Z",
        "revoked_at": null, "used_at": null, "used_client_id": null
    });
    let mut created = invite.clone();
    created["code"] = json!(code);
    created["hostname"] = json!("vca.example.com");
    created["expires_in"] = json!(600);
    created["enroll_url"] = json!(format!(
        "https://dns.dnspilot.kr/enroll#v=1&h=vca.example.com&t=12&c={code}"
    ));
    Mock::given(method("POST"))
        .and(path("/api/tunnels/12/app-invites"))
        .and(header("authorization", "Bearer dpo_at_1"))
        .and(body_json(json!({"label": "VCA phone", "ttl_sec": 600})))
        .respond_with(ResponseTemplate::new(201).set_body_json(created))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tunnels/12/app-invites"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"items": [invite.clone()], "access_mode": "app"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/tunnels/12/app-invites/9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(invite))
        .expect(1)
        .mount(&server)
        .await;
    let console = ConsoleClient::new(oauth_client(&server), tokens("dpo_at_1", "r", far_future()));
    let made = console
        .create_app_invite(
            12,
            &CreateAppInvite {
                label: Some("VCA phone".into()),
                ttl_sec: Some(600),
            },
        )
        .await
        .unwrap();
    assert_eq!(made.code.expose(), code);
    assert_eq!(made.invite.id, 9);
    assert!(!format!("{made:?}").contains(&code));
    let list = console.app_invites(12).await.unwrap();
    assert!(list.items[0].is_active());
    assert_eq!(list.access_mode.as_deref(), Some("app"));
    console.revoke_app_invite(12, 9).await.unwrap();
}

#[tokio::test]
async fn public_status_is_parsed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/public/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tunnel_status": "operational", "relays": [{"host": "relay-1", "region": "kr",
            "role": "primary", "status": "ok", "active": true, "last_check": null}],
            "updated_at": "2026-10-30T00:00:00Z", "zones_count": 3
        })))
        .mount(&server)
        .await;
    let s = public_status(&reqwest::Client::new(), &server.uri())
        .await
        .unwrap();
    assert_eq!(s.tunnel_status.as_deref(), Some("operational"));
    assert_eq!(s.relays.len(), 1);
}

/// A fake Agent local API on a Unix socket (Linux, macOS) or a named pipe
/// (Windows).
mod local_agent {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use vca_providers::dnspilot::events::{EventTracker, ResyncReason, TrackerOutcome};
    use vca_providers::dnspilot::local::{LocalApiClient, LocalEndpoint, TokenSource};
    use vca_providers::dnspilot::{ClientKey, DnsPilotProvider, DnsPilotSettings};
    use vca_providers::{ConnectivityProvider, EndpointKind, ProviderError, ProviderStatus};

    #[derive(Debug, Clone)]
    pub struct Req {
        pub method: String,
        pub path: String,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    impl Req {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    type Handler = Arc<dyn Fn(&Req) -> (u16, &'static str, String) + Send + Sync>;

    async fn handle<S: AsyncRead + AsyncWrite + Unpin>(
        mut stream: S,
        handler: Handler,
        log: Arc<Mutex<Vec<Req>>>,
    ) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let head_end = loop {
            let Ok(n) = stream.read(&mut tmp).await else {
                return;
            };
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let mut first = lines.next().unwrap().split(' ');
        let method = first.next().unwrap().to_owned();
        let path = first.next().unwrap().to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect();
        let len: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v.parse().unwrap())
            .unwrap_or(0);
        let mut body = buf[head_end + 4..].to_vec();
        while body.len() < len {
            let n = stream.read(&mut tmp).await.unwrap();
            body.extend_from_slice(&tmp[..n]);
        }
        let req = Req {
            method,
            path,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        };
        log.lock().unwrap().push(req.clone());
        let (status, ctype, body) = handler(&req);
        let resp = if ctype == "text/event-stream" {
            format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: {ctype}\r\nConnection: close\r\n\r\n{body}"
            )
        } else {
            format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        };
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    /// Starts the fake Agent; returns the endpoint to connect to.
    async fn serve(
        dir: &std::path::Path,
        handler: Handler,
        log: Arc<Mutex<Vec<Req>>>,
    ) -> LocalEndpoint {
        #[cfg(unix)]
        {
            let socket = dir.join("local.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(handle(stream, handler.clone(), log.clone()));
                }
            });
            LocalEndpoint::UnixSocket(socket)
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let _ = dir;
            let name = format!(
                r"\\.\pipe\vca-test-dnspilot-{}-{}",
                std::process::id(),
                rand_suffix()
            );
            let mut server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)
                .unwrap();
            let pipe = name.clone();
            tokio::spawn(async move {
                loop {
                    if server.connect().await.is_err() {
                        return;
                    }
                    let conn = server;
                    server = ServerOptions::new().create(&pipe).unwrap();
                    tokio::spawn(handle(conn, handler.clone(), log.clone()));
                }
            });
            LocalEndpoint::NamedPipe(name)
        }
    }

    #[cfg(windows)]
    fn rand_suffix() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        format!("{}", N.fetch_add(1, Ordering::SeqCst))
    }

    fn json_resp(v: Value) -> (u16, &'static str, String) {
        (200, "application/json", v.to_string())
    }

    #[derive(Default)]
    struct Agent {
        logged_in: bool,
        killed: bool,
        token_calls: u32,
        tunnels: Vec<Value>,
    }

    fn local_tunnel(state: &str) -> Value {
        json!({"id": 12, "hostname": "vca-k3x9q2ab4m.example.com", "local_port": 47800,
            "local_scheme": "http", "access_mode": "app", "server_status": "active",
            "paused": false, "cert": "ready", "serving": state == "serving", "state": state,
            "message": ""})
    }

    /// The client under test: on Windows the token comes from a file the
    /// test writes, on Unix from `GET /v1/token`.
    fn client(endpoint: LocalEndpoint, dir: &std::path::Path) -> LocalApiClient {
        let c = LocalApiClient::new(endpoint).insecure_skip_pipe_server_check_for_tests();
        if cfg!(windows) {
            let file = dir.join("user.token");
            std::fs::write(&file, "dpl_tok-2\n").unwrap();
            c.with_token_source(TokenSource::File(file))
        } else {
            c.with_token_source(TokenSource::Endpoint)
        }
    }

    fn agent_handler(state: Arc<Mutex<Agent>>) -> Handler {
        Arc::new(move |req: &Req| {
            let mut st = state.lock().unwrap();
            if req.path == "/v1/status" {
                assert!(
                    req.header("authorization").is_none(),
                    "status needs no token"
                );
                return json_resp(json!({
                    "version": "1.4.0", "logged_in": st.logged_in, "device_id": 3,
                    "device_name": "Desk", "relay": "kr-1", "tunnels_online": 1,
                    "killed": st.killed, "boot_id": "0123456789abcdef"
                }));
            }
            if req.path == "/v1/token" {
                st.token_calls += 1;
                return json_resp(json!({"token": format!("dpl_tok-{}", st.token_calls)}));
            }
            // The first Unix token is "stale": force one refresh.
            if req.header("authorization") != Some("Bearer dpl_tok-2") {
                return (401, "application/json", r#"{"error":"bad token"}"#.into());
            }
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/v1/tunnels") => json_resp(json!({"tunnels": st.tunnels})),
                ("PATCH", "/v1/tunnels/12") => json_resp(json!({"id": 12, "enabled": false})),
                ("PATCH", "/v1/tunnels/13") => (
                    409,
                    "application/json",
                    r#"{"error":"not_logged_in","detail":"sign in first"}"#.into(),
                ),
                ("POST", "/v1/tunnels/12/app-clients") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    (
                        201,
                        "application/json",
                        json!({"id": 5, "spki_sha256": body["spki_sha256"], "label": body["label"]})
                            .to_string(),
                    )
                }
                ("POST", "/v1/tunnels/13/app-clients") => (
                    400,
                    "application/json",
                    r#"{"error":"bad_key","detail":"spki_sha256 must be 64 hex characters"}"#
                        .into(),
                ),
                ("DELETE", "/v1/tunnels/12/app-clients/5") => {
                    (204, "application/json", String::new())
                }
                ("POST", "/v1/activation") => (
                    409,
                    "application/json",
                    r#"{"error":"already_logged_in","detail":"already signed in"}"#.into(),
                ),
                ("GET", "/v1/events") => {
                    let body = if req.header("last-event-id") == Some("41")
                        && req.header("last-boot-id") == Some("b1")
                    {
                        "data: {\"kind\":\"hello\",\"boot_id\":\"b1\",\"last_id\":41}\n\n\
                         data: {\"id\":42,\"boot_id\":\"b1\",\"kind\":\"conn.allow\",\"at\":\"t\",\"data\":{}}\n\n"
                    } else {
                        ": keep-alive\n\n\
                         data: {\"kind\":\"reset\",\"boot_id\":\"b2\",\"last_id\":0,\"reason\":\"boot_changed\"}\n\n\
                         data: {\"id\":1,\"boot_id\":\"b2\",\"kind\":\"tunnel.up\",\"at\":\"t\",\"data\":{}}\n\n"
                    };
                    (200, "text/event-stream", body.into())
                }
                _ => (404, "application/json", r#"{"error":"no route"}"#.into()),
            }
        })
    }

    #[tokio::test]
    async fn local_api_calls_use_documented_paths_and_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(Agent {
            logged_in: true,
            ..Agent::default()
        }));
        let log = Arc::new(Mutex::new(Vec::new()));
        let endpoint = serve(dir.path(), agent_handler(state.clone()), log.clone()).await;
        let local = client(endpoint, dir.path());

        let status = local.status().await.unwrap();
        assert!(status.logged_in);
        assert_eq!(status.device_id, Some(3));
        state.lock().unwrap().tunnels = vec![local_tunnel("serving")];
        let tunnels = local.list_tunnels().await.unwrap();
        assert_eq!(tunnels[0].id, 12);
        assert_eq!(tunnels[0].public_port(), 443);
        assert_eq!(tunnels[0].state.as_deref(), Some("serving"));
        assert_eq!(
            local.set_tunnel_enabled(12, false).await.unwrap(),
            vca_providers::dnspilot::contract::LocalPatchResponse {
                id: 12,
                enabled: false
            }
        );
        let der = [0x30u8, 0x03, 0x02, 0x01, 0x01];
        let key = ClientKey::from_spki_der(&der).to_request(Some("VCA app".into()));
        let client = local.register_app_client(12, &key).await.unwrap();
        assert_eq!(client.id, 5);
        assert_eq!(client.label.as_deref(), Some("VCA app"));
        local.remove_app_client(12, 5).await.unwrap();
        // Agent error codes become clear messages.
        match local.start_activation().await {
            Err(ProviderError::Conflict(m)) => {
                assert!(m.contains("already signed in (already_logged_in)"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            local.set_tunnel_enabled(13, true).await,
            Err(ProviderError::NotSignedIn(m)) if m.contains("not signed in")
        ));
        match local.register_app_client(13, &key).await {
            Err(ProviderError::Api {
                status: 400,
                message,
            }) => {
                assert!(
                    message.contains("rejected the client key (bad_key)"),
                    "{message}"
                )
            }
            other => panic!("{other:?}"),
        }

        let log = log.lock().unwrap();
        let patch = log.iter().find(|r| r.method == "PATCH").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&patch.body).unwrap(),
            json!({"enabled": false})
        );
        let reg = log
            .iter()
            .find(|r| r.method == "POST" && r.path.ends_with("/app-clients"))
            .unwrap();
        let body: Value = serde_json::from_str(&reg.body).unwrap();
        assert_eq!(body["spki_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(body["label"], "VCA app");
        if cfg!(unix) {
            // The 401 on the first token triggered exactly one refresh.
            assert_eq!(state.lock().unwrap().token_calls, 2);
        }
    }

    #[tokio::test]
    async fn provider_reports_the_tunnel_after_checking_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(Agent::default()));
        let endpoint = serve(
            dir.path(),
            agent_handler(state.clone()),
            Arc::new(Mutex::new(Vec::new())),
        )
        .await;

        // No hostname configured: the Agent's app-mode tunnel to vcad's port
        // is found by itself.
        let p = DnsPilotProvider::new(
            client(endpoint.clone(), dir.path()),
            DnsPilotSettings::default(),
        )
        .unwrap();
        state.lock().unwrap().logged_in = true;
        assert!(matches!(
            p.ensure_endpoint(47800).await,
            Err(ProviderError::InvalidConfig(m)) if m.contains("no DNSPilot app-mode tunnel")
        ));
        state.lock().unwrap().tunnels = vec![local_tunnel("waiting")];
        let e = p.ensure_endpoint(47800).await.unwrap();
        assert_eq!(e.url, "wss://vca-k3x9q2ab4m.example.com");
        let diags: Vec<String> = p
            .diagnostics()
            .await
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert!(
            diags
                .iter()
                .any(|d| d.contains("not serving: waiting (certificate ready)")),
            "{diags:?}"
        );
        // Another port: not this tunnel.
        assert!(p.ensure_endpoint(47801).await.is_err());
        state.lock().unwrap().tunnels = vec![local_tunnel("serving")];
        state.lock().unwrap().logged_in = false;

        let p = DnsPilotProvider::new(
            client(endpoint, dir.path()),
            DnsPilotSettings {
                hostname: Some("https://VCA-k3x9q2ab4m.example.com".into()),
                public_port: 443,
            },
        )
        .unwrap();
        let d = p.detect().await.unwrap();
        assert!(d.installed);
        assert_eq!(d.signed_in, Some(false));
        assert!(matches!(
            p.ensure_endpoint(47800).await,
            Err(ProviderError::NotSignedIn(_))
        ));
        state.lock().unwrap().logged_in = true;
        state.lock().unwrap().killed = true;
        assert!(p.ensure_endpoint(47800).await.is_err());
        state.lock().unwrap().killed = false;
        let e = p.ensure_endpoint(47800).await.unwrap();
        assert_eq!(e.url, "wss://vca-k3x9q2ab4m.example.com");
        assert_eq!(e.kind, EndpointKind::DnsPilotApp);
        assert_eq!(p.status().await, ProviderStatus::Active { endpoint: e });
        let diags: Vec<String> = p
            .diagnostics()
            .await
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert!(
            diags.iter().any(|d| d.contains("Desk (device id 3)")),
            "{diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.contains("127.0.0.1:47800")),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.contains("serving (certificate ready)")),
            "{diags:?}"
        );
        assert!(diags.iter().any(|d| d.contains("Relay: kr-1")), "{diags:?}");
        p.teardown().await.unwrap();
        assert_eq!(p.status().await, ProviderStatus::Inactive);
        assert!(
            DnsPilotProvider::new(
                LocalApiClient::default(),
                DnsPilotSettings {
                    hostname: Some("vca.example.com".into()),
                    public_port: 8443
                }
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn event_stream_resumes_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(Agent::default()));
        let endpoint = serve(
            dir.path(),
            agent_handler(state),
            Arc::new(Mutex::new(Vec::new())),
        )
        .await;
        let local = client(endpoint, dir.path());

        let mut tracker = EventTracker::new();
        tracker.set_boot_id(Some("b1".into()));
        let mut stream = local.events(Some((41, "b1"))).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(tracker.on_event(&first), TrackerOutcome::Resumed);
        let second = stream.next().await.unwrap().unwrap();
        assert!(
            matches!(tracker.on_event(&second), TrackerOutcome::Event(e) if e.kind == "conn.allow")
        );
        assert_eq!(tracker.resume_point(), Some((42, "b1")));
        assert!(stream.next().await.is_none());

        // The Agent restarted: the stream starts with reset.
        let mut stream = local.events(tracker.resume_point()).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(
            tracker.on_event(&first),
            TrackerOutcome::Resync {
                reason: ResyncReason::Reset("boot_changed".into())
            }
        );
        let next = stream.next().await.unwrap().unwrap();
        assert!(matches!(tracker.on_event(&next), TrackerOutcome::Event(_)));
        assert_eq!(tracker.resume_point(), Some((1, "b2")));
    }

    #[tokio::test]
    async fn missing_agent_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = if cfg!(windows) {
            LocalEndpoint::NamedPipe(format!(r"\\.\pipe\vca-test-absent-{}", std::process::id()))
        } else {
            LocalEndpoint::UnixSocket(dir.path().join("absent.sock"))
        };
        let p = DnsPilotProvider::new(
            client(endpoint, dir.path()),
            DnsPilotSettings {
                hostname: Some("vca.example.com".into()),
                public_port: 443,
            },
        )
        .unwrap();
        assert!(!p.detect().await.unwrap().installed);
        assert!(matches!(
            p.ensure_endpoint(1).await,
            Err(ProviderError::NotInstalled(_))
        ));
    }
}
