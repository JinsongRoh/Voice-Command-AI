//! Speech engine setup over the owner-only local control channel: reading
//! and writing `config.toml`, storing keys and testing each engine type
//! against mock OpenAI-compatible servers. No real engine is called.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use vca_server::api::DeviceIdentity;
use vca_server::cloud::Secret;
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};
use vca_server::daemon::{Options, Server};
use vca_server::engines::{EngineForm, EngineSetup, EngineTestResult, EngineType, KeySource};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "sk-engine-test-9f3k2m";

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
        panic!("engine tests must never terminate processes ({root})")
    }
}

/// Everything the server logs in this test binary.
fn logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "vca_server=trace,vca_speech=trace,vca_net=trace,vca_core=trace",
            ))
            .with_ansi(false)
            .with_writer(move || LogWriter(Arc::clone(&sink)))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        buf
    })
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn log_text() -> String {
    String::from_utf8_lossy(&logs().lock().unwrap()).into_owned()
}

fn control_endpoint(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\vca-test-{}-{name}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join(format!("{name}.sock"))
    }
}

/// Starts vcad with `config.toml` = `text` (plus test-only settings that
/// are not written to the file).
async fn start(dir: &Path, name: &str, text: &str) -> Server {
    logs();
    let file = dir.join("config.toml");
    std::fs::write(&file, text).unwrap();
    let mut config = Config::from_toml_str(text).unwrap();
    config.workspace_roots = vec![dir.to_owned()];
    config.discovery.enabled = false;
    config.remote.enabled = false;
    let policy = dir.join("policies.toml");
    std::fs::write(&policy, "").unwrap();
    config.broker.policy_path = Some(policy);
    Server::start(
        Options {
            config_file: Some(file),
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

async fn call(client: &mut ControlClient, req: LocalRequest) -> LocalResponse {
    let resp = tokio::time::timeout(Duration::from_secs(60), client.call(&req))
        .await
        .expect("vcad answers")
        .unwrap();
    // No answer ever carries the key.
    assert!(
        !serde_json::to_string(&resp).unwrap().contains(SECRET),
        "{resp:?}"
    );
    resp
}

fn setup(resp: LocalResponse) -> EngineSetup {
    match resp {
        LocalResponse::Engines { setup, .. } => *setup,
        other => panic!("expected engines, got {other:?}"),
    }
}

fn tested(resp: LocalResponse) -> EngineTestResult {
    match resp {
        LocalResponse::EngineTest { result, .. } => result,
        other => panic!("expected a test result, got {other:?}"),
    }
}

fn form(id: &str, url: &str) -> EngineForm {
    EngineForm {
        id: id.into(),
        kind: "openai_compatible".into(),
        base_url: Some(url.into()),
        ..EngineForm::default()
    }
}

const BASE: &str = r#"# Owner's notes stay here.
server_name = "engine-test"

[sessions]
replay_events = 777   # unrelated

[shell]
enabled = false
"#;

#[tokio::test]
async fn paired_devices_and_other_transports_are_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "forbid", BASE).await;
    for (transport, device) in [
        (
            "remote",
            Some(DeviceIdentity {
                device_id: "phone-1".into(),
                scope: vca_net::Scope::Admin,
            }),
        ),
        ("dev_ws", None),
    ] {
        let (client, _rx) = server.api.connect_as(transport, device);
        let requests = [
            LocalRequest::EnginesGet { id: 1 },
            LocalRequest::EngineSave {
                id: 2,
                kind: EngineType::Stt,
                original_id: None,
                profile: form("x", "http://127.0.0.1:9/v1"),
                secret: Some(Secret::new(SECRET)),
                make_default: false,
                gateway_token: None,
                clear_gateway_token: false,
                step_up: None,
            },
            LocalRequest::EngineRemove {
                id: 3,
                kind: EngineType::Tts,
                profile_id: "x".into(),
                delete_key: true,
            },
            LocalRequest::EngineSetDefault {
                id: 4,
                kind: EngineType::Llm,
                profile_id: None,
            },
            LocalRequest::EngineTest {
                id: 5,
                kind: EngineType::Tts,
                profile: Some(form("x", "http://127.0.0.1:9/v1")),
                profile_id: None,
                secret: None,
                gateway_token: None,
            },
        ];
        for req in requests {
            let id = req.id();
            match vca_server::engines::handle_local(&server.api, &client, req, &Default::default())
                .await
            {
                LocalResponse::Error { id: rid, code, .. } => {
                    assert_eq!(rid, id);
                    assert_eq!(code, ErrorCode::Forbidden, "{transport}");
                }
                other => panic!("{transport}: expected forbidden, got {other:?}"),
            }
        }
    }
    // Nothing was written and no key was stored.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
        BASE
    );
    assert!(!dir.path().join("config.toml.bak").exists());
    assert!(!dir.path().join("data").join("speech").join("keys").exists());
    // The owner's local channel is allowed.
    let mut local = connect(&server).await;
    let s = setup(call(&mut local, LocalRequest::EnginesGet { id: 9 }).await);
    assert!(s.stt.is_empty() && s.llm.is_empty() && s.tts.is_empty());
    assert!(!s.restart_required);
    server.shutdown().await;
}

#[tokio::test]
async fn saving_writes_atomically_keeps_other_settings_and_never_stores_keys_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "save", BASE).await;
    let mut local = connect(&server).await;

    let stt = EngineForm {
        label: Some("faster-whisper (this PC)".into()),
        model: Some("Systran/faster-whisper-small".into()),
        ..form("whisper-local", "http://127.0.0.1:8000/v1")
    };
    let s = setup(
        call(
            &mut local,
            LocalRequest::EngineSave {
                id: 1,
                kind: EngineType::Stt,
                original_id: None,
                profile: stt,
                secret: None,
                make_default: true,
                gateway_token: None,
                clear_gateway_token: false,
                step_up: None,
            },
        )
        .await,
    );
    assert!(s.restart_required, "engines load when vcad starts");
    assert_eq!(s.stt[0].key, KeySource::None);
    assert!(s.stt[0].ready, "a local server needs no key");
    let bak = std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap();
    assert_eq!(bak, BASE);

    // An OpenAI LLM with a key: stored, never written.
    let llm = EngineForm {
        id: "openai-llm".into(),
        kind: "openai_compatible".into(),
        base_url: Some("https://api.openai.com/v1".into()),
        model: Some("gpt-4o-mini".into()),
        ..EngineForm::default()
    };
    let s = setup(
        call(
            &mut local,
            LocalRequest::EngineSave {
                id: 2,
                kind: EngineType::Llm,
                original_id: None,
                profile: llm,
                secret: Some(Secret::new(SECRET)),
                make_default: true,
                gateway_token: None,
                clear_gateway_token: false,
                step_up: None,
            },
        )
        .await,
    );
    assert_eq!(s.llm[0].key, KeySource::Stored);
    assert!(s.llm[0].default);

    let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!text.contains(SECRET));
    for kept in [
        "# Owner's notes stay here.",
        "replay_events = 777   # unrelated",
        "[shell]\nenabled = false",
        "server_name = \"engine-test\"",
    ] {
        assert!(text.contains(kept), "lost {kept:?}:\n{text}");
    }
    let cfg = Config::from_toml_str(&text).unwrap();
    assert_eq!(cfg.sessions.replay_events, 777);
    assert_eq!(cfg.speech.default_engine.as_deref(), Some("whisper-local"));
    assert_eq!(cfg.speech.default_llm.as_deref(), Some("openai-llm"));
    cfg.speech.validate().unwrap();

    // Invalid input is refused and leaves the file alone.
    let resp = call(
        &mut local,
        LocalRequest::EngineSave {
            id: 3,
            kind: EngineType::Tts,
            original_id: None,
            profile: form("bad", "ftp://example.com"),
            secret: None,
            make_default: false,
            gateway_token: None,
            clear_gateway_token: false,
            step_up: None,
        },
    )
    .await;
    assert!(
        matches!(
            resp,
            LocalResponse::Error {
                code: ErrorCode::InvalidRequest,
                ..
            }
        ),
        "{resp:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
        text
    );

    // Remove with the key.
    let s = setup(
        call(
            &mut local,
            LocalRequest::EngineRemove {
                id: 4,
                kind: EngineType::Llm,
                profile_id: "openai-llm".into(),
                delete_key: true,
            },
        )
        .await,
    );
    assert!(s.llm.is_empty());
    assert!(!log_text().contains(SECRET));
    server.shutdown().await;
}

#[tokio::test]
async fn each_engine_type_is_tested_against_the_owners_endpoint() {
    let mock = MockServer::start().await;
    // Speech-to-text: the bundled tone as a WAV upload, with the key.
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(header("authorization", format!("Bearer {SECRET}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": "" })))
        .expect(1)
        .mount(&mock)
        .await;
    // LLM: chat completions.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "ok" } }]
        })))
        .expect(1)
        .mount(&mock)
        .await;
    // TTS: audio bytes.
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(vec![0xFFu8; 640]),
        )
        .expect(1)
        .mount(&mock)
        .await;
    // A server that rejects the key and quotes it back.
    Mock::given(method("POST"))
        .and(path("/bad/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": { "message": format!("Incorrect API key provided: {SECRET}") }
        })))
        .mount(&mock)
        .await;
    let url = format!("{}/v1", mock.uri());

    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "test", BASE).await;
    let mut local = connect(&server).await;

    // Save the STT engine with its key, then test it by id.
    setup(
        call(
            &mut local,
            LocalRequest::EngineSave {
                id: 1,
                kind: EngineType::Stt,
                original_id: None,
                profile: form("stt", &url),
                secret: Some(Secret::new(SECRET)),
                make_default: false,
                gateway_token: None,
                clear_gateway_token: false,
                step_up: None,
            },
        )
        .await,
    );
    let r = tested(
        call(
            &mut local,
            LocalRequest::EngineTest {
                id: 2,
                kind: EngineType::Stt,
                profile: None,
                profile_id: Some("stt".into()),
                secret: None,
                gateway_token: None,
            },
        )
        .await,
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(r.chars, Some(0));

    // A draft LLM (not saved) is tested as it stands.
    let r = tested(
        call(
            &mut local,
            LocalRequest::EngineTest {
                id: 3,
                kind: EngineType::Llm,
                profile: Some(EngineForm {
                    model: Some("qwen2.5:3b".into()),
                    ..form("draft-llm", &url)
                }),
                profile_id: None,
                secret: None,
                gateway_token: None,
            },
        )
        .await,
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(r.chars, Some(2));

    let r = tested(
        call(
            &mut local,
            LocalRequest::EngineTest {
                id: 4,
                kind: EngineType::Tts,
                profile: Some(EngineForm {
                    voice: Some("EN-US".into()),
                    ..form("tts", &url)
                }),
                profile_id: None,
                secret: None,
                gateway_token: None,
            },
        )
        .await,
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(r.audio_bytes, Some(640));

    // A failure is a result with a code and the key removed from the text.
    let r = tested(
        call(
            &mut local,
            LocalRequest::EngineTest {
                id: 5,
                kind: EngineType::Tts,
                profile: Some(form("tts-bad", &format!("{}/bad/v1", mock.uri()))),
                profile_id: None,
                secret: Some(Secret::new(SECRET)),
                gateway_token: None,
            },
        )
        .await,
    );
    assert!(!r.ok);
    assert_eq!(r.code, Some(ErrorCode::Unauthorized));
    assert!(r.message.contains("[redacted]"), "{}", r.message);

    // An unreachable server fails cleanly.
    let r = tested(
        call(
            &mut local,
            LocalRequest::EngineTest {
                id: 6,
                kind: EngineType::Llm,
                profile: Some(form("down", "http://127.0.0.1:9/v1")),
                profile_id: None,
                secret: None,
                gateway_token: None,
            },
        )
        .await,
    );
    assert!(!r.ok);
    assert_eq!(r.code, Some(ErrorCode::Unavailable));

    // Unknown saved profiles are reported.
    let resp = call(
        &mut local,
        LocalRequest::EngineTest {
            id: 7,
            kind: EngineType::Tts,
            profile: None,
            profile_id: Some("missing".into()),
            secret: None,
            gateway_token: None,
        },
    )
    .await;
    assert!(matches!(
        resp,
        LocalResponse::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));

    let logs = log_text();
    assert!(!logs.contains(SECRET), "a key reached the logs");
    assert!(
        !logs.contains("Voice Command AI test."),
        "TTS text in the logs"
    );
    assert!(!logs.contains("connection test"), "LLM prompt in the logs");
    server.shutdown().await;
}

#[test]
fn request_json_shapes() {
    let req = LocalRequest::EngineSave {
        id: 3,
        kind: EngineType::Tts,
        original_id: Some("old".into()),
        profile: form("new", "http://127.0.0.1:8880/v1"),
        secret: Some(Secret::new("k")),
        make_default: true,
        gateway_token: None,
        clear_gateway_token: false,
        step_up: None,
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["op"], "engine_save");
    assert_eq!(v["kind"], "tts");
    assert_eq!(v["profile"]["id"], "new");
    assert_eq!(v["secret"], "k");
    let back: LocalRequest = serde_json::from_value(v).unwrap();
    assert_eq!(back.id(), 3);
    // An empty key is no key.
    let v = serde_json::json!({
        "op": "engine_test", "id": 4, "kind": "stt", "profile_id": "x", "secret": ""
    });
    match serde_json::from_value::<LocalRequest>(v).unwrap() {
        LocalRequest::EngineTest { secret, .. } => assert!(secret.is_none()),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        serde_json::to_value(LocalRequest::EnginesGet { id: 1 }).unwrap(),
        serde_json::json!({ "op": "engines_get", "id": 1 })
    );
    // Debug output never shows a key.
    assert!(!format!("{req:?}").contains("\"k\""));
}

const GATEWAY_TOKEN: &str = "gw-token-engine-test-5k1x";

fn error_of(resp: &LocalResponse) -> (ErrorCode, String) {
    match resp {
        LocalResponse::Error { code, message, .. } => (*code, message.clone()),
        other => panic!("expected an error, got {other:?}"),
    }
}

/// [`call`], and the answer never carries a gateway token either.
async fn call_gw(client: &mut ControlClient, req: LocalRequest) -> LocalResponse {
    let resp = call(client, req).await;
    assert!(
        !serde_json::to_string(&resp)
            .unwrap()
            .contains(GATEWAY_TOKEN),
        "{resp:?}"
    );
    resp
}

#[tokio::test(flavor = "multi_thread")]
async fn gateway_tokens_are_stored_tested_and_never_shown() {
    // Answers only a request with the gateway token in Authorization and
    // the key in its own header; anything else gets a 404.
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(header(
            "authorization",
            format!("Bearer {GATEWAY_TOKEN}").as_str(),
        ))
        .and(header("x-vca-key", SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": ""})))
        .expect(1)
        .mount(&mock)
        .await;
    let url = format!("{}/v1", mock.uri());
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "gateway", BASE).await;
    let mut local = connect(&server).await;

    // A gateway token without a key header of its own is refused.
    let resp = call_gw(
        &mut local,
        LocalRequest::EngineSave {
            id: 1,
            kind: EngineType::Stt,
            original_id: None,
            profile: form("team", &url),
            secret: Some(Secret::new(SECRET)),
            make_default: false,
            gateway_token: Some(Secret::new(GATEWAY_TOKEN)),
            clear_gateway_token: false,
            step_up: None,
        },
    )
    .await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert!(message.contains("api_key_header"), "{message}");
    assert!(
        !std::fs::read_to_string(dir.path().join("config.toml"))
            .unwrap()
            .contains("team"),
        "nothing is written on a refused save"
    );

    // With the key header, the key and the token are stored apart from the
    // file.
    let team = EngineForm {
        api_key_header: Some("X-VCA-Key".into()),
        ..form("team", &url)
    };
    let s = setup(
        call_gw(
            &mut local,
            LocalRequest::EngineSave {
                id: 2,
                kind: EngineType::Stt,
                original_id: None,
                profile: team,
                secret: Some(Secret::new(SECRET)),
                make_default: true,
                gateway_token: Some(Secret::new(GATEWAY_TOKEN)),
                clear_gateway_token: false,
                step_up: None,
            },
        )
        .await,
    );
    let entry = s.stt.iter().find(|e| e.profile.id == "team").unwrap();
    assert_eq!(entry.key, KeySource::Stored);
    assert_eq!(entry.gateway, KeySource::Stored);
    assert_eq!(entry.profile.api_key_header.as_deref(), Some("X-VCA-Key"));
    let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(text.contains("api_key_header = \"X-VCA-Key\""), "{text}");
    assert!(!text.contains(GATEWAY_TOKEN) && !text.contains(SECRET));
    assert!(
        dir.path()
            .join("data")
            .join("speech")
            .join("gateway")
            .join("team.key")
            .exists()
    );

    // The test request carries both, each in its place.
    let r = tested(
        call_gw(
            &mut local,
            LocalRequest::EngineTest {
                id: 3,
                kind: EngineType::Stt,
                profile: None,
                profile_id: Some("team".into()),
                secret: None,
                gateway_token: None,
            },
        )
        .await,
    );
    assert!(r.ok, "{r:?}");

    // The key header cannot be dropped while a token is stored.
    let resp = call_gw(
        &mut local,
        LocalRequest::EngineSave {
            id: 4,
            kind: EngineType::Stt,
            original_id: Some("team".into()),
            profile: form("team", &url),
            secret: None,
            make_default: true,
            gateway_token: None,
            clear_gateway_token: false,
            step_up: None,
        },
    )
    .await;
    assert_eq!(error_of(&resp).0, ErrorCode::InvalidRequest);

    // A gateway login for a profile without a key header is refused.
    setup(
        call_gw(
            &mut local,
            LocalRequest::EngineSave {
                id: 5,
                kind: EngineType::Llm,
                original_id: None,
                profile: form("plain-llm", &url),
                secret: None,
                make_default: false,
                gateway_token: None,
                clear_gateway_token: false,
                step_up: None,
            },
        )
        .await,
    );
    let resp = call_gw(
        &mut local,
        LocalRequest::EngineGatewayToken {
            id: 6,
            kind: EngineType::Llm,
            profile_id: "plain-llm".into(),
            token: Some(Secret::new(GATEWAY_TOKEN)),
        },
    )
    .await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert!(message.contains("api_key_header"), "{message}");

    // Replace, then delete.
    let gateway_of = |s: &EngineSetup| {
        s.stt
            .iter()
            .find(|e| e.profile.id == "team")
            .unwrap()
            .gateway
    };
    let s = setup(
        call_gw(
            &mut local,
            LocalRequest::EngineGatewayToken {
                id: 7,
                kind: EngineType::Stt,
                profile_id: "team".into(),
                token: Some(Secret::new("gw-replaced-token")),
            },
        )
        .await,
    );
    assert_eq!(gateway_of(&s), KeySource::Stored);
    let s = setup(
        call_gw(
            &mut local,
            LocalRequest::EngineGatewayToken {
                id: 8,
                kind: EngineType::Stt,
                profile_id: "team".into(),
                token: None,
            },
        )
        .await,
    );
    assert_eq!(gateway_of(&s), KeySource::None);
    let resp = call_gw(
        &mut local,
        LocalRequest::EngineGatewayToken {
            id: 9,
            kind: EngineType::Stt,
            profile_id: "team".into(),
            token: None,
        },
    )
    .await;
    assert_eq!(error_of(&resp).0, ErrorCode::NotFound);

    let logs = log_text();
    assert!(
        !logs.contains(GATEWAY_TOKEN) && !logs.contains("gw-replaced-token"),
        "a gateway token reached the logs"
    );
    assert!(logs.contains("engine gateway token stored"));
    server.shutdown().await;
}

async fn verified_save(
    server: &Server,
    client: &mut ControlClient,
    save: impl Fn(Option<vca_server::security::Sensitive>) -> LocalRequest,
) -> LocalResponse {
    use vca_server::security::{SecurityCall, SecurityReply, Sensitive};
    let first = call(client, save(None)).await;
    let LocalResponse::StepUpRequired { challenge, .. } = first else {
        return first;
    };
    let reply = server
        .api
        .security()
        .unwrap()
        .handle_from(
            SecurityCall::StepUp {
                nonce: Sensitive(challenge.nonce),
                window: None,
                totp: None,
                presence: None,
                language: None,
            },
            // As the process that got the challenge.
            &vca_server::security::Caller {
                pid: Some(std::process::id()),
                ..Default::default()
            },
        )
        .await;
    let SecurityReply::StepUp { token, .. } = reply else {
        panic!("{reply:?}");
    };
    call(client, save(Some(token))).await
}

fn save_request(
    original: Option<&str>,
    profile: EngineForm,
    secret: Option<&str>,
    step_up: Option<vca_server::security::Sensitive>,
) -> LocalRequest {
    LocalRequest::EngineSave {
        id: 1,
        kind: EngineType::Stt,
        original_id: original.map(str::to_owned),
        profile,
        secret: secret.map(Secret::new),
        make_default: false,
        gateway_token: None,
        clear_gateway_token: false,
        step_up,
    }
}

/// A stored key may go to a URL only with the owner's approval, and a draft
/// never gets one.
#[tokio::test(flavor = "multi_thread")]
async fn stored_keys_stay_with_their_url() {
    let thief = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&thief)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "keys", "").await;
    let mut client = connect(&server).await;
    let home = "http://127.0.0.1:8000/v1";
    setup(
        call(
            &mut client,
            save_request(None, form("mine", home), Some(SECRET), None),
        )
        .await,
    );

    // A test of a draft under the same name at another URL has no key.
    let draft = form("mine", &thief.uri());
    let result = tested(
        call(
            &mut client,
            LocalRequest::EngineTest {
                id: 1,
                kind: EngineType::Stt,
                profile: Some(draft.clone()),
                profile_id: None,
                secret: None,
                gateway_token: None,
            },
        )
        .await,
    );
    let _ = result;
    let received = thief.received_requests().await.unwrap_or_default();
    for request in &received {
        let auth = format!("{:?}", request.headers);
        assert!(!auth.contains(SECRET), "the key went out");
    }

    // Saving a new URL for it needs the owner.
    let moved = call(
        &mut client,
        save_request(
            Some("mine"),
            form("mine", "http://127.0.0.1:9000/v1"),
            None,
            None,
        ),
    )
    .await;
    assert!(
        matches!(moved, LocalResponse::StepUpRequired { .. }),
        "{moved:?}"
    );
    let saved = verified_save(&server, &mut client, |step_up| {
        save_request(
            Some("mine"),
            form("mine", "http://127.0.0.1:9000/v1"),
            None,
            step_up,
        )
    })
    .await;
    setup(saved);
    // The same URL again is no move.
    let same = call(
        &mut client,
        save_request(
            Some("mine"),
            form("mine", "http://127.0.0.1:9000/v1"),
            None,
            None,
        ),
    )
    .await;
    assert!(matches!(same, LocalResponse::Engines { .. }), "{same:?}");

    // Another profile cannot borrow the key by naming its credential.
    let mut borrowed = form("other", "http://127.0.0.1:9100/v1");
    borrowed.credential = Some("mine".into());
    let resp = call(&mut client, save_request(None, borrowed, None, None)).await;
    assert!(
        matches!(resp, LocalResponse::StepUpRequired { .. }),
        "{resp:?}"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_over_plain_http_follows_the_rule() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "https", "").await;
    let mut client = connect(&server).await;
    // A public address never.
    let public = form("far", "http://203.0.113.9:8000/v1");
    let (code, message) = error_of(
        &call(
            &mut client,
            save_request(None, public.clone(), Some(SECRET), None),
        )
        .await,
    );
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert!(message.contains("public"), "{message}");
    // The Tailscale range needs nothing.
    let tailnet = form("tail", "http://100.101.102.103:8000/v1");
    setup(call(&mut client, save_request(None, tailnet, Some(SECRET), None)).await);
    // A private address: not silently, and the message offers the flag.
    let lan = form("lan", "http://192.168.0.10:8000/v1");
    let (code, message) = error_of(
        &call(
            &mut client,
            save_request(None, lan.clone(), Some(SECRET), None),
        )
        .await,
    );
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert!(message.contains("Allow plain http"), "{message}");
    // The flag goes through the gate, and only then the key may follow.
    let mut flagged = lan.clone();
    flagged.allow_plain_http = true;
    let resp = call(
        &mut client,
        save_request(None, flagged.clone(), Some(SECRET), None),
    )
    .await;
    assert!(
        matches!(resp, LocalResponse::StepUpRequired { .. }),
        "{resp:?}"
    );
    setup(
        verified_save(&server, &mut client, |step_up| {
            save_request(None, flagged.clone(), Some(SECRET), step_up)
        })
        .await,
    );
    // The flag means nothing for a public address.
    let mut bad = public;
    bad.allow_plain_http = true;
    let resp = verified_save(&server, &mut client, |step_up| {
        save_request(None, bad.clone(), None, step_up)
    })
    .await;
    let (code, _) = error_of(&resp);
    assert_eq!(code, ErrorCode::InvalidRequest);
    server.shutdown().await;
}

/// A new variable is another secret going to the same address.
#[tokio::test(flavor = "multi_thread")]
async fn a_changed_key_variable_counts_as_a_move() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "envs", "").await;
    let mut client = connect(&server).await;
    let home = "http://127.0.0.1:8000/v1";
    setup(
        call(
            &mut client,
            save_request(None, form("mine", home), None, None),
        )
        .await,
    );
    // Any variable with a value will do.
    let mut other = form("mine", home);
    other.api_key_env = Some("PATH".into());
    let resp = call(&mut client, save_request(Some("mine"), other, None, None)).await;
    assert!(
        matches!(resp, LocalResponse::StepUpRequired { .. }),
        "{resp:?}"
    );
    server.shutdown().await;
}
