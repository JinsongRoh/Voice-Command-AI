//! Contextual correction and topic detection against a mock
//! OpenAI-compatible chat completions endpoint (no real network).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use vca_speech::llm::{ChatClient, ChatConfig, ContextCorrector, CorrectionInput, Fix, prompts};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TEXT: &str = "리액트 컴포넌트에서 use effect 가 두 번 실행돼요. 클린업 함수를 확인해 줘";

fn corrector(server: &MockServer, timeout: Duration) -> ContextCorrector {
    ContextCorrector::new(Arc::new(
        ChatClient::new(ChatConfig {
            base_url: format!("{}/v1", server.uri()),
            api_key: Some("sk-llm".into()),
            model: "small-model".into(),
            timeout,
            auth: Default::default(),
        })
        .unwrap(),
    ))
}

fn reply(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}}],
    }))
}

fn input(terms: &[String]) -> CorrectionInput<'_> {
    CorrectionInput {
        text: TEXT,
        preceding: Some("프론트엔드 버그를 보고 있어요."),
        topic: Some("frontend/React"),
        terms,
        language: Some("ko"),
    }
}

async fn mount(server: &MockServer, content: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(reply(content))
        .mount(server)
        .await;
}

#[tokio::test]
async fn applies_good_fixes_and_sends_a_json_only_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-llm"))
        .and(body_partial_json(
            json!({"model": "small-model", "temperature": 0}),
        ))
        .respond_with(reply(
            r#"{"fixes":[{"from":"use effect","to":"useEffect"}]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let terms = vec!["useEffect".to_owned(), "cleanup".to_owned()];
    let out = corrector(&server, Duration::from_secs(4))
        .correct(&input(&terms))
        .await
        .unwrap();
    assert_eq!(
        out.text,
        "리액트 컴포넌트에서 useEffect 가 두 번 실행돼요. 클린업 함수를 확인해 줘"
    );
    assert_eq!(out.fixes, vec![Fix::new("use effect", "useEffect")]);

    let requests: Vec<Request> = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], prompts::CORRECTION_SYSTEM);
    let user: serde_json::Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user["text"], TEXT);
    assert_eq!(user["topic"], "frontend/React");
    assert_eq!(user["preceding_text"], "프론트엔드 버그를 보고 있어요.");
    assert_eq!(user["domain_terms"], json!(["useEffect", "cleanup"]));
}

#[tokio::test]
async fn rejects_a_from_that_is_not_in_the_text() {
    let server = MockServer::start().await;
    mount(
        &server,
        r#"{"fixes":[{"from":"use affect","to":"useEffect"}]}"#,
    )
    .await;
    let err = corrector(&server, Duration::from_secs(4))
        .correct(&input(&[]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not occur"), "{err}");
}

#[tokio::test]
async fn rejects_a_too_large_edit() {
    let server = MockServer::start().await;
    mount(
        &server,
        r#"{"fixes":[{"from":"리액트 컴포넌트에서 use effect 가 두 번 실행돼요.","to":"리액트 컴포넌트의 이펙트가 중복 실행됩니다."}]}"#,
    )
    .await;
    let err = corrector(&server, Duration::from_secs(4))
        .correct(&input(&[]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
}

#[tokio::test]
async fn times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(reply(r#"{"fixes":[]}"#).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let started = std::time::Instant::now();
    let err = corrector(&server, Duration::from_millis(300))
        .correct(&input(&[]))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(err.to_string().contains("http error"), "{err}");
}

#[tokio::test]
async fn bad_json_is_an_error() {
    for content in [
        "I fixed use effect to useEffect.",
        r#"{"fixes":[{"from":"use effect"}]}"#,
        r#"{"fixes":"#,
    ] {
        let server = MockServer::start().await;
        mount(&server, content).await;
        assert!(
            corrector(&server, Duration::from_secs(4))
                .correct(&input(&[]))
                .await
                .is_err(),
            "{content}"
        );
    }
    // A non-chat body and an error status are errors too.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    assert!(
        corrector(&server, Duration::from_secs(4))
            .correct(&input(&[]))
            .await
            .is_err()
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429).set_body_json(json!({"error": {"message": "slow down"}})),
        )
        .mount(&server)
        .await;
    let err = corrector(&server, Duration::from_secs(4))
        .correct(&input(&[]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("slow down"), "{err}");
}

#[tokio::test]
async fn detects_a_topic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"temperature": 0})))
        .respond_with(reply(
            r#"```json
{"topic":"Kubernetes deploy","terms":["kubectl","Helm","rollout","kubectl"]}
```"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let topic = corrector(&server, Duration::from_secs(4))
        .detect_topic(
            Some("frontend/React"),
            "큐브 컨트롤로 롤아웃 상태 확인해 줘",
        )
        .await
        .unwrap();
    assert_eq!(topic.topic, "Kubernetes deploy");
    assert_eq!(topic.terms, vec!["kubectl", "Helm", "rollout"]);
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["messages"][0]["content"], prompts::TOPIC_SYSTEM);
}

#[tokio::test]
async fn known_terms_are_never_replaced() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(reply(
            r#"{"fixes":[{"from":"PNPM","to":"npm"},{"from":"use effect","to":"useEffect"}]}"#,
        ))
        .mount(&server)
        .await;
    let terms = vec!["pnpm".to_owned()];
    let text = "PNPM 테스트 실행하고 리액트 컴포넌트의 use effect 정리 함수를 고쳐줘";
    let out = corrector(&server, Duration::from_secs(5))
        .correct(&CorrectionInput {
            text,
            preceding: None,
            topic: None,
            terms: &terms,
            language: Some("ko"),
        })
        .await
        .unwrap();
    assert_eq!(
        out.text,
        "PNPM 테스트 실행하고 리액트 컴포넌트의 useEffect 정리 함수를 고쳐줘"
    );
    assert_eq!(out.fixes, vec![Fix::new("use effect", "useEffect")]);
}
