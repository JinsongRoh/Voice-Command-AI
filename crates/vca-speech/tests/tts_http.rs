//! The OpenAI-compatible TTS client against a mock speech endpoint.

use std::time::Duration;

use serde_json::json;
use vca_speech::tts::{OpenAiTtsConfig, OpenAiTtsEngine};
use vca_speech::{SpeechError, SynthesisRequest, TtsEngine, TtsFormat};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn config(server: &MockServer) -> OpenAiTtsConfig {
    OpenAiTtsConfig {
        base_url: format!("{}/v1/", server.uri()),
        api_key: None,
        model: "kokoro".into(),
        voice: "af_sky".into(),
        format: TtsFormat::Mp3,
        speed: Some(1.1),
        timeout: Duration::from_secs(10),
        max_audio_bytes: 1024,
        auth: Default::default(),
    }
}

fn request(text: &str, voice: Option<&str>) -> SynthesisRequest {
    SynthesisRequest {
        text: text.into(),
        voice: voice.map(str::to_owned),
        speed: None,
        language: None,
    }
}

#[tokio::test]
async fn posts_the_openai_speech_body_with_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .and(header("authorization", "Bearer sk-tts"))
        .and(body_json(json!({
            "model": "kokoro",
            "input": "Session needs a decision",
            "voice": "ko_voice",
            "response_format": "mp3",
            "speed": 1.1,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ID3audio".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.api_key = Some("sk-tts".into());
    let engine = OpenAiTtsEngine::new(cfg).unwrap();
    let out = engine
        .synthesize(&request("Session needs a decision", Some("ko_voice")))
        .await
        .unwrap();
    assert_eq!(out.format, TtsFormat::Mp3);
    assert_eq!(out.audio, b"ID3audio");
}

#[tokio::test]
async fn keyless_servers_get_no_authorization_and_the_default_voice() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(|req: &Request| {
            assert!(req.headers.get("authorization").is_none());
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["voice"], "af_sky");
            assert_eq!(body["response_format"], "wav");
            ResponseTemplate::new(200).set_body_bytes(b"RIFF".to_vec())
        })
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.format = TtsFormat::Wav;
    let engine = OpenAiTtsEngine::new(cfg).unwrap();
    let out = engine.synthesize(&request("hi", Some("  "))).await.unwrap();
    assert_eq!(out.format, TtsFormat::Wav);
}

#[tokio::test]
async fn answers_over_the_cap_are_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 4096]))
        .mount(&server)
        .await;
    let engine = OpenAiTtsEngine::new(config(&server)).unwrap();
    let err = engine.synthesize(&request("hi", None)).await.unwrap_err();
    assert!(matches!(err, SpeechError::Decode(_)), "{err}");
    assert!(err.to_string().contains("1024"));
}

#[tokio::test]
async fn errors_are_mapped_without_the_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "bad key"}})),
        )
        .mount(&server)
        .await;
    let engine = OpenAiTtsEngine::new(config(&server)).unwrap();
    let err = engine.synthesize(&request("hi", None)).await.unwrap_err();
    match &err {
        SpeechError::Api { status, message } => {
            assert_eq!(*status, 401);
            assert_eq!(message, "bad key");
        }
        other => panic!("{other:?}"),
    }

    // A closed port: a transport error that does not name the host.
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let engine = OpenAiTtsEngine::new(OpenAiTtsConfig {
        base_url: format!("http://{addr}/v1"),
        ..config(&server)
    })
    .unwrap();
    let err = engine.synthesize(&request("hi", None)).await.unwrap_err();
    assert!(matches!(err, SpeechError::Http(_)), "{err}");
    assert!(!err.to_string().contains(&addr.to_string()), "{err}");
}

#[tokio::test]
async fn empty_answers_are_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let engine = OpenAiTtsEngine::new(config(&server)).unwrap();
    assert!(engine.synthesize(&request("hi", None)).await.is_err());
}

#[tokio::test]
async fn the_language_is_sent_as_its_base_tag() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["language"], "ko");
            ResponseTemplate::new(200).set_body_bytes(b"ID3".to_vec())
        })
        .expect(2)
        .mount(&server)
        .await;
    let engine = OpenAiTtsEngine::new(config(&server)).unwrap();
    let mut req = request("세션 완료", None);
    req.language = Some("ko-KR".into());
    engine.synthesize(&req).await.unwrap();
    req.language = Some("ko".into());
    engine.synthesize(&req).await.unwrap();
}

#[tokio::test]
async fn a_server_refusing_the_language_is_asked_again_without_it() {
    let server = MockServer::start().await;
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<bool>::new()));
    let seen = std::sync::Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let has = body.get("language").is_some();
            seen.lock().unwrap().push(has);
            if has {
                ResponseTemplate::new(400).set_body_json(
                    json!({"error": {"message": "Unrecognized request argument supplied: language"}}),
                )
            } else {
                ResponseTemplate::new(200).set_body_bytes(b"ID3".to_vec())
            }
        })
        .mount(&server)
        .await;
    let engine = OpenAiTtsEngine::new(config(&server)).unwrap();
    let mut req = request("hello", None);
    req.language = Some("en-US".into());
    engine.synthesize(&req).await.unwrap();
    // Remembered: the next request goes without it at once.
    engine.synthesize(&req).await.unwrap();
    assert_eq!(*calls.lock().unwrap(), vec![true, false, false]);
}
