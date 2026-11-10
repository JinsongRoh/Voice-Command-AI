//! Conductor rules: opt-in, targets, confirmations, allowlist, caps, audit,
//! kill switches, wake-ups, instruction grants, folder setup and the
//! built-in loop (against a mocked OpenAI-compatible server). No real agent
//! or LLM is ever started.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ClientMsg, ConductorAgent, ConductorSettings, ConductorStatus, ControlTier,
    Decision, ServerMsg, SessionId, SessionInfo, SessionOrigin, SessionState, ShellInfo,
};
use vca_net::Scope;

use super::*;
use crate::api::{Api, DeviceIdentity};
use crate::events::LogLimits;
use crate::sessions::{ManagerConfig, Observed, ObservedIdentity, SessionManager};
use crate::workspace::WorkspaceRoots;

struct Fixture {
    _dir: tempfile::TempDir,
    data: PathBuf,
    api: Api,
    conductor: Conductor,
    now: Arc<AtomicU64>,
    kill: Arc<AtomicBool>,
}

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

fn fixture_with_config(config: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let config_file = dir.path().join("config.toml");
    std::fs::write(&config_file, config).unwrap();
    let mgr = manager();
    let api = Api::new(mgr.clone(), "test".into(), Duration::from_secs(60));
    let now = Arc::new(AtomicU64::new(1_000_000_000));
    let kill = Arc::new(AtomicBool::new(false));
    let (n, k) = (Arc::clone(&now), Arc::clone(&kill));
    let conductor = Conductor::new(
        mgr,
        api.notices(),
        ConductorOptions {
            data_dir: data.clone(),
            config_file,
            vca_program: Some(PathBuf::from("/opt/vca/vca")),
            clock: Some(Arc::new(move || n.load(Ordering::SeqCst))),
            kill_switch: Some(Arc::new(move || k.load(Ordering::SeqCst))),
        },
    );
    api.set_conductor(conductor.clone());
    Fixture {
        _dir: dir,
        data,
        api,
        conductor,
        now,
        kill,
    }
}

fn fixture() -> Fixture {
    fixture_with_config("")
}

fn info(id: &str, name: &str, agent: AgentKind, state: SessionState) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        origin: SessionOrigin::Local {
            agent,
            project_dir: PathBuf::from("/work/app"),
            pid: None,
        },
        display_name: name.into(),
        state,
        control: ControlTier::TerminalApi,
        profile: None,
        created_at: 0,
        last_activity: 999_999_000,
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

fn observed() -> Observed {
    Observed {
        terminal: None,
        identity: ObservedIdentity::default(),
    }
}

impl Fixture {
    fn add(&self, info: SessionInfo) {
        self.api.sessions().insert_observed(info, observed(), None);
    }

    fn add_agent(&self, id: &str, name: &str) {
        self.add(info(id, name, AgentKind::ClaudeCode, SessionState::Idle));
    }

    /// Enabled and started, the conductor's session being `conductor`.
    fn enable(&self, conductor: Option<&str>) {
        let mut st = lock(&self.conductor.inner.state);
        st.settings.enabled = true;
        st.started = true;
        st.session_id = conductor.map(SessionId::from);
    }

    fn settings(&self, f: impl FnOnce(&mut ConductorSettings)) {
        f(&mut lock(&self.conductor.inner.state).settings);
    }

    async fn exec(&self, call: ConductorCall) -> Result<Value, Error> {
        self.conductor.execute(&self.api, call).await
    }

    fn audit(&self) -> Vec<vca_core::AuditEntry> {
        self.conductor.inner.audit.recent(100)
    }

    fn advance(&self, ms: u64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

fn send(id: &str, text: &str) -> ConductorCall {
    ConductorCall::SendToSession {
        session_id: id.into(),
        text: text.into(),
    }
}

async fn wait_pending(c: &Conductor) -> String {
    for _ in 0..200 {
        if let Some(r) = c.state().pending.first() {
            return r.id.clone();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no conductor request appeared");
}

// ---------------------------------------------------------------- tools

#[test]
fn the_only_decision_tools_are_escalate_and_a_rule_bound_answer() {
    let names: Vec<&str> = tool_specs().iter().map(|t| t.name).collect();
    assert_eq!(names.len(), 15);
    for name in &names {
        if matches!(*name, "answer_decision" | "escalate_decision") {
            continue;
        }
        for word in [
            "decide",
            "decision_answer",
            "approve",
            "answer",
            "choose",
            "permission",
        ] {
            assert!(!name.contains(word), "{name} looks like a decision tool");
        }
    }
    assert!(names.contains(&"pending_decisions"));
    assert!(parse_call("approve_decision", &json!({})).is_err());
    assert!(parse_call("answer_decision", &json!({"session_id": "a"})).is_err());
    let answer = parse_call(
        "answer_decision",
        &json!({"session_id": "a", "option": "1"}),
    )
    .unwrap();
    assert!(answer.acts(), "answers are capped and audited");
    let escalate = parse_call(
        "escalate_decision",
        &json!({"session_id": "a", "summary": "needs you"}),
    )
    .unwrap();
    assert!(!escalate.acts(), "escalating is never capped");
    assert!(!escalate.returns_session_content());
    let spec = tool_specs()
        .into_iter()
        .find(|t| t.name == "answer_decision")
        .unwrap();
    assert!(spec.description.contains("allowed_answers"));
}

#[test]
fn calls_parse_and_results_are_labelled() {
    let call = parse_call(
        "send_to_session",
        &json!({"session_id": "s1", "text": "hi"}),
    )
    .unwrap();
    assert_eq!(call, send("s1", "hi"));
    assert!(call.acts());
    assert!(parse_call("send_to_session", &json!({"session_id": "s1"})).is_err());
    assert!(parse_call("list_sessions", &json!(5)).is_err());
    let v = label_result(&ConductorCall::ListSessions, json!({"sessions": []}));
    assert_eq!(v["notice"], UNTRUSTED_NOTICE);
    assert_eq!(v["untrusted_output"]["sessions"], json!([]));
    let v = label_result(
        &ConductorCall::Interrupt {
            session_id: "s".into(),
        },
        json!({"outcome": "sent"}),
    );
    assert_eq!(v["result"]["outcome"], "sent");
    assert!(v.get("untrusted_output").is_none());
    let tools = tools::openai_tools();
    assert_eq!(tools.len(), 15);
    assert_eq!(tools[0]["type"], "function");
}

// ---------------------------------------------------------------- opt-in

#[tokio::test]
async fn off_by_default_and_setup_is_local_only() {
    let f = fixture();
    assert!(!f.conductor.settings().enabled);
    assert_eq!(f.conductor.state().status, ConductorStatus::Off);
    let err = f.exec(ConductorCall::ListSessions).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);

    // A paired device, even with admin scope, cannot enable or start it.
    let (remote, _rx) = f.api.connect_as(
        "remote",
        Some(DeviceIdentity {
            device_id: "dev_1".into(),
            scope: Scope::Admin,
        }),
    );
    let on = ConductorSettings {
        enabled: true,
        ..ConductorSettings::default()
    };
    for msg in [
        ClientMsg::ConductorConfigure {
            settings: on.clone(),
        },
        ClientMsg::ConductorStart,
        ClientMsg::ConductorAllowSession {
            session_id: "s".into(),
            minutes: 5,
        },
    ] {
        let reply = f.conductor.handle_client(&f.api, &remote, msg).await;
        assert!(
            matches!(
                reply,
                ServerMsg::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{reply:?}"
        );
    }
    // The development WebSocket is not the desktop app either.
    let (dev, _rx) = f.api.connect("devws");
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &dev,
            ClientMsg::ConductorConfigure {
                settings: on.clone(),
            },
        )
        .await;
    assert!(matches!(
        reply,
        ServerMsg::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    assert!(!f.conductor.settings().enabled);
    // Remote devices may read the state.
    let reply = f
        .conductor
        .handle_client(&f.api, &remote, ClientMsg::ConductorGet)
        .await;
    assert!(matches!(reply, ServerMsg::Conductor { .. }));

    // The desktop app enables it; the choice is kept on disk.
    let (local, _rx) = f.api.connect("local");
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &local,
            ClientMsg::ConductorConfigure { settings: on },
        )
        .await;
    let ServerMsg::Conductor { state } = reply else {
        panic!("{reply:?}");
    };
    assert!(state.settings.enabled);
    let saved: ConductorSettings =
        serde_json::from_str(&std::fs::read_to_string(f.data.join(SETTINGS_FILE)).unwrap())
            .unwrap();
    assert!(saved.enabled);
    // Enabled but not started: tools are still refused.
    let err = f.exec(ConductorCall::ListSessions).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    // Scopes: viewing, commands and setup.
    assert!(Scope::View.permits(&ClientMsg::ConductorGet));
    assert!(!Scope::View.permits(&ClientMsg::ConductorAsk { text: "x".into() }));
    assert!(Scope::Command.permits(&ClientMsg::ConductorDecide {
        request_id: "r".into(),
        allow: true
    }));
    assert!(!Scope::Command.permits(&ClientMsg::ConductorStart));
}

// ---------------------------------------------------------------- reading

#[tokio::test]
async fn reads_skip_shells_and_itself_and_redact() {
    let f = fixture();
    let mut a = info("aaa", "api", AgentKind::ClaudeCode, SessionState::Idle);
    a.preview = Some("token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 done".into());
    f.add(a);
    let mut shell = info(
        "sh1",
        "shell",
        AgentKind::Shell("cmd".into()),
        SessionState::Idle,
    );
    shell.shell = Some(ShellInfo {
        idle_timeout_secs: 60,
        idle_deadline: 0,
        hard_deadline: 0,
        opened_by: "phone".into(),
        shell: "cmd".into(),
        project_dir: PathBuf::new(),
    });
    f.add(shell);
    f.add_agent("me", "conductor");
    let waiting = info(
        "www",
        "web",
        AgentKind::Codex,
        SessionState::AwaitingDecision,
    );
    f.add(waiting);
    let mgr = f.api.sessions();
    mgr.request_decision(
        &mgr.get(&"www".into()).unwrap(),
        crate::sessions::PendingDecision {
            decision: Decision {
                id: "d1".into(),
                prompt: "Run rm -rf build?".into(),
                options: Vec::new(),
                inferred: false,
                questions: Vec::new(),
                command: None,
            },
            submit: false,
            heuristic: false,
            at: 5,
            tool_use_id: None,
        },
    );
    f.enable(Some("me"));

    let list = f.exec(ConductorCall::ListSessions).await.unwrap();
    let ids: Vec<&str> = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "{list}");
    assert!(ids.contains(&"aaa") && ids.contains(&"www"));
    let text = list.to_string();
    assert!(
        !text.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
        "{text}"
    );

    let pending = f.exec(ConductorCall::PendingDecisions).await.unwrap();
    assert_eq!(pending["decisions"][0]["session_id"], "www");
    assert!(
        pending["note"]
            .as_str()
            .unwrap()
            .contains("Only the person")
    );

    let status = f
        .exec(ConductorCall::SessionStatus {
            session_id: "ww".into(),
        })
        .await
        .unwrap();
    assert_eq!(status["decision_prompt"], "Run rm -rf build?");
    let summary = f
        .exec(ConductorCall::SessionSummary {
            session_id: "aaa".into(),
            items: Some(3),
        })
        .await
        .unwrap();
    assert_eq!(summary["items"], json!([]));
    let brief = f
        .exec(ConductorCall::Briefing {
            language: Some("en".into()),
        })
        .await
        .unwrap();
    assert!(brief["text"].as_str().unwrap().len() > 3);
    let recent = f
        .exec(ConductorCall::RecentActivity {
            since_minutes: Some(5),
        })
        .await
        .unwrap();
    assert_eq!(recent["sessions"].as_array().unwrap().len(), 2);
    // Shells and the conductor are not visible by id either.
    for id in ["sh1", "me"] {
        let err = f
            .exec(ConductorCall::SessionStatus {
                session_id: id.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Forbidden);
    }
}

// ---------------------------------------------------------------- sends

#[tokio::test]
async fn sends_never_target_shells_or_itself() {
    let f = fixture();
    let mut shell = info(
        "sh1",
        "shell",
        AgentKind::Shell("cmd".into()),
        SessionState::Idle,
    );
    shell.shell = Some(ShellInfo {
        idle_timeout_secs: 60,
        idle_deadline: 0,
        hard_deadline: 0,
        opened_by: "phone".into(),
        shell: "cmd".into(),
        project_dir: PathBuf::new(),
    });
    f.add(shell);
    f.add_agent("me", "conductor");
    f.enable(Some("me"));
    for (id, why) in [("sh1", "remote shells"), ("me", "itself")] {
        let err = f.exec(send(id, "hello")).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Forbidden);
        assert!(error_text(&err).contains(why), "{err}");
    }
    assert!(f.conductor.state().pending.is_empty());
    // The allowlist refuses them too.
    let (local, _rx) = f.api.connect("local");
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &local,
            ClientMsg::ConductorAllowSession {
                session_id: "sh1".into(),
                minutes: 10,
            },
        )
        .await;
    assert!(matches!(
        reply,
        ServerMsg::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
}

#[tokio::test]
async fn sends_wait_for_the_person_allow() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    let (local, _rx) = f.api.connect("local");
    let task = {
        let (c, api) = (f.conductor.clone(), f.api.clone());
        tokio::spawn(async move { c.execute(&api, send("aaa", "run the tests")).await })
    };
    let request_id = wait_pending(&f.conductor).await;
    let state = f.conductor.state();
    assert_eq!(state.status, ConductorStatus::WaitingForYou);
    assert_eq!(state.pending[0].text, "run the tests");
    assert_eq!(state.pending[0].session_name, "api");
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &local,
            ClientMsg::ConductorDecide {
                request_id,
                allow: true,
            },
        )
        .await;
    assert!(matches!(reply, ServerMsg::Conductor { .. }));
    // Allowed: the text goes to the session (this observed test session
    // has no terminal, so delivery itself reports that).
    let err = task.await.unwrap().unwrap_err();
    assert!(error_text(&err).contains("no input channel"), "{err}");
    assert!(f.conductor.state().pending.is_empty());
    let decided = f
        .audit()
        .into_iter()
        .find(|e| e.event == events::DECIDE)
        .unwrap();
    assert_eq!(decided.device_id, "local");
}

#[tokio::test]
async fn sends_wait_for_the_person_deny() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    // A paired phone with command scope may answer.
    let (phone, _rx) = f.api.connect_as(
        "remote",
        Some(DeviceIdentity {
            device_id: "dev_9".into(),
            scope: Scope::Command,
        }),
    );
    let task = {
        let (c, api) = (f.conductor.clone(), f.api.clone());
        tokio::spawn(async move { c.execute(&api, send("aaa", "git push --force")).await })
    };
    let request_id = wait_pending(&f.conductor).await;
    f.conductor
        .handle_client(
            &f.api,
            &phone,
            ClientMsg::ConductorDecide {
                request_id: request_id.clone(),
                allow: false,
            },
        )
        .await;
    let out = task.await.unwrap().unwrap();
    assert_eq!(out["outcome"], "denied");
    assert!(out["advice"].as_str().unwrap().contains("Do not retry"));
    // Answering twice is refused.
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &phone,
            ClientMsg::ConductorDecide {
                request_id,
                allow: true,
            },
        )
        .await;
    assert!(matches!(
        reply,
        ServerMsg::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    let sent = f
        .audit()
        .into_iter()
        .find(|e| e.event == events::SEND)
        .unwrap();
    assert_eq!(sent.device_id, ORIGIN);
    assert!(sent.detail.contains("outcome=denied"), "{}", sent.detail);
}

#[tokio::test]
async fn sends_time_out_as_denied() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    // Below the configurable minimum, to keep the test short.
    f.settings(|s| s.confirm_timeout_secs = 1);
    let out = f.exec(send("aaa", "deploy")).await.unwrap();
    assert_eq!(out["outcome"], "timed_out");
    assert!(f.conductor.state().pending.is_empty());
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == events::SEND && e.detail.contains("outcome=timed_out"))
    );
}

#[tokio::test]
async fn allowlist_skips_confirmation_until_it_expires() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    let (local, _rx) = f.api.connect("local");
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &local,
            ClientMsg::ConductorAllowSession {
                session_id: "aaa".into(),
                minutes: 10,
            },
        )
        .await;
    let ServerMsg::Conductor { state } = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(state.allowlist.len(), 1);
    // Allowlisted: delivered at once, no pending request.
    let err = f.exec(send("aaa", "status?")).await.unwrap_err();
    assert!(error_text(&err).contains("no input channel"));
    assert!(f.conductor.state().pending.is_empty());

    // Expired: confirmation again.
    f.advance(10 * 60_000 + 1);
    assert!(f.conductor.state().allowlist.is_empty());
    let task = {
        let (c, api) = (f.conductor.clone(), f.api.clone());
        tokio::spawn(async move { c.execute(&api, send("aaa", "status?")).await })
    };
    let id = wait_pending(&f.conductor).await;
    f.conductor.decide(&id, false, ("local", "local")).unwrap();
    assert_eq!(task.await.unwrap().unwrap()["outcome"], "denied");
}

// ---------------------------------------------------------------- caps

#[tokio::test]
async fn hourly_action_cap() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    f.settings(|s| s.max_actions_per_hour = 2);
    let interrupt = || ConductorCall::Interrupt {
        session_id: "aaa".into(),
    };
    // Observed test sessions refuse keys, but each attempt counts.
    for _ in 0..2 {
        let err = f.exec(interrupt()).await.unwrap_err();
        assert_ne!(err.code(), ErrorCode::Unavailable, "{err}");
    }
    let err = f.exec(interrupt()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);
    assert!(error_text(&err).contains("hourly"));
    // Reading is not capped.
    f.exec(ConductorCall::ListSessions).await.unwrap();
    assert_eq!(f.conductor.state().actions_last_hour, 2);
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == events::REFUSED && e.detail.contains("hourly"))
    );
    // An hour later it works again.
    f.advance(HOUR_MS);
    let err = f.exec(interrupt()).await.unwrap_err();
    assert_ne!(err.code(), ErrorCode::Unavailable);
}

#[tokio::test]
async fn pending_requests_are_capped() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    let mut tasks = Vec::new();
    for i in 0..MAX_PENDING {
        let (c, api) = (f.conductor.clone(), f.api.clone());
        tasks.push(tokio::spawn(async move {
            c.execute(&api, send("aaa", &format!("n{i}"))).await
        }));
    }
    for _ in 0..200 {
        if f.conductor.state().pending.len() == MAX_PENDING {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let err = f.exec(send("aaa", "one more")).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);
    f.conductor.stop(false, ("local", "local")).await;
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap()["outcome"], "denied");
    }
}

// ---------------------------------------------------------------- kill

#[tokio::test]
async fn kill_switch_revokes_everything() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    std::fs::create_dir_all(f.data.join(setup::CONDUCTOR_DIR)).unwrap();
    let mcp = f
        .data
        .join(setup::CONDUCTOR_DIR)
        .join(setup::CLAUDE_MCP_FILE);
    std::fs::write(&mcp, "{}").unwrap();
    f.add_agent("ccc", "docs");
    lock(&f.conductor.inner.state)
        .allowlist
        .push(vca_core::ConductorAllow {
            session_id: "ccc".into(),
            expires_at: u64::MAX,
        });
    let task = {
        let (c, api) = (f.conductor.clone(), f.api.clone());
        tokio::spawn(async move { c.execute(&api, send("aaa", "x")).await })
    };
    wait_pending(&f.conductor).await;

    // A paired device needs the owner's code for the kill switch: without
    // a security service to check one, it is refused and nothing changes.
    let (phone, _rx) = f.api.connect_as(
        "remote",
        Some(DeviceIdentity {
            device_id: "dev_2".into(),
            scope: Scope::Command,
        }),
    );
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &phone,
            ClientMsg::ConductorStop {
                kill: true,
                code: Some("123456".into()),
            },
        )
        .await;
    assert!(
        matches!(
            reply,
            ServerMsg::Error {
                code: ErrorCode::Unavailable,
                ..
            }
        ),
        "{reply:?}"
    );
    assert_eq!(f.conductor.state().pending.len(), 1, "nothing was revoked");
    // On the PC (the local channel verified the person before this).
    let (pc, _rx) = f.api.connect_as("local", None);
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &pc,
            ClientMsg::ConductorStop {
                kill: true,
                code: None,
            },
        )
        .await;
    let ServerMsg::Conductor { state } = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(state.status, ConductorStatus::Revoked);
    assert!(state.pending.is_empty() && state.allowlist.is_empty());
    assert_eq!(task.await.unwrap().unwrap()["outcome"], "denied");
    assert!(!mcp.exists(), "the MCP config is removed");
    let err = f.exec(ConductorCall::ListSessions).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    // Actions are refused and the refusal is audited.
    let err = f
        .exec(ConductorCall::Interrupt {
            session_id: "aaa".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    let kill = f
        .audit()
        .into_iter()
        .find(|e| e.event == events::KILL)
        .unwrap();
    assert_eq!(kill.device_id, "local");
    // The refused kill from the phone is on record too.
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == events::REFUSED && e.device_id == "dev_2")
    );
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == events::REFUSED && e.device_id == ORIGIN)
    );
}

#[tokio::test]
async fn global_kill_switch_blocks_the_conductor() {
    let f = fixture();
    f.add_agent("aaa", "api");
    f.enable(None);
    f.exec(ConductorCall::ListSessions).await.unwrap();
    f.kill.store(true, Ordering::SeqCst);
    let err = f.exec(ConductorCall::ListSessions).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    assert_eq!(f.conductor.state().status, ConductorStatus::Revoked);
    assert!(f.conductor.state().kill_switch);
    let (local, _rx) = f.api.connect("local");
    let reply = f
        .conductor
        .handle_client(&f.api, &local, ClientMsg::ConductorStart)
        .await;
    assert!(matches!(
        reply,
        ServerMsg::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
}

// ---------------------------------------------------------------- wake-ups

#[tokio::test]
async fn a_removed_session_is_forgotten() {
    let f = fixture();
    f.add_agent("aaa", "api");
    let running = info("aaa", "api", AgentKind::ClaudeCode, SessionState::Running);
    f.conductor.observe(&running);
    assert_eq!(lock(&f.conductor.inner.state).seen.len(), 1);
    f.conductor.forget(&"aaa".into());
    assert!(lock(&f.conductor.inner.state).seen.is_empty());
}

#[tokio::test]
async fn wake_ups_are_batched_rate_limited_and_quiet_while_typing() {
    let f = fixture();
    f.add(info(
        "me",
        "conductor",
        AgentKind::ClaudeCode,
        SessionState::AwaitingInput,
    ));
    f.add_agent("aaa", "api");
    f.add_agent("bbb", "web");
    f.enable(Some("me"));
    let running = info("aaa", "api", AgentKind::ClaudeCode, SessionState::Running);
    let idle = info("aaa", "api", AgentKind::ClaudeCode, SessionState::Idle);

    // Off by default: nothing is collected.
    f.conductor.observe(&running);
    f.conductor.observe(&idle);
    assert!(!f.conductor.flush_wakes(&f.api).await);

    f.settings(|s| {
        s.wake_events = true;
        s.max_wakes_per_hour = 2;
    });
    f.conductor.observe(&running);
    f.conductor.observe(&idle);
    f.conductor.observe(&info(
        "bbb",
        "web",
        AgentKind::Codex,
        SessionState::Error {
            message: "boom".into(),
        },
    ));
    // The person typed to the conductor a moment ago: wait.
    f.conductor.note_input(&"me".into());
    assert!(!f.conductor.flush_wakes(&f.api).await);
    f.advance(QUIET_AFTER_INPUT_MS);
    assert!(f.conductor.flush_wakes(&f.api).await);
    let chat = f.conductor.state().chat;
    let event = chat.iter().find(|t| t.role == "event").unwrap();
    assert!(
        event.text.contains("api: finished a turn"),
        "{}",
        event.text
    );
    assert!(event.text.contains("error (boom)"));

    // At most one per two minutes.
    f.conductor.observe(&running);
    f.conductor.observe(&idle);
    f.advance(60_000);
    assert!(!f.conductor.flush_wakes(&f.api).await);
    f.advance(60_000);
    assert!(f.conductor.flush_wakes(&f.api).await);
    // And at most N per hour.
    f.conductor.observe(&running);
    f.conductor.observe(&idle);
    f.advance(CONDUCTOR_MIN_WAKE_GAP_SECS * 1000);
    assert!(!f.conductor.flush_wakes(&f.api).await);
    assert_eq!(f.conductor.state().wakes_last_hour, 2);
    assert_eq!(
        f.audit().iter().filter(|e| e.event == events::WAKE).count(),
        2
    );
    // A wake is audited as counts: its text quotes what sessions said.
    for wake in f.audit().iter().filter(|e| e.event == events::WAKE) {
        assert!(wake.detail.starts_with("events="), "{}", wake.detail);
        assert!(wake.detail.contains(" chars="), "{}", wake.detail);
        assert!(!wake.detail.contains("VCA events"), "{}", wake.detail);
    }
}

#[tokio::test]
async fn the_conductor_never_wakes_on_its_own_events() {
    let f = fixture();
    f.add(info(
        "me",
        "Conductor",
        AgentKind::ClaudeCode,
        SessionState::AwaitingInput,
    ));
    // An earlier conductor session, still listed.
    let mut old = info(
        "old",
        "Conductor",
        AgentKind::ClaudeCode,
        SessionState::Idle,
    );
    old.conductor = true;
    f.add(old.clone());
    f.add_agent("aaa", "api");
    f.enable(Some("me"));
    f.settings(|s| s.wake_events = true);
    f.advance(QUIET_AFTER_INPUT_MS);

    // The conductor's own turns, decisions and errors: nothing is batched.
    let me = |state| info("me", "Conductor", AgentKind::ClaudeCode, state);
    f.conductor.observe(&me(SessionState::Running));
    f.conductor.observe(&me(SessionState::AwaitingInput));
    f.conductor.observe(&me(SessionState::AwaitingDecision));
    f.conductor.observe(&me(SessionState::Error {
        message: "boom".into(),
    }));
    f.conductor.observe(&me(SessionState::AwaitingInput));
    for state in [
        SessionState::Running,
        SessionState::Idle,
        SessionState::AwaitingDecision,
    ] {
        f.conductor.observe(&SessionInfo {
            state,
            ..old.clone()
        });
    }
    assert!(lock(&f.conductor.inner.state).batch.is_empty());
    assert!(!f.conductor.flush_wakes(&f.api).await);
    // Not a target either.
    assert!(f.exec(send("old", "hi")).await.is_err());
    let list = f.exec(ConductorCall::ListSessions).await.unwrap();
    let ids: Vec<&str> = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["aaa"]);

    // Other sessions still wake it.
    f.conductor.observe(&info(
        "aaa",
        "api",
        AgentKind::ClaudeCode,
        SessionState::Running,
    ));
    f.conductor.observe(&info(
        "aaa",
        "api",
        AgentKind::ClaudeCode,
        SessionState::Idle,
    ));
    assert!(f.conductor.flush_wakes(&f.api).await);
    let chat = f.conductor.state().chat;
    let event = chat.iter().find(|t| t.role == "event").unwrap();
    assert!(event.text.contains("api: finished a turn"));
    assert!(!event.text.contains("Conductor"), "{}", event.text);
}

// ---------------------------------------------------------------- first run

/// Claude Code's first-run question in the conductor folder (fake screen).
const TRUST_SCREEN: &str = "\x1b[2J Accessing workspace:\n\n C:\\data\\conductor\n\n \
    Quick safety check: Is this a project you created or one you trust? (Like your own\n \
    code, a well-known open source project, or work from your team).\n\n \
    \x1b[36m❯ 1. Yes, I trust this folder\x1b[0m\n   2. No, exit\n\n \
    Enter to confirm · Esc to cancel\n";

#[test]
fn enter_trusts_only_the_trust_question_on_yes() {
    assert!(check_trust_screen(TRUST_SCREEN).is_ok());
    let old = "Do you trust the files in this folder?\n\n❯ 1. Yes, proceed\n  2. No, exit\n";
    assert!(check_trust_screen(old).is_ok());
    // The cursor on "No".
    let moved = TRUST_SCREEN
        .replace("❯ 1. Yes", "  1. Yes")
        .replace("  2. No, exit", "❯ 2. No, exit");
    let err = check_trust_screen(&moved).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Conflict);
    // Any other menu or an idle prompt: refused, nothing is pressed.
    for screen in [
        "Choose the text style\n\n❯ 1. Dark mode\n  2. Light mode\n",
        "Do you want to proceed?\n❯ 1. Yes\n  2. No\n",
        "╭───╮\n│ > │\n╰───╯\n",
        "",
    ] {
        assert_eq!(
            check_trust_screen(screen).unwrap_err().code(),
            ErrorCode::Conflict,
            "{screen}"
        );
    }
}

#[tokio::test]
async fn trusting_needs_a_running_conductor_with_a_screen() {
    let f = fixture();
    let (local, _rx) = f.api.connect("local");
    // Not running.
    let reply = f
        .conductor
        .handle_client(&f.api, &local, ClientMsg::ConductorTrustFolder)
        .await;
    assert!(
        matches!(
            reply,
            ServerMsg::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ),
        "{reply:?}"
    );
    // A session without a terminal screen (observed): nothing to answer.
    f.add(info(
        "me",
        "Conductor",
        AgentKind::ClaudeCode,
        SessionState::AwaitingInput,
    ));
    f.enable(Some("me"));
    let reply = f
        .conductor
        .handle_client(&f.api, &local, ClientMsg::ConductorTrustFolder)
        .await;
    assert!(
        matches!(
            reply,
            ServerMsg::Error {
                code: ErrorCode::Unavailable,
                ..
            }
        ),
        "{reply:?}"
    );
    assert!(f.audit().iter().all(|e| e.event != events::TRUST));
    // A command-scope phone may answer it, like pressing Enter in the terminal.
    assert!(Scope::Command.permits(&ClientMsg::ConductorTrustFolder));
    assert!(!Scope::View.permits(&ClientMsg::ConductorTrustFolder));
}

// ---------------------------------------------------------------- setup

#[test]
fn folder_and_mcp_config_per_agent() {
    let dir = tempfile::tempdir().unwrap();
    let vca = Path::new(r"C:\Program Files\VCA\vca.exe");

    let claude = setup::prepare(dir.path(), ConductorAgent::ClaudeCode, vca, "run-token").unwrap();
    let folder = dir.path().join(setup::CONDUCTOR_DIR);
    assert_eq!(claude.dir, folder);
    assert_eq!(claude.program, "claude");
    let config = folder.join(setup::CLAUDE_MCP_FILE);
    assert_eq!(
        claude.args,
        [
            "--strict-mcp-config".to_owned(),
            "--mcp-config".to_owned(),
            config.to_string_lossy().into_owned()
        ]
    );
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        json["mcpServers"]["vca"]["command"],
        vca.to_string_lossy().as_ref()
    );
    assert_eq!(json["mcpServers"]["vca"]["args"], json!(["mcp"]));
    let md = std::fs::read_to_string(folder.join("CLAUDE.md")).unwrap();
    assert!(md.contains("untrusted") && md.contains("escalate_decision"));
    assert!(md.contains("allowed_answers"));

    let codex = setup::prepare(dir.path(), ConductorAgent::Codex, vca, "run-token").unwrap();
    assert_eq!(codex.program, "codex");
    assert_eq!(
        codex.args[1],
        r"mcp_servers.vca.command='C:\Program Files\VCA\vca.exe'"
    );
    assert_eq!(codex.args[3], "mcp_servers.vca.args=['mcp']");
    // The override parses as TOML.
    let parsed: toml::Value = toml::from_str(&codex.args[1].replacen('=', " = ", 1)).unwrap();
    assert_eq!(
        parsed["mcp_servers"]["vca"]["command"].as_str(),
        Some(r"C:\Program Files\VCA\vca.exe")
    );
    assert!(folder.join("AGENTS.md").is_file());
    assert!(
        setup::prepare(
            dir.path(),
            ConductorAgent::Codex,
            Path::new("a'b"),
            "run-token"
        )
        .is_err()
    );

    let gemini = setup::prepare(dir.path(), ConductorAgent::GeminiCli, vca, "run-token").unwrap();
    assert_eq!(gemini.program, "gemini");
    assert_eq!(gemini.args, ["--allowed-mcp-server-names", "vca"]);
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join(".gemini").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["mcpServers"]["vca"]["args"], json!(["mcp"]));
    assert!(folder.join("GEMINI.md").is_file());

    assert!(setup::prepare(dir.path(), ConductorAgent::Builtin, vca, "run-token").is_err());
    // Everything stays inside the conductor folder.
    for plan in [claude, codex, gemini] {
        for file in plan.files {
            assert!(folder.join(&file).starts_with(&folder));
            assert!(file.is_relative());
        }
    }
    assert!(setup::experimental(ConductorAgent::Codex));
    assert!(!setup::experimental(ConductorAgent::ClaudeCode));
    setup::revoke(dir.path());
    assert!(!config.exists());
}

// ---------------------------------------------------------------- built-in

fn llm_config(url: &str) -> String {
    format!(
        "[[speech.llms]]\nid = \"local-llm\"\nkind = \"openai_compatible\"\n\
         base_url = \"{url}/v1\"\nmodel = \"tiny-tools\"\n"
    )
}

async fn builtin_fixture(server: &wiremock::MockServer) -> Fixture {
    let f = fixture_with_config(&llm_config(&server.uri()));
    f.add_agent("aaa", "api");
    f.settings(|s| {
        s.enabled = true;
        s.agent = ConductorAgent::Builtin;
        s.builtin_llm = Some("local-llm".into());
    });
    let (local, _rx) = f.api.connect("local");
    let reply = f
        .conductor
        .handle_client(&f.api, &local, ClientMsg::ConductorStart)
        .await;
    let ServerMsg::Conductor { state } = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(state.status, ConductorStatus::Ready);
    assert_eq!(state.model.as_deref(), Some("tiny-tools"));
    assert_eq!(state.llms.len(), 1);
    f
}

fn tool_call(name: &str, args: Value) -> Value {
    json!({"choices": [{"message": {"role": "assistant", "content": null,
        "tool_calls": [{"id": "call_1", "type": "function",
            "function": {"name": name, "arguments": args.to_string()}}]}}]})
}

fn final_answer(text: &str) -> Value {
    json!({"choices": [{"message": {"role": "assistant", "content": text}}]})
}

async fn wait_reply(f: &Fixture) -> String {
    for _ in 0..400 {
        if let Some(t) = f
            .conductor
            .state()
            .chat
            .iter()
            .find(|t| t.role == "assistant")
        {
            return t.text.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no reply");
}

async fn ask(f: &Fixture, text: &str) -> ServerMsg {
    let (local, _rx) = f.api.connect("local");
    f.conductor
        .handle_client(
            &f.api,
            &local,
            ClientMsg::ConductorAsk { text: text.into() },
        )
        .await
}

#[tokio::test]
async fn builtin_loop_calls_a_tool_then_answers() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Second request: it carries the labelled tool result.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("tool_call_id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("One session: api.")))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(tool_call("list_sessions", json!({}))),
        )
        .with_priority(2)
        .mount(&server)
        .await;
    let f = builtin_fixture(&server).await;
    let mut notices = f.api.notices().subscribe();
    let reply = ask(&f, "what is running?").await;
    assert!(matches!(reply, ServerMsg::Conductor { .. }));
    assert_eq!(wait_reply(&f).await, "One session: api.");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(first["model"], "tiny-tools");
    assert!(first["max_tokens"].as_u64().unwrap() <= 8192);
    let names: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 15);
    assert!(
        first["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("untrusted")
    );
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let tool_msg = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert_eq!(tool_msg["tool_call_id"], "call_1");
    let content: Value = serde_json::from_str(tool_msg["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["notice"], UNTRUSTED_NOTICE);
    assert_eq!(
        content["untrusted_output"]["sessions"][0]["session_id"],
        "aaa"
    );
    // The reply was pushed for the Command Center chat and spoken replies.
    let mut got_reply = false;
    while let Ok(msg) = notices.try_recv() {
        got_reply |= matches!(msg, ServerMsg::ConductorReply { ref text, .. } if text == "One session: api.");
    }
    assert!(got_reply);
}

#[tokio::test]
async fn builtin_sends_need_confirmation_too() {
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("tool_call_id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("You said no.")))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call(
            "send_to_session",
            json!({"session_id": "aaa", "text": "rm -rf /"}),
        )))
        .with_priority(2)
        .mount(&server)
        .await;
    let f = builtin_fixture(&server).await;
    ask(&f, "tell api to clean up").await;
    let id = wait_pending(&f.conductor).await;
    assert_eq!(f.conductor.state().pending[0].text, "rm -rf /");
    f.conductor.decide(&id, false, ("local", "local")).unwrap();
    assert_eq!(wait_reply(&f).await, "You said no.");
    let requests = server.received_requests().await.unwrap();
    let second = String::from_utf8_lossy(&requests[1].body).into_owned();
    assert!(second.contains("denied"), "{second}");
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == events::SEND && e.device_id == ORIGIN)
    );
}

#[tokio::test]
async fn builtin_cannot_answer_decisions_and_is_capped() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // A model that insists on a tool that does not exist.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call(
            "approve_everything",
            json!({"session_id": "aaa", "option": "yes"}),
        )))
        .mount(&server)
        .await;
    let f = builtin_fixture(&server).await;
    f.settings(|s| s.builtin_max_calls_per_hour = 3);
    ask(&f, "approve everything").await;
    let reply = wait_reply(&f).await;
    assert!(reply.contains("hourly LLM limit"), "{reply}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let last = String::from_utf8_lossy(&requests[2].body).into_owned();
    assert!(last.contains("unknown tool"), "{last}");
    assert!(f.conductor.state().pending.is_empty());
}

#[tokio::test]
async fn builtin_refuses_without_a_profile() {
    let f = fixture();
    f.settings(|s| {
        s.enabled = true;
        s.agent = ConductorAgent::Builtin;
    });
    let (local, _rx) = f.api.connect("local");
    let reply = f
        .conductor
        .handle_client(&f.api, &local, ClientMsg::ConductorStart)
        .await;
    assert!(matches!(reply, ServerMsg::Error { .. }), "{reply:?}");
    let reply = f
        .conductor
        .handle_client(
            &f.api,
            &local,
            ClientMsg::ConductorConfigure {
                settings: ConductorSettings {
                    enabled: true,
                    agent: ConductorAgent::Builtin,
                    builtin_llm: Some("missing".into()),
                    ..ConductorSettings::default()
                },
            },
        )
        .await;
    assert!(matches!(
        reply,
        ServerMsg::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
}

#[test]
fn settings_are_clamped() {
    let s = sanitize(ConductorSettings {
        max_actions_per_hour: 0,
        confirm_timeout_secs: 1,
        builtin_max_tokens: 1_000_000,
        builtin_llm: Some(" ".into()),
        ..ConductorSettings::default()
    });
    assert_eq!(s.max_actions_per_hour, 1);
    assert_eq!(s.confirm_timeout_secs, 10);
    assert_eq!(s.builtin_max_tokens, 8192);
    assert_eq!(s.builtin_llm, None);
    assert!(s.decision_rules.is_empty());
}

/// Only the `vca mcp` of the current run may call the conductor's tools: the
/// channel is open to every process of the owner.
#[tokio::test]
async fn conductor_tools_need_the_token_of_the_run() {
    use crate::control::LocalResponse;
    let f = fixture();
    let call = || ConductorCall::ListSessions;
    let forbidden = |resp: LocalResponse| match resp {
        LocalResponse::Error { code, .. } => assert_eq!(code, ErrorCode::Forbidden),
        other => panic!("{other:?}"),
    };
    forbidden(local_call(&f.api, 1, None, call()).await);
    forbidden(local_call(&f.api, 1, Some("guess"), call()).await);
    lock(&f.conductor.inner.state).mcp_token = Some(mcp_token_hash("run-token"));
    forbidden(local_call(&f.api, 1, None, call()).await);
    forbidden(local_call(&f.api, 1, Some("guess"), call()).await);
    let resp = local_call(&f.api, 1, Some("run-token"), call()).await;
    assert!(
        !matches!(
            resp,
            LocalResponse::Error {
                code: ErrorCode::Forbidden,
                ref message,
                ..
            } if message.contains("vca mcp")
        ),
        "{resp:?}"
    );
}

/// A normal stop (or switching to the built-in conductor) ends the run:
/// its token stops working and the files holding it go, not only after
/// the kill switch.
#[tokio::test]
async fn a_normal_stop_retires_the_run_token() {
    let f = fixture();
    let plan = setup::prepare(
        f.conductor.data_dir(),
        ConductorAgent::ClaudeCode,
        Path::new("/bin/vca"),
        "run-token",
    )
    .unwrap();
    let mcp_file = plan.dir.join(setup::CLAUDE_MCP_FILE);
    assert!(mcp_file.is_file());
    lock(&f.conductor.inner.state).mcp_token = Some(mcp_token_hash("run-token"));
    assert!(f.conductor.accepts_mcp_token(Some("run-token")));
    f.conductor.stop(false, ("local", "local")).await;
    assert!(!f.conductor.accepts_mcp_token(Some("run-token")));
    assert!(!mcp_file.exists());
}

/// The file with the run's token is owner-only, also when an earlier run
/// left one with a wider mode.
#[cfg(unix)]
#[test]
fn the_mcp_config_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let conductor_dir = dir.path().join(setup::CONDUCTOR_DIR);
    std::fs::create_dir_all(&conductor_dir).unwrap();
    let file = conductor_dir.join(setup::CLAUDE_MCP_FILE);
    std::fs::write(&file, "old").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    setup::prepare(
        dir.path(),
        ConductorAgent::ClaudeCode,
        Path::new("/bin/vca"),
        "run-token",
    )
    .unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("run-token")
    );
}

#[test]
fn the_run_token_goes_into_the_mcp_config_only() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup::prepare(
        dir.path(),
        ConductorAgent::ClaudeCode,
        Path::new("/bin/vca"),
        "run-token-1",
    )
    .unwrap();
    assert!(!plan.args.iter().any(|a| a.contains("run-token-1")));
    let config = dir
        .path()
        .join(setup::CONDUCTOR_DIR)
        .join(setup::CLAUDE_MCP_FILE);
    let text = std::fs::read_to_string(config).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json["mcpServers"]["vca"]["env"][crate::control::CONDUCTOR_TOKEN_ENV],
        "run-token-1"
    );
}

#[test]
fn held_events_go_in_front_of_newer_ones() {
    let f = fixture();
    lock(&f.conductor.inner.state).batch = vec!["new".to_owned()];
    f.conductor.hold_wake(vec!["old".to_owned()]);
    assert_eq!(lock(&f.conductor.inner.state).batch, ["old", "new"]);
}

#[tokio::test]
async fn a_conductor_without_hook_events_reports_missing_hooks() {
    let f = fixture();
    f.add(info(
        "c1",
        "Conductor",
        AgentKind::ClaudeCode,
        SessionState::Idle,
    ));
    f.enable(Some("c1"));
    // Started long ago (created at 0) and never sent a hook event.
    assert_eq!(
        f.conductor.state().problem.as_deref(),
        Some(vca_core::CONDUCTOR_PROBLEM_HOOKS_MISSING)
    );
    let json = serde_json::to_value(f.conductor.state()).unwrap();
    assert_eq!(json["problem"], "hooks_missing");
    // One hook event and the problem is gone.
    let session = f.api.sessions().get(&SessionId::from("c1")).unwrap();
    session.hook_seen.store(true, Ordering::SeqCst);
    assert_eq!(f.conductor.state().problem, None);
    // Not started: nothing to report.
    session.hook_seen.store(false, Ordering::SeqCst);
    lock(&f.conductor.inner.state).started = false;
    assert_eq!(f.conductor.state().problem, None);
}

#[tokio::test]
async fn a_fresh_conductor_session_gets_time_for_its_first_hook() {
    let f = fixture();
    let mut fresh = info("c2", "Conductor", AgentKind::ClaudeCode, SessionState::Idle);
    fresh.created_at = f.now.load(Ordering::SeqCst) - 1_000;
    f.add(fresh);
    f.enable(Some("c2"));
    assert_eq!(f.conductor.state().problem, None);
    f.advance(HOOKS_GRACE_MS);
    assert_eq!(
        f.conductor.state().problem.as_deref(),
        Some("hooks_missing")
    );
}

#[test]
fn a_claude_conductor_can_bring_its_own_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let vca = Path::new("/opt/vca/vca");
    let mut plan =
        setup::prepare(dir.path(), ConductorAgent::ClaudeCode, vca, "run-token").unwrap();
    setup::add_own_hooks(&mut plan, vca).unwrap();
    let file = plan.dir.join(setup::CLAUDE_HOOKS_FILE);
    let n = plan.args.len();
    assert_eq!(plan.args[n - 2], "--settings");
    assert_eq!(plan.args[n - 1], file.to_string_lossy());
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for event in ["SessionStart", "Stop", "PermissionRequest", "Notification"] {
        let command = json["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(command.contains(" hook claude "), "{event}: {command}");
        assert!(command.contains("/opt/vca/vca"), "{event}: {command}");
    }
    // No token in the hooks file or on the command line.
    assert!(
        !std::fs::read_to_string(&file)
            .unwrap()
            .contains("run-token")
    );
    assert!(!plan.args.iter().any(|a| a.contains("run-token")));
    // User settings without VCA hooks are incomplete; with them, complete.
    let config = dir.path().join("claude");
    std::fs::create_dir_all(&config).unwrap();
    assert!(!setup::user_hooks_complete(&config, Path::new("vca")));
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&setup::claude_hooks_json(Path::new("vca"))).unwrap(),
    )
    .unwrap();
    assert!(setup::user_hooks_complete(&config, Path::new("vca")));
}

#[test]
fn user_hooks_of_a_stale_vca_do_not_count() {
    // Temporary config dirs only; never the real `~/.claude`.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("claude");
    std::fs::create_dir_all(&config).unwrap();
    let installed = dir.path().join("app").join("vca");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, "").unwrap();
    let old = dir.path().join("old-version").join("vca");
    let settings = config.join("settings.json");

    // Hooks of an older install in a folder that no longer exists.
    std::fs::write(
        &settings,
        serde_json::to_string(&setup::claude_hooks_json(&old)).unwrap(),
    )
    .unwrap();
    let before = std::fs::read_to_string(&settings).unwrap();
    assert!(!setup::user_hooks_complete(&config, &installed));
    // Read-only: nothing rewritten.
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);

    // Hooks of the installed vca count.
    std::fs::write(
        &settings,
        serde_json::to_string(&setup::claude_hooks_json(&installed)).unwrap(),
    )
    .unwrap();
    assert!(setup::user_hooks_complete(&config, &installed));

    // The same hooks, once that vca is gone, do not.
    std::fs::remove_file(&installed).unwrap();
    assert!(!setup::user_hooks_complete(&config, &installed));
}

#[tokio::test]
async fn a_phone_may_pause_and_resume_the_command_center() {
    let f = fixture();
    f.add(info(
        "me",
        "conductor",
        AgentKind::ClaudeCode,
        SessionState::Idle,
    ));
    f.add_agent("aaa", "api");
    f.enable(Some("me"));
    f.settings(|s| s.wake_events = true);
    let (phone, _rx) = f.api.connect_as(
        "remote",
        Some(DeviceIdentity {
            device_id: "dev_2".into(),
            scope: Scope::Command,
        }),
    );
    let reply = f
        .conductor
        .handle_client(&f.api, &phone, ClientMsg::CcPause { paused: true })
        .await;
    let ServerMsg::Conductor { state } = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(state.status, ConductorStatus::Paused);
    assert!(state.settings.paused);
    // Persisted, and audited.
    let saved: ConductorSettings =
        serde_json::from_str(&std::fs::read_to_string(f.data.join(SETTINGS_FILE)).unwrap())
            .unwrap();
    assert!(saved.paused);
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == "cc_pause" && e.detail == "paused=true")
    );
    // Paused: no wake-up reaches the conductor, whatever is waiting.
    f.conductor.observe(&info(
        "aaa",
        "api",
        AgentKind::ClaudeCode,
        SessionState::Running,
    ));
    f.conductor.observe(&info(
        "aaa",
        "api",
        AgentKind::ClaudeCode,
        SessionState::Idle,
    ));
    f.advance(QUIET_AFTER_INPUT_MS + CONDUCTOR_MIN_WAKE_GAP_SECS * 1000);
    assert!(!f.conductor.flush_wakes(&f.api).await);
    // The pause survives a saved settings form (which knows nothing of it).
    let mut form = f.conductor.settings();
    form.paused = false;
    f.conductor
        .configure(form, ("local", "local"))
        .await
        .unwrap();
    assert!(f.conductor.settings().paused);
    // Resuming from the phone is allowed too; the waiting events go out.
    let reply = f
        .conductor
        .handle_client(&f.api, &phone, ClientMsg::CcPause { paused: false })
        .await;
    let ServerMsg::Conductor { state } = reply else {
        panic!("{reply:?}");
    };
    assert_ne!(state.status, ConductorStatus::Paused);
    assert!(f.conductor.flush_wakes(&f.api).await);
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == "cc_pause" && e.detail == "paused=false")
    );
}

#[test]
fn the_kill_switch_is_gated_on_the_pc_and_a_plain_stop_is_not() {
    let kill = crate::local::gated_api_target(&ClientMsg::ConductorStop {
        kill: true,
        code: None,
    })
    .unwrap();
    assert_eq!(kill.action, crate::security::GatedAction::ConductorKill);
    assert_eq!(kill.action.as_str(), "conductor_kill");
    assert!(!kill.action.shared_window());
    assert!(
        crate::local::gated_api_target(&ClientMsg::ConductorStop {
            kill: false,
            code: None,
        })
        .is_none()
    );
    // The phone's code never goes into the JSON when absent.
    let json = serde_json::to_value(ClientMsg::ConductorStop {
        kill: true,
        code: None,
    })
    .unwrap();
    assert_eq!(json, json!({"type": "conductor_stop", "kill": true}));
}

#[tokio::test]
async fn a_turn_without_an_end_is_reported_as_stalled() {
    let f = fixture();
    f.add(info(
        "me",
        "conductor",
        AgentKind::ClaudeCode,
        SessionState::Running,
    ));
    f.enable(Some("me"));
    let session = f.api.sessions().get(&SessionId::from("me")).unwrap();
    session.hook_seen.store(true, Ordering::SeqCst);
    let now = f.now.load(Ordering::SeqCst);
    lock(&f.conductor.inner.state).turn_started_at = Some(now);
    assert_eq!(f.conductor.state().problem, None);
    f.advance(TURN_STALL_MS);
    assert_eq!(
        f.conductor.state().problem.as_deref(),
        Some(vca_core::CONDUCTOR_PROBLEM_TURN_STALLED)
    );
    // The end of the turn clears it.
    let idle = info("me", "conductor", AgentKind::ClaudeCode, SessionState::Idle);
    f.conductor
        .observe_conductor(&idle, Some(&SessionState::Running));
    assert_eq!(f.conductor.state().problem, None);
}

#[test]
fn the_command_center_is_announced_with_the_conductor() {
    assert_eq!(vca_core::protocol::FEATURE_COMMAND_CENTER, "command_center");
    let json = serde_json::to_value(ConductorStatus::Paused).unwrap();
    assert_eq!(json, "paused");
    let msg: ClientMsg =
        serde_json::from_value(json!({"type": "cc_pause", "paused": true})).unwrap();
    assert_eq!(msg, ClientMsg::CcPause { paused: true });
}

// ---------------------------------------------------------------- instruction grants

const BY: (&str, &str) = ("local", "local");

impl Fixture {
    /// Sends are recorded instead of typed (test sessions have no input).
    fn record_sends(&self) {
        lock(&self.conductor.inner.state).delivered = Some(Vec::new());
    }

    fn sent(&self) -> Vec<(String, String)> {
        lock(&self.conductor.inner.state)
            .delivered
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, text)| (id.0, text))
            .collect()
    }

    /// Two work sessions, the conductor enabled, sends recorded.
    fn grant_fixture() -> Self {
        let f = fixture();
        f.add_agent("aaa", "api");
        f.add_agent("bbb", "Orbit 3D");
        f.enable(None);
        f.record_sends();
        f
    }

    /// Runs `call`, expects it to wait for the person, denies it and
    /// returns the request's reason.
    async fn needs_person(&self, call: ConductorCall) -> String {
        let task = {
            let (c, api) = (self.conductor.clone(), self.api.clone());
            tokio::spawn(async move { c.execute(&api, call).await })
        };
        let id = wait_pending(&self.conductor).await;
        let reason = self.conductor.state().pending[0].reason.clone().unwrap();
        self.conductor.decide(&id, false, BY).unwrap();
        assert_eq!(task.await.unwrap().unwrap()["outcome"], "denied");
        reason
    }
}

#[tokio::test]
async fn an_instruction_naming_a_session_sends_without_asking_again() {
    let f = Fixture::grant_fixture();
    let grant = f
        .conductor
        .open_grant(
            "Orbit 3D에게 빌드 확인하라고 하고 알려줘",
            InstructionSource::Owner,
            BY,
        )
        .unwrap();
    let out = f
        .exec(send("bbb", "빌드가 통과했는지 확인해 줘"))
        .await
        .unwrap();
    assert_eq!(out["outcome"], "sent");
    assert_eq!(out["confirmed_by"], "instruction");
    assert_eq!(out["grant"], grant.as_str());
    assert!(f.conductor.state().pending.is_empty());
    assert_eq!(
        f.sent(),
        [("bbb".to_owned(), "빌드가 통과했는지 확인해 줘".to_owned())]
    );
    // The audit names the grant, which the person's instruction opened.
    let audit = f.audit();
    let opened = audit.iter().find(|e| e.event == events::GRANT).unwrap();
    assert_eq!(opened.device_id, "local");
    assert!(
        opened
            .detail
            .contains(&format!("grant={grant} all=false minutes=10 sessions=bbb")),
        "{}",
        opened.detail
    );
    let sent = audit.iter().find(|e| e.event == events::SEND).unwrap();
    assert_eq!(sent.device_id, ORIGIN);
    assert_eq!(sent.session_id.as_ref().map(|s| s.0.as_str()), Some("bbb"));
    assert!(
        sent.detail.starts_with(&format!(
            "outcome=sent confirmed_by=instruction grant={grant} chars="
        )),
        "{}",
        sent.detail
    );
    // The audit keeps the length of the text, never the text.
    let chars = "빌드가 통과했는지 확인해 줘".chars().count();
    assert!(
        sent.detail.ends_with(&format!(" chars={chars}")),
        "{}",
        sent.detail
    );
    assert!(!sent.detail.contains("빌드"), "{}", sent.detail);
    // By card number too.
    f.conductor
        .open_grant("1번 세션에 상태 물어봐", InstructionSource::Owner, BY)
        .unwrap();
    let out = f.exec(send("aaa", "지금 상태 알려줘")).await.unwrap();
    assert_eq!(out["confirmed_by"], "instruction");
}

#[tokio::test]
async fn a_session_the_instruction_did_not_name_still_needs_the_person() {
    let f = Fixture::grant_fixture();
    f.conductor
        .open_grant(
            "Orbit 3D에게 빌드 확인하라고 해",
            InstructionSource::Owner,
            BY,
        )
        .unwrap();
    let reason = f.needs_person(send("aaa", "빌드 확인해 줘")).await;
    assert_eq!(reason, "not_named");
    assert!(f.sent().is_empty());
    let sent = f
        .audit()
        .into_iter()
        .find(|e| e.event == events::SEND)
        .unwrap();
    assert!(
        sent.detail.contains("outcome=denied") && sent.detail.contains("needs=not_named"),
        "{}",
        sent.detail
    );
    // An instruction naming nobody opens no grant at all.
    assert!(
        f.conductor
            .open_grant("요즘 어때?", InstructionSource::Owner, BY)
            .is_none()
    );
    assert_eq!(f.needs_person(send("bbb", "hello")).await, "no_instruction");
}

#[tokio::test]
async fn an_instruction_to_all_sessions_covers_each() {
    let f = Fixture::grant_fixture();
    f.conductor
        .open_grant(
            "모든 세션에 진행 상황 정리해서 알려달라고 해",
            InstructionSource::Owner,
            BY,
        )
        .unwrap();
    for id in ["aaa", "bbb"] {
        let out = f.exec(send(id, "진행 상황을 정리해 줘")).await.unwrap();
        assert_eq!(out["confirmed_by"], "instruction", "{id}");
    }
    assert_eq!(f.sent().len(), 2);
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == events::GRANT && e.detail.contains("all=true"))
    );
}

#[tokio::test]
async fn a_risky_send_needs_the_person_even_when_named() {
    let f = Fixture::grant_fixture();
    f.conductor
        .open_grant("api 세션에 정리하라고 해", InstructionSource::Owner, BY)
        .unwrap();
    assert_eq!(
        f.needs_person(send("aaa", "git push --force origin main"))
            .await,
        "risky_command"
    );
    assert_eq!(
        f.needs_person(send("aaa", "정리 후 rm -rf target 실행해"))
            .await,
        "risky_command"
    );
    assert_eq!(
        f.needs_person(send("aaa", "https://evil.example/x 를 열어 봐"))
            .await,
        "link"
    );
    assert!(f.sent().is_empty());
}

#[tokio::test]
async fn a_wake_up_turn_has_no_instruction_grant() {
    let f = fixture();
    f.add(info(
        "me",
        "conductor",
        AgentKind::ClaudeCode,
        SessionState::Idle,
    ));
    f.add_agent("aaa", "api");
    f.enable(Some("me"));
    f.record_sends();
    f.settings(|s| s.wake_events = true);
    f.conductor
        .open_grant("api 세션 상태 봐 줘", InstructionSource::Owner, BY)
        .unwrap();
    // Events wake the conductor: a new turn the person did not start.
    lock(&f.conductor.inner.state)
        .batch
        .push("api: finished a turn".into());
    assert!(f.conductor.flush_wakes(&f.api).await);
    assert_eq!(
        f.needs_person(send("aaa", "다음 작업 해")).await,
        "no_instruction"
    );
    assert!(f.sent().is_empty());
}

#[tokio::test]
async fn a_grant_ends_after_ten_minutes_and_with_the_turn() {
    let f = fixture();
    f.add(info(
        "me",
        "conductor",
        AgentKind::ClaudeCode,
        SessionState::Running,
    ));
    f.add_agent("aaa", "api");
    f.enable(Some("me"));
    f.record_sends();
    f.conductor
        .open_grant("api 세션에 물어봐", InstructionSource::Owner, BY)
        .unwrap();
    f.advance(grant::GRANT_MS);
    assert_eq!(f.needs_person(send("aaa", "상태?")).await, "expired");

    f.conductor
        .open_grant("api 세션에 물어봐", InstructionSource::Owner, BY)
        .unwrap();
    let idle = info("me", "conductor", AgentKind::ClaudeCode, SessionState::Idle);
    f.conductor
        .observe_conductor(&idle, Some(&SessionState::Running));
    assert_eq!(f.needs_person(send("aaa", "상태?")).await, "no_instruction");
    assert!(f.sent().is_empty());
}

#[tokio::test]
async fn only_the_owner_opens_instruction_grants() {
    let f = Fixture::grant_fixture();
    assert!(
        f.conductor
            .open_grant("api 세션에 물어봐", InstructionSource::Other, BY)
            .is_none()
    );
    assert_eq!(f.needs_person(send("aaa", "상태?")).await, "no_instruction");

    let (pc, _rx) = f.api.connect("local");
    assert_eq!(InstructionSource::of(&pc), InstructionSource::Owner);
    // A process inside a session (or one the channel cannot name).
    pc.set_untrusted_caller(true);
    assert_eq!(InstructionSource::of(&pc), InstructionSource::Other);
    let (dev, _rx) = f.api.connect("dev-ws");
    assert_eq!(InstructionSource::of(&dev), InstructionSource::Other);
    let device = |scope| {
        Some(DeviceIdentity {
            device_id: "dev_1".into(),
            scope,
        })
    };
    let (phone, _rx) = f.api.connect_as("remote", device(Scope::Command));
    assert_eq!(InstructionSource::of(&phone), InstructionSource::Owner);
    let (viewer, _rx) = f.api.connect_as("remote", device(Scope::View));
    assert_eq!(InstructionSource::of(&viewer), InstructionSource::Other);
}

#[tokio::test]
async fn builtin_sends_on_the_persons_instruction() {
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("tool_call_id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("Asked api.")))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call(
            "send_to_session",
            json!({"session_id": "aaa", "text": "빌드 상태를 확인해 줘"}),
        )))
        .with_priority(2)
        .mount(&server)
        .await;
    let f = builtin_fixture(&server).await;
    f.record_sends();
    ask(&f, "api 세션에 빌드 상태 확인하라고 하고 알려줘").await;
    assert_eq!(wait_reply(&f).await, "Asked api.");
    assert!(f.conductor.state().pending.is_empty());
    assert_eq!(f.sent().len(), 1);
    assert!(
        f.audit()
            .iter()
            .any(|e| e.event == events::SEND && e.detail.contains("confirmed_by=instruction"))
    );
    // The turn is over: so is its grant.
    for _ in 0..200 {
        if lock(&f.conductor.inner.state).grant.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the grant outlived its turn");
}

// ---------------------------------------------------------------- decision triage

/// Opens Claude Code's permission dialog on `id`.
fn open_decision(f: &Fixture, id: &str, decision_id: &str, command: Option<&str>, at: u64) {
    let mgr = f.api.sessions();
    mgr.request_decision(
        &mgr.get(&id.into()).unwrap(),
        crate::sessions::PendingDecision {
            decision: Decision {
                id: decision_id.into(),
                prompt: format!("Run {}?", command.unwrap_or("a tool")),
                options: vca_adapters::claude::hook_event::claude_permission_options(),
                inferred: false,
                questions: Vec::new(),
                command: command.map(str::to_owned),
            },
            submit: false,
            heuristic: false,
            at,
            tool_use_id: None,
        },
    );
}

fn close_decision(f: &Fixture, id: &str) {
    let session = f.api.sessions().get(&id.into()).unwrap();
    *lock(&session.pending) = None;
}

fn current(f: &Fixture, id: &str) -> SessionInfo {
    f.api.sessions().get(&id.into()).unwrap().info()
}

/// A running CLI conductor `me` with working hooks, events passed to it,
/// and the session `aaa` ("api").
fn triage_fixture() -> Fixture {
    let f = fixture();
    f.add(info(
        "me",
        "Conductor",
        AgentKind::ClaudeCode,
        SessionState::AwaitingInput,
    ));
    f.api
        .sessions()
        .get(&"me".into())
        .unwrap()
        .hook_seen
        .store(true, Ordering::SeqCst);
    f.add(info(
        "aaa",
        "api",
        AgentKind::ClaudeCode,
        SessionState::Running,
    ));
    f.enable(Some("me"));
    f.settings(|s| s.wake_events = true);
    lock(&f.conductor.inner.state).answered = Some(Vec::new());
    f
}

fn rule(
    id: &str,
    answer: vca_core::ConductorRuleAnswer,
    commands: &[&str],
) -> vca_core::ConductorDecisionRule {
    vca_core::ConductorDecisionRule {
        id: id.into(),
        answer,
        commands: commands.iter().map(|c| (*c).to_owned()).collect(),
        sessions: Vec::new(),
        expires_at: None,
    }
}

fn answer(id: &str, option: &str) -> ConductorCall {
    ConductorCall::AnswerDecision {
        session_id: id.into(),
        option: option.into(),
    }
}

#[tokio::test]
async fn conductor_off_puts_nothing_in_the_inbox() {
    let f = fixture();
    f.add(info(
        "aaa",
        "api",
        AgentKind::ClaudeCode,
        SessionState::Running,
    ));
    open_decision(&f, "aaa", "d1", Some("cargo test"), 5);
    f.conductor.observe(&current(&f, "aaa"));
    f.advance(10 * 60_000);
    f.conductor.check_triage();
    let state = f.conductor.state();
    assert!(state.escalations.is_empty(), "{:?}", state.escalations);
    assert!(lock(&f.conductor.inner.state).batch.is_empty());
    // Turned on but not started: now it goes to the inbox by itself.
    f.settings(|s| s.enabled = true);
    f.conductor.retrack();
    let state = f.conductor.state();
    assert_eq!(state.escalations.len(), 1);
    assert_eq!(state.escalations[0].reason.as_deref(), Some("stopped"));
    // Turned off again: the inbox empties (Alerts keep the decision).
    f.settings(|s| s.enabled = false);
    f.conductor.check_triage();
    assert!(lock(&f.conductor.inner.state).escalations.is_empty());
}

#[tokio::test]
async fn conductor_wakes_on_a_decision_and_its_escalation_follows_the_decision() {
    let f = triage_fixture();
    open_decision(&f, "aaa", "d1", Some("cargo publish"), 5);
    f.conductor.observe(&current(&f, "aaa"));
    // Not in the inbox yet: the conductor gets it first, at once (no
    // two-minute gap and no typing pause for a decision).
    assert!(f.conductor.state().escalations.is_empty());
    f.conductor.note_input(&"me".into());
    assert!(f.conductor.flush_wakes(&f.api).await);
    let chat = f.conductor.state().chat;
    let event = &chat.iter().find(|t| t.role == "event").unwrap().text;
    assert!(
        event.contains("api (session aaa): waits for a decision"),
        "{event}"
    );
    assert!(event.contains("command: cargo publish"), "{event}");
    assert!(
        event.contains("options: Yes / Yes, and don't ask again / No"),
        "{event}"
    );
    assert!(event.contains("escalate_decision"), "{event}");

    let pending = f.exec(ConductorCall::PendingDecisions).await.unwrap();
    assert_eq!(pending["decisions"][0]["allowed_answers"], json!([]));
    assert_eq!(pending["decisions"][0]["command"], "cargo publish");

    let r = f
        .exec(ConductorCall::EscalateDecision {
            session_id: "aaa".into(),
            summary: "api 세션이 패키지를 배포하려고 합니다. 외부 공개라 확인이 필요합니다.".into(),
            recommendation: Some("버전을 확인한 뒤 허용하세요.".into()),
        })
        .await
        .unwrap();
    assert_eq!(r["outcome"], "escalated");
    let state = f.conductor.state();
    assert_eq!(state.escalations.len(), 1);
    let e = &state.escalations[0];
    assert_eq!(e.source, vca_core::ConductorEscalationSource::Conductor);
    assert_eq!(e.session_name, "api");
    assert_eq!(e.decision_id, "d1");
    assert!(e.summary.contains("배포"));
    assert_eq!(
        e.recommendation.as_deref(),
        Some("버전을 확인한 뒤 허용하세요.")
    );
    assert_eq!(e.request, "Run cargo publish?");
    assert_eq!(e.command.as_deref(), Some("cargo publish"));
    assert!(
        f.audit()
            .iter()
            .any(|a| a.event == events::ESCALATE && a.detail.contains("source=conductor"))
    );
    // The summary stays in the approvals; the audit keeps its length only.
    let escalated = f
        .audit()
        .into_iter()
        .find(|a| a.event == events::ESCALATE)
        .unwrap();
    assert!(
        escalated.detail.contains("summary_chars="),
        "{}",
        escalated.detail
    );
    assert!(!escalated.detail.contains("배포"), "{}", escalated.detail);
    // No fallback after it was escalated.
    f.advance(10 * 60_000);
    f.conductor.check_triage();
    assert_eq!(f.conductor.state().escalations.len(), 1);

    // Answered elsewhere (the person, on the phone): the escalation goes.
    close_decision(&f, "aaa");
    assert!(f.conductor.state().escalations.is_empty());
    // An escalation of nothing is refused.
    let err = f
        .exec(ConductorCall::EscalateDecision {
            session_id: "aaa".into(),
            summary: "x".into(),
            recommendation: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Conflict);
}

#[tokio::test]
async fn conductor_a_replaced_decision_drops_the_stale_escalation() {
    let f = triage_fixture();
    open_decision(&f, "aaa", "d1", Some("cargo test"), 5);
    f.conductor.observe(&current(&f, "aaa"));
    f.exec(ConductorCall::EscalateDecision {
        session_id: "aaa".into(),
        summary: "needs you".into(),
        recommendation: None,
    })
    .await
    .unwrap();
    // A new decision of the same session, the state staying the same.
    open_decision(&f, "aaa", "d2", Some("cargo build"), 9);
    assert!(f.conductor.state().escalations.is_empty(), "stale one gone");
    f.conductor.observe(&current(&f, "aaa"));
    assert!(
        lock(&f.conductor.inner.state)
            .triage
            .get(&"aaa".into())
            .is_some_and(|t| t.decision_id == "d2"),
        "the new decision waits for the conductor"
    );
}

#[tokio::test]
async fn conductor_answers_are_refused_without_a_rule_and_for_risky_decisions() {
    let f = triage_fixture();
    open_decision(&f, "aaa", "d1", Some("cargo test -p core"), 5);
    f.conductor.observe(&current(&f, "aaa"));
    // No rule by default: neither yes nor no.
    for option in ["1", "Yes", "3", "No"] {
        let err = f.exec(answer("aaa", option)).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Forbidden, "{option}");
        assert!(error_text(&err).contains("escalate_decision"));
    }
    // A rule that names a risky command still never approves it.
    f.settings(|s| {
        s.decision_rules = vec![rule(
            "push",
            vca_core::ConductorRuleAnswer::Approve,
            &["git push", "cargo test"],
        )]
    });
    // "Don't ask again" is never the conductor's to pick.
    let err = f.exec(answer("aaa", "2")).await.unwrap_err();
    assert!(
        error_text(&err).contains("not_a_plain_answer"),
        "{}",
        error_text(&err)
    );
    // An approve rule is not a deny rule.
    assert!(f.exec(answer("aaa", "No")).await.is_err());
    close_decision(&f, "aaa");
    open_decision(&f, "aaa", "d2", Some("git push origin main"), 6);
    let err = f.exec(answer("aaa", "Yes")).await.unwrap_err();
    assert!(
        error_text(&err).contains("always_escalate"),
        "{}",
        error_text(&err)
    );
    close_decision(&f, "aaa");
    open_decision(&f, "aaa", "d3", Some("cargo test && rm -rf /"), 7);
    assert!(f.exec(answer("aaa", "Yes")).await.is_err());
    assert!(
        lock(&f.conductor.inner.state)
            .answered
            .as_ref()
            .unwrap()
            .is_empty()
    );
    let refused = f
        .audit()
        .iter()
        .filter(|a| a.event == events::REFUSED && a.detail.contains("tool=answer_decision"))
        .count();
    assert!(refused >= 6, "every refusal is audited ({refused})");
    // The conductor's own decisions are never its to answer.
    open_decision(&f, "me", "own", Some("cargo test"), 8);
    let err = f.exec(answer("me", "Yes")).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
}

#[tokio::test]
async fn conductor_answers_within_a_rule_and_audits_the_rule() {
    let f = triage_fixture();
    f.settings(|s| {
        s.decision_rules = vec![
            rule(
                "tests",
                vca_core::ConductorRuleAnswer::Approve,
                &["cargo test"],
            ),
            rule("no-web", vca_core::ConductorRuleAnswer::Deny, &[]),
        ]
    });
    open_decision(&f, "aaa", "d1", Some("cargo test -p core"), 5);
    f.conductor.observe(&current(&f, "aaa"));
    let pending = f.exec(ConductorCall::PendingDecisions).await.unwrap();
    assert_eq!(
        pending["decisions"][0]["allowed_answers"],
        json!(["approve", "deny"])
    );
    let r = f.exec(answer("aaa", "Yes")).await.unwrap();
    assert_eq!(r["outcome"], "answered");
    assert_eq!(r["rule"], "tests");
    assert_eq!(
        lock(&f.conductor.inner.state).answered.clone().unwrap(),
        vec![(SessionId::from("aaa"), "d1".to_owned(), "1".to_owned())]
    );
    let audit = f.audit();
    let entry = audit.iter().find(|a| a.event == events::ANSWER).unwrap();
    assert!(entry.detail.contains("rule=tests"), "{}", entry.detail);
    assert!(entry.detail.contains("answer=approve"), "{}", entry.detail);
    assert!(entry.detail.contains("command=cargo test -p core"));
    // Nothing waits for the fallback any more.
    assert!(lock(&f.conductor.inner.state).triage.is_empty());
    // A deny rule covers the plain no.
    close_decision(&f, "aaa");
    open_decision(&f, "aaa", "d2", None, 6);
    let r = f.exec(answer("aaa", "No")).await.unwrap();
    assert_eq!(r["rule"], "no-web");
    // An expired rule allows nothing.
    f.settings(|s| s.decision_rules[1].expires_at = Some(1));
    close_decision(&f, "aaa");
    open_decision(&f, "aaa", "d3", None, 7);
    assert!(f.exec(answer("aaa", "No")).await.is_err());
}

#[tokio::test]
async fn conductor_fallback_raises_a_decision_after_the_grace_period() {
    let f = triage_fixture();
    open_decision(&f, "aaa", "d1", Some("cargo test"), 5);
    f.conductor.observe(&current(&f, "aaa"));
    assert!(f.conductor.flush_wakes(&f.api).await);
    f.advance(60_000);
    f.conductor.check_triage();
    assert!(
        f.conductor.state().escalations.is_empty(),
        "still in its grace"
    );
    f.advance(CONDUCTOR_TRIAGE_GRACE_SECS * 1000);
    f.conductor.check_triage();
    let state = f.conductor.state();
    assert_eq!(state.escalations.len(), 1);
    let e = &state.escalations[0];
    assert_eq!(e.source, vca_core::ConductorEscalationSource::Fallback);
    assert_eq!(e.reason.as_deref(), Some("timeout"));
    assert!(e.summary.is_empty());
    assert_eq!(e.request, "Run cargo test?");
    assert!(
        f.audit()
            .iter()
            .any(|a| a.event == events::ESCALATE && a.detail.contains("reason=timeout"))
    );
    // A wake-up that never reached the conductor falls back too.
    f.add(info("bbb", "web", AgentKind::Codex, SessionState::Running));
    f.settings(|s| s.max_wakes_per_hour = 1);
    open_decision(&f, "bbb", "e1", None, 9);
    f.conductor.observe(&current(&f, "bbb"));
    assert!(!f.conductor.flush_wakes(&f.api).await, "hourly cap");
    f.advance(UNDELIVERED_MS);
    f.conductor.check_triage();
    assert_eq!(f.conductor.state().escalations.len(), 2);
}

#[tokio::test]
async fn conductor_fallback_when_stopped_paused_or_without_hooks() {
    // Paused while a decision waits for it: to the inbox at once.
    let f = triage_fixture();
    open_decision(&f, "aaa", "d1", None, 5);
    f.conductor.observe(&current(&f, "aaa"));
    assert!(f.conductor.state().escalations.is_empty());
    f.settings(|s| s.paused = true);
    f.conductor.check_triage();
    assert_eq!(
        f.conductor.state().escalations[0].reason.as_deref(),
        Some("paused")
    );

    // Stopped: a new decision goes straight to the inbox.
    let f = triage_fixture();
    f.conductor.stop(false, ("local", "local")).await;
    open_decision(&f, "aaa", "d1", None, 5);
    f.conductor.observe(&current(&f, "aaa"));
    let state = f.conductor.state();
    assert_eq!(state.escalations.len(), 1);
    assert_eq!(state.escalations[0].reason.as_deref(), Some("stopped"));

    // Stopped while one waits: it is raised by the stop itself.
    let f = triage_fixture();
    open_decision(&f, "aaa", "d1", None, 5);
    f.conductor.observe(&current(&f, "aaa"));
    f.conductor.stop(false, ("local", "local")).await;
    assert_eq!(
        f.conductor.state().escalations[0].reason.as_deref(),
        Some("stopped")
    );

    // Hooks missing: the conductor could never answer.
    let f = triage_fixture();
    f.api
        .sessions()
        .get(&"me".into())
        .unwrap()
        .hook_seen
        .store(false, Ordering::SeqCst);
    open_decision(&f, "aaa", "d1", None, 5);
    f.conductor.observe(&current(&f, "aaa"));
    assert_eq!(
        f.conductor.state().escalations[0].reason.as_deref(),
        Some(vca_core::CONDUCTOR_PROBLEM_HOOKS_MISSING)
    );

    // Other events not passed to the conductor: decisions still go to it
    // for triage, not straight to the inbox.
    let f = triage_fixture();
    f.settings(|s| s.wake_events = false);
    open_decision(&f, "aaa", "d1", None, 5);
    f.conductor.observe(&current(&f, "aaa"));
    assert!(f.conductor.state().escalations.is_empty());
    assert!(!lock(&f.conductor.inner.state).triage.is_empty());
}

#[tokio::test]
async fn conductor_own_decisions_are_never_escalations() {
    let f = triage_fixture();
    open_decision(&f, "me", "own", Some("cargo test"), 5);
    f.conductor.observe(&current(&f, "me"));
    f.advance(10 * 60_000);
    f.conductor.check_triage();
    assert!(f.conductor.state().escalations.is_empty());
    assert!(lock(&f.conductor.inner.state).triage.is_empty());
}
