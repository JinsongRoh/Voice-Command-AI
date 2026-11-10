//! Spoken briefings and result summaries through the API, as a view-only
//! paired device. No LLM and no TTS engine is called; no session process is
//! started (the summary logic itself is covered by the unit tests of
//! `voice_brief`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use vca_broker::contain::{ContainResult, Priority, ProcessControl, Scope};
use vca_core::error::ErrorCode;
use vca_core::{ClientMsg, Envelope, ServerMsg, SessionId};
use vca_server::api::DeviceIdentity;
use vca_server::config::Config;
use vca_server::daemon::{Options, Server};

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
        panic!("voice tests must never terminate processes ({root})")
    }
}

fn control_endpoint(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\vca-test-{}-voice-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    } else {
        dir.join("voice.sock")
    }
}

async fn start(dir: &Path) -> Server {
    let mut config = Config {
        workspace_roots: vec![dir.to_owned()],
        ..Config::default()
    };
    config.discovery.enabled = false;
    config.remote.enabled = false;
    let policy = dir.join("policies.toml");
    std::fs::write(&policy, "").unwrap();
    config.broker.policy_path = Some(policy);
    Server::start(
        Options {
            config_file: Some(dir.join("config.toml")),
            control: Some(control_endpoint(dir)),
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

/// Sends a request as a paired view-only device and waits for its answer.
async fn as_viewer(server: &Server, body: ClientMsg) -> ServerMsg {
    let (client, mut rx) = server.api.connect_as(
        "test",
        Some(DeviceIdentity {
            device_id: "dev_view".into(),
            scope: vca_net::Scope::View,
        }),
    );
    server.api.handle(&client, Envelope { id: 9, body }).await;
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let env = rx.recv().await.unwrap();
            if env.id == 9 {
                return env.body;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn view_devices_get_briefings_and_summaries_need_an_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path()).await;

    for (language, want) in [
        (None, "There are no sessions on this computer."),
        (Some("ko-KR"), "이 컴퓨터에는 세션이 없습니다."),
        (Some("ja"), "このコンピューターにセッションはありません。"),
        (Some("es"), "No hay sesiones en este equipo."),
    ] {
        let reply = as_viewer(
            &server,
            ClientMsg::Briefing {
                language: language.map(str::to_owned),
                order: Vec::new(),
            },
        )
        .await;
        assert_eq!(reply, ServerMsg::Briefing { text: want.into() });
    }

    let opt_in = as_viewer(
        &server,
        ClientMsg::SetVoiceSummary {
            enabled: true,
            details: false,
        },
    )
    .await;
    assert_eq!(opt_in, ServerMsg::Ok);
    let summary = as_viewer(
        &server,
        ClientMsg::Summarize {
            session_id: SessionId("missing".into()),
            language: Some("ko".into()),
            number: Some(3),
        },
    )
    .await;
    assert!(
        matches!(
            summary,
            ServerMsg::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ),
        "{summary:?}"
    );
    server.shutdown().await;
}
