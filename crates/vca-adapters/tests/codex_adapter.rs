//! Codex adapter against realistic rollout fixtures.

use std::path::{Path, PathBuf};

use vca_adapters::codex::CodexAdapter;
use vca_adapters::{
    AgentAdapter, HistoryItem, HookOptions, HookScope, ProcInfo, ProcessTable, SessionLocator,
};

const ID: &str = "0199f2a4-6c1e-7d30-a1b2-c3d4e5f60718";
const LEGACY_ID: &str = "5973b6c0-94b8-487b-a530-2aeb6098ae0e";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn setup() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let day = home.path().join("sessions/2026/10/21");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::copy(
        fixture("codex_rollout.jsonl"),
        day.join(format!("rollout-2026-10-21T14-02-11-{ID}.jsonl")),
    )
    .unwrap();
    let old = home.path().join("sessions/2025/05/07");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::copy(
        fixture("codex_rollout_legacy.jsonl"),
        old.join(format!("rollout-2025-05-07T17-24-21-{LEGACY_ID}.jsonl")),
    )
    .unwrap();
    home
}

fn describe(items: &[HistoryItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            HistoryItem::Message { role, text, .. } => format!("{role:?}: {text}"),
            HistoryItem::ToolCall { name, summary, .. } => format!("call {name}: {summary}"),
            HistoryItem::ToolResult { text, is_error, .. } => format!("result({is_error}): {text}"),
        })
        .collect()
}

#[tokio::test]
async fn discovers_rollouts_and_running_process() {
    let home = setup();
    let adapter = CodexAdapter::with_home(home.path());
    let procs = ProcessTable::from_procs(vec![ProcInfo {
        pid: 900,
        parent: None,
        name: "codex".into(),
        cmd: vec!["codex".into()],
        exe: None,
        cwd: Some("/home/dev/projects/api-server".into()),
        start_time: 0,
    }]);
    let found = adapter.discover(&procs).await.unwrap();
    assert_eq!(found.len(), 2, "{found:#?}");
    let current = found
        .iter()
        .find(|s| s.agent_session_id.as_deref() == Some(ID))
        .unwrap();
    assert_eq!(
        current.project_dir.as_deref(),
        Some(Path::new("/home/dev/projects/api-server"))
    );
    assert_eq!(
        current.title.as_deref(),
        Some("Add a health check endpoint")
    );
    assert_eq!(current.pid, Some(900));
    let legacy = found
        .iter()
        .find(|s| s.agent_session_id.as_deref() == Some(LEGACY_ID))
        .unwrap();
    assert_eq!(legacy.title.as_deref(), Some("explain this repo"));
    assert_eq!(legacy.pid, None);
}

#[tokio::test]
async fn reads_history_without_duplicates_or_injected_context() {
    let home = setup();
    let adapter = CodexAdapter::with_home(home.path());
    let items = adapter
        .read_history(&SessionLocator {
            agent_session_id: Some(ID.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        describe(&items),
        [
            "User: Add a health check endpoint",
            r#"call shell: rg -n "Router::new" src"#,
            "result(false): src/main.rs:12:    let app = Router::new()",
            "call apply_patch: *** Begin Patch *** Update File: src/main.rs @@ +    .route(\"/healthz\", get(|| async { \"ok\" })) *** End Patch",
            "result(false): Success. Updated the following files:\nM src/main.rs",
            "Assistant: Added `GET /healthz` returning `ok`.",
        ]
    );

    let legacy = adapter
        .read_history(&SessionLocator {
            agent_session_id: Some(LEGACY_ID.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        describe(&legacy),
        ["User: explain this repo", "Assistant: It is a CLI."]
    );
}

#[tokio::test]
async fn installs_notify_in_codex_home() {
    let home = setup();
    let adapter = CodexAdapter::with_home(home.path());
    let st = adapter
        .install_hooks(&HookScope::User, &HookOptions::default())
        .await
        .unwrap();
    assert!(st.all_installed());
    assert_eq!(st.config_path, home.path().join("config.toml"));
    let text = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert_eq!(text.trim(), r#"notify = ["vca", "hook", "codex"]"#);
}
