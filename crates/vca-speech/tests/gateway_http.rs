//! Engines behind an authenticating gateway: the exact headers of every
//! OpenAI-compatible request (speech-to-text, chat completions with and
//! without tools, TTS) in the three modes, against local mock servers.
//!
//! * default: `Authorization: Bearer <key>`;
//! * key header: the key raw in that header, no `Authorization`;
//! * gateway: `Authorization: Bearer <gateway token>` and the key raw in
//!   the key header.

use std::time::Duration;

use serde_json::json;
use vca_speech::llm::{ChatClient, ChatConfig};
use vca_speech::stt::{OpenAiSttConfig, OpenAiSttEngine};
use vca_speech::tts::{OpenAiTtsConfig, OpenAiTtsEngine};
use vca_speech::{
    AudioInput, EngineAuth, SpeechError, SttEngine, SynthesisRequest, TranscribeOptions, TtsEngine,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "sk-engine-key-71c2";
const TOKEN: &str = "gw-token-4d9e1a";
const KEY_HEADER: &str = "X-VCA-Key";

/// The three ways a request can authenticate.
fn modes() -> Vec<(&'static str, EngineAuth)> {
    vec![
        ("default", EngineAuth::default()),
        (
            "key header",
            EngineAuth {
                key_header: Some(KEY_HEADER.into()),
                gateway_token: None,
            },
        ),
        (
            "gateway",
            EngineAuth {
                key_header: Some(KEY_HEADER.into()),
                gateway_token: Some(TOKEN.into()),
            },
        ),
    ]
}

fn header<'a>(req: &'a Request, name: &str) -> Vec<&'a str> {
    req.headers
        .get_all(name)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect()
}

/// Asserts the headers of one request in `mode`.
fn assert_headers(mode: &str, req: &Request) {
    let auth = header(req, "authorization");
    let key = header(req, "x-vca-key");
    match mode {
        "default" => {
            assert_eq!(auth, vec![format!("Bearer {KEY}")], "{mode}");
            assert!(key.is_empty(), "{mode}");
        }
        "key header" => {
            assert!(auth.is_empty(), "{mode}: {auth:?}");
            assert_eq!(key, vec![KEY], "{mode}");
        }
        "gateway" => {
            assert_eq!(auth, vec![format!("Bearer {TOKEN}")], "{mode}");
            assert_eq!(key, vec![KEY], "{mode}");
        }
        other => panic!("unknown mode {other}"),
    }
}

async fn mock(server: &MockServer, body: ResponseTemplate) {
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(body)
        .mount(server)
        .await;
}

async fn last_request(server: &MockServer) -> Request {
    server.received_requests().await.unwrap().pop().unwrap()
}

#[tokio::test]
async fn speech_to_text_headers() {
    for (mode, auth) in modes() {
        let server = MockServer::start().await;
        mock(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({"text": "ok"})),
        )
        .await;
        let engine = OpenAiSttEngine::new(OpenAiSttConfig {
            base_url: format!("{}/v1", server.uri()),
            api_key: Some(KEY.into()),
            model: "gpt-4o-transcribe".into(),
            auth,
            ..OpenAiSttConfig::default()
        })
        .unwrap();
        let audio = AudioInput::from_pcm16(&[0, 100, -100, 0], 16_000);
        let out = engine
            .transcribe(&audio, &TranscribeOptions::default())
            .await
            .unwrap();
        assert_eq!(out.text, "ok");
        assert_headers(mode, &last_request(&server).await);
    }
}

fn chat(server: &MockServer, auth: EngineAuth) -> ChatClient {
    ChatClient::new(ChatConfig {
        base_url: format!("{}/v1", server.uri()),
        api_key: Some(KEY.into()),
        model: "small".into(),
        timeout: Duration::from_secs(5),
        auth,
    })
    .unwrap()
}

#[tokio::test]
async fn chat_completion_headers_with_and_without_tools() {
    for (mode, auth) in modes() {
        let server = MockServer::start().await;
        mock(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "ok"}}]})),
        )
        .await;
        let client = chat(&server, auth);
        assert_eq!(client.complete("system", "user").await.unwrap(), "ok");
        assert_headers(mode, &last_request(&server).await);
        let reply = client
            .complete_with_tools(&[json!({"role": "user", "content": "hi"})], &[], 16)
            .await
            .unwrap();
        assert_eq!(reply.content.as_deref(), Some("ok"));
        assert_headers(mode, &last_request(&server).await);
    }
}

#[tokio::test]
async fn text_to_speech_headers() {
    for (mode, auth) in modes() {
        let server = MockServer::start().await;
        mock(
            &server,
            ResponseTemplate::new(200).set_body_bytes(b"ID3audio".to_vec()),
        )
        .await;
        let engine = OpenAiTtsEngine::new(OpenAiTtsConfig {
            base_url: format!("{}/v1", server.uri()),
            api_key: Some(KEY.into()),
            auth,
            ..OpenAiTtsConfig::default()
        })
        .unwrap();
        let out = engine
            .synthesize(&SynthesisRequest {
                text: "hello".into(),
                voice: None,
                speed: None,
                language: None,
            })
            .await
            .unwrap();
        assert_eq!(out.audio, b"ID3audio");
        assert_headers(mode, &last_request(&server).await);
    }
}

#[tokio::test]
async fn a_gateway_token_without_a_key_header_is_rejected() {
    for key_header in [None, Some("Authorization".to_owned())] {
        let auth = EngineAuth {
            key_header,
            gateway_token: Some(TOKEN.into()),
        };
        let errors = [
            OpenAiSttEngine::new(OpenAiSttConfig {
                auth: auth.clone(),
                ..OpenAiSttConfig::default()
            })
            .unwrap_err(),
            ChatClient::new(ChatConfig {
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: Some(KEY.into()),
                model: "m".into(),
                timeout: Duration::from_secs(1),
                auth: auth.clone(),
            })
            .unwrap_err(),
            OpenAiTtsEngine::new(OpenAiTtsConfig {
                auth: auth.clone(),
                ..OpenAiTtsConfig::default()
            })
            .unwrap_err(),
        ];
        for err in errors {
            assert!(matches!(err, SpeechError::Config(_)), "{err:?}");
            let text = format!("{err} {err:?}");
            assert!(text.contains("api_key_header"), "{text}");
            assert!(!text.contains(TOKEN), "{text}");
        }
    }
    let bad_header = EngineAuth {
        key_header: Some("X Key".into()),
        gateway_token: None,
    };
    assert!(
        OpenAiTtsEngine::new(OpenAiTtsConfig {
            auth: bad_header,
            ..OpenAiTtsConfig::default()
        })
        .is_err()
    );
}

#[tokio::test]
async fn secrets_never_appear_in_debug_or_errors() {
    let (_, auth) = modes().pop().unwrap();
    let stt = OpenAiSttConfig {
        api_key: Some(KEY.into()),
        auth: auth.clone(),
        ..OpenAiSttConfig::default()
    };
    let chat_cfg = ChatConfig {
        base_url: "http://127.0.0.1:9/v1".into(),
        api_key: Some(KEY.into()),
        model: "m".into(),
        timeout: Duration::from_secs(1),
        auth: auth.clone(),
    };
    let tts = OpenAiTtsConfig {
        api_key: Some(KEY.into()),
        auth: auth.clone(),
        ..OpenAiTtsConfig::default()
    };
    let engine = OpenAiTtsEngine::new(tts.clone()).unwrap();
    for text in [
        format!("{stt:?}"),
        format!("{chat_cfg:?}"),
        format!("{tts:?}"),
        format!("{engine:?}"),
        format!("{:?}", OpenAiSttEngine::new(stt.clone()).unwrap()),
        format!("{:?}", chat(&MockServer::start().await, auth.clone())),
    ] {
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains(TOKEN), "{text}");
    }
    // A rejected request: the error names the status, not the secrets.
    let server = MockServer::start().await;
    mock(
        &server,
        ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "denied"}})),
    )
    .await;
    let err = chat(&server, auth.clone())
        .complete("s", "u")
        .await
        .unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains(KEY) && !text.contains(TOKEN), "{text}");
    // A transport failure neither.
    let err = OpenAiTtsEngine::new(OpenAiTtsConfig {
        base_url: "http://127.0.0.1:9/v1".into(),
        timeout: Duration::from_secs(2),
        ..tts
    })
    .unwrap()
    .synthesize(&SynthesisRequest {
        text: "x".into(),
        voice: None,
        speed: None,
        language: None,
    })
    .await
    .unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains(KEY) && !text.contains(TOKEN), "{text}");
}
