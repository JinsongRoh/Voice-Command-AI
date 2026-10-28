//! DNS Pilot provider tests: a fake Agent on a Unix socket and a wiremock
//! console for the OAuth device flow.

use std::time::Duration;

use serde_json::json;
use vca_providers::dnspilot::oauth::{DeviceFlowClient, OAuthConfig, OAuthError};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn oauth_client(server: &MockServer) -> DeviceFlowClient {
    DeviceFlowClient::new(OAuthConfig {
        console_url: server.uri(),
        interval_unit: Duration::from_millis(5),
        ..OAuthConfig::default()
    })
}

#[tokio::test]
async fn device_flow_honors_pending_and_slow_down() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .and(body_string_contains("client_id=vca"))
        .and(body_string_contains(
            "scope=tunnels%3Aread+tunnels%3Awrite+p2p%3Aread",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dev-code-secret", "user_code": "WDJB-MJHT",
            "verification_uri": "https://dns.dnspilot.kr/device",
            "verification_uri_complete": "https://dns.dnspilot.kr/device?user_code=WDJB-MJHT",
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
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-1", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "rt-1", "scope": "tunnels:read tunnels:write"
        })))
        .mount(&server)
        .await;

    let client = oauth_client(&server);
    let auth = client.start().await.unwrap();
    assert_eq!(auth.user_code, "WDJB-MJHT");
    assert_eq!(auth.interval_secs, 1);
    let started = std::time::Instant::now();
    let tokens = client.poll(&auth).await.unwrap();
    // 1 + 1 + (1 + 5) interval units of 5 ms at least.
    assert!(
        started.elapsed() >= Duration::from_millis(40),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(tokens.access_token.expose(), "at-1");
    assert_eq!(tokens.refresh_token.as_ref().unwrap().expose(), "rt-1");
    let token_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count();
    assert_eq!(token_calls, 3);
}

#[tokio::test]
async fn device_flow_denied_and_expired() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "d", "user_code": "U", "verification_uri": "https://x/device", "expires_in": 600
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "access_denied"})))
        .mount(&server)
        .await;
    let client = oauth_client(&server);
    let auth = client.start().await.unwrap();
    assert_eq!(auth.interval_secs, 5, "RFC 8628 default interval");
    assert!(matches!(
        client.poll(&auth).await,
        Err(OAuthError::AccessDenied)
    ));

    let server2 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": "invalid_scope", "error_description": "p2p:write"})),
        )
        .mount(&server2)
        .await;
    assert!(matches!(
        oauth_client(&server2).start().await,
        Err(OAuthError::Other { code, .. }) if code == "invalid_scope"
    ));
}

#[tokio::test]
async fn refresh_rotates_and_revoke() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=rt-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-2", "expires_in": 3600, "refresh_token": "rt-2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("refresh_token=rt-old"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(body_string_contains("token=rt-2"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let client = oauth_client(&server);
    let t1: vca_providers::dnspilot::oauth::TokenSet = serde_json::from_value(json!({
        "access_token": "at-1", "refresh_token": "rt-1", "token_type": "Bearer", "expires_at": 1, "scope": null
    }))
    .unwrap();
    let t2 = client.refresh(&t1).await.unwrap();
    assert_eq!(t2.access_token.expose(), "at-2");
    assert_eq!(t2.refresh_token.as_ref().unwrap().expose(), "rt-2");
    let old: vca_providers::dnspilot::oauth::TokenSet = serde_json::from_value(json!({
        "access_token": "a", "refresh_token": "rt-old", "token_type": "Bearer", "expires_at": null, "scope": null
    }))
    .unwrap();
    assert!(matches!(
        client.refresh(&old).await,
        Err(OAuthError::InvalidGrant)
    ));
    client
        .revoke(t2.refresh_token.as_ref().unwrap(), Some("refresh_token"))
        .await
        .unwrap();
}

#[cfg(unix)]
mod local_agent {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use vca_providers::dnspilot::events::{EventTracker, ResyncReason, TrackerOutcome};
    use vca_providers::dnspilot::local::{LocalApiClient, LocalEndpoint};
    use vca_providers::dnspilot::{ClientKey, DnsPilotProvider, TunnelCreation};
    use vca_providers::{ConnectivityProvider, EndpointKind, ProviderError, ProviderStatus};

    #[derive(Debug, Clone)]
    struct Req {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
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

    /// A minimal HTTP/1.1 server on a Unix socket: one request per connection.
    async fn serve(socket: PathBuf, handler: Handler, log: Arc<Mutex<Vec<Req>>>) {
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let head_end = loop {
                        let n = stream.read(&mut tmp).await.unwrap();
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
                });
            }
        });
    }

    fn json_resp(v: Value) -> (u16, &'static str, String) {
        (200, "application/json", v.to_string())
    }

    /// Fake Agent state shared with the handler.
    #[derive(Default)]
    struct Agent {
        tunnels: Vec<Value>,
        token_calls: u32,
    }

    fn agent_handler(state: Arc<Mutex<Agent>>) -> Handler {
        Arc::new(move |req: &Req| {
            let mut st = state.lock().unwrap();
            if req.path == "/v1/token" {
                st.token_calls += 1;
                return json_resp(json!({"token": format!("tok-{}", st.token_calls)}));
            }
            // The first token is "expired": force one refresh.
            if req.header("authorization") != Some("Bearer tok-2") {
                return (401, "application/json", r#"{"message":"bad token"}"#.into());
            }
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/v1/status") => json_resp(json!({
                    "boot_id": "boot-1", "version": "0.5.4", "activated": true, "killed": false,
                    "relay_connected": true, "unknown": 1
                })),
                ("GET", "/v1/tunnels") => json_resp(json!({"tunnels": st.tunnels})),
                ("POST", "/v1/tunnels") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    let label = body["hostname_label"].as_str().unwrap().to_owned();
                    let t = json!({
                        "id": "tun-1", "hostname": format!("{label}.vca.dnspilot.net"),
                        "access_mode": body["access_mode"], "local_scheme": body["local_scheme"],
                        "local_host": body["local_host"], "local_port": body["local_port"],
                        "public_port": body["public_port"], "enabled": true, "state": "active"
                    });
                    st.tunnels.push(t.clone());
                    json_resp(t)
                }
                ("PATCH", "/v1/tunnels/tun-1") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    let t = st.tunnels.iter_mut().find(|t| t["id"] == "tun-1").unwrap();
                    if let Some(e) = body.get("enabled") {
                        t["enabled"] = e.clone();
                    }
                    if let Some(p) = body.get("local_port") {
                        t["local_port"] = p.clone();
                    }
                    json_resp(t.clone())
                }
                ("POST", "/v1/tunnels/tun-1/app-clients") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    json_resp(
                        json!({"id": "cli-1", "label": body["label"], "public_key_sha256": body["public_key_sha256"]}),
                    )
                }
                ("DELETE", "/v1/tunnels/tun-1/app-clients/cli-1") => {
                    (204, "application/json", String::new())
                }
                ("GET", "/v1/events") => {
                    let body = if req.header("last-event-id") == Some("41") {
                        "event: hello\ndata: {\"boot_id\":\"boot-1\"}\n\nid: 42\nevent: tunnel.updated\ndata: {\"boot_id\":\"boot-1\",\"tunnel_id\":\"tun-1\"}\n\n"
                    } else {
                        "event: reset\ndata: {\"boot_id\":\"boot-2\"}\n\nid: 1\nevent: tunnel.updated\ndata: {\"boot_id\":\"boot-2\",\"tunnel_id\":\"tun-1\"}\n\n"
                    };
                    (200, "text/event-stream", body.into())
                }
                _ => (404, "application/json", r#"{"message":"no route"}"#.into()),
            }
        })
    }

    #[tokio::test]
    async fn provider_creates_app_tunnel_and_registers_keys() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("local.sock");
        let state = Arc::new(Mutex::new(Agent::default()));
        let log = Arc::new(Mutex::new(Vec::new()));
        serve(socket.clone(), agent_handler(state.clone()), log.clone()).await;

        let local = LocalApiClient::new(LocalEndpoint::UnixSocket(socket.clone()));
        let provider = DnsPilotProvider::new(local, TunnelCreation::LocalAgent);

        let detection = provider.detect().await.unwrap();
        assert!(detection.installed);
        assert_eq!(detection.signed_in, Some(true));
        assert_eq!(detection.version.as_deref(), Some("0.5.4"));

        let endpoint = provider.ensure_endpoint(47800).await.unwrap();
        assert_eq!(endpoint.kind, EndpointKind::DnsPilotApp);
        assert!(endpoint.url.starts_with("wss://vca-"), "{}", endpoint.url);
        assert!(endpoint.url.ends_with(".vca.dnspilot.net"));
        assert_eq!(provider.tunnel_id().await.as_deref(), Some("tun-1"));

        let create = log
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.method == "POST" && r.path == "/v1/tunnels")
            .cloned()
            .unwrap();
        let body: Value = serde_json::from_str(&create.body).unwrap();
        assert_eq!(body["access_mode"], "app");
        assert_eq!(body["local_scheme"], "http");
        assert_eq!(body["local_host"], "127.0.0.1");
        assert_eq!(body["local_port"], 47800);
        assert_eq!(body["public_port"], 443);

        // Idempotent: a second call reuses the tunnel.
        assert_eq!(provider.ensure_endpoint(47800).await.unwrap(), endpoint);
        // A new port patches the existing tunnel.
        provider.ensure_endpoint(47801).await.unwrap();
        assert_eq!(state.lock().unwrap().tunnels[0]["local_port"], 47801);
        assert_eq!(state.lock().unwrap().tunnels.len(), 1);

        let der = [0x30u8, 0x03, 0x02, 0x01, 0x01];
        let client = provider
            .register_app_client(&ClientKey::from_spki_der(&der))
            .await
            .unwrap();
        assert_eq!(client.id, "cli-1");
        assert!(client.label.unwrap().starts_with("vca-device-"));
        provider.remove_app_client("cli-1").await.unwrap();

        provider.teardown().await.unwrap();
        assert_eq!(provider.status().await, ProviderStatus::Inactive);
        assert_eq!(state.lock().unwrap().tunnels[0]["enabled"], false);
        // Enabling again keeps the same hostname.
        assert_eq!(
            provider.ensure_endpoint(47801).await.unwrap().url,
            endpoint.url
        );

        // The 401 on the first token triggered exactly one refresh.
        assert_eq!(state.lock().unwrap().token_calls, 2);
    }

    #[tokio::test]
    async fn event_stream_hello_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("local.sock");
        let state = Arc::new(Mutex::new(Agent::default()));
        serve(
            socket.clone(),
            agent_handler(state),
            Arc::new(Mutex::new(Vec::new())),
        )
        .await;
        let local = LocalApiClient::new(LocalEndpoint::UnixSocket(socket));

        let mut tracker = EventTracker::new();
        tracker.set_boot_id(Some("boot-1".into()));
        // Resume with a valid Last-Event-ID: hello, then the next event.
        let mut stream = local.events(Some("41")).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(tracker.on_event(&first), TrackerOutcome::Resumed);
        let second = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            tracker.on_event(&second),
            TrackerOutcome::Event(_)
        ));
        assert_eq!(tracker.last_event_id(), Some("42"));
        assert!(stream.next().await.is_none());

        // The Agent restarted: the stream starts with reset.
        let mut stream = local.events(Some("99")).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(
            tracker.on_event(&first),
            TrackerOutcome::Resync {
                reason: ResyncReason::ResetEvent
            }
        );
        assert_eq!(tracker.boot_id(), Some("boot-2"));
        let next = stream.next().await.unwrap().unwrap();
        assert!(matches!(tracker.on_event(&next), TrackerOutcome::Event(_)));
    }

    #[tokio::test]
    async fn console_creation_uses_oauth_token() {
        use vca_providers::dnspilot::oauth::{DeviceFlowClient, OAuthConfig, TokenSet};
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("local.sock");
        let state = Arc::new(Mutex::new(Agent::default()));
        serve(
            socket.clone(),
            agent_handler(state),
            Arc::new(Mutex::new(Vec::new())),
        )
        .await;

        let console = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/tunnels"))
            .and(header("authorization", "Bearer at-console"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "tun-c", "hostname": "vca-zz.vca.dnspilot.net", "access_mode": "app",
                "local_scheme": "http", "local_port": 47800, "public_port": 443, "enabled": true
            })))
            .expect(1)
            .mount(&console)
            .await;
        let tokens: TokenSet = serde_json::from_value(json!({
            "access_token": "at-console", "refresh_token": "rt", "token_type": "Bearer",
            "expires_at": null, "scope": "tunnels:write"
        }))
        .unwrap();
        let creation = TunnelCreation::Console {
            oauth: DeviceFlowClient::new(OAuthConfig {
                console_url: console.uri(),
                ..OAuthConfig::default()
            }),
            tokens: Arc::new(tokio::sync::Mutex::new(tokens)),
            device_id: Some("dev-1".into()),
        };
        let provider = DnsPilotProvider::new(
            LocalApiClient::new(LocalEndpoint::UnixSocket(socket)),
            creation,
        );
        let e = provider.ensure_endpoint(47800).await.unwrap();
        assert_eq!(e.url, "wss://vca-zz.vca.dnspilot.net");
        let reqs = console.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["access_mode"], "app");
        assert_eq!(body["device_id"], "dev-1");
        assert!(body["hostname_label"].as_str().unwrap().starts_with("vca-"));
    }

    #[tokio::test]
    async fn missing_agent_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalApiClient::new(LocalEndpoint::UnixSocket(dir.path().join("absent.sock")));
        let provider = DnsPilotProvider::new(local, TunnelCreation::LocalAgent);
        assert!(!provider.detect().await.unwrap().installed);
        assert!(matches!(
            provider.ensure_endpoint(1).await,
            Err(ProviderError::NotInstalled(_))
        ));
    }
}
