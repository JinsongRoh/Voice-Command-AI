//! Refresh-token rotation safety (incident 2026-11-01: a refresh token was
//! presented twice and DNSPilot revoked the grant). A mock token endpoint
//! enforces strict rotation and revokes the grant on reuse, exactly like the
//! console. No live DNSPilot calls.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use vca_providers::ProviderError;
use vca_providers::dnspilot::console::ConsoleClient;
use vca_providers::dnspilot::keeper::{
    LockFile, LockGuard, MemoryTokenStore, REAUTHORIZE_MESSAGE, TokenKeeper, TokenStore,
};
use vca_providers::dnspilot::oauth::{DeviceFlowClient, OAuthConfig, OAuthError, TokenSet};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// The console's grant: one valid refresh token at a time; presenting a
/// used one revokes everything.
#[derive(Default)]
struct Grant {
    current: String,
    used: HashSet<String>,
    seq: u32,
    refreshes: u32,
    reuse: u32,
    revoked: bool,
}

#[derive(Clone)]
struct StrictRotation {
    grant: Arc<Mutex<Grant>>,
    /// Delay of the answer; the rotation is committed before it.
    delay: Duration,
}

impl StrictRotation {
    fn new(first: &str, delay: Duration) -> Self {
        Self {
            grant: Arc::new(Mutex::new(Grant {
                current: first.into(),
                ..Grant::default()
            })),
            delay,
        }
    }

    fn refreshes(&self) -> u32 {
        self.grant.lock().unwrap().refreshes
    }

    fn assert_no_reuse(&self) {
        let g = self.grant.lock().unwrap();
        assert_eq!(g.reuse, 0, "a refresh token was presented twice");
        assert!(!g.revoked, "the grant was revoked");
    }
}

fn invalid_grant() -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"}))
}

impl Respond for StrictRotation {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let token = url::form_urlencoded::parse(&req.body)
            .find(|(k, _)| k == "refresh_token")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        let mut g = self.grant.lock().unwrap();
        g.refreshes += 1;
        if g.revoked {
            return invalid_grant();
        }
        if token == g.current {
            g.seq += 1;
            let next = format!("dpo_rt_{}", g.seq);
            let old = std::mem::replace(&mut g.current, next);
            g.used.insert(old);
            return ResponseTemplate::new(200)
                .set_body_json(json!({
                    "access_token": format!("dpo_at_{}", g.seq), "token_type": "Bearer",
                    "expires_in": 3600, "refresh_token": g.current.clone(),
                    "scope": "tunnels:read tunnels:write devices:read"
                }))
                .set_delay(self.delay);
        }
        if g.used.contains(&token) {
            // Reuse: treated as theft, the whole grant is revoked.
            g.reuse += 1;
            g.revoked = true;
        }
        invalid_grant()
    }
}

async fn console(rotation: &StrictRotation) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(rotation.clone())
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/auth/me"))
        .and(header("authorization", "Bearer dpo_at_0"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"detail": "expired"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/auth/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user": "x"})))
        .mount(&server)
        .await;
    server
}

fn oauth(url: &str, timeout: Duration) -> DeviceFlowClient {
    DeviceFlowClient::new(OAuthConfig {
        console_url: url.to_owned(),
        interval_unit: Duration::from_millis(5),
        request_timeout: timeout,
        ..OAuthConfig::default()
    })
}

fn tokens(n: u32, expires_at: u64) -> TokenSet {
    serde_json::from_value(json!({
        "access_token": format!("dpo_at_{n}"), "refresh_token": format!("dpo_rt_{n}"),
        "token_type": "Bearer", "expires_at": expires_at,
        "scope": "tunnels:read tunnels:write devices:read"
    }))
    .unwrap()
}

const EXPIRED: u64 = 1;
const FAR_FUTURE: u64 = 4_000_000_000;

/// The key store of one PC as several processes see it: shared data, a
/// shared lock file, and a switch that makes storing new tokens fail (the
/// process "dies" between the console's answer and the write).
struct PcStore {
    data: Arc<Mutex<Option<TokenSet>>>,
    lock: LockFile,
    fail_new_tokens: AtomicBool,
}

impl PcStore {
    fn new(data: &Arc<Mutex<Option<TokenSet>>>, dir: &std::path::Path) -> Self {
        Self {
            data: Arc::clone(data),
            lock: LockFile::new(dir.join("dnspilot-oauth.lock")),
            fail_new_tokens: AtomicBool::new(false),
        }
    }
}

impl TokenStore for PcStore {
    fn load(&self) -> vca_providers::Result<Option<TokenSet>> {
        Ok(self.data.lock().unwrap().clone())
    }

    fn save(&self, tokens: &TokenSet) -> vca_providers::Result<()> {
        if tokens.refresh_in_flight.is_none() && self.fail_new_tokens.load(Ordering::SeqCst) {
            return Err(ProviderError::Io(std::io::Error::other("simulated crash")));
        }
        *self.data.lock().unwrap() = Some(tokens.clone());
        Ok(())
    }

    fn try_lock(&self) -> vca_providers::Result<Option<LockGuard>> {
        self.lock.try_acquire()
    }
}

fn is_reauthorize(e: &ProviderError) -> bool {
    matches!(e, ProviderError::NotSignedIn(m) if m == REAUTHORIZE_MESSAGE)
}

#[tokio::test]
async fn the_mock_console_revokes_the_grant_on_reuse() {
    let rotation = StrictRotation::new("dpo_rt_0", Duration::ZERO);
    let server = console(&rotation).await;
    let client = oauth(&server.uri(), Duration::from_secs(5));
    let fresh = client.refresh(&tokens(0, EXPIRED)).await.unwrap();
    assert_eq!(fresh.refresh_token.expose(), "dpo_rt_1");
    assert!(matches!(
        client.refresh(&tokens(0, EXPIRED)).await,
        Err(OAuthError::InvalidGrant)
    ));
    // The reuse killed the newest token too.
    assert!(client.refresh(&fresh).await.is_err());
    assert_eq!(rotation.grant.lock().unwrap().reuse, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_share_one_refresh() {
    let rotation = StrictRotation::new("dpo_rt_0", Duration::from_millis(200));
    let server = console(&rotation).await;
    let store = Arc::new(MemoryTokenStore::new(Some(tokens(0, EXPIRED)), None));
    let keeper = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_secs(5)),
        store,
    ));
    // What the desktop UI does when the DNSPilot screen opens: devices,
    // zones and tunnels at once, each with its own console client.
    let calls = (0..8).map(|_| {
        let c = ConsoleClient::with_keeper(Arc::clone(&keeper));
        tokio::spawn(async move { c.me().await })
    });
    for call in calls.collect::<Vec<_>>() {
        assert_eq!(call.await.unwrap().unwrap(), json!({"user": "x"}));
    }
    assert_eq!(rotation.refreshes(), 1);
    rotation.assert_no_reuse();
    let stored = keeper.stored_tokens().unwrap();
    assert_eq!(stored.refresh_token.expose(), "dpo_rt_1");
    assert!(stored.refresh_in_flight.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_401s_share_one_refresh() {
    let rotation = StrictRotation::new("dpo_rt_0", Duration::from_millis(100));
    let server = console(&rotation).await;
    // Not expired, but the console rejects dpo_at_0 (revoked early).
    let store = Arc::new(MemoryTokenStore::new(Some(tokens(0, FAR_FUTURE)), None));
    let keeper = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_secs(5)),
        store,
    ));
    let calls: Vec<_> = (0..6)
        .map(|_| {
            let c = ConsoleClient::with_keeper(Arc::clone(&keeper));
            tokio::spawn(async move { c.me().await })
        })
        .collect();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    assert_eq!(rotation.refreshes(), 1);
    rotation.assert_no_reuse();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_processes_on_one_store_refresh_once() {
    let rotation = StrictRotation::new("dpo_rt_0", Duration::from_millis(300));
    let server = console(&rotation).await;
    let dir = tempfile::tempdir().unwrap();
    let data = Arc::new(Mutex::new(Some(tokens(0, EXPIRED))));
    // Both "processes" loaded the same tokens before either refreshed
    // (old and new app instance overlapping during an update).
    let a = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_secs(5)),
        Arc::new(PcStore::new(&data, dir.path())),
    ));
    let b = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_secs(5)),
        Arc::new(PcStore::new(&data, dir.path())),
    ));
    assert!(a.tokens().await.unwrap().is_some());
    assert!(b.tokens().await.unwrap().is_some());
    let ca = ConsoleClient::with_keeper(Arc::clone(&a));
    let cb = ConsoleClient::with_keeper(Arc::clone(&b));
    let (ra, rb) = tokio::join!(ca.me(), cb.me());
    ra.unwrap();
    rb.unwrap();
    assert_eq!(rotation.refreshes(), 1);
    rotation.assert_no_reuse();
    assert!(!dir.path().join("dnspilot-oauth.lock").exists());
}

#[tokio::test]
async fn a_crash_between_answer_and_write_never_reuses_the_token() {
    let rotation = StrictRotation::new("dpo_rt_0", Duration::ZERO);
    let server = console(&rotation).await;
    let dir = tempfile::tempdir().unwrap();
    let data = Arc::new(Mutex::new(Some(tokens(0, EXPIRED))));
    {
        let dying = PcStore::new(&data, dir.path());
        dying.fail_new_tokens.store(true, Ordering::SeqCst);
        let keeper = Arc::new(TokenKeeper::new(
            oauth(&server.uri(), Duration::from_secs(5)),
            Arc::new(dying),
        ));
        let err = ConsoleClient::with_keeper(keeper).me().await.unwrap_err();
        assert!(matches!(err, ProviderError::Io(_)), "{err}");
        // The process ends here: the new tokens were never written.
    }
    assert!(
        data.lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .refresh_in_flight
            .is_some()
    );
    // Restart: the stored refresh token may have been rotated, so it is not
    // sent again; the person is asked to connect DNSPilot again.
    let restarted = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_secs(5)),
        Arc::new(PcStore::new(&data, dir.path())),
    ));
    let err = ConsoleClient::with_keeper(restarted)
        .me()
        .await
        .unwrap_err();
    assert!(is_reauthorize(&err), "{err}");
    assert_eq!(rotation.refreshes(), 1);
    rotation.assert_no_reuse();
}

#[tokio::test]
async fn a_failed_write_is_retried_without_a_second_refresh() {
    let rotation = StrictRotation::new("dpo_rt_0", Duration::ZERO);
    let server = console(&rotation).await;
    let dir = tempfile::tempdir().unwrap();
    let data = Arc::new(Mutex::new(Some(tokens(0, EXPIRED))));
    let store = Arc::new(PcStore::new(&data, dir.path()));
    store.fail_new_tokens.store(true, Ordering::SeqCst);
    let keeper = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_secs(5)),
        Arc::clone(&store) as Arc<dyn TokenStore>,
    ));
    let client = ConsoleClient::with_keeper(Arc::clone(&keeper));
    assert!(client.me().await.is_err());
    store.fail_new_tokens.store(false, Ordering::SeqCst);
    client.me().await.unwrap();
    assert_eq!(rotation.refreshes(), 1);
    rotation.assert_no_reuse();
    let stored = data.lock().unwrap().clone().unwrap();
    assert_eq!(stored.refresh_token.expose(), "dpo_rt_1");
    assert!(stored.refresh_in_flight.is_none());
}

#[tokio::test]
async fn a_timeout_after_sending_is_not_resent() {
    // The console rotates, then answers too late.
    let rotation = StrictRotation::new("dpo_rt_0", Duration::from_secs(3));
    let server = console(&rotation).await;
    let dir = tempfile::tempdir().unwrap();
    let data = Arc::new(Mutex::new(Some(tokens(0, EXPIRED))));
    let keeper = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_millis(300)),
        Arc::new(PcStore::new(&data, dir.path())),
    ));
    let client = ConsoleClient::with_keeper(Arc::clone(&keeper));
    let err = client.me().await.unwrap_err();
    assert!(is_reauthorize(&err), "{err}");
    // Asking again does not resend the token, in this process or after a
    // restart.
    assert!(is_reauthorize(&client.me().await.unwrap_err()));
    let restarted = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_millis(300)),
        Arc::new(PcStore::new(&data, dir.path())),
    ));
    let err = ConsoleClient::with_keeper(restarted)
        .me()
        .await
        .unwrap_err();
    assert!(is_reauthorize(&err), "{err}");
    assert_eq!(rotation.refreshes(), 1);
    rotation.assert_no_reuse();
}

#[tokio::test]
async fn a_request_that_never_left_keeps_the_token_usable() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // Nothing listens there any more: the connection is refused.
    let store = Arc::new(MemoryTokenStore::new(Some(tokens(0, EXPIRED)), None));
    let keeper = Arc::new(TokenKeeper::new(
        oauth(&format!("http://127.0.0.1:{port}"), Duration::from_secs(5)),
        store,
    ));
    let err = ConsoleClient::with_keeper(Arc::clone(&keeper))
        .me()
        .await
        .unwrap_err();
    assert!(!is_reauthorize(&err), "{err}");
    let stored = keeper.stored_tokens().unwrap();
    assert_eq!(stored.refresh_token.expose(), "dpo_rt_0");
    assert!(stored.refresh_in_flight.is_none(), "marker cleared");
}

#[tokio::test]
async fn a_valid_access_token_is_not_refreshed_on_start() {
    let rotation = StrictRotation::new("dpo_rt_5", Duration::ZERO);
    let server = console(&rotation).await;
    let store = Arc::new(MemoryTokenStore::new(Some(tokens(5, FAR_FUTURE)), None));
    let keeper = Arc::new(TokenKeeper::new(
        oauth(&server.uri(), Duration::from_secs(5)),
        store,
    ));
    ConsoleClient::with_keeper(keeper).me().await.unwrap();
    assert_eq!(rotation.refreshes(), 0);
}
