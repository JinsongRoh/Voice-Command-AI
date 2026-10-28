//! Claude Code adapter against realistic on-disk fixtures.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vca_adapters::claude::ClaudeAdapter;
use vca_adapters::{
    AgentAdapter, HistoryItem, HookOptions, HookScope, ProcInfo, ProcessTable, SessionLocator,
};
use vca_core::{AgentKind, MessageRole};

const SESSION_ID: &str = "7f3c2a10-5b2e-4c8e-9d1a-2b3c4d5e6f70";
const PROJECT: &str = "/home/dev/projects/date-utils";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn setup() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir
        .path()
        .join("projects")
        .join("-home-dev-projects-date-utils");
    std::fs::create_dir_all(&project_dir).unwrap();
    let transcript = project_dir.join(format!("{SESSION_ID}.jsonl"));
    std::fs::copy(fixture("claude_session.jsonl"), &transcript).unwrap();
    // A subagent transcript next to it must be ignored by discovery.
    std::fs::write(project_dir.join("agent-1234.jsonl"), "{}\n").unwrap();
    (dir, transcript)
}

fn claude_proc(pid: u32, cwd: &str, cmd: &[&str]) -> ProcInfo {
    ProcInfo {
        pid,
        parent: Some(1),
        name: "claude".into(),
        cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
        exe: None,
        cwd: Some(PathBuf::from(cwd)),
        start_time: 1_790_000_000,
    }
}

#[tokio::test]
async fn discovers_stored_and_running_sessions() {
    let (dir, transcript) = setup();
    let adapter = ClaudeAdapter::with_config_dir(dir.path());
    let procs = ProcessTable::from_procs(vec![
        claude_proc(500, PROJECT, &["claude"]),
        claude_proc(600, "/home/dev/other", &["claude"]),
    ]);
    let mut found = adapter.discover(&procs).await.unwrap();
    found.sort_by_key(|s| s.pid);
    assert_eq!(found.len(), 2, "{found:#?}");

    let stored = &found[0];
    assert_eq!(stored.agent, AgentKind::ClaudeCode);
    assert_eq!(stored.agent_session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(stored.project_dir.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(stored.pid, Some(500));
    assert_eq!(stored.title.as_deref(), Some("Fix ISO week date parsing"));
    assert_eq!(
        stored.transcript_path.as_deref(),
        Some(transcript.as_path())
    );
    assert!(stored.last_activity.is_some());

    // A running process without a transcript yet is reported on its own.
    let fresh = &found[1];
    assert_eq!(fresh.pid, Some(600));
    assert_eq!(fresh.agent_session_id, None);
    assert_eq!(
        fresh.project_dir.as_deref(),
        Some(Path::new("/home/dev/other"))
    );
}

#[tokio::test]
async fn explicit_resume_id_wins_over_cwd() {
    let (dir, _) = setup();
    let adapter = ClaudeAdapter::with_config_dir(dir.path());
    let procs = ProcessTable::from_procs(vec![claude_proc(
        700,
        "/somewhere/else",
        &["claude", "--resume", SESSION_ID],
    )]);
    let found = adapter.discover(&procs).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pid, Some(700));
    assert_eq!(found[0].agent_session_id.as_deref(), Some(SESSION_ID));
}

#[tokio::test]
async fn reads_normalized_history() {
    let (dir, _) = setup();
    let adapter = ClaudeAdapter::with_config_dir(dir.path());
    let locator = SessionLocator {
        agent_session_id: Some(SESSION_ID.into()),
        transcript_path: None,
        project_dir: Some(PROJECT.into()),
    };
    let items = adapter.read_history(&locator).await.unwrap();
    let summary: Vec<String> = items
        .iter()
        .map(|i| match i {
            HistoryItem::Message { role, text, .. } => format!("{role:?}: {text}"),
            HistoryItem::ToolCall { name, summary, .. } => format!("call {name}: {summary}"),
            HistoryItem::ToolResult { text, is_error, .. } => format!("result({is_error}): {text}"),
        })
        .collect();
    assert_eq!(
        summary,
        [
            "User: Fix the failing date parser test",
            "Assistant: I'll run the test suite to see what fails.",
            "call Bash: npm test -- date-parser",
            "result(true): FAIL src/parser.test.ts\n  ✕ parses ISO week dates (4 ms)",
            "call Edit: /home/dev/projects/date-utils/src/parser.ts",
            "result(false): The file has been updated.",
            "Assistant: Fixed: the week number needs two digits. All date parser tests pass now.",
            "System: <command-name>/cost</command-name>\n<command-message>cost</command-message>",
        ]
    );
    // Timestamps are carried over.
    assert!(matches!(
        items[0],
        HistoryItem::Message {
            at: Some(_),
            role: MessageRole::User,
            ..
        }
    ));
    // Unknown sessions are reported as not found.
    let missing = SessionLocator {
        agent_session_id: Some("nope".into()),
        ..Default::default()
    };
    assert!(adapter.read_history(&missing).await.is_err());
}

#[tokio::test]
async fn tails_appended_records() {
    let (dir, transcript) = setup();
    let adapter = ClaudeAdapter::with_config_dir(dir.path());
    let locator = SessionLocator {
        transcript_path: Some(transcript.clone()),
        ..Default::default()
    };
    let mut rx = adapter.tail_history(&locator, false).await.unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        f,
        r#"{{"type":"user","isSidechain":false,"message":{{"role":"user","content":"Now add a changelog entry"}},"timestamp":"2026-10-20T09:05:00.000Z"}}"#
    )
    .unwrap();
    f.flush().unwrap();
    let item = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("item within timeout")
        .expect("stream open");
    assert!(
        matches!(item, HistoryItem::Message { ref text, .. } if text == "Now add a changelog entry")
    );
}

#[tokio::test]
async fn installs_hooks_in_profile_config_dir() {
    let (dir, _) = setup();
    let adapter = ClaudeAdapter::with_config_dir(dir.path());
    let st = adapter
        .install_hooks(&HookScope::User, &HookOptions::default())
        .await
        .unwrap();
    assert_eq!(st.config_path, dir.path().join("settings.json"));
    assert!(st.all_installed());
    let st = adapter
        .hooks_status(&HookScope::User, &HookOptions::default())
        .await
        .unwrap();
    assert!(st.all_installed());
    let st = adapter.uninstall_hooks(&HookScope::User).await.unwrap();
    assert!(!st.all_installed());
    let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
    assert_eq!(text.trim(), "{}");
}
