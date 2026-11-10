//! Hook-free approval inference for observed sessions.
//!
//! Without agent hooks (an opt-in; VCA installs none into the user's agent
//! configuration on its own) a permission prompt in a terminal VCA did not
//! start is invisible: the transcript only shows the tool call, and its
//! result arrives after the person answered. This module infers the prompt
//! instead:
//!
//! * the transcript tailer records tool calls that have no result yet
//!   ([`OpenTools`], fed by [`observe_item`]);
//! * a cheap watcher ([`Watcher`], every [`WatchConfig::interval`]) marks a
//!   session as probably waiting for permission when a tool that usually
//!   asks has been unanswered for a while and the agent's process tree is
//!   idle: low CPU over the last two samples and no extra child process
//!   compared with the tree while no tool was running. A long-running
//!   command keeps its child process (and so the tree) busy and does not
//!   count.
//!
//! The inferred decision takes the same path as a hook decision (pending
//! decision, `awaiting_decision`, alerts) with a stable id derived from the
//! tool call id, admitted through the hook's duplicate filter. It is marked
//! `inferred` and has no options: an observed session is answered in its
//! own terminal (at the PC or with the terminal key buttons). It clears as
//! soon as the result appears, the conversation moves on, or the tree turns
//! busy (the command was allowed and runs).
//!
//! Nothing is inferred while the transcript reports a permission mode that
//! settles calls without a prompt (`auto`, `dontAsk`, `bypassPermissions`;
//! see [`crate::asks`]): there a long command whose tree looks idle (a
//! build blocked on a lock, a test run waiting) is no prompt.
//!
//! Monitor-only: the process samples come from the broker's monitor-only
//! list. Nothing here touches a process, a console or the agent's files.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vca_adapters::HistoryItem;
use vca_adapters::claude::ToolPermission;
use vca_broker::Broker;
use vca_core::{AgentKind, Decision, MessageRole, SessionId, SessionState, now_millis};

use crate::hooks::admit_decision;
use crate::sessions::{Kind, PendingDecision, Session, SessionManager};
use crate::util::lock;

/// Tuning of the inference.
#[derive(Debug, Clone)]
pub(crate) struct WatchConfig {
    /// How often sessions are checked.
    pub interval: Duration,
    /// How long a tool call must stay unanswered before it counts.
    pub unanswered_after: Duration,
    /// The same for tools that may wait on the network with no child
    /// process (web fetches, MCP tools).
    pub slow_unanswered_after: Duration,
    /// Highest CPU use of the whole process tree that still counts as idle,
    /// in percent of one core.
    pub idle_cpu_core_percent: f32,
    /// Samples taken while no tool ran that form the process count
    /// baseline (their minimum).
    pub baseline_samples: usize,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(2500),
            unanswered_after: Duration::from_secs(6),
            slow_unanswered_after: Duration::from_secs(20),
            idle_cpu_core_percent: 15.0,
            baseline_samples: 5,
        }
    }
}

/// Most tool calls remembered per session; older ones are dropped.
const MAX_OPEN: usize = 64;

/// A tool call without a result yet.
#[derive(Debug, Clone)]
pub(crate) struct OpenTool {
    pub id: String,
    pub name: String,
    pub summary: String,
    /// When the tailer first saw the call.
    pub first_seen: Instant,
}

/// Tool calls of a transcript that have no result yet, oldest first.
#[derive(Debug, Default)]
pub(crate) struct OpenTools {
    calls: Vec<OpenTool>,
}

impl OpenTools {
    /// Applies one transcript item: a call opens, its result closes it, and
    /// a new user or assistant message means the turn moved on.
    pub(crate) fn observe(&mut self, item: &HistoryItem, now: Instant) {
        match item {
            HistoryItem::ToolCall {
                id: Some(id),
                name,
                summary,
                ..
            } => {
                if self.calls.iter().any(|c| &c.id == id) {
                    return;
                }
                if self.calls.len() >= MAX_OPEN {
                    self.calls.remove(0);
                }
                self.calls.push(OpenTool {
                    id: id.clone(),
                    name: name.clone(),
                    summary: summary.clone(),
                    first_seen: now,
                });
            }
            HistoryItem::ToolResult { id: Some(id), .. } => self.calls.retain(|c| &c.id != id),
            // System notes (task notifications, markers) can arrive while a
            // prompt is open; they do not end it.
            HistoryItem::Message {
                role: MessageRole::User | MessageRole::Assistant,
                ..
            } => self.calls.clear(),
            _ => {}
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    fn contains(&self, id: &str) -> bool {
        self.calls.iter().any(|c| c.id == id)
    }

    /// The oldest open call of a tool that usually asks for permission.
    fn waiting(&self, classify: Classify) -> Option<(OpenTool, ToolPermission)> {
        self.calls.iter().find_map(|c| {
            let permission = classify(&c.name);
            permission.may_ask().then(|| (c.clone(), permission))
        })
    }
}

/// Classifies a tool by name.
type Classify = fn(&str) -> ToolPermission;

/// Tool classification for agents whose transcripts pair calls and results.
fn classifier(agent: &AgentKind) -> Option<(&'static str, Classify)> {
    match agent {
        AgentKind::ClaudeCode => Some(("claude", vca_adapters::claude::tool_permission)),
        AgentKind::Codex => Some(("codex", vca_adapters::codex::rollout::tool_permission)),
        _ => None,
    }
}

const INFERRED_MARK: &str = ":inferred-";

/// Stable decision id of an inferred prompt for one tool call.
fn decision_id(agent: &str, session: &Session, tool_id: &str) -> String {
    let scope = session
        .agent_session_id()
        .unwrap_or_else(|| session.id.0.clone());
    format!("{agent}:{scope}{INFERRED_MARK}{tool_id}")
}

/// The tool call id of an inferred decision id.
fn inferred_tool_id(decision_id: &str) -> Option<&str> {
    decision_id.rsplit_once(INFERRED_MARK).map(|(_, tool)| tool)
}

/// The tool call id of the session's pending inferred decision.
fn pending_inferred(session: &Session) -> Option<String> {
    lock(&session.pending)
        .as_ref()
        .filter(|p| p.decision.inferred)
        .and_then(|p| inferred_tool_id(&p.decision.id).map(str::to_owned))
}

/// Feeds one transcript item to the session's open tool calls (called by
/// the transcript tailer) and clears an inferred decision whose call ended.
pub(crate) fn observe_item(
    mgr: &SessionManager,
    session: &Session,
    item: &HistoryItem,
    now: Instant,
) {
    let still_open = {
        let mut open = lock(&session.open_tools);
        open.observe(item, now);
        pending_inferred(session).map(|id| open.contains(&id))
    };
    if still_open == Some(false) {
        clear_inferred(mgr, session);
    }
}

/// Takes the session's inferred decision, if any; the session runs again.
fn clear_inferred(mgr: &SessionManager, session: &Session) {
    let taken = {
        let mut pending = lock(&session.pending);
        if pending.as_ref().is_some_and(|p| p.decision.inferred) {
            pending.take()
        } else {
            None
        }
    };
    if taken.is_none() {
        return;
    }
    if session.info().state == SessionState::AwaitingDecision {
        mgr.set_state(session, SessionState::Running);
    } else {
        mgr.notify(session);
    }
}

/// CPU and size of a session's process tree at one point in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TreeSample {
    /// The whole tree, in percent of one core.
    pub cpu_core_percent: f32,
    pub process_count: u32,
}

/// Where process tree samples come from (monitor-only).
pub(crate) trait TreeSampler: Send + Sync {
    /// The latest sample of every monitored session.
    fn sample(&self) -> HashMap<SessionId, TreeSample>;
}

/// Reads the broker's monitor-only usage of observed sessions. The broker
/// only lists these sessions for display; it cannot act on them.
struct BrokerSampler {
    broker: Arc<Broker>,
    cores: f32,
}

impl TreeSampler for BrokerSampler {
    fn sample(&self) -> HashMap<SessionId, TreeSample> {
        self.broker
            .snapshot()
            .sessions
            .into_iter()
            .filter(|u| u.monitor_only)
            .map(|u| {
                let sample = TreeSample {
                    // Session CPU is a share of the whole machine.
                    cpu_core_percent: u.cpu_percent * self.cores,
                    process_count: u.process_count,
                };
                (u.session_id, sample)
            })
            .collect()
    }
}

/// Recent samples of one session.
#[derive(Debug, Default)]
struct History {
    /// CPU of the last two samples.
    cpu: VecDeque<f32>,
    /// Process counts while no tool call was open.
    idle_counts: VecDeque<u32>,
}

impl History {
    fn push_cpu(&mut self, sample: Option<TreeSample>) {
        match sample {
            Some(s) => {
                self.cpu.push_back(s.cpu_core_percent);
                while self.cpu.len() > 2 {
                    self.cpu.pop_front();
                }
            }
            // No data (process gone, broker without a monitor): not idle.
            None => self.cpu.clear(),
        }
    }

    fn push_idle_count(&mut self, count: u32, keep: usize) {
        self.idle_counts.push_back(count);
        while self.idle_counts.len() > keep.max(1) {
            self.idle_counts.pop_front();
        }
    }

    fn cpu_idle(&self, limit: f32) -> bool {
        self.cpu.len() == 2 && self.cpu.iter().all(|c| *c <= limit)
    }

    /// Process count of the tree with no tool running (the minimum, so a
    /// sample racing a just-started command does not raise it).
    fn baseline(&self) -> Option<u32> {
        self.idle_counts.iter().copied().min()
    }
}

/// Checks observed sessions for prompts that hooks would have reported.
pub(crate) struct Watcher {
    cfg: WatchConfig,
    history: HashMap<SessionId, History>,
}

impl Watcher {
    pub(crate) fn new(cfg: WatchConfig) -> Self {
        Self {
            cfg,
            history: HashMap::new(),
        }
    }

    /// One round over all sessions with the latest samples.
    pub(crate) fn tick(
        &mut self,
        mgr: &SessionManager,
        samples: &HashMap<SessionId, TreeSample>,
        now: Instant,
    ) {
        let sessions: Vec<Arc<Session>> = mgr
            .all()
            .into_iter()
            .filter(|s| matches!(s.kind, Kind::Observed(_)))
            .collect();
        let live: HashSet<&SessionId> = sessions.iter().map(|s| &s.id).collect();
        self.history.retain(|id, _| live.contains(id));
        for session in &sessions {
            self.check(mgr, session, samples.get(&session.id).copied(), now);
        }
    }

    fn check(
        &mut self,
        mgr: &SessionManager,
        session: &Session,
        sample: Option<TreeSample>,
        now: Instant,
    ) {
        let Some((agent, classify)) = session.agent().as_ref().and_then(classifier) else {
            return;
        };
        let history = self.history.entry(session.id.clone()).or_default();
        history.push_cpu(sample);
        let waiting = {
            let open = lock(&session.open_tools);
            if open.is_empty() {
                if let Some(s) = sample {
                    history.push_idle_count(s.process_count, self.cfg.baseline_samples);
                }
                None
            } else {
                open.waiting(classify)
            }
        };
        if matches!(session.info().state, SessionState::Exited { .. }) {
            return;
        }
        let pending = pending_inferred(session);
        let Some((tool, permission)) = waiting else {
            if pending.is_some() {
                clear_inferred(mgr, session);
            }
            return;
        };
        // A child process beyond the idle tree: a command runs.
        let extra_child = match (sample, history.baseline()) {
            (Some(s), Some(base)) => Some(s.process_count > base),
            _ => None,
        };
        if let Some(pending_tool) = pending {
            if pending_tool != tool.id || extra_child == Some(true) {
                // Another call is the oldest now, or the command was
                // allowed and runs: the prompt is gone.
                clear_inferred(mgr, session);
            }
            return;
        }
        // Without a baseline a shell command may be running; other tools
        // run inside the agent.
        let child_busy = extra_child.unwrap_or(permission == ToolPermission::Command);
        // Hooks report this session exactly; any other decision stays.
        if session.hook_seen.load(std::sync::atomic::Ordering::SeqCst)
            || lock(&session.pending).is_some()
        {
            return;
        }
        // In `auto`, `dontAsk` and `bypassPermissions` the agent settles
        // calls without a prompt: a quiet tree is a long command (a build
        // waiting for a lock, tests sleeping), not a person to ask.
        if lock(&session.turn).mode_settles_without_prompt() {
            return;
        }
        let wait = if permission == ToolPermission::Slow {
            self.cfg.slow_unanswered_after
        } else {
            self.cfg.unanswered_after
        };
        if now.saturating_duration_since(tool.first_seen) < wait
            || !history.cpu_idle(self.cfg.idle_cpu_core_percent)
            || child_busy
        {
            return;
        }
        let prompt = if tool.summary.is_empty() {
            format!("Allow {}?", tool.name)
        } else {
            format!("Allow {}: {}?", tool.name, tool.summary)
        };
        let decision = Decision {
            id: decision_id(agent, session, &tool.id),
            prompt,
            options: Vec::new(),
            inferred: true,
            questions: Vec::new(),
            command: None,
        };
        // The same call is announced once, also after a transcript re-read.
        if let Some(decision) = admit_decision(session, &decision) {
            tracing::debug!(session = %session.id, tool = %tool.name, "inferred a permission prompt");
            mgr.request_decision(
                session,
                PendingDecision {
                    decision,
                    submit: false,
                    heuristic: true,
                    at: now_millis(),
                    tool_use_id: Some(tool.id.clone()),
                },
            );
        }
    }
}

/// Aborts the watcher when dropped (it lives as long as discovery does).
pub(crate) struct WatchGuard(Option<tokio::task::JoinHandle<()>>);

impl Drop for WatchGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// Starts the watcher on the broker's monitor-only samples. Without a
/// broker there is nothing to tell an idle agent from a busy one, so
/// nothing is inferred.
pub(crate) fn spawn(mgr: SessionManager) -> WatchGuard {
    let Some(broker) = mgr.broker().cloned() else {
        return WatchGuard(None);
    };
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
    let sampler = BrokerSampler { broker, cores };
    WatchGuard(Some(tokio::spawn(run(
        mgr,
        Box::new(sampler),
        WatchConfig::default(),
    ))))
}

async fn run(mgr: SessionManager, sampler: Box<dyn TreeSampler>, cfg: WatchConfig) {
    let mut tick = tokio::time::interval(cfg.interval.max(Duration::from_millis(500)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watcher = Watcher::new(cfg);
    loop {
        tick.tick().await;
        let samples = sampler.sample();
        watcher.tick(&mgr, &samples, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use vca_adapters::SessionLocator;
    use vca_broker::contain::Scope;
    use vca_core::{ControlTier, SessionEvent, SessionInfo, SessionOrigin};

    use super::*;
    use crate::events::LogLimits;
    use crate::sessions::{ManagerConfig, Observed, ObservedIdentity};
    use crate::workspace::WorkspaceRoots;

    // Fixture lines in the shape of Claude Code 2.1 and Codex rollouts.
    const BASH_CALL: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"rm -rf build"}}]},"timestamp":"2026-11-01T10:00:00.000Z"}"#;
    const BASH_RESULT: &str = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"done"}]},"toolUseResult":{}}"#;
    const READ_CALL: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"/w/a.rs"}}]}}"#;
    const EDIT_CALL: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_3","name":"Edit","input":{"file_path":"/w/a.rs"}}]}}"#;
    const FETCH_CALL: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_4","name":"WebFetch","input":{"url":"https://example.com"}}]}}"#;
    const USER_PROMPT: &str = r#"{"type":"user","message":{"role":"user","content":"never mind"},"origin":{"kind":"human"}}"#;
    const TASK_NOTICE: &str = r#"{"type":"user","message":{"role":"user","content":"<task-notification>x</task-notification>"},"origin":{"kind":"task-notification"}}"#;
    const CODEX_CALL: &str = r#"{"timestamp":"2026-11-01T10:00:00.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"cargo publish\"]}","call_id":"call_7"}}"#;
    const CODEX_OUTPUT: &str = r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_7","output":"ok"}}"#;

    const BASH_ID: &str = "claude:abc:inferred-toolu_1";

    fn manager(names: &Path) -> SessionManager {
        SessionManager::new(
            ManagerConfig {
                limits: LogLimits::default(),
                exited_retention: Duration::from_secs(60),
                pause_scope: Scope::ChildrenOnly,
                heuristics_interval: Duration::from_secs(1),
                control_endpoint: PathBuf::from("unused"),
                attach_dir: None,
                workspace_roots: WorkspaceRoots::new(Vec::new()),
                session_names: Some(names.to_owned()),
                allow_generic_launch: false,
            },
            None,
        )
    }

    /// Inserts an observed session the way discovery does.
    fn observe(
        mgr: &SessionManager,
        agent: AgentKind,
        transcript: Option<PathBuf>,
    ) -> Arc<Session> {
        let project_dir = PathBuf::from("/code/shop");
        let info = SessionInfo {
            id: SessionId::new(),
            origin: SessionOrigin::Local {
                agent,
                project_dir,
                pid: Some(4_242_424),
            },
            display_name: "shop".into(),
            state: SessionState::Running,
            control: ControlTier::TerminalApi,
            profile: None,
            created_at: now_millis(),
            last_activity: now_millis(),
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
        };
        let observed = Observed {
            terminal: None,
            identity: ObservedIdentity {
                transcript_path: transcript,
                process_start: None,
            },
        };
        mgr.insert_observed(info, observed, Some("abc".into()))
    }

    /// A fixture world: one observed session, a fake sampler, a clock.
    struct World {
        _dir: tempfile::TempDir,
        mgr: SessionManager,
        s: Arc<Session>,
        watcher: Watcher,
        t0: Instant,
    }

    impl World {
        fn new(agent: AgentKind) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(&dir.path().join("names.json"));
            let s = observe(&mgr, agent, None);
            Self {
                _dir: dir,
                mgr,
                s,
                watcher: Watcher::new(WatchConfig::default()),
                t0: Instant::now(),
            }
        }

        fn at(&self, secs: f32) -> Instant {
            self.t0 + Duration::from_secs_f32(secs)
        }

        /// Feeds transcript lines at `secs`.
        fn feed(&self, parse: fn(&str) -> Vec<HistoryItem>, line: &str, secs: f32) {
            for item in parse(line) {
                observe_item(&self.mgr, &self.s, &item, self.at(secs));
            }
        }

        fn claude(&self, line: &str, secs: f32) {
            self.feed(vca_adapters::claude::transcript::parse_line, line, secs);
        }

        /// One watcher round with a fake sample of the tree.
        fn tick(&mut self, secs: f32, cpu_core_percent: f32, process_count: u32) {
            let samples = HashMap::from([(
                self.s.id.clone(),
                TreeSample {
                    cpu_core_percent,
                    process_count,
                },
            )]);
            let now = self.at(secs);
            self.watcher.tick(&self.mgr, &samples, now);
        }

        /// The agent idles with three processes (agent and two MCP servers).
        fn idle_before(&mut self) {
            self.tick(0.0, 0.4, 3);
            self.tick(2.5, 0.2, 3);
        }

        fn requested(&self) -> Vec<Decision> {
            lock(self.s.log())
                .replay_after(0)
                .events
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::DecisionRequested { decision, .. } => Some(decision),
                    _ => None,
                })
                .collect()
        }

        fn pending_id(&self) -> Option<String> {
            self.s.info().pending_decision.map(|p| p.decision.id)
        }
    }

    #[tokio::test]
    async fn unanswered_bash_call_on_an_idle_tree_is_one_inferred_decision() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        w.tick(5.0, 0.3, 3);
        assert!(w.requested().is_empty(), "before the threshold");
        w.tick(7.5, 0.3, 3);
        assert!(w.requested().is_empty(), "6 s not reached yet");
        w.tick(10.0, 0.3, 3);
        let requested = w.requested();
        assert_eq!(requested.len(), 1, "{requested:?}");
        let d = &requested[0];
        assert_eq!(d.id, BASH_ID);
        assert!(d.inferred);
        assert!(d.options.is_empty());
        assert_eq!(d.prompt, "Allow Bash: rm -rf build?");
        let info = w.s.info();
        assert_eq!(info.state, SessionState::AwaitingDecision);
        assert!(info.pending_decision.unwrap().decision.inferred);
        assert_eq!(info.unread, 1);
        // Later rounds keep the one decision.
        w.tick(12.5, 0.3, 3);
        w.tick(15.0, 0.3, 3);
        assert_eq!(w.requested().len(), 1);
    }

    #[tokio::test]
    async fn nothing_is_inferred_in_modes_that_settle_calls_on_their_own() {
        // The CEO's case: an auto-mode session runs `cargo test; cargo
        // clippy` for minutes. Its tree looks idle (cargo waits for the
        // build directory lock other sessions hold) and shows no extra
        // child against a baseline taken while a background task ran.
        for mode in ["auto", "dontAsk", "bypassPermissions"] {
            let mut w = World::new(AgentKind::ClaudeCode);
            let line = format!(r#"{{"type":"permission-mode","permissionMode":"{mode}"}}"#);
            for item in vca_adapters::claude::transcript::parse_line(&line) {
                crate::asks::observe_item(&w.mgr, &w.s, &item);
            }
            w.idle_before();
            w.claude(BASH_CALL, 3.0);
            for i in 0..40 {
                w.tick(5.0 + 2.5 * i as f32, 0.3, 3);
            }
            assert!(w.requested().is_empty(), "{mode}");
            assert_eq!(w.s.info().state, SessionState::Running, "{mode}");
        }
        // The same session back in default mode: a prompt again.
        let mut w = World::new(AgentKind::ClaudeCode);
        crate::asks::observe_item(
            &w.mgr,
            &w.s,
            &HistoryItem::PermissionMode {
                at: None,
                mode: "default".into(),
            },
        );
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        w.tick(10.0, 0.3, 3);
        w.tick(12.5, 0.3, 3);
        assert_eq!(w.requested().len(), 1);
    }

    #[tokio::test]
    async fn answered_call_gives_nothing() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        w.claude(BASH_RESULT, 3.5);
        for i in 0..10 {
            w.tick(5.0 + 2.5 * i as f32, 0.3, 3);
        }
        assert!(w.requested().is_empty());
        assert_eq!(w.s.info().state, SessionState::Running);
    }

    #[tokio::test]
    async fn long_running_command_with_a_child_gives_nothing() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        // `sleep 600`: a fourth process, no CPU.
        for i in 0..40 {
            w.tick(5.0 + 2.5 * i as f32, 0.1, 4);
        }
        assert!(w.requested().is_empty());
        // A command that keeps the agent's own CPU busy does not count either.
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        for i in 0..10 {
            w.tick(5.0 + 2.5 * i as f32, 85.0, 3);
        }
        assert!(w.requested().is_empty());
    }

    #[tokio::test]
    async fn read_only_tools_give_nothing() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(READ_CALL, 3.0);
        for i in 0..10 {
            w.tick(5.0 + 2.5 * i as f32, 0.1, 3);
        }
        assert!(w.requested().is_empty());
    }

    #[tokio::test]
    async fn clears_when_the_result_arrives() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        w.tick(10.0, 0.3, 3);
        assert_eq!(w.pending_id().as_deref(), Some(BASH_ID));
        // Answered in the terminal: the result clears it right away.
        w.claude(BASH_RESULT, 11.0);
        assert_eq!(w.pending_id(), None);
        assert_eq!(w.s.info().state, SessionState::Running);
        w.tick(12.5, 0.3, 3);
        assert_eq!(w.requested().len(), 1);
    }

    #[tokio::test]
    async fn clears_when_the_conversation_moves_on_or_the_command_starts() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        w.tick(10.0, 0.3, 3);
        // A background task notice does not end the prompt.
        w.claude(TASK_NOTICE, 10.5);
        assert_eq!(w.pending_id().as_deref(), Some(BASH_ID));
        // Rejected with a new prompt: the turn moved on.
        w.claude(USER_PROMPT, 11.0);
        assert_eq!(w.pending_id(), None);

        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        w.tick(10.0, 0.3, 3);
        assert!(w.pending_id().is_some());
        // Allowed: the command's process shows up before its result.
        w.tick(12.5, 2.0, 4);
        assert_eq!(w.pending_id(), None);
        assert_eq!(w.s.info().state, SessionState::Running);
        // It is not announced again while the command runs or idles.
        w.tick(15.0, 0.1, 4);
        w.tick(17.5, 0.1, 3);
        w.tick(20.0, 0.1, 3);
        assert_eq!(w.requested().len(), 1);
    }

    #[tokio::test]
    async fn a_reread_transcript_does_not_alert_again() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        w.tick(10.0, 0.3, 3);
        // The tailer restarts and replays the file.
        *lock(&w.s.open_tools) = OpenTools::default();
        w.claude(BASH_CALL, 11.0);
        w.claude(BASH_CALL, 11.0);
        w.tick(20.0, 0.3, 3);
        assert_eq!(w.requested().len(), 1);
        // Even once the pending one is gone (taken elsewhere) the same call
        // is never announced twice.
        lock(&w.s.pending).take();
        *lock(&w.s.open_tools) = OpenTools::default();
        w.claude(BASH_CALL, 21.0);
        w.tick(30.0, 0.3, 3);
        w.tick(32.5, 0.3, 3);
        assert_eq!(w.requested().len(), 1);
    }

    #[tokio::test]
    async fn without_an_idle_baseline_only_in_agent_tools_are_inferred() {
        // Watching starts while the call is open: a shell command may be
        // running, so nothing is inferred for it.
        let mut w = World::new(AgentKind::ClaudeCode);
        w.claude(BASH_CALL, 0.0);
        for i in 0..10 {
            w.tick(2.5 * i as f32, 0.1, 3);
        }
        assert!(w.requested().is_empty());
        // A file edit runs inside the agent.
        let mut w = World::new(AgentKind::ClaudeCode);
        w.claude(EDIT_CALL, 0.0);
        for i in 0..4 {
            w.tick(2.5 * i as f32, 0.1, 3);
        }
        assert_eq!(w.requested().len(), 1);
        assert_eq!(w.requested()[0].prompt, "Allow Edit: /w/a.rs?");
    }

    #[tokio::test]
    async fn network_tools_wait_longer() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(FETCH_CALL, 3.0);
        w.tick(15.0, 0.1, 3);
        w.tick(17.5, 0.1, 3);
        assert!(w.requested().is_empty());
        w.tick(25.0, 0.1, 3);
        assert_eq!(w.requested().len(), 1);
    }

    #[tokio::test]
    async fn sessions_with_hooks_or_without_samples_are_left_alone() {
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.s.hook_seen
            .store(true, std::sync::atomic::Ordering::SeqCst);
        w.claude(BASH_CALL, 3.0);
        w.tick(10.0, 0.1, 3);
        w.tick(12.5, 0.1, 3);
        assert!(w.requested().is_empty());
        // No monitor sample (process gone or no broker): never idle.
        let mut w = World::new(AgentKind::ClaudeCode);
        w.idle_before();
        w.claude(BASH_CALL, 3.0);
        for i in 0..5 {
            w.watcher
                .tick(&w.mgr, &HashMap::new(), w.at(10.0 + i as f32));
        }
        assert!(w.requested().is_empty());
    }

    #[tokio::test]
    async fn codex_function_call_without_output_is_inferred() {
        let codex = vca_adapters::codex::rollout::parse_line;
        let mut w = World::new(AgentKind::Codex);
        w.idle_before();
        w.feed(codex, CODEX_CALL, 3.0);
        w.tick(10.0, 0.2, 3);
        let requested = w.requested();
        assert_eq!(requested.len(), 1);
        assert_eq!(requested[0].id, "codex:abc:inferred-call_7");
        assert_eq!(requested[0].prompt, "Allow shell: cargo publish?");
        w.feed(codex, CODEX_OUTPUT, 11.0);
        assert_eq!(w.pending_id(), None);
    }

    #[test]
    fn inferred_flag_is_optional_on_the_wire() {
        let plain: Decision =
            serde_json::from_str(r#"{"id":"d","prompt":"p","options":[]}"#).unwrap();
        assert!(!plain.inferred);
        assert!(!serde_json::to_string(&plain).unwrap().contains("inferred"));
        let inferred = Decision {
            inferred: true,
            ..plain
        };
        assert!(
            serde_json::to_string(&inferred)
                .unwrap()
                .contains(r#""inferred":true"#)
        );
        assert_eq!(inferred_tool_id(BASH_ID), Some("toolu_1"));
        assert_eq!(inferred_tool_id("claude:abc:toolu_1"), None);
    }

    /// End to end through the real transcript tailer on a fixture file.
    #[tokio::test]
    async fn the_tailer_tracks_calls_of_a_transcript_file() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.jsonl");
        std::fs::write(&path, format!("{USER_PROMPT}\n{BASH_CALL}\n")).unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, AgentKind::ClaudeCode, Some(path.clone()));
        mgr.follow_transcript(
            &s,
            SessionLocator {
                agent_session_id: None,
                transcript_path: Some(path.clone()),
                project_dir: None,
            },
        );
        let wait_for = |want_open: bool| {
            let s = Arc::clone(&s);
            async move {
                for _ in 0..100 {
                    if lock(&s.open_tools).is_empty() != want_open {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                false
            }
        };
        assert!(wait_for(true).await, "the call is tracked");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{BASH_RESULT}").unwrap();
        drop(file);
        assert!(wait_for(false).await, "the result closes it");
    }
}
