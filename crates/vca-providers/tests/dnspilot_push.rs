//! DNSPilot push calls against a wiremock console. No live DNSPilot calls.

use std::time::Duration;

use serde_json::json;
use vca_providers::dnspilot::console::ConsoleClient;
use vca_providers::dnspilot::oauth::{DeviceFlowClient, OAuthConfig, TokenSet};
use vca_providers::dnspilot::push::{
    PushCiphertext, PushPlatform, PushSend, RegisterPushDevice, decode_base64,
};
use vca_providers::{ProviderError, Secret};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn console(server: &MockServer) -> ConsoleClient {
    let tokens: TokenSet = serde_json::from_value(json!({
        "access_token": "dpo_at_1", "refresh_token": "dpo_rt_1", "token_type": "Bearer",
        "expires_at": 4_000_000_000u64, "scope": "push:register push:send"
    }))
    .unwrap();
    ConsoleClient::new(
        DeviceFlowClient::new(OAuthConfig {
            console_url: server.uri(),
            ..OAuthConfig::default()
        }),
        tokens,
    )
}

fn registration() -> RegisterPushDevice {
    RegisterPushDevice {
        app_id: "test.app".into(),
        platform: PushPlatform::Android,
        token: Secret::new("fcm-token-value"),
        agent_device_id: Some(7),
        e2e_pubkey: Some("cHVibGlj".into()),
        locale: Some("ko".into()),
    }
}

#[tokio::test]
async fn register_posts_the_documented_body_with_the_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/devices"))
        .and(header("authorization", "Bearer dpo_at_1"))
        .and(body_json(json!({
            "app_id": "test.app", "platform": "android", "token": "fcm-token-value",
            "agent_device_id": 7, "e2e_pubkey": "cHVibGlj", "locale": "ko"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 41, "app_id": "test.app", "platform": "android", "agent_device_id": 7,
            "e2e_pubkey": "cHVibGlj", "created_at": "2026-10-31T00:00:00Z",
            "invalid_at": null, "last_ok_at": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let device = console(&server)
        .push_register(&registration())
        .await
        .unwrap();
    assert_eq!(device.id, 41);
    assert_eq!(device.invalid_at, None);
}

#[tokio::test]
async fn optional_registration_fields_are_left_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/devices"))
        .and(body_json(json!({
            "app_id": "test.app", "platform": "ios", "token": "apns"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    console(&server)
        .push_register(&RegisterPushDevice {
            app_id: "test.app".into(),
            platform: PushPlatform::Ios,
            token: Secret::new("apns"),
            agent_device_id: None,
            e2e_pubkey: None,
            locale: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn unregister_deletes_by_id() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/push/devices/41"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    console(&server).push_unregister(41).await.unwrap();
}

#[tokio::test]
async fn send_posts_base64_ciphertext_to_one_device_and_uses_default_ttl_and_priority() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/send"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "ok": true, "queued": 1, "message_ids": [9]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let sent = console(&server)
        .push_send(&PushSend::new(
            PushCiphertext::new(b"sealed").unwrap(),
            41,
            "abc".into(),
        ))
        .await
        .unwrap();
    assert_eq!(sent.queued, 1);
    assert_eq!(sent.message_ids, vec![9]);

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["device_id"], 41);
    assert_eq!(body["collapse_key"], "abc");
    assert_eq!(
        decode_base64(body["ciphertext"].as_str().unwrap()).unwrap(),
        b"sealed"
    );
    assert!(body.get("ttl").is_none() && body.get("priority").is_none());
}

#[tokio::test]
async fn errors_keep_their_kind_and_lose_the_server_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/devices"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"detail": "scope missing for fcm-token-value"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/push/send"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/push/devices/5"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "gone"})))
        .mount(&server)
        .await;

    let client = console(&server);
    let e = client.push_register(&registration()).await.unwrap_err();
    assert!(matches!(e, ProviderError::Forbidden(_)));
    assert!(!e.to_string().contains("fcm-token-value"));

    let e = client
        .push_send(&PushSend::new(
            PushCiphertext::new(b"x").unwrap(),
            1,
            "k".into(),
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        e,
        ProviderError::RateLimited {
            retry_after: Some(d)
        } if d == Duration::from_secs(30)
    ));

    assert!(matches!(
        client.push_unregister(5).await.unwrap_err(),
        ProviderError::NotFound(_)
    ));
}
