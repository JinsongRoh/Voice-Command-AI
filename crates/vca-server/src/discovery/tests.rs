//! Discovery passes over a made-up process table: no real process is read,
//! signalled or typed into.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use vca_adapters::generic::{AgentSpec, GenericAdapter};
use vca_adapters::types::{Capabilities, DiscoveredSession, LaunchSpec, Profile};
use vca_adapters::{AgentAdapter, ProcInfo, ProcessMatcher, ProcessTable, TerminalTarget};
use vca_broker::contain::Scope as PauseScope;
use vca_core::error::ErrorCode;
use vca_core::protocol::{FoundAgent, FoundStatus};
use vca_core::{AgentKind, ClientMsg, Envelope, LaunchRequest, ServerMsg, SessionId};

use super::*;
use crate::api::{Api, DeviceIdentity};
use crate::events::LogLimits;
use crate::sessions::ManagerConfig;
use crate::workspace::WorkspaceRoots;

/// A process table that tests change between passes.
#[derive(Default)]
struct FakeProcesses {
    procs: Mutex<Vec<ProcInfo>>,
    snapshots: AtomicUsize,
}

struct Source(Arc<FakeProcesses>);

#[async_trait]
impl ProcessSource for Source {
    async fn snapshot(&self) -> ProcessTable {
        self.0.snapshots.fetch_add(1, Ordering::SeqCst);
        ProcessTable::from_procs(lock(&self.0.procs).clone())
    }

    async fn terminal(&self, pid: u32, _: &ProcessTable) -> Option<TerminalTarget> {
        Some(TerminalTarget::WindowsConsole { pid, created: None })
    }
}

/// Claude Code as discovery sees it before the first message: a process
/// and no conversation file. Reads no files.
struct NoTranscriptClaude;

#[async_trait]
impl AgentAdapter for NoTranscriptClaude {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transcripts: true,
            ..Capabilities::default()
        }
    }

    fn process_matcher(&self) -> ProcessMatcher {
        ProcessMatcher::new(&["claude"], &["@anthropic-ai/claude-code"])
    }

    async fn discover(&self, procs: &ProcessTable) -> vca_adapters::Result<Vec<DiscoveredSession>> {
        Ok(procs
            .find(&self.process_matcher())
            .into_iter()
            .map(|p| DiscoveredSession {
                agent: AgentKind::ClaudeCode,
                agent_session_id: None,
                project_dir: p.cwd.clone(),
                pid: Some(p.pid),
                last_activity: None,
                title: None,
                transcript_path: None,
            })
            .collect())
    }

    fn launch_spec(
        &self,
        _: &LaunchRequest,
        _: Option<&Profile>,
    ) -> vca_adapters::Result<LaunchSpec> {
        Err(self.unsupported("launching"))
    }
}

/// Codex by process name only (the real adapter reads `~/.codex`).
fn codex_by_name() -> Arc<dyn AgentAdapter> {
    let spec = AgentSpec {
        kind: AgentKind::Codex,
        display_name: "Codex".into(),
        program: "codex".into(),
        process_names: vec!["codex".into()],
        script_markers: vec!["@openai/codex".into()],
        profile_env_var: None,
        capabilities: Capabilities::default(),
    };
    Arc::new(GenericAdapter::new(spec))
}

fn adapters() -> Vec<Arc<dyn AgentAdapter>> {
    let mut out: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(NoTranscriptClaude), codex_by_name()];
    out.extend(
        AgentSpec::builtin()
            .into_iter()
            .map(|s| Arc::new(GenericAdapter::new(s)) as Arc<dyn AgentAdapter>),
    );
    out
}

fn proc(pid: u32, parent: Option<u32>, name: &str, cmd: &[&str], cwd: &str) -> ProcInfo {
    ProcInfo {
        pid,
        parent,
        name: name.into(),
        cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
        exe: None,
        cwd: Some(PathBuf::from(cwd)),
        start_time: u64::from(pid) * 10,
    }
}

/// Pids far above real ones, so nothing here names a live process.
const CLAUDE: u32 = 4_100_004;
const GEMINI: u32 = 4_100_006;
const CODEX_APP: u32 = 4_100_008;
const ANTIGRAVITY: u32 = 4_100_009;
const AIDER: u32 = 4_100_011;
const SHELL_RUNNING_CLAUDE: u32 = 4_100_010;

/// A Windows PC: two terminal tabs with agents, Codex inside the ChatGPT
/// app, an IDE agent, and plain shells.
fn desktop() -> Vec<ProcInfo> {
    vec![
        proc(4_100_001, None, "explorer.exe", &["explorer.exe"], "/"),
        proc(
            4_100_002,
            Some(4_100_001),
            "WindowsTerminal.exe",
            &["WindowsTerminal.exe"],
            "/",
        ),
        proc(
            4_100_003,
            Some(4_100_002),
            "pwsh.exe",
            &["pwsh.exe"],
            "D:\\shop",
        ),
        proc(
            CLAUDE,
            Some(4_100_003),
            "claude.exe",
            &["claude.exe"],
            "D:\\shop",
        ),
        proc(
            4_100_005,
            Some(4_100_002),
            "powershell.exe",
            &["powershell.exe"],
            "D:\\api",
        ),
        proc(
            GEMINI,
            Some(4_100_005),
            "node.exe",
            &[
                "C:\\Program Files\\nodejs\\node.exe",
                "C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\\@google\\gemini-cli\\dist\\index.js",
            ],
            "D:\\api",
        ),
        proc(
            AIDER,
            Some(4_100_005),
            "python.exe",
            &["python.exe", "-m", "aider"],
            "D:\\api",
        ),
        proc(
            4_100_007,
            Some(4_100_001),
            "ChatGPT.exe",
            &["ChatGPT.exe"],
            "/",
        ),
        proc(
            CODEX_APP,
            Some(4_100_007),
            "codex.exe",
            &["codex.exe", "app-server"],
            "/",
        ),
        proc(
            ANTIGRAVITY,
            Some(4_100_001),
            "Antigravity.exe",
            &["Antigravity.exe"],
            "/",
        ),
        // A shell whose command line names an agent is still a shell.
        proc(
            SHELL_RUNNING_CLAUDE,
            Some(4_100_002),
            "cmd.exe",
            &["cmd.exe", "/c", "claude"],
            "D:\\shop",
        ),
        proc(4_100_012, Some(4_100_002), "bash", &["bash"], "/home/me"),
    ]
}

fn manager(dir: &Path) -> SessionManager {
    SessionManager::new(
        ManagerConfig {
            limits: LogLimits::default(),
            exited_retention: Duration::from_secs(60),
            pause_scope: PauseScope::ChildrenOnly,
            heuristics_interval: Duration::from_secs(1),
            control_endpoint: PathBuf::from("unused"),
            attach_dir: None,
            workspace_roots: WorkspaceRoots::new(Vec::new()),
            session_names: Some(dir.join("names.json")),
            allow_generic_launch: false,
        },
        None,
    )
}

struct Fixture {
    mgr: SessionManager,
    discovery: Discovery,
    procs: Arc<FakeProcesses>,
}

fn fixture(dir: &Path, enabled: bool, debounce: Duration) -> Fixture {
    let procs = Arc::new(FakeProcesses::default());
    *lock(&procs.procs) = desktop();
    let mgr = manager(dir);
    let discovery = Discovery::with_source(
        mgr.clone(),
        adapters(),
        DiscoverySettings {
            enabled,
            interval: Duration::from_secs(3600),
            detect_terminals: true,
            data_dir: Some(dir.to_owned()),
        },
        Box::new(Source(Arc::clone(&procs))),
        debounce,
    );
    Fixture {
        mgr,
        discovery,
        procs,
    }
}

fn found(msg: ServerMsg) -> Vec<FoundAgent> {
    match msg {
        ServerMsg::ScanReport { found, .. } => found,
        other => panic!("expected a scan report, got {other:?}"),
    }
}

fn row(found: &[FoundAgent], pid: u32) -> &FoundAgent {
    found
        .iter()
        .find(|f| f.pid == pid)
        .unwrap_or_else(|| panic!("process {pid} missing from {found:?}"))
}

fn listed_pids(mgr: &SessionManager) -> Vec<u32> {
    let mut pids: Vec<u32> = mgr
        .list()
        .iter()
        .filter_map(|s| match &s.origin {
            vca_core::SessionOrigin::Local { pid, .. } => *pid,
            vca_core::SessionOrigin::Cloud { .. } => None,
        })
        .collect();
    pids.sort_unstable();
    pids
}

#[tokio::test]
async fn reports_every_status_and_imports_terminal_sessions_only() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), true, SCAN_DEBOUNCE);
    let found = found(f.discovery.scan().await.unwrap());

    assert_eq!(found.len(), 5, "{found:?}");
    let claude = row(&found, CLAUDE);
    assert_eq!(claude.status, FoundStatus::NoTranscriptYet);
    assert!(claude.session_id.is_some());
    assert_eq!(claude.terminal.as_deref(), Some("Windows Terminal"));
    assert_eq!(claude.project_dir.as_deref(), Some("D:\\shop"));
    // The title names the folder as this platform splits the path: the
    // fixture's Windows path is one plain name elsewhere.
    let folder = if cfg!(windows) { "shop" } else { "D:\\shop" };
    assert_eq!(
        claude.title.as_deref(),
        Some(format!("{folder} · Claude Code").as_str())
    );

    let gemini = row(&found, GEMINI);
    assert_eq!(gemini.agent, AgentKind::GeminiCli);
    assert_eq!(gemini.status, FoundStatus::Imported);
    assert!(gemini.session_id.is_some());
    assert_eq!(row(&found, AIDER).status, FoundStatus::Imported);

    let codex = row(&found, CODEX_APP);
    assert_eq!(codex.status, FoundStatus::NotInATerminal);
    assert_eq!(codex.terminal.as_deref(), Some("ChatGPT"));
    assert_eq!(codex.session_id, None);

    assert_eq!(
        row(&found, ANTIGRAVITY).status,
        FoundStatus::UnsupportedAgent
    );
    // Plain shells never appear, whatever they run.
    assert!(!found.iter().any(|f| f.pid == SHELL_RUNNING_CLAUDE));
    assert!(
        found
            .iter()
            .all(|f| !matches!(f.agent, AgentKind::Shell(_)))
    );

    // Only the terminal sessions are on the list.
    assert_eq!(listed_pids(&f.mgr), vec![CLAUDE, GEMINI, AIDER]);
    let session = f.mgr.get(claude.session_id.as_ref().unwrap()).unwrap();
    assert_eq!(session.info().control, vca_core::ControlTier::TerminalApi);
}

#[tokio::test]
async fn hidden_sessions_leave_the_list_and_stay_hidden_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), true, SCAN_DEBOUNCE);
    let first = found(f.discovery.scan().await.unwrap());
    let gemini_id = row(&first, GEMINI).session_id.clone().unwrap();

    // By session id.
    let after = found(
        f.discovery
            .hide(Some(gemini_id.clone()), None)
            .await
            .unwrap(),
    );
    assert_eq!(row(&after, GEMINI).status, FoundStatus::HiddenByUser);
    assert_eq!(row(&after, GEMINI).session_id, None);
    assert!(f.mgr.get(&gemini_id).is_none());
    assert_eq!(listed_pids(&f.mgr), vec![CLAUDE, AIDER]);
    // By pid.
    let after = found(f.discovery.hide(None, Some(AIDER)).await.unwrap());
    assert_eq!(row(&after, AIDER).status, FoundStatus::HiddenByUser);
    assert_eq!(listed_pids(&f.mgr), vec![CLAUDE]);
    // The process was never touched: it is still in the table.
    assert_eq!(lock(&f.procs.procs).len(), desktop().len());

    // A later pass keeps them off.
    let again = found(f.discovery.scan().await.unwrap());
    assert_eq!(row(&again, GEMINI).status, FoundStatus::HiddenByUser);
    assert_eq!(listed_pids(&f.mgr), vec![CLAUDE]);

    // A restarted server reads the hidden list from its data directory.
    let restarted = fixture(dir.path(), true, SCAN_DEBOUNCE);
    let report = found(restarted.discovery.scan().await.unwrap());
    assert_eq!(row(&report, GEMINI).status, FoundStatus::HiddenByUser);
    assert_eq!(row(&report, AIDER).status, FoundStatus::HiddenByUser);
    assert_eq!(listed_pids(&restarted.mgr), vec![CLAUDE]);

    // Unhide one, then all.
    let back = found(
        restarted
            .discovery
            .unhide(None, Some(GEMINI), false)
            .await
            .unwrap(),
    );
    assert_eq!(row(&back, GEMINI).status, FoundStatus::Imported);
    assert_eq!(row(&back, AIDER).status, FoundStatus::HiddenByUser);
    assert_eq!(listed_pids(&restarted.mgr), vec![CLAUDE, GEMINI]);
    let back = found(restarted.discovery.unhide(None, None, true).await.unwrap());
    assert_eq!(row(&back, AIDER).status, FoundStatus::Imported);
    assert_eq!(listed_pids(&restarted.mgr), vec![CLAUDE, GEMINI, AIDER]);

    // Nothing hidden is left.
    let err = restarted
        .discovery
        .unhide(None, Some(GEMINI), false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn hide_refuses_what_is_not_an_agent_process() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), true, SCAN_DEBOUNCE);
    f.discovery.scan().await.unwrap();
    // A plain shell, an unknown pid, an unknown session, nothing at all.
    for (session, pid, code) in [
        (None, Some(SHELL_RUNNING_CLAUDE), ErrorCode::NotFound),
        (None, Some(4_199_999), ErrorCode::NotFound),
        (Some(SessionId::from("nope")), None, ErrorCode::NotFound),
        (None, None, ErrorCode::InvalidRequest),
    ] {
        let err = f.discovery.hide(session, pid).await.unwrap_err();
        assert_eq!(err.code(), code);
    }
    assert_eq!(listed_pids(&f.mgr), vec![CLAUDE, GEMINI, AIDER]);
}

#[tokio::test]
async fn ended_processes_leave_the_list_and_the_hidden_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), true, Duration::ZERO);
    f.discovery.scan().await.unwrap();
    f.discovery.hide(None, Some(AIDER)).await.unwrap();
    lock(&f.procs.procs).retain(|p| p.pid != AIDER && p.pid != GEMINI);
    let found = found(f.discovery.scan().await.unwrap());
    assert!(!found.iter().any(|r| r.pid == AIDER || r.pid == GEMINI));
    let gemini = f
        .mgr
        .list()
        .into_iter()
        .find(|s| {
            matches!(
                s.origin,
                vca_core::SessionOrigin::Local {
                    pid: Some(GEMINI),
                    ..
                }
            )
        })
        .unwrap();
    assert!(matches!(
        gemini.state,
        vca_core::SessionState::Exited { .. }
    ));
    let saved = std::fs::read_to_string(dir.path().join(HIDDEN_FILE)).unwrap();
    assert!(!saved.contains(&AIDER.to_string()), "{saved}");
}

#[tokio::test]
async fn scans_are_debounced() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), true, SCAN_DEBOUNCE);
    f.discovery.scan().await.unwrap();
    f.discovery.scan().await.unwrap();
    f.discovery.scan().await.unwrap();
    // Neither an unknown pid nor "list all again" with nothing hidden
    // forces a new pass.
    f.discovery.hide(None, Some(4_199_999)).await.unwrap_err();
    f.discovery.unhide(None, None, true).await.unwrap();
    assert_eq!(f.procs.snapshots.load(Ordering::SeqCst), 1);
    // Hiding is immediate and listing again runs a pass.
    f.discovery.hide(None, Some(GEMINI)).await.unwrap();
    assert_eq!(f.procs.snapshots.load(Ordering::SeqCst), 1);
    f.discovery.unhide(None, Some(GEMINI), false).await.unwrap();
    assert_eq!(f.procs.snapshots.load(Ordering::SeqCst), 2);

    let quick = tempfile::tempdir().unwrap();
    let f = fixture(quick.path(), true, Duration::ZERO);
    f.discovery.scan().await.unwrap();
    f.discovery.scan().await.unwrap();
    assert_eq!(f.procs.snapshots.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn turned_off_discovery_answers_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), false, SCAN_DEBOUNCE);
    assert!(f.discovery.start().is_none());
    let err = f.discovery.scan().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);
    assert_eq!(f.procs.snapshots.load(Ordering::SeqCst), 0);
    assert!(f.mgr.list().is_empty());
}

async fn ask(
    client: &Arc<crate::api::Client>,
    rx: &mut tokio::sync::mpsc::Receiver<Envelope<ServerMsg>>,
    api: &Api,
    id: u64,
    body: ClientMsg,
) -> ServerMsg {
    api.handle(client, Envelope { id, body }).await;
    loop {
        let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("an answer")
            .expect("open channel");
        if env.id == id {
            return env.body;
        }
    }
}

#[tokio::test]
async fn view_devices_scan_and_command_devices_hide() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path(), true, SCAN_DEBOUNCE);
    let api = Api::new(f.mgr.clone(), "test".into(), Duration::from_secs(3600));
    api.set_discovery(f.discovery.clone());

    let (viewer, mut view_rx) = api.connect_as(
        "test",
        Some(DeviceIdentity {
            device_id: "phone".into(),
            scope: vca_net::Scope::View,
        }),
    );
    let hello = ClientMsg::Hello {
        protocol_version: vca_core::PROTOCOL_VERSION,
        app_version: "t".into(),
    };
    match ask(&viewer, &mut view_rx, &api, 1, hello).await {
        ServerMsg::Welcome { features, .. } => {
            assert!(features.contains(&vca_core::protocol::FEATURE_SCAN_SESSIONS.to_owned()));
        }
        other => panic!("{other:?}"),
    }
    let report = ask(&viewer, &mut view_rx, &api, 2, ClientMsg::ScanSessions).await;
    assert_eq!(found(report).len(), 5);
    let hide = ClientMsg::HideObserved {
        session_id: None,
        pid: Some(GEMINI),
    };
    match ask(&viewer, &mut view_rx, &api, 3, hide.clone()).await {
        ServerMsg::Error { code, .. } => assert_eq!(code, ErrorCode::Forbidden),
        other => panic!("{other:?}"),
    }
    let unhide = ClientMsg::UnhideObserved {
        session_id: None,
        pid: None,
        all: true,
    };
    match ask(&viewer, &mut view_rx, &api, 4, unhide).await {
        ServerMsg::Error { code, .. } => assert_eq!(code, ErrorCode::Forbidden),
        other => panic!("{other:?}"),
    }

    let (commander, mut cmd_rx) = api.connect_as(
        "test",
        Some(DeviceIdentity {
            device_id: "tablet".into(),
            scope: vca_net::Scope::Command,
        }),
    );
    let report = found(ask(&commander, &mut cmd_rx, &api, 5, hide).await);
    assert_eq!(row(&report, GEMINI).status, FoundStatus::HiddenByUser);
    match ask(&commander, &mut cmd_rx, &api, 6, ClientMsg::ListSessions).await {
        ServerMsg::Sessions { sessions } => {
            assert_eq!(sessions.len(), 2);
            assert!(sessions.iter().all(|s| !matches!(
                s.origin,
                vca_core::SessionOrigin::Local {
                    pid: Some(GEMINI),
                    ..
                }
            )));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn servers_without_discovery_do_not_advertise_it() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(dir.path());
    let api = Api::new(mgr, "test".into(), Duration::from_secs(3600));
    let (client, mut rx) = api.connect("test");
    let report = ask(&client, &mut rx, &api, 1, ClientMsg::ScanSessions).await;
    assert!(matches!(
        report,
        ServerMsg::Error {
            code: ErrorCode::Unsupported,
            ..
        }
    ));
    let hello = ClientMsg::Hello {
        protocol_version: vca_core::PROTOCOL_VERSION,
        app_version: "t".into(),
    };
    match ask(&client, &mut rx, &api, 2, hello).await {
        ServerMsg::Welcome { features, .. } => {
            assert!(!features.contains(&vca_core::protocol::FEATURE_SCAN_SESSIONS.to_owned()));
        }
        other => panic!("{other:?}"),
    }
}

/// The conversation file of each running process (see [`TranscriptClaude`]).
type Files = Arc<Mutex<Vec<(u32, PathBuf)>>>;

/// Claude Code with a conversation file per running process, as the real
/// adapter assigns them: the newest file in the process's folder. Tests
/// change the file between passes (`/clear` writes a new one). Reads no
/// files.
struct TranscriptClaude {
    /// The transcript each pid's newest conversation is in.
    files: Files,
}

#[async_trait]
impl AgentAdapter for TranscriptClaude {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transcripts: true,
            ..Capabilities::default()
        }
    }

    fn process_matcher(&self) -> ProcessMatcher {
        ProcessMatcher::new(&["claude"], &["@anthropic-ai/claude-code"])
    }

    async fn discover(&self, procs: &ProcessTable) -> vca_adapters::Result<Vec<DiscoveredSession>> {
        let files = lock(&self.files).clone();
        Ok(procs
            .find(&self.process_matcher())
            .into_iter()
            .map(|p| {
                let file = files.iter().find(|(pid, _)| *pid == p.pid).map(|(_, f)| f);
                DiscoveredSession {
                    agent: AgentKind::ClaudeCode,
                    agent_session_id: file
                        .and_then(|f| f.file_stem())
                        .map(|s| s.to_string_lossy().into_owned()),
                    project_dir: p.cwd.clone(),
                    pid: Some(p.pid),
                    last_activity: None,
                    title: None,
                    transcript_path: file.cloned(),
                }
            })
            .collect())
    }

    fn launch_spec(
        &self,
        _: &LaunchRequest,
        _: Option<&Profile>,
    ) -> vca_adapters::Result<LaunchSpec> {
        Err(self.unsupported("launching"))
    }
}

fn transcript_fixture(dir: &Path, procs: Vec<ProcInfo>) -> (Fixture, Files) {
    let files = Arc::new(Mutex::new(Vec::new()));
    let fake = Arc::new(FakeProcesses::default());
    *lock(&fake.procs) = procs;
    let mgr = manager(dir);
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(TranscriptClaude {
        files: Arc::clone(&files),
    })];
    let discovery = Discovery::with_source(
        mgr.clone(),
        adapters,
        DiscoverySettings {
            enabled: true,
            interval: Duration::from_secs(3600),
            detect_terminals: true,
            data_dir: Some(dir.to_owned()),
        },
        Box::new(Source(Arc::clone(&fake))),
        Duration::ZERO,
    );
    (
        Fixture {
            mgr,
            discovery,
            procs: fake,
        },
        files,
    )
}

/// One Claude Code conversation file with an assistant message.
fn conversation(folder: &Path, id: &str, text: &str) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let path = folder.join(format!("{id}.jsonl"));
    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": id,
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
    });
    std::fs::write(&path, format!("{line}\n")).unwrap();
    path
}

fn claude_session(mgr: &SessionManager, pid: u32) -> Arc<crate::sessions::Session> {
    mgr.all()
        .into_iter()
        .find(|s| s.pid() == Some(pid))
        .unwrap_or_else(|| panic!("no session for {pid}"))
}

async fn chat_shows(session: &crate::sessions::Session, text: &str) -> bool {
    for _ in 0..100 {
        let shown = lock(session.log())
            .replay_after(0)
            .events
            .iter()
            .any(|e| matches!(e, vca_core::SessionEvent::Message { text: t, .. } if t == text));
        if shown {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn an_observed_session_follows_its_new_conversation_after_clear() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("projects").join("D--shop");
    let (f, files) = transcript_fixture(dir.path(), desktop());
    let old = conversation(&folder, "old-conv", "before clear");
    lock(&files).push((CLAUDE, old.clone()));
    f.discovery.scan().await.unwrap();
    let session = claude_session(&f.mgr, CLAUDE);
    assert_eq!(
        session.followed_transcript().as_deref(),
        Some(old.as_path())
    );
    assert!(chat_shows(&session, "before clear").await);

    // `/clear` in the same terminal: the process writes a new file.
    let new = conversation(&folder, "new-conv", "after clear");
    lock(&files)[0].1 = new.clone();
    f.discovery.scan().await.unwrap();

    let same = claude_session(&f.mgr, CLAUDE);
    assert_eq!(same.id, session.id, "the card stays the same");
    assert_eq!(same.followed_transcript().as_deref(), Some(new.as_path()));
    assert_eq!(same.agent_session_id().as_deref(), Some("new-conv"));
    assert_eq!(same.transcript_path().as_deref(), Some(new.as_path()));
    assert!(chat_shows(&same, "after clear").await);
}

#[tokio::test]
async fn a_new_file_is_not_taken_when_two_agents_share_the_folder_or_another_owns_it() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("projects").join("D--shop");
    const SECOND: u32 = 4_100_020;
    let mut procs = desktop();
    procs.push(proc(
        SECOND,
        Some(4_100_003),
        "claude.exe",
        &["claude.exe"],
        "D:\\shop",
    ));
    let (f, files) = transcript_fixture(dir.path(), procs);
    let a = conversation(&folder, "conv-a", "a");
    let b = conversation(&folder, "conv-b", "b");
    lock(&files).extend([(CLAUDE, a.clone()), (SECOND, b.clone())]);
    f.discovery.scan().await.unwrap();
    let first = claude_session(&f.mgr, CLAUDE);
    let second = claude_session(&f.mgr, SECOND);
    assert_eq!(first.followed_transcript().as_deref(), Some(a.as_path()));

    // The adapter now names the other agent's file for the first process:
    // it belongs to another session and stays there.
    lock(&files)[0].1 = b.clone();
    f.discovery.scan().await.unwrap();
    assert_eq!(first.followed_transcript().as_deref(), Some(a.as_path()));
    assert_eq!(second.followed_transcript().as_deref(), Some(b.as_path()));

    // A new, unowned file in a folder two agents share: which one wrote it
    // is unknown, so neither switches.
    let c = conversation(&folder, "conv-c", "c");
    lock(&files)[0].1 = c;
    f.discovery.scan().await.unwrap();
    assert_eq!(first.followed_transcript().as_deref(), Some(a.as_path()));
    assert_eq!(first.agent_session_id().as_deref(), Some("conv-a"));
}
