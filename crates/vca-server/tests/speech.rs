//! Server-side transcription over the local control channel, against a
//! mock OpenAI-compatible transcription API.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{AudioEncoding, ClientMsg, PROTOCOL_VERSION, ServerMsg, TranscriptIntent};
use vca_server::config::{Config, SpeechEngineKind};
use vca_server::control::ControlClient;
use vca_server::daemon::{Options, Server};
use wiremock::matchers::{body_string_contains, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Process control that never touches a real process (shared machines).
struct NoProcessControl;

impl ProcessControl for NoProcessControl {
    fn set_priority(&self, root: u32, _: Scope, _: Priority) -> ContainResult {
        Ok(vec![root])
    }
    fn suspend(&self, root: u32, _: Scope) -> ContainResult {
        Ok(vec![root])
    }
    fn resume(&self, root: u32, _: Scope) -> ContainResult {
        Ok(vec![root])
    }
    fn terminate(&self, root: u32, _: Scope, _: Duration) -> ContainResult {
        panic!("speech tests must never terminate processes ({root})")
    }
}

fn control_endpoint(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\.\pipe\vca-test-{}-{name}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join(format!("{name}.sock"))
    }
}

async fn start(dir: &Path, name: &str, config: Config) -> Server {
    let mut config = config;
    config.workspace_roots = vec![dir.to_owned()];
    config.discovery.enabled = false;
    config.remote.enabled = false;
    // An empty policy file keeps the developer's own policies out of the test.
    let policy = dir.join("policies.toml");
    std::fs::write(&policy, "").unwrap();
    config.broker.policy_path = Some(policy);
    Server::start(
        Options {
            config_file: Some(dir.join("config.toml")),
            control: Some(control_endpoint(dir, name)),
            data_dir: Some(dir.join("data")),
            process_control: Some(Arc::new(NoProcessControl)),
            restore_backend: None,
            verifier: Some(vca_server::security::verifier::FakeVerifier::available()),
        },
        Some(config),
    )
    .await
    .expect("vcad failed to start")
}

async fn connect(server: &Server) -> ControlClient {
    let mut client = ControlClient::connect(&server.control_endpoint)
        .await
        .unwrap();
    let welcome = client
        .api(ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: "test".into(),
        })
        .await
        .unwrap();
    assert!(matches!(welcome, ServerMsg::Welcome { .. }));
    client
}

fn wav() -> Vec<u8> {
    vca_speech::audio::encode_wav_pcm16(&[0i16; 1600], 16_000, 1).unwrap()
}

fn transcribe(audio: Vec<u8>, format: AudioEncoding) -> ClientMsg {
    ClientMsg::Transcribe {
        audio,
        format,
        sample_rate: None,
        language: None,
        session_id: None,
        context: None,
        is_final: false,
    }
}

fn chunk(is_final: bool) -> ClientMsg {
    ClientMsg::Transcribe {
        audio: wav(),
        format: AudioEncoding::Wav,
        sample_rate: None,
        language: None,
        session_id: None,
        context: Some("checkout page bug".into()),
        is_final,
    }
}

fn chat(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": content}}]
    }))
}

#[tokio::test]
async fn transcribes_through_an_openai_compatible_server() {
    let stt = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "uh run p n p m test and check kube control",
            "language": "english",
        })))
        .expect(1)
        .mount(&stt)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.speech.engine = SpeechEngineKind::Openai;
    config.speech.language = Some("en".into());
    config.speech.openai.base_url = format!("{}/v1", stt.uri());
    config.speech.openai.api_key_env = None;
    let server = start(dir.path(), "speech-openai", config).await;
    let mut client = connect(&server).await;

    let reply = tokio::time::timeout(TIMEOUT, client.api(transcribe(wav(), AudioEncoding::Wav)))
        .await
        .unwrap()
        .unwrap();
    let ServerMsg::Transcript {
        text,
        raw_text,
        language,
        corrections,
        intent,
        engine,
        topic,
    } = reply
    else {
        panic!("expected a transcript, got {reply:?}");
    };
    assert_eq!(raw_text, "uh run p n p m test and check kube control");
    assert_eq!(text, "run pnpm test and check kubectl");
    assert_eq!(language.as_deref(), Some("en"));
    assert!(
        corrections
            .iter()
            .any(|c| c.from == "kube control" && c.to == "kubectl")
    );
    assert_eq!(intent, TranscriptIntent::Prompt);
    assert_eq!(engine, "openai");
    assert_eq!(topic, None);
    server.shutdown().await;
}

#[tokio::test]
async fn context_correction_runs_on_final_chunks_only() {
    let stt = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "the payment form fails in the sub mitt handler when the user clicks twice",
            "language": "english",
        })))
        .mount(&stt)
        .await;
    let llm = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains(
            "You correct speech-recognition errors",
        ))
        .respond_with(chat(
            r#"{"fixes":[{"from":"sub mitt handler","to":"submit handler"}]}"#,
        ))
        // One interim and one final request: only the final one asks.
        .expect(1)
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("You identify the subject"))
        .respond_with(chat(r#"{"topic":"frontend forms","terms":["onSubmit"]}"#))
        .mount(&llm)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.speech.engine = SpeechEngineKind::Openai;
    config.speech.language = Some("en".into());
    config.speech.openai.base_url = format!("{}/v1", stt.uri());
    config.speech.openai.api_key_env = None;
    config.speech.llm.enabled = true;
    config.speech.llm.base_url = format!("{}/v1", llm.uri());
    config.speech.llm.api_key_env = None;
    let server = start(dir.path(), "speech-llm", config).await;
    let mut client = connect(&server).await;

    let interim = tokio::time::timeout(TIMEOUT, client.api(chunk(false)))
        .await
        .unwrap()
        .unwrap();
    let ServerMsg::Transcript { text, .. } = interim else {
        panic!("expected a transcript, got {interim:?}");
    };
    assert!(text.contains("sub mitt handler"), "{text}");

    let final_reply = tokio::time::timeout(TIMEOUT, client.api(chunk(true)))
        .await
        .unwrap()
        .unwrap();
    let ServerMsg::Transcript {
        text,
        raw_text,
        corrections,
        ..
    } = final_reply
    else {
        panic!("expected a transcript, got {final_reply:?}");
    };
    assert!(text.contains("submit handler"), "{text}");
    assert!(raw_text.contains("sub mitt handler"), "{raw_text}");
    assert!(
        corrections
            .iter()
            .any(|c| c.layer == "context" && c.to == "submit handler")
    );

    // The topic detected after the final chunk comes back with later replies
    // (interim requests, which never ask the model for corrections).
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let reply = client.api(chunk(false)).await.unwrap();
        let ServerMsg::Transcript { topic, .. } = reply else {
            panic!("expected a transcript, got {reply:?}");
        };
        if topic.as_deref() == Some("frontend forms") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no topic");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    llm.verify().await;
    server.shutdown().await;
}

#[tokio::test]
async fn reports_engine_errors_without_secrets() {
    let stt = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "error": { "message": "quota exceeded" }
        })))
        .mount(&stt)
        .await;
    // Nothing must carry an Authorization header when no key is configured.
    Mock::given(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&stt)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.speech.engine = SpeechEngineKind::Openai;
    config.speech.openai.base_url = format!("{}/v1", stt.uri());
    config.speech.openai.api_key_env = None;
    let server = start(dir.path(), "speech-error", config).await;
    let mut client = connect(&server).await;

    let reply = client
        .api(transcribe(wav(), AudioEncoding::Wav))
        .await
        .unwrap();
    match reply {
        ServerMsg::Error { code, message } => {
            assert_eq!(code, ErrorCode::Unavailable);
            assert!(message.contains("quota exceeded"), "{message}");
        }
        other => panic!("expected an error, got {other:?}"),
    }
    // Requests on the same connection keep working.
    let pong = client.api(ClientMsg::Ping { at: 1 }).await.unwrap();
    assert_eq!(pong, ServerMsg::Pong { at: 1 });
    server.shutdown().await;
}

#[tokio::test]
async fn transcription_is_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "speech-off", Config::default()).await;
    let mut client = connect(&server).await;
    let reply = client
        .api(transcribe(wav(), AudioEncoding::Webm))
        .await
        .unwrap();
    assert!(
        matches!(
            &reply,
            ServerMsg::Error {
                code: ErrorCode::Unsupported,
                ..
            }
        ),
        "{reply:?}"
    );
    server.shutdown().await;
}

async fn stt_server(text: &str) -> MockServer {
    let stt = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": text,
            "language": "english",
        })))
        .mount(&stt)
        .await;
    stt
}

#[tokio::test]
async fn speech_profiles_are_listed_chosen_and_logged_in_locally() {
    let local_stt = stt_server("heard by the local engine").await;
    let cloud_stt = MockServer::start().await;
    // The cloud engine is only reached with the key from `vca speech login`.
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer sk-cloud",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "heard by the cloud engine",
            "language": "english",
        })))
        .expect(1)
        .mount(&cloud_stt)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let config = Config::from_toml_str(&format!(
        r#"
        [speech]
        language = "en"
        default_engine = "local"
        [[speech.engines]]
        id = "local"
        label = "Whisper (this PC)"
        base_url = "{}/v1"
        [[speech.engines]]
        id = "cloud"
        kind = "openai"
        base_url = "{}/v1"
        api_key_env = "VCA_TEST_UNSET_CLOUD_STT_KEY_3391"
        [[speech.llms]]
        id = "claude"
        kind = "anthropic"
        api_key_env = "VCA_TEST_UNSET_ANTHROPIC_KEY_3391"
        "#,
        local_stt.uri(),
        cloud_stt.uri()
    ))
    .unwrap();
    let server = start(dir.path(), "speech-profiles", config).await;
    let mut client = connect(&server).await;

    let reply = client.api(ClientMsg::ListSpeechOptions).await.unwrap();
    let ServerMsg::SpeechOptions {
        engines,
        llms,
        current_engine,
        current_llm,
    } = reply
    else {
        panic!("expected options, got {reply:?}");
    };
    assert_eq!(engines.len(), 2);
    assert!(engines[0].configured && engines[0].default);
    assert_eq!(engines[0].label, "Whisper (this PC)");
    assert!(!engines[1].configured);
    assert!(!llms[0].configured);
    assert_eq!(current_engine.as_deref(), Some("local"));
    assert_eq!(current_llm, None);

    let reply = client
        .api(transcribe(wav(), AudioEncoding::Wav))
        .await
        .unwrap();
    let ServerMsg::Transcript { raw_text, .. } = reply else {
        panic!("expected a transcript, got {reply:?}");
    };
    assert_eq!(raw_text, "heard by the local engine");

    // An unconfigured profile cannot be chosen yet.
    let reply = client
        .api(ClientMsg::SetSpeechPreferences {
            engine: Some("cloud".into()),
            llm: None,
        })
        .await
        .unwrap();
    assert!(
        matches!(&reply, ServerMsg::Error { message, .. } if message.contains("vca speech login cloud")),
        "{reply:?}"
    );

    // `vca speech login cloud` (local channel only).
    let id = client.next_id();
    let resp = client
        .call(&vca_server::control::LocalRequest::SpeechCredentialSet {
            id,
            profile: "cloud".into(),
            secret: vca_server::cloud::Secret::new("sk-cloud"),
        })
        .await
        .unwrap();
    assert!(
        matches!(resp, vca_server::control::LocalResponse::Done { .. }),
        "{resp:?}"
    );
    let reply = client
        .api(ClientMsg::SetSpeechPreferences {
            engine: Some("cloud".into()),
            llm: Some("none".into()),
        })
        .await
        .unwrap();
    let ServerMsg::SpeechOptions {
        engines,
        current_engine,
        ..
    } = reply
    else {
        panic!("expected options, got {reply:?}");
    };
    assert!(engines[1].configured);
    assert_eq!(current_engine.as_deref(), Some("cloud"));
    // Secrets never appear in replies.
    assert!(!format!("{engines:?}").contains("sk-cloud"));

    let reply = client
        .api(transcribe(wav(), AudioEncoding::Wav))
        .await
        .unwrap();
    let ServerMsg::Transcript { raw_text, .. } = reply else {
        panic!("expected a transcript, got {reply:?}");
    };
    assert_eq!(raw_text, "heard by the cloud engine");

    // Logout removes the key again.
    let id = client.next_id();
    let resp = client
        .call(&vca_server::control::LocalRequest::SpeechCredentialDelete {
            id,
            profile: "cloud".into(),
        })
        .await
        .unwrap();
    assert!(
        matches!(resp, vca_server::control::LocalResponse::Done { .. }),
        "{resp:?}"
    );
    cloud_stt.verify().await;
    server.shutdown().await;
}
