//! Spoken alerts synthesized by the PC (`[tts]`) against a mock
//! OpenAI-compatible speech server. No real TTS engine is called.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{ClientMsg, Envelope, PROTOCOL_VERSION, ServerMsg};
use vca_server::api::DeviceIdentity;
use vca_server::config::Config;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse};
use vca_server::daemon::{Options, Server};
use wiremock::matchers::{body_json, header, method, path};
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
        panic!("tts tests must never terminate processes ({root})")
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

async fn start(dir: &Path, name: &str, mut config: Config) -> Server {
    logs();
    config.workspace_roots = vec![dir.to_owned()];
    config.discovery.enabled = false;
    config.remote.enabled = false;
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

fn config(base_url: &str, extra: &str) -> Config {
    Config::from_toml_str(&format!(
        r#"
        [tts]
        default_engine = "local-tts"
        max_audio_bytes = 1000
        {extra}
        [[tts.engines]]
        name = "local-tts"
        base_url = "{base_url}/v1"
        model = "kokoro"
        voice = "af_sky"
        api_key_env = "VCA_TEST_UNSET_TTS_KEY_5521"
        disclosure = "Machine-generated voice"
        [tts.engines.voices]
        ko = "ko_voice"
        [[tts.engines.speakers]]
        id = "F1"
        label = "Female 1"
        gender = "female"
        [[tts.engines.speakers]]
        id = "M1"
        label = "Male 1"
        gender = "male"
        "#
    ))
    .unwrap()
}

fn say(text: &str, language: Option<&str>) -> ClientMsg {
    ClientMsg::Synthesize {
        text: text.into(),
        language: language.map(str::to_owned),
        engine: None,
        voice: None,
    }
}

fn expect_error(reply: &ServerMsg, code: ErrorCode) -> String {
    match reply {
        ServerMsg::Error { code: c, message } if *c == code => message.clone(),
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

/// Sends a request as a paired device and waits for its answer.
async fn as_device(server: &Server, device: &str, body: ClientMsg) -> ServerMsg {
    let (client, mut rx) = server.api.connect_as(
        "test",
        Some(DeviceIdentity {
            device_id: device.into(),
            scope: vca_net::Scope::View,
        }),
    );
    server.api.handle(&client, Envelope { id: 7, body }).await;
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let env = rx.recv().await.unwrap();
            if env.id == 7 {
                return env.body;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn synthesizes_with_the_stored_key_and_never_logs_the_text() {
    const SENTINEL: &str = "Zebra-Quartz session needs a decision";
    let tts = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .and(header("authorization", "Bearer sk-tts-store"))
        .and(body_json(serde_json::json!({
            "model": "kokoro",
            "input": SENTINEL,
            "voice": "ko_voice",
            "response_format": "mp3",
            "speed": 1.0,
            "language": "ko",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ID3-fake-mp3".to_vec()))
        .expect(1)
        .mount(&tts)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "tts-key", config(&tts.uri(), "")).await;
    let mut client = connect(&server).await;

    // `vca tts login local-tts`, over the owner-only local channel.
    let id = client.next_id();
    let resp = client
        .call(&LocalRequest::TtsCredentialSet {
            id,
            engine: "local-tts".into(),
            secret: vca_server::cloud::Secret::new("sk-tts-store"),
        })
        .await
        .unwrap();
    assert_eq!(resp, LocalResponse::Done { id });
    let id = client.next_id();
    let resp = client
        .call(&LocalRequest::TtsCredentialSet {
            id,
            engine: "unknown".into(),
            secret: vca_server::cloud::Secret::new("sk"),
        })
        .await
        .unwrap();
    assert!(matches!(resp, LocalResponse::Error { .. }), "{resp:?}");

    let reply = client.api(say(SENTINEL, Some("ko-KR"))).await.unwrap();
    assert_eq!(
        reply,
        ServerMsg::Audio {
            format: "mp3".into(),
            data: b"ID3-fake-mp3".to_vec(),
        }
    );
    server.shutdown().await;

    let logged = String::from_utf8_lossy(&logs().lock().unwrap()).into_owned();
    assert!(logged.contains("TTS credential stored"), "logs: {logged}");
    assert!(!logged.contains("Zebra-Quartz"), "the text was logged");
    assert!(!logged.contains("sk-tts-store"), "the key was logged");
    assert!(!logged.contains("ID3-fake"), "the audio was logged");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chosen_speaker_reaches_the_engine_and_unknown_ones_are_refused() {
    let tts = MockServer::start().await;
    // Only a request naming M1 is answered; anything else would be a 404.
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .and(body_json(serde_json::json!({
            "model": "kokoro",
            "input": "Build failed",
            "voice": "M1",
            "response_format": "mp3",
            "speed": 1.0,
            "language": "ko",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ID3-male".to_vec()))
        .expect(1)
        .mount(&tts)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "tts-voice", config(&tts.uri(), "")).await;
    let voice = |v: &str| ClientMsg::Synthesize {
        text: "Build failed".into(),
        language: Some("ko-KR".into()),
        engine: None,
        voice: Some(v.into()),
    };

    let reply = as_device(&server, "dev_a", voice("M1")).await;
    assert_eq!(
        reply,
        ServerMsg::Audio {
            format: "mp3".into(),
            data: b"ID3-male".to_vec(),
        }
    );
    // A voice that is not a configured speaker never reaches the engine,
    // not even the engine's own default voice.
    for bad in ["F9", "af_sky", "ko_voice", "../M1"] {
        let reply = as_device(&server, "dev_b", voice(bad)).await;
        let message = expect_error(&reply, ErrorCode::InvalidRequest);
        assert!(!message.contains(bad), "{message}");
    }
    assert_eq!(tts.received_requests().await.unwrap().len(), 1);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_hides_urls_and_models() {
    let tts = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "tts-list", config(&tts.uri(), "")).await;
    let mut client = connect(&server).await;
    let reply = client.api(ClientMsg::ListTtsEngines).await.unwrap();
    let ServerMsg::TtsEngines { engines } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(engines.len(), 1);
    assert_eq!(engines[0].name, "local-tts");
    assert_eq!(engines[0].languages, vec!["ko".to_owned()]);
    assert!(engines[0].default);
    assert_eq!(engines[0].speakers.len(), 2);
    assert_eq!(engines[0].speakers[0].id, "F1");
    assert_eq!(engines[0].speakers[1].gender, vca_core::TtsGender::Male);
    assert_eq!(
        engines[0].disclosure.as_deref(),
        Some("Machine-generated voice")
    );
    let json = serde_json::to_string(&reply).unwrap();
    assert!(!json.contains(&tts.uri()), "{json}");
    assert!(!json.contains("127.0.0.1"), "{json}");
    assert!(!json.contains("kokoro"), "{json}");
    assert!(!json.contains("VCA_TEST_UNSET"), "{json}");
    // A view-only device sees the same list.
    let reply = as_device(&server, "dev_view", ClientMsg::ListTtsEngines).await;
    assert!(matches!(reply, ServerMsg::TtsEngines { .. }), "{reply:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn view_devices_are_rate_limited_and_capped() {
    let tts = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"OggS".to_vec()))
        .mount(&tts)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let server = start(
        dir.path(),
        "tts-limit",
        config(&tts.uri(), "requests_per_minute = 2"),
    )
    .await;

    // View scope is enough to have an alert spoken.
    for _ in 0..2 {
        let reply = as_device(&server, "dev_a", say("Build failed", None)).await;
        assert!(matches!(reply, ServerMsg::Audio { .. }), "{reply:?}");
    }
    let reply = as_device(&server, "dev_a", say("Build failed", None)).await;
    let message = expect_error(&reply, ErrorCode::Unavailable);
    assert_eq!(message, vca_server::tts::RATE_LIMITED);
    // Another device has its own budget.
    let reply = as_device(&server, "dev_b", say("Build failed", None)).await;
    assert!(matches!(reply, ServerMsg::Audio { .. }), "{reply:?}");

    // At most 300 characters, refused before any engine call.
    let long = "a".repeat(vca_core::TTS_MAX_TEXT_CHARS + 1);
    let reply = as_device(&server, "dev_c", say(&long, None)).await;
    let message = expect_error(&reply, ErrorCode::InvalidRequest);
    assert!(message.contains("300"), "{message}");
    assert!(!message.contains("aaaa"), "{message}");
    let reply = as_device(
        &server,
        "dev_c",
        ClientMsg::Synthesize {
            text: "hi".into(),
            language: None,
            engine: Some("nope".into()),
            voice: None,
        },
    )
    .await;
    expect_error(&reply, ErrorCode::NotFound);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_audio_and_engine_errors_are_reported() {
    let tts = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 5000]))
        .mount(&tts)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "tts-cap", config(&tts.uri(), "")).await;
    let mut client = connect(&server).await;
    let reply = client.api(say("Session finished", None)).await.unwrap();
    let message = expect_error(&reply, ErrorCode::Internal);
    assert!(message.contains("1000"), "{message}");

    tts.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"error": {"message": "invalid api key"}})),
        )
        .mount(&tts)
        .await;
    let reply = client.api(say("Session finished", None)).await.unwrap();
    let message = expect_error(&reply, ErrorCode::Unauthorized);
    assert!(message.contains("local-tts"), "{message}");
    assert!(!message.contains(&tts.uri()), "{message}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tts_is_off_without_engines() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "tts-off", Config::default()).await;
    let mut client = connect(&server).await;
    let reply = client.api(ClientMsg::ListTtsEngines).await.unwrap();
    assert_eq!(reply, ServerMsg::TtsEngines { engines: vec![] });
    let reply = client.api(say("hello", None)).await.unwrap();
    expect_error(&reply, ErrorCode::Unsupported);
    server.shutdown().await;
}

/// The `authorization` and `x-vca-key` headers of the last request.
async fn auth_headers(server: &MockServer) -> (Option<String>, Option<String>) {
    let req = server.received_requests().await.unwrap().pop().unwrap();
    let get = |name: &str| {
        req.headers
            .get(name)
            .map(|v| v.to_str().unwrap().to_owned())
    };
    (get("authorization"), get("x-vca-key"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gateway_token_is_sent_and_replaced_without_a_restart() {
    const KEY: &str = "sk-tts-gateway-key-8q";
    const TOKEN_1: &str = "gw-tts-token-one-3h";
    const TOKEN_2: &str = "gw-tts-token-two-7c";
    let tts = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ID3-gw".to_vec()))
        .mount(&tts)
        .await;
    let text = format!(
        r#"
        [[tts.engines]]
        name = "team-tts"
        base_url = "{}/v1"
        api_key_header = "X-VCA-Key"
        "#,
        tts.uri()
    );
    let dir = tempfile::tempdir().unwrap();
    // The engine admin (gateway tokens) reads the file vcad started with.
    std::fs::write(dir.path().join("config.toml"), &text).unwrap();
    let server = start(
        dir.path(),
        "tts-gateway",
        Config::from_toml_str(&text).unwrap(),
    )
    .await;
    let mut client = connect(&server).await;
    let local = async |client: &mut ControlClient, req: LocalRequest| {
        let resp = client.call(&req).await.unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains(KEY) && !json.contains("gw-tts-token"),
            "{json}"
        );
        resp
    };
    let gateway = |id: u64, token: Option<&str>| LocalRequest::EngineGatewayToken {
        id,
        kind: vca_server::engines::EngineType::Tts,
        profile_id: "team-tts".into(),
        token: token.map(vca_server::cloud::Secret::new),
    };

    let resp = local(
        &mut client,
        LocalRequest::TtsCredentialSet {
            id: 1,
            engine: "team-tts".into(),
            secret: vca_server::cloud::Secret::new(KEY),
        },
    )
    .await;
    assert_eq!(resp, LocalResponse::Done { id: 1 });

    // Key header only: the key travels raw in X-VCA-Key, no Authorization.
    client.api(say("Build done", None)).await.unwrap();
    assert_eq!(auth_headers(&tts).await, (None, Some(KEY.to_owned())));

    // A gateway token is used from the next request on.
    let resp = local(&mut client, gateway(2, Some(TOKEN_1))).await;
    assert!(matches!(resp, LocalResponse::Engines { .. }), "{resp:?}");
    client.api(say("Build done", None)).await.unwrap();
    assert_eq!(
        auth_headers(&tts).await,
        (Some(format!("Bearer {TOKEN_1}")), Some(KEY.to_owned()))
    );

    // Rotated without restarting vcad.
    local(&mut client, gateway(3, Some(TOKEN_2))).await;
    client.api(say("Build done", None)).await.unwrap();
    assert_eq!(
        auth_headers(&tts).await,
        (Some(format!("Bearer {TOKEN_2}")), Some(KEY.to_owned()))
    );

    // Deleted: straight to the engine again.
    local(&mut client, gateway(4, None)).await;
    client.api(say("Build done", None)).await.unwrap();
    assert_eq!(auth_headers(&tts).await, (None, Some(KEY.to_owned())));
    server.shutdown().await;

    let config = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!config.contains("gw-tts-token") && !config.contains(KEY));
    let logged = String::from_utf8_lossy(&logs().lock().unwrap()).into_owned();
    assert!(
        !logged.contains("gw-tts-token"),
        "a gateway token was logged"
    );
    assert!(!logged.contains(KEY), "the key was logged");
}
