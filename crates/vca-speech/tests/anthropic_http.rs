//! The Anthropic Messages backend against a mock server (no real network,
//! and no real `ant` CLI: tokens come from an injected provider).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use vca_speech::llm::anthropic::{ANTHROPIC_VERSION, OAUTH_BETA};
use vca_speech::llm::{
    AnthropicAuth, AnthropicClient, AnthropicConfig, ChatBackend, ContextCorrector,
    CorrectionInput, Fix, TokenProvider, prompts,
};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEXT: &str = "please check the use effect cleanup in the settings page component";

fn client(server: &MockServer, auth: AnthropicAuth) -> AnthropicClient {
    let mut cfg = AnthropicConfig::new(auth, Duration::from_secs(4));
    cfg.base_url = server.uri();
    AnthropicClient::new(cfg).unwrap()
}

fn message(text: &str, stop_reason: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-haiku-5-5",
        "content": [{"type": "text", "text": text}],
        "stop_reason": stop_reason,
    }))
}

/// Hands out `token-1`, `token-2`, ... and counts refreshes.
#[derive(Default)]
struct CountingTokens {
    calls: AtomicUsize,
    refreshes: AtomicUsize,
}

#[async_trait]
impl TokenProvider for CountingTokens {
    async fn token(&self, refresh: bool) -> vca_speech::Result<String> {
        if refresh {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
        }
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("token-{n}"))
    }
}

#[tokio::test]
async fn corrects_with_an_api_key_and_no_sampling_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-test"))
        .and(header("anthropic-version", ANTHROPIC_VERSION))
        .and(header("content-type", "application/json"))
        .respond_with(message(
            r#"{"fixes":[{"from":"use effect","to":"useEffect"}]}"#,
            "end_turn",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let backend = client(&server, AnthropicAuth::ApiKey("sk-ant-test".into()));
    let out = ContextCorrector::new(Arc::new(backend))
        .correct(&CorrectionInput {
            text: TEXT,
            ..CorrectionInput::default()
        })
        .await
        .unwrap();
    assert_eq!(out.fixes, vec![Fix::new("use effect", "useEffect")]);
    assert!(out.text.contains("useEffect cleanup"));

    let req = &server.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["model"], "claude-haiku-5-5");
    assert_eq!(body["max_tokens"], 1024);
    assert_eq!(body["system"], prompts::CORRECTION_SYSTEM);
    for forbidden in ["temperature", "top_p", "top_k", "tool_choice"] {
        assert!(
            body.get(forbidden).is_none(),
            "{forbidden} must not be sent"
        );
    }
    // One user turn, no prefilled assistant turn.
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert!(req.headers.get("authorization").is_none());
}

#[tokio::test]
async fn refusal_and_max_tokens_are_errors() {
    for stop in ["refusal", "max_tokens"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(message(r#"{"fixes":[]}"#, stop))
            .mount(&server)
            .await;
        let backend = client(&server, AnthropicAuth::ApiKey("k".into()));
        assert!(backend.complete("s", "u").await.is_err(), "{stop}");
        // The corrector fails too, so the caller keeps its text.
        assert!(
            ContextCorrector::new(Arc::new(backend))
                .correct(&CorrectionInput {
                    text: TEXT,
                    ..CorrectionInput::default()
                })
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn bearer_tokens_are_refreshed_once_after_a_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer token-1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "expired"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer token-2"))
        .and(header("anthropic-beta", OAUTH_BETA))
        .respond_with(message(
            "{\"topic\":\"frontend/React\",\"terms\":[]}",
            "end_turn",
        ))
        .expect(1)
        .mount(&server)
        .await;
    // Never an API key header with bearer auth.
    Mock::given(header_exists("x-api-key"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tokens = Arc::new(CountingTokens::default());
    let backend = client(&server, AnthropicAuth::Token(tokens.clone()));
    let reply = backend.complete("s", "u").await.unwrap();
    assert!(reply.contains("frontend/React"));
    assert_eq!(tokens.refreshes.load(Ordering::SeqCst), 1);

    // A second 401 after the refresh is reported, not retried forever.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;
    let tokens = Arc::new(CountingTokens::default());
    let backend = client(&server, AnthropicAuth::Token(tokens.clone()));
    let err = backend.complete("s", "u").await.unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");
}

#[tokio::test]
async fn api_keys_are_not_retried_on_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let backend = client(&server, AnthropicAuth::ApiKey("bad".into()));
    assert!(backend.complete("s", "u").await.is_err());
}
