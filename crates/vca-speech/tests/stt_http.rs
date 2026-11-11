//! HTTP engine tests against local mock servers (no real network).

use serde_json::json;
use vca_speech::stt::{
    GoogleAuth, GoogleSttConfig, GoogleSttEngine, OpenAiSttConfig, OpenAiSttEngine,
};
use vca_speech::{AudioFormat, AudioInput, SpeechError, SttEngine, TranscribeOptions};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn pcm_audio() -> AudioInput {
    AudioInput::from_pcm16(&[0, 100, -100, 200, -200, 0, 50, -50], 16_000)
}

fn openai(server: &MockServer, model: &str) -> OpenAiSttEngine {
    OpenAiSttEngine::new(OpenAiSttConfig {
        base_url: format!("{}/v1", server.uri()),
        api_key: Some("sk-test".into()),
        model: model.into(),
        ..OpenAiSttConfig::default()
    })
    .unwrap()
}

fn body_text(req: &Request) -> String {
    String::from_utf8_lossy(&req.body).into_owned()
}

/// Returns the value of a multipart text field.
fn form_field(body: &str, name: &str) -> Option<String> {
    let marker = format!("name=\"{name}\"");
    let start = body.find(&marker)? + marker.len();
    let rest = &body[start..];
    let value_start = rest.find("\r\n\r\n")? + 4;
    let value_end = rest[value_start..].find("\r\n--")? + value_start;
    Some(rest[value_start..value_end].to_owned())
}

#[tokio::test]
async fn openai_multipart_request_and_verbose_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": " run kubectl get pods ",
            "language": "english",
            "segments": [
                {"text": " run kubectl", "start": 0.0, "end": 1.2, "avg_logprob": -0.1},
                {"text": " get pods", "start": 1.2, "end": 2.0, "avg_logprob": -0.5}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let engine = openai(&server, "whisper-1");
    let options = TranscribeOptions {
        primary_language: Some("en-US".into()),
        secondary_languages: vec![],
        prompt_hint: Some("kubectl, useEffect".into()),
        phrase_hints: vec![],
    };
    let t = engine.transcribe(&pcm_audio(), &options).await.unwrap();
    assert_eq!(t.text, "run kubectl get pods");
    assert_eq!(t.language.as_deref(), Some("en"));
    assert_eq!(t.segments.len(), 2);
    assert_eq!(t.segments[1].start_ms, Some(1200));
    let c = t.segments[0].confidence.unwrap();
    assert!((c - (-0.1f32).exp()).abs() < 1e-4);

    let req = &server.received_requests().await.unwrap()[0];
    let body = body_text(req);
    assert_eq!(form_field(&body, "model").as_deref(), Some("whisper-1"));
    assert_eq!(form_field(&body, "language").as_deref(), Some("en"));
    assert_eq!(
        form_field(&body, "prompt").as_deref(),
        Some("kubectl, useEffect")
    );
    assert_eq!(
        form_field(&body, "response_format").as_deref(),
        Some("verbose_json")
    );
    // Raw PCM is uploaded as a WAV file.
    assert!(body.contains("filename=\"audio.wav\""));
    assert!(body.contains("Content-Type: audio/wav"));
    assert!(body.contains("RIFF"));
}

#[tokio::test]
async fn openai_code_switching_does_not_force_language() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"text": "セッション3を pause して"})),
        )
        .mount(&server)
        .await;
    let engine = openai(&server, "gpt-4o-transcribe");
    let options = TranscribeOptions {
        primary_language: Some("ja".into()),
        secondary_languages: vec!["en".into()],
        ..TranscribeOptions::default()
    };
    let audio = AudioInput::from_encoded(b"webm-bytes".to_vec(), AudioFormat::Webm, 48_000);
    let t = engine.transcribe(&audio, &options).await.unwrap();
    assert_eq!(t.text, "セッション3を pause して");
    assert!(t.segments.is_empty());

    let body = body_text(&server.received_requests().await.unwrap()[0]);
    assert_eq!(form_field(&body, "language"), None);
    assert_eq!(form_field(&body, "prompt"), None);
    assert_eq!(
        form_field(&body, "response_format").as_deref(),
        Some("json")
    );
    assert!(body.contains("filename=\"audio.webm\""));
}

#[tokio::test]
async fn openai_errors_are_mapped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}
        })))
        .mount(&server)
        .await;
    let err = openai(&server, "whisper-1")
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap_err();
    match &err {
        SpeechError::Api { status, message } => {
            assert_eq!(*status, 401);
            assert_eq!(message, "Incorrect API key provided");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(err.code(), vca_core::error::ErrorCode::Unauthorized);
}

#[tokio::test]
async fn openai_self_hosted_without_key_and_text_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello world\n"))
        .mount(&server)
        .await;
    let engine = OpenAiSttEngine::new(OpenAiSttConfig {
        base_url: format!("{}/", server.uri()),
        api_key: None,
        model: "Systran/faster-whisper-small".into(),
        response_format: Some("text".into()),
        ..OpenAiSttConfig::default()
    })
    .unwrap();
    let t = engine
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap();
    assert_eq!(t.text, "hello world");
    let req = &server.received_requests().await.unwrap()[0];
    assert!(req.headers.get("authorization").is_none());
}

#[tokio::test]
async fn openai_connection_failure_is_http_error() {
    // Reserve a free port, then close it so nothing listens there.
    let uri = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let engine = OpenAiSttEngine::new(OpenAiSttConfig {
        base_url: uri,
        ..OpenAiSttConfig::default()
    })
    .unwrap();
    let err = engine
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, SpeechError::Http(_)), "{err:?}");
}

fn google(server: &MockServer, auth: GoogleAuth) -> GoogleSttEngine {
    let mut config = GoogleSttConfig::new("my-project", auth);
    config.base_url = server.uri();
    config.model = "chirp_2".into();
    GoogleSttEngine::new(config).unwrap()
}

#[tokio::test]
async fn google_recognize_with_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/v2/projects/my-project/locations/global/recognizers/_:recognize",
        ))
        .and(query_param("key", "AIza-test"))
        .and(body_partial_json(json!({
            "config": {
                "model": "chirp_2",
                "languageCodes": ["ko-KR", "en-US"],
                "explicitDecodingConfig": {"encoding": "LINEAR16", "sampleRateHertz": 16000, "audioChannelCount": 1},
                "adaptation": {"phraseSets": [{"inlinePhraseSet": {"phrases": [
                    {"value": "kubectl", "boost": 10.0},
                    {"value": "Claude Code", "boost": 10.0}
                ]}}]}
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"alternatives": [{"transcript": "세션 2 pause 해줘", "confidence": 0.91}],
                 "languageCode": "ko-KR", "resultEndOffset": "1.800s"},
                {"alternatives": [{"transcript": "and run kubectl", "confidence": 0.8}],
                 "languageCode": "en-US", "resultEndOffset": "3.100s"}
            ],
            "metadata": {"totalBilledDuration": "4s"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let engine = google(
        &server,
        GoogleAuth::ApiKey {
            key: "AIza-test".into(),
        },
    );
    let options = TranscribeOptions {
        primary_language: Some("ko".into()),
        secondary_languages: vec!["en".into()],
        prompt_hint: Some("ignored because phrase hints are set".into()),
        phrase_hints: vec!["kubectl".into(), "Claude Code".into()],
    };
    let t = engine.transcribe(&pcm_audio(), &options).await.unwrap();
    assert_eq!(t.text, "세션 2 pause 해줘 and run kubectl");
    assert_eq!(t.language.as_deref(), Some("ko-kr"));
    assert_eq!(t.segments.len(), 2);
    assert_eq!(t.segments[1].start_ms, Some(1800));
    assert_eq!(t.segments[1].end_ms, Some(3100));
    assert_eq!(t.segments[0].confidence, Some(0.91));
    let conf = t.confidence().unwrap();
    assert!((conf - 0.855).abs() < 1e-3);
}

#[tokio::test]
async fn google_bearer_auth_auto_decoding_and_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer ya29.token"))
        .and(header("x-goog-user-project", "billing-project"))
        .and(body_partial_json(
            json!({"config": {"autoDecodingConfig": {}}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let mut config = GoogleSttConfig::new(
        "my-project",
        GoogleAuth::Bearer {
            token: "ya29.token".into(),
        },
    );
    config.base_url = server.uri();
    config.quota_project = Some("billing-project".into());
    let engine = GoogleSttEngine::new(config).unwrap();
    let audio = AudioInput::from_encoded(b"OggS".to_vec(), AudioFormat::Ogg, 48_000);
    let t = engine
        .transcribe(&audio, &TranscribeOptions::default())
        .await
        .unwrap();
    assert_eq!(t.text, "");
    assert!(t.segments.is_empty());

    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "message": "Permission denied on project", "status": "PERMISSION_DENIED"}
        })))
        .mount(&failing)
        .await;
    let err = google(&failing, GoogleAuth::ApiKey { key: "k".into() })
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, SpeechError::Api { status: 403, message } if message == "Permission denied on project"),
        "{err:?}"
    );
}

#[test]
fn google_requires_project() {
    let err = GoogleSttEngine::new(GoogleSttConfig::new(
        "",
        GoogleAuth::ApiKey { key: "k".into() },
    ))
    .unwrap_err();
    assert!(matches!(err, SpeechError::Config(_)));
}
