//! Task inbox rules: extraction fixtures (en/ko/ja/es), dedupe, the
//! explicit tool, manual tasks, state changes and snooze, sending results,
//! reminders, persistence, scopes and the administrator shell (always a
//! mock: no test opens a real UAC prompt or shell). No real LLM is called.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use vca_core::error::ErrorCode;
use vca_core::task::{
    TaskChange, TaskDraft, TaskExportFormat, TaskPriority, TaskReminderKind, TaskResultDraft,
    TaskSettings, TaskShell, TaskSource, TaskStatus,
};
use vca_core::{
    AgentKind, ClientMsg, ControlTier, DangerLevel, Envelope, MessageRole, ServerMsg, SessionEvent,
    SessionId, SessionInfo, SessionOrigin, SessionState,
};
use vca_net::Scope;

use super::extract::extract;
use super::*;
use crate::api::{Api, DeviceIdentity};
use crate::events::LogLimits;
use crate::sessions::{ManagerConfig, Observed, ObservedIdentity, SessionManager};
use crate::workspace::WorkspaceRoots;

const START: u64 = 1_700_000_000_000;

fn manager() -> SessionManager {
    SessionManager::new(
        ManagerConfig {
            limits: LogLimits::default(),
            exited_retention: Duration::from_secs(60),
            pause_scope: vca_broker::contain::Scope::ChildrenOnly,
            heuristics_interval: Duration::from_secs(1),
            control_endpoint: PathBuf::from("unused"),
            attach_dir: None,
            workspace_roots: WorkspaceRoots::new(Vec::new()),
            session_names: None,
            allow_generic_launch: false,
        },
        None,
    )
}

fn info(id: &str, state: SessionState) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        origin: SessionOrigin::Local {
            agent: AgentKind::ClaudeCode,
            project_dir: PathBuf::from("/work/app"),
            pid: None,
        },
        display_name: format!("app {id}"),
        state,
        control: ControlTier::TerminalApi,
        profile: None,
        created_at: 0,
        last_activity: 0,
        unread: 0,
        preview: None,
        auto_name: None,
        custom_name: false,
        shell: None,
        pending_decision: None,
        subagents: 0,
        conductor: false,
        terminal_prompt: None,
        question: None,
        progress: None,
        problem: None,
    }
}

#[derive(Default)]
struct FakeLauncher {
    opened: Mutex<Vec<AdminShellRequest>>,
}

impl AdminShellLauncher for FakeLauncher {
    fn open(&self, request: &AdminShellRequest) -> Result<(), Error> {
        lock(&self.opened).push(request.clone());
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    data: PathBuf,
    mgr: SessionManager,
    tasks: Tasks,
    now: Arc<AtomicU64>,
    launcher: Arc<FakeLauncher>,
    sent: Arc<Mutex<Vec<(SessionId, String)>>>,
    audit: Arc<Mutex<Vec<(String, String, String)>>>,
    notices: broadcast::Sender<ServerMsg>,
}

fn open_tasks(
    mgr: &SessionManager,
    data: &Path,
    now: &Arc<AtomicU64>,
    launcher: &Arc<FakeLauncher>,
    sent: &Arc<Mutex<Vec<(SessionId, String)>>>,
    notices: broadcast::Sender<ServerMsg>,
) -> Tasks {
    let clock_now = Arc::clone(now);
    let sink = Arc::clone(sent);
    Tasks::new(
        mgr.clone(),
        notices,
        TasksOptions {
            data_dir: Some(data.to_owned()),
            machine: "test-pc".into(),
            clock: Some(Arc::new(move || clock_now.load(Ordering::SeqCst))),
            launcher: Some(Arc::clone(launcher) as Arc<dyn AdminShellLauncher>),
            deliver: Some(Arc::new(move |id: SessionId, text: String| {
                lock(&sink).push((id, text));
                Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), Error>>
            })),
        },
    )
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mgr = manager();
    let now = Arc::new(AtomicU64::new(START));
    let launcher = Arc::new(FakeLauncher::default());
    let sent = Arc::new(Mutex::new(Vec::new()));
    let notices = broadcast::channel(256).0;
    let tasks = open_tasks(&mgr, &data, &now, &launcher, &sent, notices.clone());
    let audit = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&audit);
    tasks.set_audit(Arc::new(move |actor: &Actor, event: &str, detail: &str| {
        lock(&log).push((actor.device_id.clone(), event.to_owned(), detail.to_owned()));
    }));
    Fixture {
        _dir: dir,
        data,
        mgr,
        tasks,
        now,
        launcher,
        sent,
        audit,
        notices,
    }
}

impl Fixture {
    fn add_session(&self, id: &str, state: SessionState) -> Arc<crate::sessions::Session> {
        self.mgr.insert_observed(
            info(id, state),
            Observed {
                terminal: None,
                identity: ObservedIdentity::default(),
            },
            None,
        )
    }

    fn advance(&self, ms: u64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }

    fn all(&self) -> Vec<HumanTask> {
        match self.tasks.list() {
            ServerMsg::Tasks { tasks, .. } => tasks,
            other => panic!("{other:?}"),
        }
    }

    fn manual(&self, title: &str) -> HumanTask {
        self.tasks
            .create(
                &Actor::local(),
                TaskDraft {
                    title: title.into(),
                    ..TaskDraft::default()
                },
            )
            .unwrap()
    }
}

fn say(session: &crate::sessions::Session, text: &str) -> u64 {
    session
        .push(|seq| SessionEvent::Message {
            seq,
            at: 1,
            role: MessageRole::Assistant,
            text: text.into(),
        })
        .seq()
}

// ------------------------------------------------------------- extraction

const KO_ADMIN: &str = "설정을 바꾸려면 관리자 권한이 필요합니다.\n\n\
관리자 PowerShell에서 다음 명령을 실행해 주세요:\n\n\
```powershell\nSet-ExecutionPolicy RemoteSigned -Scope LocalMachine\n```\n\n\
완료되면 알려 주세요.";

#[test]
fn korean_admin_powershell_with_code_block() {
    let found = extract(KO_ADMIN);
    // The explanation line ("... 관리자 권한이 필요합니다") asks nothing.
    assert_eq!(found.len(), 1, "{found:#?}");
    let run = &found[0];
    assert_eq!(run.title, "관리자 PowerShell에서 다음 명령을 실행해 주세요");
    assert_eq!(
        run.command.as_deref(),
        Some("Set-ExecutionPolicy RemoteSigned -Scope LocalMachine")
    );
    assert_eq!(run.shell, TaskShell::Powershell);
    assert!(run.needs_admin);
    // "완료되면 알려 주세요" closes the message: not a task.
    assert!(found.iter().all(|c| !c.title.contains("완료되면")));
}

#[test]
fn english_requests_with_urls_inline_commands_and_blocking() {
    let text = "I updated the config.\n\n\
Please sign in at https://console.example.com/approve and approve the pending request.\n\n\
Run this as administrator:\n```\nnet stop spooler\n```\n\n\
Please run `npm login` in your terminal, then I can publish the package.\n\n\
Let me know if you have any questions!";
    let found = extract(text);
    assert_eq!(found.len(), 3, "{found:#?}");
    assert_eq!(
        found[0].title,
        "Please sign in at https://console.example.com/approve and approve the pending request."
    );
    assert_eq!(found[0].steps, "- <https://console.example.com/approve>");
    assert!(found[0].command.is_none());
    assert_eq!(found[1].command.as_deref(), Some("net stop spooler"));
    assert!(found[1].needs_admin);
    assert_eq!(found[2].command.as_deref(), Some("npm login"));
    assert!(found[2].blocking, "\"then I can\" means the agent waits");
    assert!(!found[2].needs_admin);
}

#[test]
fn japanese_and_spanish_requests() {
    let ja = "管理者としてPowerShellで次のコマンドを実行してください。\n```\nRestart-Service Spooler\n```\n";
    let found = extract(ja);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].command.as_deref(), Some("Restart-Service Spooler"));
    assert!(found[0].needs_admin);
    assert_eq!(found[0].shell, TaskShell::Powershell);

    let es = "Por favor, ejecuta este comando como administrador:\n\n```bash\nsudo systemctl restart nginx\n```\n\nSi tienes dudas, avísame.";
    let found = extract(es);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(
        found[0].command.as_deref(),
        Some("sudo systemctl restart nginx")
    );
    assert_eq!(found[0].shell, TaskShell::Bash);
    assert!(found[0].needs_admin);
}

#[test]
fn steps_after_a_colon_and_no_false_positives() {
    let text = "To finish the setup you need to do this:\n\
1. Open the router page\n2. Enable UPnP\n\nThat is all.";
    let found = extract(text);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].steps, "- Open the router page\n- Enable UPnP");

    for quiet in [
        "I ran the tests and all 42 passed. Let me know if you want anything else.",
        "Would you like me to continue with the refactor?",
        "Please note that the cache is cleared on restart.",
        "궁금한 점이 있으면 알려 주세요.",
        "テストはすべて成功しました。ご質問があればお気軽にどうぞ。",
        "Listo. Si quieres, puedo seguir con la siguiente parte.",
        "```\nplease run this\n```",
    ] {
        assert!(extract(quiet).is_empty(), "{quiet}");
    }
}

#[test]
fn duplicates_in_one_message_collapse() {
    let text = "Please run `cargo login`.\n\nAgain: please run `cargo login`.";
    assert_eq!(extract(text).len(), 1);
}

#[tokio::test]
async fn scanning_the_same_request_twice_keeps_one_task() {
    let f = fixture();
    f.add_session("s1", SessionState::Idle);
    let sid = SessionId::from("s1");
    let first = f.tasks.scan_message(&sid, 5, KO_ADMIN).await;
    assert_eq!(first.len(), 1);
    let again = f.tasks.scan_message(&sid, 9, KO_ADMIN).await;
    assert!(again.is_empty());
    assert_eq!(f.all().len(), 1);
    let task = f.all().into_iter().find(|t| t.command.is_some()).unwrap();
    assert_eq!(task.source, TaskSource::Extracted);
    assert_eq!(task.message_seq, Some(5));
    assert_eq!(task.session_name.as_deref(), Some("app s1"));
    let command = task.command.unwrap();
    // Classified, never run.
    assert!(command.needs_admin);
    assert!(matches!(
        command.danger,
        DangerLevel::Safe | DangerLevel::Caution | DangerLevel::Dangerous
    ));
    assert!(f.sent.lock().unwrap().is_empty());
    assert!(lock(&f.launcher.opened).is_empty());
    // The same command from another session is another task.
    f.add_session("s2", SessionState::Idle);
    let other = f
        .tasks
        .scan_message(&SessionId::from("s2"), 1, KO_ADMIN)
        .await;
    assert_eq!(other.len(), 1);
    // Turned off: nothing is extracted.
    let mut settings = f.tasks.settings();
    settings.extract = false;
    f.tasks.configure(&Actor::local(), settings);
    f.add_session("s3", SessionState::Idle);
    assert!(
        f.tasks
            .scan_message(&SessionId::from("s3"), 1, KO_ADMIN)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn the_conductor_session_is_never_scanned() {
    let f = fixture();
    let s = f.add_session("cond", SessionState::Idle);
    f.mgr.set_conductor(&"cond".into(), true).unwrap();
    let sid = SessionId::from("cond");
    f.tasks.observe(&sid, &SessionState::Idle).await;
    f.tasks.observe(&sid, &SessionState::Running).await;
    say(&s, "Done. Please run `npm login` so I can publish.");
    f.tasks.observe(&sid, &SessionState::Idle).await;
    // The conductor files tasks through its MCP tool, not its replies.
    assert!(f.all().is_empty());

    // The same turn in an ordinary session is scanned.
    let other = f.add_session("s1", SessionState::Idle);
    let sid = SessionId::from("s1");
    f.tasks.observe(&sid, &SessionState::Idle).await;
    f.tasks.observe(&sid, &SessionState::Running).await;
    say(&other, "Done. Please run `npm login` so I can publish.");
    f.tasks.observe(&sid, &SessionState::Idle).await;
    assert_eq!(f.all().len(), 1);
}

#[tokio::test]
async fn a_finished_turn_is_scanned_once_and_history_is_not() {
    let f = fixture();
    let s = f.add_session("s1", SessionState::Idle);
    say(&s, "Please run `old-command` first.");
    let sid = SessionId::from("s1");
    // First sight: what is there already is the baseline.
    f.tasks.observe(&sid, &SessionState::Idle).await;
    assert!(f.all().is_empty());
    // A turn: running, a final message, idle.
    f.tasks.observe(&sid, &SessionState::Running).await;
    say(&s, "Done. Please run `npm login` so I can publish.");
    f.tasks.observe(&sid, &SessionState::Idle).await;
    let all = f.all();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].command.as_ref().unwrap().text, "npm login");
    assert!(all[0].blocking);
    assert_eq!(all[0].priority, TaskPriority::High);
    // Idle again without a new turn: nothing new.
    f.tasks.observe(&sid, &SessionState::Idle).await;
    assert_eq!(f.all().len(), 1);

    // The agent moves on (another turn ends): ask whether still needed.
    let mut rx = f.notices.subscribe();
    f.advance(1000);
    f.tasks.observe(&sid, &SessionState::Running).await;
    say(&s, "I published a dry run instead.");
    f.tasks.observe(&sid, &SessionState::Idle).await;
    let task = &f.all()[0];
    assert!(task.agent_moved_on);
    let mut kinds = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let ServerMsg::TaskReminder { reminder } = msg {
            kinds.push(reminder.kind);
        }
    }
    assert_eq!(kinds, [TaskReminderKind::AgentMovedOn]);
    // "Still needed" clears it.
    let task = f
        .tasks
        .update(&Actor::local(), &task.id, TaskChange::StillNeeded)
        .unwrap();
    assert!(!task.agent_moved_on);
}

#[derive(Default)]
struct FakeChat {
    reply: String,
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ChatBackend for FakeChat {
    async fn complete(&self, system: &str, user: &str) -> vca_speech::Result<String> {
        assert!(system.contains("untrusted"));
        lock(&self.seen).push(user.to_owned());
        Ok(self.reply.clone())
    }
}

#[tokio::test]
async fn llm_extraction_is_opt_in_and_redacted() {
    let f = fixture();
    f.add_session("s1", SessionState::Idle);
    let chat = Arc::new(FakeChat {
        reply: r#"Sure: {"tasks":[{"title":"Rotate the deploy key","steps":"In the console","command":null,"needs_admin":false,"blocking":true}]}"#.into(),
        ..FakeChat::default()
    });
    f.tasks.set_llm(Arc::clone(&chat) as Arc<dyn ChatBackend>);
    let text = "The deploy failed with token ghp_abcdefghijklmnopqrstuvwxyz0123456789 \
                and you should rotate it in the console.";
    let sid = SessionId::from("s1");
    assert!(f.tasks.scan_message(&sid, 1, text).await.is_empty());
    assert!(lock(&chat.seen).is_empty(), "off by default");

    let mut settings = f.tasks.settings();
    settings.extract_llm = true;
    f.tasks.configure(&Actor::local(), settings);
    let found = f.tasks.scan_message(&sid, 2, text).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "Rotate the deploy key");
    assert!(found[0].blocking);
    let seen = lock(&chat.seen).clone();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(seen[0].contains("[REDACTED]"));
    // Garbage replies add nothing.
    assert!(parse_llm_reply("no json here").is_empty());
    assert!(parse_llm_reply(r#"{"tasks":[{"title":"  "}]}"#).is_empty());
}

// ------------------------------------------------------- explicit and manual

#[tokio::test]
async fn explicit_tool_creates_once_and_lists_my_requests() {
    let f = fixture();
    f.add_session("s1", SessionState::Running);
    let caller = Some(SessionId::from("s1"));
    let args = json!({
        "title": "Install the signing certificate",
        "steps": ["Open certmgr", "Import cert.pfx"],
        "command": "certutil -addstore Root cert.cer",
        "needs_admin": true,
        "blocking": true,
        "due": "2030-01-02T03:04"
    });
    let out = mcp::call(
        &f.tasks,
        caller.clone(),
        "mcp",
        mcp::REQUEST_HUMAN_ACTION,
        &args,
    )
    .unwrap();
    assert_eq!(out["created"], true);
    let id = out["task"]["id"].as_str().unwrap().to_owned();
    let task = f.tasks.get(&id).unwrap();
    assert_eq!(task.source, TaskSource::Explicit);
    assert_eq!(task.steps, "1. Open certmgr\n2. Import cert.pfx");
    assert_eq!(task.priority, TaskPriority::High);
    assert!(task.command.as_ref().unwrap().needs_admin);
    assert_eq!(task.due, Some(1_893_553_440_000));
    // Asking again returns the open task.
    let again = mcp::call(
        &f.tasks,
        caller.clone(),
        "mcp",
        mcp::REQUEST_HUMAN_ACTION,
        &args,
    )
    .unwrap();
    assert_eq!(again["created"], false);
    assert_eq!(again["task"]["id"], id.as_str());

    let mine = mcp::call(&f.tasks, caller, "mcp", mcp::LIST_MY_REQUESTS, &json!({})).unwrap();
    assert_eq!(mine["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(mine["open"], 1);
    // Without a calling session there is nothing that is "mine".
    let err = mcp::call(&f.tasks, None, "mcp", mcp::LIST_MY_REQUESTS, &json!({})).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    // Only the conductor reads every session's tasks.
    f.manual("Unrelated");
    let all = mcp::call(
        &f.tasks,
        None,
        mcp::CONDUCTOR_ORIGIN,
        mcp::LIST_MY_REQUESTS,
        &json!({"all": true}),
    )
    .unwrap();
    assert_eq!(all["tasks"].as_array().unwrap().len(), 2);
    // No tool checks a task off or sends a result.
    assert!(mcp::tool_specs().iter().all(|t| mcp::is_task_tool(t.name)));
    assert_eq!(mcp::tool_specs().len(), 2);
    let err = mcp::call(&f.tasks, None, "mcp", "complete_task", &json!({})).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    let err = mcp::call(
        &f.tasks,
        None,
        "mcp",
        mcp::REQUEST_HUMAN_ACTION,
        &json!({"title": " "}),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    assert!(
        lock(&f.audit)
            .iter()
            .any(|(who, event, _)| who == "mcp" && event == events::CREATE)
    );
}

#[tokio::test]
async fn manual_task_from_a_message() {
    let f = fixture();
    f.add_session("s1", SessionState::Idle);
    let task = f
        .tasks
        .create(
            &Actor::local(),
            TaskDraft {
                session_id: Some("s1".into()),
                title: "Check the printer queue".into(),
                steps: "From the message".into(),
                command: Some("Get-PrintJob -PrinterName Office".into()),
                message_seq: Some(42),
                ..TaskDraft::default()
            },
        )
        .unwrap();
    assert_eq!(task.source, TaskSource::Manual);
    assert_eq!(task.message_seq, Some(42));
    assert_eq!(task.machine, "test-pc");
    assert_eq!(task.command.as_ref().unwrap().shell, TaskShell::Powershell);
    let err = f
        .tasks
        .create(
            &Actor::local(),
            TaskDraft {
                session_id: Some("gone".into()),
                title: "x".into(),
                ..TaskDraft::default()
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
    let err = f
        .tasks
        .create(&Actor::local(), TaskDraft::default())
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    let audit = lock(&f.audit).clone();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].1, events::CREATE);
    // Ids only: no task text in the audit log.
    assert!(!audit[0].2.contains("printer"));
}

// ---------------------------------------------------------- source context

fn say_at(session: &crate::sessions::Session, role: MessageRole, at: u64, text: &str) -> u64 {
    session
        .push(|seq| SessionEvent::Message {
            seq,
            at,
            role,
            text: text.into(),
        })
        .seq()
}

#[tokio::test]
async fn extraction_stores_the_source_anchor_with_a_redacted_excerpt() {
    let f = fixture();
    let s = f.add_session("s1", SessionState::Idle);
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let text = format!("The deploy used token {secret}.\n\n{KO_ADMIN}");
    let seq = say_at(&s, MessageRole::Assistant, START - 5_000, &text);
    let found = f.tasks.scan_message(&"s1".into(), seq, &text).await;
    assert_eq!(found.len(), 1);
    let task = &found[0];
    assert_eq!(task.session_id.as_ref().map(|s| s.0.as_str()), Some("s1"));
    assert_eq!(task.message_seq, Some(seq));
    assert_eq!(task.message_at, Some(START - 5_000));
    let excerpt = task.excerpt.as_deref().unwrap();
    assert!(!excerpt.contains(secret), "{excerpt}");
    assert!(excerpt.contains("[REDACTED]"));
    assert!(excerpt.contains("Set-ExecutionPolicy"));
    // Stored: it survives a restart.
    let reopened = open_tasks(
        &f.mgr,
        &f.data,
        &f.now,
        &f.launcher,
        &f.sent,
        f.notices.clone(),
    );
    assert_eq!(reopened.get(&task.id).unwrap().excerpt, task.excerpt);

    // A long message keeps the part around the request.
    let long = format!("{}\n\n{KO_ADMIN}", "padding line. ".repeat(200));
    let excerpt = source_excerpt(&long, Some("Set-ExecutionPolicy")).unwrap();
    assert!(excerpt.chars().count() <= TASK_MAX_EXCERPT_CHARS);
    assert!(excerpt.starts_with('…') && excerpt.contains("Set-ExecutionPolicy"));
}

#[tokio::test]
async fn manual_alert_and_explicit_tasks_point_at_their_source() {
    let f = fixture();
    let s = f.add_session("s1", SessionState::Idle);
    let asked = say_at(&s, MessageRole::User, 10, "Please deploy the API");
    let answer = say_at(
        &s,
        MessageRole::Assistant,
        20,
        "Sign in to the console first.",
    );
    // "할 일로 추가" on a message.
    let task = f
        .tasks
        .create(
            &Actor::local(),
            TaskDraft {
                session_id: Some("s1".into()),
                title: "Sign in".into(),
                message_seq: Some(answer),
                ..TaskDraft::default()
            },
        )
        .unwrap();
    assert_eq!(task.message_at, Some(20));
    assert_eq!(
        task.excerpt.as_deref(),
        Some("Sign in to the console first.")
    );
    // From an alert: the newest agent message.
    let alert = f
        .tasks
        .create(
            &Actor::local(),
            TaskDraft {
                session_id: Some("s1".into()),
                title: "Answer the agent".into(),
                anchor_latest: true,
                ..TaskDraft::default()
            },
        )
        .unwrap();
    assert_eq!(alert.message_seq, Some(answer));
    // A plain manual task has no source.
    let plain = f.manual("Buy toner");
    assert!(plain.message_seq.is_none() && plain.excerpt.is_none());
    // An agent's request points at the request it was working on.
    let (explicit, _) = f
        .tasks
        .request_human_action(
            Some("s1".into()),
            "mcp",
            serde_json::from_value(json!({"title": "Approve the release"})).unwrap(),
        )
        .unwrap();
    assert_eq!(explicit.message_seq, Some(asked));
    assert_eq!(explicit.excerpt.as_deref(), Some("Please deploy the API"));
    // A message the log no longer has keeps only its number.
    let gone = f
        .tasks
        .create(
            &Actor::local(),
            TaskDraft {
                session_id: Some("s1".into()),
                title: "Old one".into(),
                message_seq: Some(9_999),
                ..TaskDraft::default()
            },
        )
        .unwrap();
    assert_eq!(gone.message_seq, Some(9_999));
    assert!(gone.message_at.is_none() && gone.excerpt.is_none());
}

#[test]
fn a_task_file_without_source_context_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(TASKS_FILE);
    let saved = json!({
        "tasks": [{
            "task": {
                "id": "task_old", "session_id": "s1", "title": "Sign in",
                "source": "extracted", "created_at": 1, "updated_at": 1, "message_seq": 3
            },
            "key": "s1|sign in"
        }]
    });
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let data = load(&path);
    let task = &data.tasks[0].task;
    assert_eq!(task.message_seq, Some(3));
    assert!(task.message_at.is_none() && task.excerpt.is_none());
}

// ------------------------------------------------------- changes, reminders

#[tokio::test]
async fn status_changes_snooze_edit_and_order() {
    let f = fixture();
    let a = f.manual("First");
    let b = f.manual("Second");
    let c = f.manual("Third");
    let me = Actor::local();
    let done = f
        .tasks
        .update(
            &me,
            &a.id,
            TaskChange::Status {
                status: TaskStatus::Done,
            },
        )
        .unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    let order: Vec<String> = f.all().into_iter().map(|t| t.title).collect();
    assert_eq!(order, ["Second", "Third", "First"], "closed tasks last");
    f.tasks
        .update(
            &me,
            &a.id,
            TaskChange::Status {
                status: TaskStatus::Open,
            },
        )
        .unwrap();
    f.tasks
        .update(&me, &c.id, TaskChange::Move { position: 0 })
        .unwrap();
    let order: Vec<String> = f.all().into_iter().map(|t| t.title).collect();
    assert_eq!(order, ["Third", "First", "Second"]);

    // Snooze: only into the future; reminders wait; the end is announced.
    let err = f
        .tasks
        .update(
            &me,
            &b.id,
            TaskChange::Snooze {
                until: Some(START - 1),
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    let snoozed = f
        .tasks
        .update(
            &me,
            &b.id,
            TaskChange::Snooze {
                until: Some(START + 60_000),
            },
        )
        .unwrap();
    assert_eq!(snoozed.snoozed_until, Some(START + 60_000));
    assert!(f.tasks.tick(START + 30_000).is_empty());
    let r = f.tasks.tick(START + 61_000);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].kind, TaskReminderKind::SnoozeEnded);
    assert_eq!(f.tasks.get(&b.id).unwrap().snoozed_until, None);

    let edited = f
        .tasks
        .update(
            &me,
            &b.id,
            TaskChange::Edit {
                title: Some("Second, edited".into()),
                steps: None,
                command: Some("rm -rf build".into()),
                shell: Some(TaskShell::Bash),
                needs_admin: Some(false),
                priority: Some(TaskPriority::Low),
                due: Some(START + 5),
                clear_due: false,
            },
        )
        .unwrap();
    assert_eq!(edited.title, "Second, edited");
    assert_eq!(edited.priority, TaskPriority::Low);
    let command = edited.command.unwrap();
    assert_eq!(command.danger, DangerLevel::Dangerous);
    assert!(!command.spans.is_empty());
    let out = f
        .tasks
        .update(
            &me,
            &b.id,
            TaskChange::Output {
                output: "ok\r\n".into(),
            },
        )
        .unwrap();
    assert_eq!(out.result.unwrap().output.as_deref(), Some("ok\n"));
    let err = f
        .tasks
        .update(&me, "nope", TaskChange::StillNeeded)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
    assert!(
        lock(&f.audit)
            .iter()
            .filter(|(_, e, _)| e == events::UPDATE)
            .count()
            >= 6
    );
}

#[tokio::test]
async fn stale_blocking_and_due_reminders() {
    let f = fixture();
    f.tasks.configure(
        &Actor::local(),
        TaskSettings {
            stale_hours: 2,
            remind_blocking_minutes: 30,
            ..TaskSettings::default()
        },
    );
    let plain = f.manual("Plain");
    f.add_session("s1", SessionState::Idle);
    let (blocking, _) = f
        .tasks
        .request_human_action(
            Some("s1".into()),
            "mcp",
            HumanActionRequest {
                title: "Approve the login".into(),
                blocking: true,
                due: Some(json!(START + 10 * 60_000)),
                ..HumanActionRequest::default()
            },
        )
        .unwrap();
    let mut rx = f.notices.subscribe();
    assert!(f.tasks.tick(START + 60_000).is_empty());
    let r = f.tasks.tick(START + 11 * 60_000);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].kind, TaskReminderKind::Due);
    let r = f.tasks.tick(START + 31 * 60_000);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].kind, TaskReminderKind::Blocking);
    assert_eq!(r[0].task_id.as_deref(), Some(blocking.id.as_str()));
    assert!(
        f.tasks.tick(START + 40 * 60_000).is_empty(),
        "not again yet"
    );
    let r = f.tasks.tick(START + 2 * HOUR_MS + 1);
    let kinds: Vec<_> = r.iter().map(|x| (x.kind, x.task_id.clone())).collect();
    assert!(kinds.contains(&(TaskReminderKind::Stale, Some(plain.id.clone()))));
    assert!(f.tasks.get(&plain.id).unwrap().stale);
    // Stale is said once.
    let r = f.tasks.tick(START + 3 * HOUR_MS);
    assert!(r.iter().all(|x| x.kind != TaskReminderKind::Stale));
    // Reminders reach every client with the counts.
    let mut pushed = 0;
    while let Ok(msg) = rx.try_recv() {
        if let ServerMsg::TaskReminder { reminder } = msg {
            assert_eq!(reminder.open, 2);
            assert_eq!(reminder.blocking, 1);
            pushed += 1;
        }
    }
    assert!(pushed >= 4);
    // Done tasks are quiet.
    f.tasks
        .update(
            &Actor::local(),
            &blocking.id,
            TaskChange::Status {
                status: TaskStatus::Done,
            },
        )
        .unwrap();
    assert!(f.tasks.tick(START + 10 * HOUR_MS).is_empty());
}

// ------------------------------------------------------------ send result

#[tokio::test]
async fn send_result_composes_the_message_and_marks_done() {
    let f = fixture();
    f.add_session("s1", SessionState::AwaitingInput);
    let (task, _) = f
        .tasks
        .request_human_action(
            Some("s1".into()),
            "mcp",
            HumanActionRequest {
                title: "Run the installer as administrator".into(),
                command: Some("msiexec /i agent.msi".into()),
                needs_admin: true,
                ..HumanActionRequest::default()
            },
        )
        .unwrap();
    f.tasks
        .update(
            &Actor::local(),
            &task.id,
            TaskChange::Output {
                output: "Installation completed successfully.".into(),
            },
        )
        .unwrap();
    let draft = TaskResultDraft {
        text: "Installed; screenshot attached.".into(),
        attachments: vec![".vca/attachments/20301010-shot.png".into()],
        mentions: vec!["Attached file: @.vca/attachments/20301010-shot.png".into()],
        output: None,
    };
    // A preview sends nothing.
    let (_, preview, sent) = f
        .tasks
        .send_result(&Actor::local(), &task.id, draft.clone(), true)
        .await
        .unwrap();
    assert!(!sent);
    assert!(lock(&f.sent).is_empty());
    let (done, text, sent) = f
        .tasks
        .send_result(&Actor::local(), &task.id, draft, false)
        .await
        .unwrap();
    assert!(sent);
    assert_eq!(text, preview);
    assert_eq!(
        text,
        "Result of the task you asked me to do: \"Run the installer as administrator\"\n\n\
         Installed; screenshot attached.\n\n\
         Output:\n```\nInstallation completed successfully.\n```\n\n\
         Attached file: @.vca/attachments/20301010-shot.png"
    );
    let delivered = lock(&f.sent).clone();
    assert_eq!(delivered, [(SessionId::from("s1"), text.clone())]);
    assert_eq!(done.status, TaskStatus::Done);
    assert!(done.sent_to_session);
    assert_eq!(done.sent_at, Some(START));
    let result = done.result.unwrap();
    assert_eq!(result.attachments, [".vca/attachments/20301010-shot.png"]);
    assert_eq!(
        result.output.as_deref(),
        Some("Installation completed successfully.")
    );
    assert!(
        lock(&f.audit)
            .iter()
            .any(|(_, e, d)| e == events::SEND_RESULT && d.contains("attachments=1"))
    );

    // Bad input is refused before anything is typed.
    for bad in [
        TaskResultDraft::default(),
        TaskResultDraft {
            text: "x".into(),
            attachments: vec!["../../etc/passwd".into()],
            ..TaskResultDraft::default()
        },
        TaskResultDraft {
            text: "x".into(),
            mentions: vec!["rm -rf /".into()],
            ..TaskResultDraft::default()
        },
    ] {
        let err = f
            .tasks
            .send_result(&Actor::local(), &task.id, bad, false)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
    }
    // A task without a session has nowhere to go.
    let plain = f.manual("Plain");
    let err = f
        .tasks
        .send_result(
            &Actor::local(),
            &plain.id,
            TaskResultDraft {
                text: "x".into(),
                ..TaskResultDraft::default()
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    assert_eq!(lock(&f.sent).len(), 1);
}

// ------------------------------------------------------------- persistence

#[tokio::test]
async fn tasks_and_settings_survive_a_restart() {
    let f = fixture();
    let a = f.manual("Keep me");
    f.tasks
        .update(
            &Actor::local(),
            &a.id,
            TaskChange::Status {
                status: TaskStatus::Waiting,
            },
        )
        .unwrap();
    f.tasks.configure(
        &Actor::local(),
        TaskSettings {
            extract_llm: true,
            stale_hours: 3,
            ..TaskSettings::default()
        },
    );
    assert!(f.data.join(TASKS_FILE).exists());
    let reopened = open_tasks(
        &f.mgr,
        &f.data,
        &f.now,
        &f.launcher,
        &f.sent,
        broadcast::channel(4).0,
    );
    let ServerMsg::Tasks { tasks, settings } = reopened.list() else {
        panic!();
    };
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "Keep me");
    assert_eq!(tasks[0].status, TaskStatus::Waiting);
    assert!(settings.extract_llm);
    assert_eq!(settings.stale_hours, 3);
    // Dedupe keys survive too.
    let (_, created) = reopened
        .request_human_action(
            None,
            "mcp",
            HumanActionRequest {
                title: "Keep me".into(),
                ..HumanActionRequest::default()
            },
        )
        .unwrap();
    assert!(!created);
    // A broken file starts empty and is kept aside.
    std::fs::write(f.data.join(TASKS_FILE), b"{not json").unwrap();
    let broken = open_tasks(
        &f.mgr,
        &f.data,
        &f.now,
        &f.launcher,
        &f.sent,
        broadcast::channel(4).0,
    );
    assert!(matches!(broken.list(), ServerMsg::Tasks { tasks, .. } if tasks.is_empty()));
    assert!(f.data.join("tasks.json.bad").exists());
}

#[tokio::test]
async fn export_markdown_csv_and_digest() {
    let f = fixture();
    let a = f.manual("=cmd|' /C calc'!A0");
    f.manual("Sign in, then approve");
    f.tasks
        .update(
            &Actor::local(),
            &a.id,
            TaskChange::Status {
                status: TaskStatus::Done,
            },
        )
        .unwrap();
    let md = f.tasks.export(TaskExportFormat::Markdown, None);
    assert!(md.starts_with("# To-do list from agents (test-pc)"));
    assert!(md.contains("- [ ] Sign in, then approve"));
    assert!(md.contains("## Closed"));
    let csv = f.tasks.export(TaskExportFormat::Csv, None);
    assert!(csv.lines().next().unwrap().starts_with("id,status"));
    assert!(csv.contains("\"Sign in, then approve\""));
    assert!(csv.contains("'=cmd"), "formulas are defused");
    assert_eq!(f.tasks.digest(Some("ko")), "열린 할 일이 1개 있습니다.");
    assert_eq!(f.tasks.digest(Some("en")), "You have 1 open task.");
    f.tasks
        .update(
            &Actor::local(),
            &a.id,
            TaskChange::Status {
                status: TaskStatus::Open,
            },
        )
        .unwrap();
    assert_eq!(f.tasks.digest(Some("ja")), "未完了のタスクが2件あります。");
    // Per-session history.
    assert!(
        f.tasks
            .export(TaskExportFormat::Csv, Some(&SessionId::from("none")))
            .lines()
            .count()
            == 1
    );
}

// ------------------------------------------------------- admin shell, scopes

#[tokio::test]
async fn admin_shell_is_mocked_and_local_only() {
    let f = fixture();
    let task = f
        .tasks
        .create(
            &Actor::local(),
            TaskDraft {
                title: "Stop the spooler".into(),
                command: Some("net stop spooler".into()),
                needs_admin: true,
                ..TaskDraft::default()
            },
        )
        .unwrap();
    let remote = Actor {
        device_id: "dev_1".into(),
        device_name: "phone".into(),
    };
    let err = f
        .tasks
        .open_admin_shell(&remote, &task.id, false)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    assert!(lock(&f.launcher.opened).is_empty());
    f.tasks
        .open_admin_shell(&Actor::local(), &task.id, true)
        .unwrap();
    let opened = lock(&f.launcher.opened).clone();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].command, "net stop spooler");
    let plain = f.manual("No command");
    let err = f
        .tasks
        .open_admin_shell(&Actor::local(), &plain.id, true)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    let audit = lock(&f.audit).clone();
    assert!(
        audit
            .iter()
            .any(|(who, e, d)| who == "local" && e == events::ADMIN_SHELL && d.contains("opened"))
    );

    // The task travels in a file, never on the command line; the script
    // shows and copies the command and never runs it.
    let request = AdminShellRequest {
        task_id: "t".into(),
        title: "It's a \"test\"".into(),
        command: "echo 'hi'; Remove-Item x".into(),
        shell: TaskShell::Powershell,
    };
    let json: serde_json::Value = serde_json::from_str(&admin_task_json(&request)).unwrap();
    assert_eq!(json["title"], "It's a \"test\"");
    assert_eq!(json["command"], "echo 'hi'; Remove-Item x");
    let file = std::path::Path::new(r"C:\Users\O'Brien\vca\admin-shell\task-1.json");
    let script = admin_script(file);
    assert!(script.contains(r"-LiteralPath 'C:\Users\O''Brien\vca\admin-shell\task-1.json'"));
    assert!(script.contains("Write-Host $t.command"));
    assert!(script.contains("Set-Clipboard -Value $t.command"));
    assert!(
        !script.contains("Remove-Item"),
        "no task text in the script"
    );
    let lower = script.to_lowercase();
    for runner in [
        "invoke-expression",
        "iex ",
        "& {",
        "start-process",
        "-encodedcommand",
    ] {
        assert!(!lower.contains(runner), "{runner}");
    }
    let params = admin_shell_parameters(file).unwrap();
    assert!(params.starts_with("-NoExit -NoProfile -Command \"$Host.UI.RawUI.WindowTitle"));
    assert_eq!(params.matches('"').count(), 2);
    let lower = params.to_lowercase();
    for flag in [
        "-encodedcommand",
        "-executionpolicy",
        "-windowstyle",
        "hidden",
    ] {
        assert!(!lower.contains(flag), "{flag}");
    }
    // Task files get fresh names and left-over ones are cleaned up.
    let dir = tempfile::tempdir().unwrap();
    let first = write_admin_file(dir.path(), &request).unwrap();
    let second = write_admin_file(dir.path(), &request).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read_to_string(&first).unwrap(),
        admin_task_json(&request)
    );
    std::fs::write(dir.path().join("keep.txt"), "x").unwrap();
    remove_stale_admin_files(dir.path(), Duration::from_secs(3600));
    assert!(first.is_file() && second.is_file());
    remove_stale_admin_files(dir.path(), Duration::ZERO);
    assert!(!first.exists() && !second.exists());
    assert!(dir.path().join("keep.txt").is_file());
    // Test builds never fall back to the real launcher.
    let t = Tasks::new(manager(), broadcast::channel(4).0, TasksOptions::default());
    let task = t
        .create(
            &Actor::local(),
            TaskDraft {
                title: "x".into(),
                command: Some("whoami".into()),
                ..TaskDraft::default()
            },
        )
        .unwrap();
    let err = t
        .open_admin_shell(&Actor::local(), &task.id, true)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported);
}

async fn request(
    api: &Api,
    client: &Arc<crate::api::Client>,
    rx: &mut mpsc::Receiver<Envelope<ServerMsg>>,
    body: ClientMsg,
) -> ServerMsg {
    api.handle(client, Envelope { id: 7, body }).await;
    loop {
        let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if env.id == 7 {
            return env.body;
        }
    }
}

fn code(msg: &ServerMsg) -> Option<ErrorCode> {
    match msg {
        ServerMsg::Error { code, .. } => Some(*code),
        _ => None,
    }
}

#[tokio::test]
async fn scopes_through_the_api() {
    let f = fixture();
    let api = Api::new(f.mgr.clone(), "test-pc".into(), Duration::from_secs(60));
    api.set_tasks(f.tasks.clone());
    let task = f
        .tasks
        .create(
            &Actor::local(),
            TaskDraft {
                title: "Run it".into(),
                command: Some("whoami".into()),
                ..TaskDraft::default()
            },
        )
        .unwrap();
    let device = |scope| {
        Some(DeviceIdentity {
            device_id: "dev_1".into(),
            scope,
        })
    };
    let check = ClientMsg::TaskUpdate {
        task_id: task.id.clone(),
        change: TaskChange::Status {
            status: TaskStatus::Done,
        },
    };
    let send = ClientMsg::TaskSendResult {
        task_id: task.id.clone(),
        result: TaskResultDraft {
            text: "x".into(),
            ..TaskResultDraft::default()
        },
        preview: true,
    };
    let open = ClientMsg::TaskOpenAdminShell {
        task_id: task.id.clone(),
    };
    let configure = ClientMsg::TaskConfigure {
        settings: TaskSettings::default(),
    };

    // View: reads, never changes.
    let (viewer, mut vrx) = api.connect_as("remote", device(Scope::View));
    let list = request(&api, &viewer, &mut vrx, ClientMsg::TaskList).await;
    assert!(matches!(list, ServerMsg::Tasks { ref tasks, .. } if tasks.len() == 1));
    let digest = request(
        &api,
        &viewer,
        &mut vrx,
        ClientMsg::TaskDigest { language: None },
    )
    .await;
    assert!(matches!(digest, ServerMsg::Briefing { .. }));
    for msg in [check.clone(), send.clone(), open.clone(), configure.clone()] {
        let reply = request(&api, &viewer, &mut vrx, msg).await;
        assert_eq!(code(&reply), Some(ErrorCode::Forbidden), "{reply:?}");
    }
    // Command: checks off and sends results, but no admin shell.
    let (commander, mut crx) = api.connect_as("remote", device(Scope::Command));
    let reply = request(&api, &commander, &mut crx, send.clone()).await;
    assert!(
        matches!(reply, ServerMsg::TaskResultSent { sent: false, .. }),
        "{reply:?}"
    );
    let reply = request(&api, &commander, &mut crx, check.clone()).await;
    assert!(matches!(reply, ServerMsg::TaskSaved { ref task } if task.status == TaskStatus::Done));
    // Admin devices are still remote: no administrator shell, no settings.
    let (admin, mut arx) = api.connect_as("remote", device(Scope::Admin));
    for msg in [open.clone(), configure.clone()] {
        let reply = request(&api, &admin, &mut arx, msg).await;
        assert_eq!(code(&reply), Some(ErrorCode::Forbidden), "{reply:?}");
    }
    // The dev socket is not the PC's app either.
    let (dev, mut drx) = api.connect("devws");
    let reply = request(&api, &dev, &mut drx, open.clone()).await;
    assert_eq!(code(&reply), Some(ErrorCode::Forbidden));
    assert!(lock(&f.launcher.opened).is_empty());
    // The PC's own app.
    let (local, mut lrx) = api.connect("local");
    let reply = request(&api, &local, &mut lrx, open).await;
    assert_eq!(reply, ServerMsg::Ok);
    assert_eq!(lock(&f.launcher.opened).len(), 1);
    let reply = request(&api, &local, &mut lrx, configure).await;
    assert!(matches!(reply, ServerMsg::Tasks { .. }));
    // Welcome lists the feature.
    let welcome = request(
        &api,
        &local,
        &mut lrx,
        ClientMsg::Hello {
            protocol_version: vca_core::PROTOCOL_VERSION,
            app_version: "t".into(),
        },
    )
    .await;
    assert!(matches!(
        welcome,
        ServerMsg::Welcome { ref features, .. } if features.iter().any(|x| x == vca_core::task::FEATURE_HUMAN_TASKS)
    ));
}

#[test]
fn the_conductor_lists_the_task_tools_and_none_completes_a_task() {
    use crate::conductor::{ConductorCall, parse_call, tool_specs};
    let names: Vec<&str> = tool_specs().iter().map(|t| t.name).collect();
    assert!(names.contains(&mcp::REQUEST_HUMAN_ACTION));
    assert!(names.contains(&mcp::LIST_MY_REQUESTS));
    for name in &names {
        for word in ["complete", "done", "check_off", "send_result", "mark"] {
            assert!(!name.contains(word), "{name}");
        }
    }
    let call = parse_call(
        mcp::REQUEST_HUMAN_ACTION,
        &json!({"title": "Sign in", "steps": ["a", "b"], "blocking": true}),
    )
    .unwrap();
    assert!(matches!(
        call,
        ConductorCall::RequestHumanAction { blocking: true, .. }
    ));
    assert_eq!(call.name(), mcp::REQUEST_HUMAN_ACTION);
    assert!(
        call.acts(),
        "creating a task counts against the conductor's cap"
    );
    let list = parse_call(mcp::LIST_MY_REQUESTS, &json!({"all": true})).unwrap();
    assert!(!list.acts());
    assert!(
        list.returns_session_content(),
        "titles come from other sessions"
    );
    assert!(parse_call(mcp::REQUEST_HUMAN_ACTION, &json!({})).is_err());
}

#[test]
fn commands_show_invisible_characters_escaped() {
    // U+202E would display `rm -rf /` reversed; zero-width space hides text.
    let c = make_command("echo a\u{202E}fr- mr\u{200B}x", TaskShell::Powershell, true).unwrap();
    assert_eq!(c.text, "echo a<U+202E>fr- mr<U+200B>x");
    assert_eq!(visible_text("a\tb\nc"), "a\tb\nc");
    assert_eq!(visible_text("Bob\u{2019}s"), "Bob\u{2019}s");
}

#[test]
fn the_admin_script_quotes_typographic_apostrophes() {
    let script = admin_script(std::path::Path::new("C:/Users/Bob\u{2019}s/task.json"));
    assert!(
        script.contains("-LiteralPath 'C:/Users/Bob\u{2019}\u{2019}s/task.json' -Raw"),
        "{script}"
    );
}
