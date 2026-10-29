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
use wiremock::matchers::{header_exists, method, path};
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
    }
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
