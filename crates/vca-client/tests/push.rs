//! Approval alerts against a wiremock DNSPilot console. No live calls.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use vca_client::push::{
    NotifyOutcome, PUSH_FILE, PUSH_OAUTH_TOKENS_NAME, PendingAlert, PhoneRegistration, PushConfig,
    PushManager, PushPlatform, open_sealed, phone_public_key,
};
use vca_net::{KeyStore, MemoryKeyStore};
use vca_providers::Secret;
use vca_providers::dnspilot::oauth::OAuthConfig;
use vca_providers::dnspilot::push::PUSH_SCOPES;
use wiremock::matchers::{body_partial_json, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROMPT: &str = "Allow Bash: rm -rf E:/private/project?";

struct Fixture {
    server: MockServer,
    secrets: Arc<MemoryKeyStore>,
    dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/push/devices"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 41})))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/api/v1/push/devices/\d+$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Self {
            server,
            secrets: Arc::new(MemoryKeyStore::default()),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn config(&self, app_id: Option<&str>) -> PushConfig {
        PushConfig {
            oauth: OAuthConfig {
                console_url: self.server.uri(),
                scopes: PUSH_SCOPES.iter().map(|s| (*s).to_owned()).collect(),
                interval_unit: Duration::from_millis(5),
                ..OAuthConfig::default()
            },
            app_id: app_id.map(str::to_owned),
            failure_backoff: Duration::ZERO,
            not_configured_backoff: Duration::ZERO,
        }
    }

    fn manager(&self) -> PushManager {
        self.manager_with(Some("test.app"))
    }

    fn manager_with(&self, app_id: Option<&str>) -> PushManager {
        PushManager::open(
            self.dir.path().to_path_buf(),
            self.secrets.clone(),
            self.config(app_id),
        )
    }

    fn authorize(&self) {
        let tokens = json!({
            "access_token": "dpo_at_push", "refresh_token": "dpo_rt_push", "token_type": "Bearer",
            "expires_at": 4_000_000_000u64, "scope": "push:register push:send"
        });
        self.secrets
            .save(PUSH_OAUTH_TOKENS_NAME, tokens.to_string().as_bytes())
            .unwrap();
    }

    async fn enabled(&self) -> PushManager {
        self.authorize();
        let m = self.manager();
        m.set_enabled(true).await.unwrap();
        m
    }

    async fn accept_sends(&self) {
        Mock::given(method("POST"))
            .and(path("/api/v1/push/send"))
            .respond_with(
                ResponseTemplate::new(202)
                    .set_body_json(json!({"ok": true, "queued": 1, "message_ids": [1]})),
            )
            .mount(&self.server)
            .await;
    }

    async fn bodies(&self, url: &str) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == url && r.method.as_str() == "POST")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    async fn sends(&self) -> Vec<Value> {
        self.bodies("/api/v1/push/send").await
    }

    async fn deletes(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "DELETE")
            .count()
    }
}

fn phone(id: &str, keys: &MemoryKeyStore, token: &str) -> PhoneRegistration {
    PhoneRegistration {
        phone: id.into(),
        machine: "0123456789abcdef".into(),
        platform: PushPlatform::Android,
        token: Secret::new(token),
        e2e_pubkey: phone_public_key(keys).unwrap(),
        locale: Some("ko".into()),
    }
}

fn alert(decision: &str) -> PendingAlert {
    PendingAlert {
        session_id: "session-1".into(),
        decision_id: decision.into(),
    }
}

fn sessions(decision: Option<&str>) -> Value {
    json!({"type": "sessions", "sessions": [{
        "id": "session-1",
        "display_name": "secret project",
        "preview": "secret output",
        "pending_decision": decision.map(|d| json!({
            "decision": {"id": d, "prompt": PROMPT, "options": [{"id": "y", "label": "Yes"}]},
            "at": 1
        })),
    }]})
}

#[tokio::test]
async fn alerts_are_off_by_default_and_nothing_leaves_the_pc() {
    let f = Fixture::new().await;
    f.authorize();
    f.accept_sends().await;
    let m = f.manager();
    let keys = MemoryKeyStore::default();
    assert!(!m.enabled());
    let phones = [phone("p1", &keys, "fcm-token")];
    let outcome = m
        .tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    assert_eq!(outcome, NotifyOutcome::default());
    assert!(f.server.received_requests().await.unwrap().is_empty());
    assert!(m.status().unwrap().phones.is_empty());
}

#[tokio::test]
async fn enabling_needs_the_app_id_and_the_grant() {
    let f = Fixture::new().await;
    let m = f.manager();
    assert_eq!(
        m.set_enabled(true).await.unwrap_err().code(),
        "push_not_authorized"
    );
    f.authorize();
    let unconfigured = f.manager_with(None);
    assert!(!unconfigured.status().unwrap().available);
    assert_eq!(
        unconfigured.set_enabled(true).await.unwrap_err().code(),
        "push_not_configured"
    );
    assert!(!m.enabled());
    assert!(m.set_enabled(true).await.unwrap().enabled);
    // Persisted.
    assert!(f.manager().enabled());
}

#[tokio::test]
async fn a_grant_without_the_push_scopes_does_not_count() {
    let f = Fixture::new().await;
    let tokens = json!({
        "access_token": "a", "refresh_token": "r", "token_type": "Bearer",
        "expires_at": 4_000_000_000u64, "scope": "push:register"
    });
    f.secrets
        .save(PUSH_OAUTH_TOKENS_NAME, tokens.to_string().as_bytes())
        .unwrap();
    assert!(!f.manager().status().unwrap().authorized);
}

#[tokio::test]
async fn approval_requests_only_the_push_scopes_and_stores_a_separate_grant() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device_authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc", "user_code": "BCDF-GHJK",
            "verification_uri": format!("{}/oauth/activate", f.server.uri()),
            "expires_in": 600, "interval": 1
        })))
        .expect(1)
        .mount(&f.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dpo_at_new", "refresh_token": "dpo_rt_new",
            "token_type": "Bearer", "expires_in": 3600, "scope": "push:register push:send"
        })))
        .mount(&f.server)
        .await;
    let m = f.manager();
    let prompt = m.start_authorization().await.unwrap();
    assert_eq!(prompt.user_code, "BCDF-GHJK");
    assert!(m.status().unwrap().pending_approval);
    assert!(m.activation_url().unwrap().ends_with("/oauth/activate"));
    assert!(m.wait_for_authorization().await.unwrap().authorized);

    let requests = f.server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&requests[0].body).replace("%3A", ":");
    assert!(body.contains("push:register") && body.contains("push:send"));
    assert!(!body.contains("tunnels:"));
    assert!(
        f.secrets
            .load(vca_client::dnspilot::OAUTH_TOKENS_NAME)
            .unwrap()
            .is_none()
    );
    assert!(f.secrets.load(PUSH_OAUTH_TOKENS_NAME).unwrap().is_some());
}

#[tokio::test]
async fn a_phone_is_registered_once_and_its_token_is_not_stored() {
    let f = Fixture::new().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "fcm-token-secret")];
    m.sync_phones(&phones, Some(7)).await.unwrap();
    m.sync_phones(&phones, Some(7)).await.unwrap();

    let posts = f.bodies("/api/v1/push/devices").await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["app_id"], "test.app");
    assert_eq!(posts[0]["platform"], "android");
    assert_eq!(posts[0]["token"], "fcm-token-secret");
    assert_eq!(posts[0]["agent_device_id"], 7);
    assert_eq!(posts[0]["locale"], "ko");
    assert_eq!(posts[0]["e2e_pubkey"], phones[0].e2e_pubkey);

    let info = m.status().unwrap().phones;
    assert_eq!(info.len(), 1);
    assert!(info[0].registered && !info[0].muted);
    let file = std::fs::read_to_string(f.dir.path().join(PUSH_FILE)).unwrap();
    assert!(!file.contains("fcm-token-secret"));
    assert!(!file.contains("dpo_"));
}

#[tokio::test]
async fn a_new_token_replaces_the_old_device_and_keeps_the_mute() {
    let f = Fixture::new().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "old")], None)
        .await
        .unwrap();
    m.set_muted("p1", true).unwrap();
    m.sync_phones(&[phone("p1", &keys, "new")], None)
        .await
        .unwrap();
    assert_eq!(f.deletes().await, 1);
    assert_eq!(f.bodies("/api/v1/push/devices").await.len(), 2);
    assert!(m.status().unwrap().phones[0].muted);
}

#[tokio::test]
async fn an_unpaired_phone_is_removed_from_dnspilot() {
    let f = Fixture::new().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();
    m.sync_phones(&[], None).await.unwrap();
    assert_eq!(f.deletes().await, 1);
    assert!(m.status().unwrap().phones.is_empty());
}

#[tokio::test]
async fn a_token_dnspilot_calls_invalid_is_not_used() {
    let f = Fixture::new().await;
    f.authorize();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/devices"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 1, "invalid_at": "2026-10-31T00:00:00Z"
        })))
        .mount(&server)
        .await;
    let mut config = f.config(Some("test.app"));
    config.oauth.console_url = server.uri();
    let m = PushManager::open(f.dir.path().to_path_buf(), f.secrets.clone(), config);
    m.set_enabled(true).await.unwrap();
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();
    let info = &m.status().unwrap().phones[0];
    assert!(!info.registered);
    assert_eq!(info.last_error.as_deref(), Some("push_token_invalid"));
    // The same token is not offered again; a new one is.
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    m.sync_phones(&[phone("p1", &keys, "t2")], None)
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_409_means_dnspilot_has_not_configured_push_yet() {
    let f = Fixture::new().await;
    f.authorize();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/devices"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"detail": "push not configured for this app"})),
        )
        .mount(&server)
        .await;
    let mut config = f.config(Some("io.metavu.voicecommandai.mobile"));
    config.oauth.console_url = server.uri();
    config.not_configured_backoff = Duration::from_secs(600);
    let m = PushManager::open(f.dir.path().to_path_buf(), f.secrets.clone(), config);
    m.set_enabled(true).await.unwrap();
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "fcm-token-secret")];
    m.sync_phones(&phones, None).await.unwrap();
    let status = m.status().unwrap();
    assert!(status.not_configured);
    assert!(!status.phones[0].registered);
    assert_eq!(
        status.phones[0].last_error.as_deref(),
        Some("push_not_configured")
    );
    // Not retried every tick: that needs a DNSPilot admin.
    m.sync_phones(&phones, None).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let body =
        String::from_utf8_lossy(&server.received_requests().await.unwrap()[0].body).into_owned();
    assert!(body.contains("io.metavu.voicecommandai.mobile"));
    assert!(!format!("{status:?}").contains("fcm-token-secret"));
}

#[tokio::test]
async fn a_new_pending_decision_sends_one_sealed_reference() {
    let f = Fixture::new().await;
    f.accept_sends().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "fcm-token")];

    let idle = m.tick(&sessions(None), Some(&phones), None).await.unwrap();
    assert_eq!(idle.sent, 0);
    let first = m
        .tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    assert_eq!(first.sent, 1);

    let sends = f.sends().await;
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["device_id"], 41);
    let ciphertext = sends[0]["ciphertext"].as_str().unwrap();
    let opened = open_sealed(&keys, ciphertext).unwrap();
    assert_eq!(opened.m, "0123456789abcdef");
    assert_eq!(opened.s, "session-1");
    assert_eq!(opened.d, "d1");
}

#[tokio::test]
async fn nothing_private_leaves_in_any_request() {
    let f = Fixture::new().await;
    f.accept_sends().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "fcm-token")];
    m.tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    for request in f.server.received_requests().await.unwrap() {
        let body = String::from_utf8_lossy(&request.body);
        for private in [
            "rm -rf",
            "private/project",
            "secret project",
            "secret output",
        ] {
            assert!(!body.contains(private), "{private} leaked");
        }
        // The session and decision ids are hashed or sealed.
        for id in ["session-1", "\"d1\""] {
            assert!(!body.contains(id), "{id} leaked");
        }
    }
}

#[tokio::test]
async fn one_push_per_decision_even_across_restarts() {
    let f = Fixture::new().await;
    f.accept_sends().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "t")];
    m.tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    let again = m
        .tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    assert_eq!((again.sent, again.deduped), (0, 1));

    let restarted = f.manager();
    let after = restarted
        .tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    assert_eq!((after.sent, after.deduped), (0, 1));
    assert_eq!(f.sends().await.len(), 1);

    let next = restarted
        .tick(&sessions(Some("d2")), Some(&phones), None)
        .await
        .unwrap();
    assert_eq!(next.sent, 1);
    assert_eq!(f.sends().await.len(), 2);
}

#[tokio::test]
async fn a_muted_phone_gets_nothing_and_the_other_one_does() {
    let f = Fixture::new().await;
    f.accept_sends().await;
    let m = f.enabled().await;
    let (a, b) = (MemoryKeyStore::default(), MemoryKeyStore::default());
    m.sync_phones(&[phone("p1", &a, "t1"), phone("p2", &b, "t2")], None)
        .await
        .unwrap();
    m.set_muted("p1", true).unwrap();
    let outcome = m.notify(&alert("d1")).await.unwrap();
    assert_eq!((outcome.sent, outcome.muted), (1, 1));
    assert_eq!(f.sends().await.len(), 1);
    let sent = f.sends().await;
    let ciphertext = sent[0]["ciphertext"].as_str().unwrap();
    assert!(open_sealed(&a, ciphertext).is_none());
    assert!(open_sealed(&b, ciphertext).is_some());

    assert_eq!(m.set_muted("nobody", true).unwrap_err().code(), "not_found");
    // The mute is persisted.
    assert!(f.manager().status().unwrap().phones[0].muted);
}

#[tokio::test]
async fn each_phone_gets_its_own_collapse_key_and_retries_share_it() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/send"))
        .and(body_partial_json(json!({"device_id": 41})))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&f.server)
        .await;
    f.accept_sends().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();

    let first = m.notify(&alert("d1")).await.unwrap();
    assert_eq!((first.sent, first.failed), (0, 1));
    assert_eq!(
        m.status().unwrap().phones[0].last_error.as_deref(),
        Some("push_failed")
    );
    // Not in the ledger: the next tick tries again and clears the error.
    let second = m.notify(&alert("d1")).await.unwrap();
    assert_eq!(second.sent, 1);
    assert_eq!(m.status().unwrap().phones[0].last_error, None);

    let sends = f.sends().await;
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0]["collapse_key"], sends[1]["collapse_key"]);
    assert_eq!(sends[0]["collapse_key"].as_str().unwrap().len(), 32);
    assert_ne!(sends[0]["ciphertext"], sends[1]["ciphertext"]);
}

#[tokio::test]
async fn retry_after_is_honored() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/send"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "600"))
        .mount(&f.server)
        .await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();
    assert!(m.notify(&alert("d1")).await.unwrap().deferred);
    assert!(m.notify(&alert("d1")).await.unwrap().deferred);
    assert_eq!(f.sends().await.len(), 1);
}

#[tokio::test]
async fn a_device_dnspilot_no_longer_knows_is_registered_again() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/send"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "gone"})))
        .up_to_n_times(1)
        .mount(&f.server)
        .await;
    f.accept_sends().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "t")];
    m.sync_phones(&phones, None).await.unwrap();
    assert_eq!(m.notify(&alert("d1")).await.unwrap().failed, 1);
    let info = &m.status().unwrap().phones[0];
    assert!(!info.registered);
    assert_eq!(info.last_error.as_deref(), Some("push_device_gone"));
    let next = m
        .tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    assert_eq!(next.sent, 1);
    assert_eq!(f.bodies("/api/v1/push/devices").await.len(), 2);
}

#[tokio::test]
async fn a_missing_role_is_reported_per_phone() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/send"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"detail": "viewer"})))
        .mount(&f.server)
        .await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();
    assert_eq!(m.notify(&alert("d1")).await.unwrap().failed, 1);
    assert_eq!(
        m.status().unwrap().phones[0].last_error.as_deref(),
        Some("push_forbidden")
    );
}

#[tokio::test]
async fn turning_off_removes_the_phones_and_stops_sends() {
    let f = Fixture::new().await;
    f.accept_sends().await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "t")];
    m.sync_phones(&phones, None).await.unwrap();
    let off = m.set_enabled(false).await.unwrap();
    assert!(!off.enabled);
    assert!(!off.phones[0].registered);
    assert_eq!(f.deletes().await, 1);
    m.tick(&sessions(Some("d1")), Some(&phones), None)
        .await
        .unwrap();
    assert!(f.sends().await.is_empty());
    assert_eq!(f.bodies("/api/v1/push/devices").await.len(), 1);

    // On again: the next sync registers the same phone.
    m.set_enabled(true).await.unwrap();
    m.sync_phones(&phones, None).await.unwrap();
    assert!(m.status().unwrap().phones[0].registered);
}

#[tokio::test]
async fn disconnect_revokes_the_grant_and_turns_alerts_off() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&f.server)
        .await;
    let m = f.enabled().await;
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();
    let status = m.disconnect().await.unwrap();
    assert!(!status.enabled && !status.authorized);
    assert_eq!(f.deletes().await, 1);
    assert!(f.secrets.load(PUSH_OAUTH_TOKENS_NAME).unwrap().is_none());
}

#[tokio::test]
async fn a_failure_pauses_dnspilot_calls_and_an_idle_sync_makes_none() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/send"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&f.server)
        .await;
    f.authorize();
    let mut config = f.config(Some("test.app"));
    config.failure_backoff = Duration::from_secs(600);
    let m = PushManager::open(f.dir.path().to_path_buf(), f.secrets.clone(), config);
    m.set_enabled(true).await.unwrap();
    let keys = MemoryKeyStore::default();
    let phones = [phone("p1", &keys, "t")];
    m.sync_phones(&phones, None).await.unwrap();
    m.sync_phones(&phones, None).await.unwrap();
    assert_eq!(f.server.received_requests().await.unwrap().len(), 1);

    assert_eq!(m.notify(&alert("d1")).await.unwrap().failed, 1);
    assert!(m.notify(&alert("d1")).await.unwrap().deferred);
    assert_eq!(f.sends().await.len(), 1);
}

#[tokio::test]
async fn a_rotated_refresh_token_is_stored() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dpo_at_2", "refresh_token": "dpo_rt_2",
            "token_type": "Bearer", "expires_in": 3600, "scope": "push:register push:send"
        })))
        .mount(&f.server)
        .await;
    f.accept_sends().await;
    let tokens = json!({
        "access_token": "dpo_at_1", "refresh_token": "dpo_rt_1", "token_type": "Bearer",
        "expires_at": 1, "scope": "push:register push:send"
    });
    f.secrets
        .save(PUSH_OAUTH_TOKENS_NAME, tokens.to_string().as_bytes())
        .unwrap();
    let m = f.manager();
    m.set_enabled(true).await.unwrap();
    let keys = MemoryKeyStore::default();
    m.sync_phones(&[phone("p1", &keys, "t")], None)
        .await
        .unwrap();
    assert_eq!(m.notify(&alert("d1")).await.unwrap().sent, 1);
    let stored = f.secrets.load(PUSH_OAUTH_TOKENS_NAME).unwrap().unwrap();
    assert!(String::from_utf8_lossy(&stored).contains("dpo_rt_2"));
}
