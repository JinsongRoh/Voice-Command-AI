//! Session manager: managed sessions (a PTY, an attach server and broker
//! registration per session), observed sessions found by discovery, and the
//! per-session event logs behind the API.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use vca_adapters::{AgentAdapter, HeuristicMatch, HistoryItem, SessionLocator, TerminalTarget};
use vca_broker::Broker;
use vca_broker::contain::Scope;
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ControlTier, Decision, DecisionOption, Error, Input, LaunchRequest,
    PendingDecisionInfo, SessionAction, SessionEvent, SessionId, SessionInfo, SessionOrigin,
    SessionState, ShellInfo, TerminalPrompt, UnixMillis, now_millis, validate_session_name,
};
use vca_pty::{AttachServer, PtySession, PtySize, SpawnOptions};

use crate::cloud::CloudHandle;
use crate::control::{CONTROL_ENV, HOOK_TOKEN_ENV, REMOTE_SHELL_ENV, RunRequest, SESSION_ENV};
use crate::events::{EventLog, LogLimits};
use crate::names::{MANAGED_PREFIX, NameStore};
use crate::order::{ORDER_FILE, OrderKeys, OrderStore};
use crate::util::{lock, truncate_chars};
use crate::workspace::WorkspaceRoots;

/// Lines of screen output shown on a session card.
const PREVIEW_LINES: usize = 3;
const PREVIEW_CHARS: usize = 240;
/// Output newer than this counts as "the agent is working".
const RECENT_OUTPUT: Duration = Duration::from_millis(1500);
const DEFAULT_SIZE: PtySize = PtySize {
    cols: 120,
    rows: 40,
};

/// Settings of the session manager.
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    pub limits: LogLimits,
    /// How long exited sessions stay listed.
    pub exited_retention: Duration,
    pub pause_scope: Scope,
    pub heuristics_interval: Duration,
    /// Set as [`CONTROL_ENV`] in managed sessions so hooks reach this vcad.
    pub control_endpoint: PathBuf,
    /// Directory for attach sockets (Unix). `None` uses the per-user default.
    pub attach_dir: Option<PathBuf>,
    pub workspace_roots: WorkspaceRoots,
    /// Where user-chosen session names are kept (`session-names.json`).
    /// `None` keeps them in memory only. The card order the user chose
    /// ([`crate::order`]) is kept next to it.
    pub session_names: Option<PathBuf>,
    /// Development/testing only: let client `Launch` start `Generic` agents.
    pub allow_generic_launch: bool,
}

/// Change notifications for session cards.
#[derive(Debug, Clone)]
pub enum Update {
    Changed(Box<SessionInfo>),
    Removed(SessionId),
}

/// A decision the agent is waiting on, with how to answer it.
#[derive(Debug, Clone)]
pub(crate) struct PendingDecision {
    pub decision: Decision,
    /// Press Enter after typing the option id (line-based prompts such as
    /// `[y/n]`); numbered menus select on the key alone.
    pub submit: bool,
    /// Created by output heuristics rather than a hook.
    pub heuristic: bool,
    /// When the agent asked.
    pub at: UnixMillis,
    /// The agent's tool call this is about, when known.
    pub tool_use_id: Option<String>,
}

/// Between the text and Enter in a terminal VCA did not start, where the
/// check before Enter has to run outside the terminal's own tool.
const OBSERVED_SUBMIT_DELAY: Duration = Duration::from_millis(150);

/// How long a new managed agent session waits for its first hook event
/// before VCA looks for its transcript itself (the hooks may be missing from
/// the agent's settings: another tool can rewrite that file).
const TRANSCRIPT_FALLBACK_DELAY: Duration = Duration::from_secs(4);
/// How often it looks again until the transcript appears (Claude Code
/// writes it with the first message).
const TRANSCRIPT_FALLBACK_POLL: Duration = Duration::from_secs(3);

/// How long "Remove from VCA" waits for a stopped agent to exit.
pub const STOP_WAIT: Duration = Duration::from_secs(10);

struct InputJob {
    input: Input,
    done: oneshot::Sender<Result<(), Error>>,
}

pub(crate) struct Managed {
    pty: Mutex<PtySession>,
    attach: Mutex<Option<AttachServer>>,
    endpoint: PathBuf,
    adapter: Arc<dyn AgentAdapter>,
    spawn: SpawnOptions,
    /// Proves a hook event comes from inside this session
    /// ([`HOOK_TOKEN_ENV`]). A restart keeps it.
    hook_token: String,
    /// Bumped on every (re)start so stale exit notifications are ignored.
    generation: AtomicU64,
    last_output: Mutex<Instant>,
    /// The processes this session started (Windows Job Object).
    job: Mutex<Option<ProcessJob>>,
}

impl Managed {
    /// Is `token` this session's hook token?
    pub(crate) fn accepts_hook_token(&self, token: Option<&str>) -> bool {
        use subtle::ConstantTimeEq;
        token.is_some_and(|t| bool::from(t.as_bytes().ct_eq(self.hook_token.as_bytes())))
    }

    /// Ends the agent and everything it started: the Job Object first (on
    /// Windows), then the PTY child. Both act on handles VCA owns, never on
    /// pids it merely observed.
    fn end(&self) -> Result<(), vca_pty::PtyError> {
        if let Some(job) = lock(&self.job).as_ref() {
            job.terminate();
        }
        lock(&self.pty).kill()
    }
}

/// Groups a managed agent with the processes it starts, so that stopping it
/// reaches exactly those processes.
#[cfg(windows)]
pub(crate) struct ProcessJob(vca_broker::contain::JobGroup);

#[cfg(windows)]
impl ProcessJob {
    /// Creates the session's job first and starts the PTY child already
    /// inside it (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), so nothing the child
    /// starts can escape the job. If Windows refuses the attribute the
    /// child is assigned right after spawn instead (logged; processes it
    /// started in those milliseconds are then not covered). The pid cannot
    /// have been reused because the PTY still holds the child's handle.
    fn spawn(opts: SpawnOptions) -> Result<(PtySession, Option<Self>), vca_pty::PtyError> {
        use std::os::windows::io::AsHandle;
        let job = match vca_broker::contain::JobGroup::new() {
            Ok(job) => job,
            Err(e) => {
                tracing::warn!("cannot create a job for the session: {e}");
                return Ok((PtySession::spawn(opts)?, None));
            }
        };
        let (pty, in_job) = PtySession::spawn_in_job(opts, job.as_handle())?;
        if in_job {
            return Ok((pty, Some(Self(job))));
        }
        tracing::warn!("the session job is assigned after spawn (job attribute refused)");
        match pty.pid().map(|pid| job.assign(pid)) {
            Some(Ok(())) => Ok((pty, Some(Self(job)))),
            Some(Err(e)) => {
                tracing::warn!("cannot assign the session job: {e}");
                Ok((pty, None))
            }
            None => Ok((pty, None)),
        }
    }

    fn terminate(&self) {
        if let Err(e) = self.0.terminate(1) {
            tracing::warn!("terminating the session job failed: {e}");
        }
    }
}

#[cfg(not(windows))]
pub(crate) struct ProcessJob;

#[cfg(not(windows))]
impl ProcessJob {
    fn spawn(opts: SpawnOptions) -> Result<(PtySession, Option<Self>), vca_pty::PtyError> {
        Ok((PtySession::spawn(opts)?, None))
    }

    fn terminate(&self) {}
}

pub(crate) struct Observed {
    pub terminal: Option<TerminalTarget>,
    /// What identifies the agent session across vcad restarts when the
    /// agent's own session id is unknown.
    pub identity: ObservedIdentity,
}

/// Stable identity of an observed session, used to key its custom name.
#[derive(Debug, Clone, Default)]
pub(crate) struct ObservedIdentity {
    pub transcript_path: Option<PathBuf>,
    /// Start time of the root process (as the process table reports it);
    /// with the pid it identifies the process across vcad restarts.
    pub process_start: Option<u64>,
}

pub(crate) enum Kind {
    Managed(Box<Managed>),
    Observed(Observed),
    /// A cloud coding agent session; no local process.
    Cloud(Box<CloudHandle>),
}

/// One session card and everything behind it.
pub struct Session {
    pub(crate) id: SessionId,
    /// Insertion order: sessions created in the same millisecond keep it.
    seq: u64,
    info: Mutex<SessionInfo>,
    log: Arc<Mutex<EventLog>>,
    pub(crate) kind: Kind,
    pub(crate) pending: Mutex<Option<PendingDecision>>,
    /// A decision held back until it outlives the grace period (see
    /// [`crate::hooks::DECISION_GRACE`]); invisible and silent until then.
    pub(crate) deferred: Mutex<crate::hooks::DeferredQueue>,
    /// Recent shell tool calls, to tie a permission request to its call.
    pub(crate) recent_calls: Mutex<crate::hooks::RecentCalls>,
    /// Hook decision ids seen so far with their reuse generation; keeps
    /// repeated deliveries idempotent and separates a later identical request.
    pub(crate) decision_gens: Mutex<crate::hooks::DecisionGens>,
    /// True once a hook reported on this session; output heuristics then
    /// no longer drive its state.
    pub(crate) hook_seen: AtomicBool,
    pub(crate) agent_session_id: Mutex<Option<String>>,
    transcript_started: AtomicBool,
    /// The transcript being followed and its task: a managed agent that
    /// starts a new conversation (`/clear`, a restart) writes a new file,
    /// which replaces it (see [`SessionManager::follow_transcript`]).
    followed: Mutex<Option<(PathBuf, tokio::task::AbortHandle)>>,
    /// Tool calls in the transcript without a result yet (see
    /// [`crate::observed_watch`]).
    pub(crate) open_tools: Mutex<crate::observed_watch::OpenTools>,
    /// A question form the transcript shows as unanswered (see
    /// [`crate::questions`]).
    pub(crate) open_question: Mutex<Option<crate::questions::OpenQuestion>>,
    /// The question form (decision id) whose answers were typed into the
    /// terminal: it is never typed into twice (see [`crate::questions`]).
    pub(crate) typed_answer: Mutex<Option<String>>,
    /// Sub-agents the agent runs now (see [`crate::subagents`]).
    pub(crate) subagents: Mutex<vca_adapters::SubagentTracker>,
    /// Permission mode and the question of the last turn (see
    /// [`crate::asks`]).
    pub(crate) turn: Mutex<crate::asks::TurnState>,
    /// Turn progress bookkeeping (see [`crate::progress`]).
    pub(crate) progress: Mutex<crate::progress::ProgressState>,
    input: mpsc::Sender<InputJob>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

/// Inputs that may wait for delivery to one session; more are refused
/// with an error rather than queued without bound.
pub const INPUT_QUEUE: usize = 32;
/// Events an exited session keeps in its log (and their size) while it
/// stays listed: the end of the conversation, not the whole buffer.
const EXITED_LOG_EVENTS: usize = crate::events::MAX_PAGE_EVENTS;
const EXITED_LOG_BYTES: usize = 512 * 1024;
/// Session updates a subscriber may fall behind by; one that lags reloads
/// the list (see [`Update`] receivers).
const UPDATES_CAPACITY: usize = 128;

impl Drop for Session {
    fn drop(&mut self) {
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
    }
}

impl Session {
    pub fn info(&self) -> SessionInfo {
        let mut info = lock(&self.info).clone();
        info.pending_decision = lock(&self.pending).as_ref().map(|p| PendingDecisionInfo {
            decision: p.decision.clone(),
            at: p.at,
        });
        info.question = lock(&self.turn).question.as_ref().map(|q| q.info.clone());
        info
    }

    pub fn log(&self) -> &Arc<Mutex<EventLog>> {
        &self.log
    }

    pub fn agent(&self) -> Option<AgentKind> {
        match &lock(&self.info).origin {
            SessionOrigin::Local { agent, .. } => Some(agent.clone()),
            SessionOrigin::Cloud { .. } => None,
        }
    }

    pub fn project_dir(&self) -> Option<PathBuf> {
        match &lock(&self.info).origin {
            SessionOrigin::Local { project_dir, .. } => Some(project_dir.clone()),
            SessionOrigin::Cloud { .. } => None,
        }
    }

    /// The PTY of a managed session.
    pub(crate) fn pty(&self) -> Option<PtySession> {
        self.managed().map(|m| lock(&m.pty).clone())
    }

    /// The text on the terminal screen of a managed session.
    pub(crate) fn screen_text(&self) -> Option<String> {
        let m = self.managed()?;
        let pty = lock(&m.pty).clone();
        pty.screen_snapshot().map(|s| s.text())
    }

    /// True for the conductor's own session ([`SessionInfo::conductor`]).
    pub fn is_conductor(&self) -> bool {
        lock(&self.info).conductor
    }

    /// True for remote shell sessions ([`AgentKind::Shell`]).
    pub fn is_shell(&self) -> bool {
        matches!(
            &lock(&self.info).origin,
            SessionOrigin::Local {
                agent: AgentKind::Shell(_),
                ..
            }
        )
    }

    pub fn pid(&self) -> Option<u32> {
        match &lock(&self.info).origin {
            SessionOrigin::Local { pid, .. } => *pid,
            SessionOrigin::Cloud { .. } => None,
        }
    }

    /// The attach endpoint of a managed session.
    pub fn attach_endpoint(&self) -> Option<&Path> {
        match &self.kind {
            Kind::Managed(m) => Some(&m.endpoint),
            Kind::Observed(_) | Kind::Cloud(_) => None,
        }
    }

    pub(crate) fn managed(&self) -> Option<&Managed> {
        match &self.kind {
            Kind::Managed(m) => Some(m),
            Kind::Observed(_) | Kind::Cloud(_) => None,
        }
    }

    pub(crate) fn cloud(&self) -> Option<&CloudHandle> {
        match &self.kind {
            Kind::Cloud(c) => Some(c),
            Kind::Managed(_) | Kind::Observed(_) => None,
        }
    }

    pub(crate) fn push(&self, make: impl FnOnce(u64) -> SessionEvent) -> SessionEvent {
        lock(&self.log).push(make)
    }

    /// The key under which the session's custom name is stored. Managed
    /// and cloud sessions use their session id. Observed sessions get a new
    /// session id on every discovery, so they use the agent kind plus the
    /// agent's own session id, else its transcript path, else the project
    /// directory and the root process (pid and start time).
    pub(crate) fn name_key(&self) -> String {
        match &self.kind {
            Kind::Managed(_) => format!("{MANAGED_PREFIX}{}", self.id),
            Kind::Cloud(_) => format!("cloud:{}", self.id),
            Kind::Observed(o) => {
                let (agent, project_dir, pid) = match &lock(&self.info).origin {
                    SessionOrigin::Local {
                        agent,
                        project_dir,
                        pid,
                    } => (agent_slug(agent), project_dir.clone(), *pid),
                    SessionOrigin::Cloud { .. } => (String::new(), PathBuf::new(), None),
                };
                if let Some(id) = lock(&self.agent_session_id).as_deref() {
                    return format!("observed:{agent}:session:{id}");
                }
                if let Some(path) = &o.identity.transcript_path {
                    return format!("observed:{agent}:transcript:{}", path.display());
                }
                format!(
                    "observed:{agent}:process:{}:{}:{}",
                    project_dir.display(),
                    pid.unwrap_or(0),
                    o.identity.process_start.unwrap_or(0)
                )
            }
        }
    }

    /// The stable keys of the session's place in the card order (see
    /// [`crate::order`]): the agent's conversation, the session id (only
    /// within this run), then the restore identity of agent, folder and
    /// name, then agent and folder.
    pub(crate) fn order_keys(&self) -> OrderKeys {
        let conversation = lock(&self.agent_session_id).clone();
        let session = Some(format!("id:{}", self.id));
        let info = lock(&self.info);
        match &info.origin {
            SessionOrigin::Local {
                agent, project_dir, ..
            } => {
                let agent = agent_slug(agent);
                let folder = crate::restore::matching::folder_key(project_dir);
                OrderKeys {
                    conversation: conversation.map(|id| format!("conv:{agent}:{id}")),
                    session,
                    place: Some(format!("place:{agent}:{folder}:{}", info.display_name)),
                    folder: Some(format!("folder:{agent}:{folder}")),
                }
            }
            SessionOrigin::Cloud {
                provider,
                remote_id,
                ..
            } => OrderKeys {
                conversation: Some(format!("cloud:{provider:?}:{remote_id}")),
                session,
                place: None,
                folder: None,
            },
        }
    }

    /// True when the session's name key can come back for a later session
    /// (the agent session can be resumed), so its name is kept after the
    /// session ends.
    fn name_outlives_session(&self) -> bool {
        match &self.kind {
            Kind::Observed(o) => {
                lock(&self.agent_session_id).is_some() || o.identity.transcript_path.is_some()
            }
            Kind::Managed(_) | Kind::Cloud(_) => false,
        }
    }

    /// The agent's own session id, when known.
    pub fn agent_session_id(&self) -> Option<String> {
        lock(&self.agent_session_id).clone()
    }

    /// The agent's transcript file of an observed session: the one
    /// followed now (a new conversation after `/clear` writes a new file),
    /// else the one discovery found.
    pub(crate) fn transcript_path(&self) -> Option<PathBuf> {
        match &self.kind {
            Kind::Observed(o) => self
                .followed_transcript()
                .or_else(|| o.identity.transcript_path.clone()),
            Kind::Managed(_) | Kind::Cloud(_) => None,
        }
    }

    /// The transcript file being followed, if any.
    pub(crate) fn followed_transcript(&self) -> Option<PathBuf> {
        lock(&self.followed).as_ref().map(|(p, _)| p.clone())
    }

    pub(crate) fn add_task(&self, task: JoinHandle<()>) {
        let mut tasks = lock(&self.tasks);
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }
}

pub(crate) struct Inner {
    pub(crate) cfg: ManagerConfig,
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
    updates: broadcast::Sender<Update>,
    pub(crate) broker: Option<Arc<Broker>>,
    names: NameStore,
    order: OrderStore,
    order_updates: broadcast::Sender<SessionOrder>,
    /// The VCA hooks are missing from Claude Code's user settings (see
    /// [`crate::hooks_watch`]); false until a check says so.
    claude_hooks_missing: AtomicBool,
}

/// The card order as clients see it ([`vca_core::ServerMsg::SessionOrder`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOrder {
    /// Every listed session, first card first.
    pub session_ids: Vec<SessionId>,
    /// The user chose it.
    pub custom: bool,
}

/// Owns all sessions. Cheap to clone.
#[derive(Clone)]
pub struct SessionManager {
    pub(crate) inner: Arc<Inner>,
}

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

fn not_found(id: &SessionId) -> Error {
    api_error(ErrorCode::NotFound, format!("no session {id}"))
}

fn shell_needs_open_shell() -> Error {
    api_error(
        ErrorCode::InvalidRequest,
        "shells are opened with open_shell (TOTP required)",
    )
}

/// Guesses the agent from the command a user typed.
pub fn detect_agent(program: &str) -> AgentKind {
    // Split on both separators so Windows paths work on every platform.
    let file = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match stem.as_str() {
        "claude" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        "gemini" => AgentKind::GeminiCli,
        "antigravity" => AgentKind::Antigravity,
        "grok" => AgentKind::Grok,
        "cursor-agent" => AgentKind::CursorCli,
        "copilot" => AgentKind::CopilotCli,
        "aider" => AgentKind::Aider,
        "opencode" => AgentKind::Opencode,
        "qwen" => AgentKind::QwenCode,
        _ => AgentKind::Generic(stem),
    }
}

fn agent_label(agent: &AgentKind) -> String {
    match agent {
        AgentKind::ClaudeCode => "Claude Code".into(),
        AgentKind::Codex => "Codex".into(),
        AgentKind::GeminiCli => "Gemini CLI".into(),
        AgentKind::Antigravity => "Antigravity".into(),
        AgentKind::Grok => "Grok".into(),
        AgentKind::CursorCli => "Cursor CLI".into(),
        AgentKind::CopilotCli => "Copilot CLI".into(),
        AgentKind::Aider => "Aider".into(),
        AgentKind::Opencode => "opencode".into(),
        AgentKind::QwenCode => "Qwen Code".into(),
        AgentKind::Generic(name) => name.clone(),
        AgentKind::Shell(name) => match name.as_str() {
            "powershell" => "PowerShell".into(),
            "pwsh" => "PowerShell 7".into(),
            "cmd" => "Command Prompt".into(),
            other => other.to_owned(),
        },
    }
}

/// The adapter for an agent kind. Shells have no agent adapter: they get a
/// generic one (output heuristics only) that keeps the shell kind.
pub(crate) fn adapter_for(agent: &AgentKind) -> Arc<dyn AgentAdapter> {
    match agent {
        AgentKind::Shell(name) => {
            let mut spec = vca_adapters::generic::AgentSpec::generic(name);
            spec.kind = agent.clone();
            Arc::new(vca_adapters::generic::GenericAdapter::new(spec))
        }
        other => vca_adapters::adapter_for(other),
    }
}

/// Agent kind as used in name keys, e.g. `claude_code` or `generic/foo`.
fn agent_slug(agent: &AgentKind) -> String {
    match agent {
        AgentKind::ClaudeCode => "claude_code".into(),
        AgentKind::Codex => "codex".into(),
        AgentKind::GeminiCli => "gemini_cli".into(),
        AgentKind::Antigravity => "antigravity".into(),
        AgentKind::Grok => "grok".into(),
        AgentKind::CursorCli => "cursor_cli".into(),
        AgentKind::CopilotCli => "copilot_cli".into(),
        AgentKind::Aider => "aider".into(),
        AgentKind::Opencode => "opencode".into(),
        AgentKind::QwenCode => "qwen_code".into(),
        AgentKind::Generic(name) => format!("generic/{name}"),
        AgentKind::Shell(name) => format!("shell/{name}"),
    }
}

/// Default card name: "<project folder> · <agent>".
pub(crate) fn default_name(agent: &AgentKind, project_dir: &Path) -> String {
    let folder = project_dir
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| project_dir.display().to_string());
    format!("{folder} · {}", agent_label(agent))
}

/// Sets the card name: the user's `custom` name, or the automatic one.
/// The automatic name is remembered the first time.
fn set_name(info: &mut SessionInfo, custom: Option<String>) {
    let auto = info
        .auto_name
        .get_or_insert_with(|| info.display_name.clone())
        .clone();
    info.custom_name = custom.is_some();
    info.display_name = custom.unwrap_or(auto);
}

/// Whether a heuristic decision is answered with Enter after the option.
fn options_need_submit(options: &[DecisionOption]) -> bool {
    !options
        .iter()
        .all(|o| !o.id.is_empty() && o.id.chars().all(|c| c.is_ascii_digit()))
}

impl SessionManager {
    pub fn new(cfg: ManagerConfig, broker: Option<Arc<Broker>>) -> Self {
        let (updates, _) = broadcast::channel(UPDATES_CAPACITY);
        let (order_updates, _) = broadcast::channel(64);
        let names = NameStore::open(cfg.session_names.clone());
        let order = OrderStore::open(
            cfg.session_names
                .as_deref()
                .map(|names| names.with_file_name(ORDER_FILE)),
        );
        Self {
            inner: Arc::new(Inner {
                cfg,
                sessions: Mutex::new(HashMap::new()),
                updates,
                broker,
                names,
                order,
                order_updates,
                claude_hooks_missing: AtomicBool::new(false),
            }),
        }
    }

    pub fn broker(&self) -> Option<&Arc<Broker>> {
        self.inner.broker.as_ref()
    }

    pub fn subscribe_updates(&self) -> broadcast::Receiver<Update> {
        self.inner.updates.subscribe()
    }

    /// Every session, in the card order: the order the user chose, then
    /// the rest oldest first.
    pub fn list(&self) -> Vec<SessionInfo> {
        self.ordered().iter().map(|s| s.info()).collect()
    }

    /// The sessions in the card order (see [`crate::order`]).
    fn ordered(&self) -> Vec<Arc<Session>> {
        let mut all = self.all();
        all.sort_by_cached_key(|s| (lock(&s.info).created_at, s.seq));
        let keys: Vec<OrderKeys> = all.iter().map(|s| s.order_keys()).collect();
        self.inner
            .order
            .arrange(&keys)
            .into_iter()
            .map(|i| Arc::clone(&all[i]))
            .collect()
    }

    /// The card order clients show.
    pub fn session_order(&self) -> SessionOrder {
        SessionOrder {
            session_ids: self.ordered().iter().map(|s| s.id.clone()).collect(),
            custom: self.inner.order.is_custom(),
        }
    }

    /// Order changes, for pushing to clients.
    pub fn subscribe_order(&self) -> broadcast::Receiver<SessionOrder> {
        self.inner.order_updates.subscribe()
    }

    fn announce_order(&self) -> SessionOrder {
        let order = self.session_order();
        let _ = self.inner.order_updates.send(order.clone());
        order
    }

    /// Puts the cards in the order of `ids` (first card first), stores it
    /// and announces it to every client. Sessions not in `ids` follow in
    /// their current order; unknown and repeated ids are ignored.
    pub fn set_session_order(&self, ids: &[SessionId]) -> Result<SessionOrder, Error> {
        let current = self.ordered();
        let mut picked: Vec<Arc<Session>> = Vec::with_capacity(current.len());
        for id in ids {
            if let Some(s) = current.iter().find(|s| &s.id == id)
                && !picked.iter().any(|p| Arc::ptr_eq(p, s))
            {
                picked.push(Arc::clone(s));
            }
        }
        for s in &current {
            if !picked.iter().any(|p| Arc::ptr_eq(p, s)) {
                picked.push(Arc::clone(s));
            }
        }
        let keys: Vec<OrderKeys> = picked.iter().map(|s| s.order_keys()).collect();
        self.inner.order.set(&keys).map_err(|e| {
            api_error(
                ErrorCode::Internal,
                format!("cannot save the session order: {e}"),
            )
        })?;
        Ok(self.announce_order())
    }

    /// Goes back to the default order (oldest first) and announces it.
    pub fn reset_session_order(&self) -> Result<SessionOrder, Error> {
        self.inner.order.reset().map_err(|e| {
            api_error(
                ErrorCode::Internal,
                format!("cannot save the session order: {e}"),
            )
        })?;
        Ok(self.announce_order())
    }

    /// Lets the stored order learn the keys sessions have now (a new name,
    /// a newly known conversation), so they keep their place.
    fn learn_order(&self) {
        if self.inner.order.is_custom() {
            let _ = self.ordered();
        }
    }

    pub fn get(&self, id: &SessionId) -> Option<Arc<Session>> {
        lock(&self.inner.sessions).get(id).cloned()
    }

    /// A managed session's hook token. In-process only: tests stand in for
    /// the session's own `vca hook` with it.
    #[doc(hidden)]
    pub fn hook_token(&self, id: &SessionId) -> Option<String> {
        Some(self.get(id)?.managed()?.hook_token.clone())
    }

    /// The process ids of the managed agent sessions and of the shells.
    pub fn session_pids(&self) -> (Vec<u32>, Vec<u32>) {
        let mut agents = Vec::new();
        let mut shells = Vec::new();
        for session in self.all() {
            if session.managed().is_none() {
                continue;
            }
            if let Some(pid) = session.pid() {
                if session.is_shell() {
                    shells.push(pid);
                } else {
                    agents.push(pid);
                }
            }
        }
        (agents, shells)
    }

    pub(crate) fn all(&self) -> Vec<Arc<Session>> {
        lock(&self.inner.sessions).values().cloned().collect()
    }

    fn require(&self, id: &SessionId) -> Result<Arc<Session>, Error> {
        self.get(id).ok_or_else(|| not_found(id))
    }

    /// Announces the session's current info to every client, while it is
    /// listed: a session's own tasks may still hold it for a moment after
    /// its removal, and a late announcement would add the removed card to
    /// every client again. (A session not listed yet is announced in full
    /// when it is inserted.)
    pub(crate) fn notify(&self, session: &Session) {
        let listed = lock(&self.inner.sessions)
            .get(&session.id)
            .is_some_and(|s| std::ptr::eq(Arc::as_ptr(s), session));
        if !listed {
            return;
        }
        let _ = self
            .inner
            .updates
            .send(Update::Changed(Box::new(session.info())));
    }

    /// Changes a session's state, logging and announcing it.
    pub(crate) fn set_state(&self, session: &Session, state: SessionState) {
        let exited = matches!(state, SessionState::Exited { .. });
        {
            let mut info = lock(&session.info);
            if info.state == state {
                return;
            }
            // An exited session stays exited.
            if matches!(info.state, SessionState::Exited { .. })
                && !matches!(state, SessionState::Starting)
            {
                return;
            }
            info.state = state.clone();
            info.last_activity = now_millis();
            if exited {
                // Its sub-agents ended with it.
                info.subagents = 0;
            }
            // Logged before another change can take the state: the log's
            // last state is the session's state.
            session.push(|seq| SessionEvent::StateChanged {
                seq,
                at: now_millis(),
                state,
            });
        }
        if exited {
            lock(&session.subagents).reset();
            // Nothing can answer an ended process: its waits and its open
            // question leave the alerts with it.
            lock(&session.pending).take();
            lock(&session.deferred).clear();
            lock(&session.turn).question.take();
        }
        if exited {
            // The card stays listed for a while; its buffers do not.
            lock(&session.log).trim(EXITED_LOG_EVENTS, EXITED_LOG_BYTES);
            if let Some(m) = session.managed() {
                lock(&m.pty).release_buffers();
            }
            // Its turn and its problems ended with it.
            let mut info = lock(&session.info);
            info.problem = None;
            if let Some(turn) = info.progress.as_mut().filter(|t| t.ended_at.is_none()) {
                turn.ended_at = Some(now_millis().max(turn.started_at));
                turn.activity = None;
            }
        }
        self.notify(session);
    }

    /// Records whether the VCA hooks are missing from Claude Code's user
    /// settings and updates the problem of every session it changes.
    pub(crate) fn set_claude_hooks_missing(&self, missing: bool) {
        let before = self
            .inner
            .claude_hooks_missing
            .swap(missing, Ordering::SeqCst);
        if before != missing {
            for session in self.all() {
                self.refresh_problem(&session);
            }
        }
    }

    /// Whether the last check found the Claude Code hooks missing.
    pub(crate) fn claude_hooks_missing(&self) -> bool {
        self.inner.claude_hooks_missing.load(Ordering::SeqCst)
    }

    /// Updates [`SessionInfo::problem`]: a Claude Code session that never
    /// reported through a hook while the hooks are missing from the
    /// settings shows its progress late (from the transcript only).
    pub(crate) fn refresh_problem(&self, session: &Session) {
        let missing = self.claude_hooks_missing()
            && session.agent() == Some(AgentKind::ClaudeCode)
            && !session.hook_seen.load(Ordering::SeqCst);
        self.update_info(session, |info| {
            let problem = (missing && !matches!(info.state, SessionState::Exited { .. }))
                .then(|| vca_core::SESSION_PROBLEM_HOOKS_MISSING.to_owned());
            let changed = info.problem != problem;
            info.problem = problem;
            changed
        });
    }

    /// Counts an unread notification. The conductor's own session never
    /// counts: its conversation lives in the conductor view, and its
    /// decisions reach people through `pending_decision`.
    pub(crate) fn add_unread(&self, session: &Session) {
        let mut info = lock(&session.info);
        if !info.conductor {
            info.unread += 1;
        }
    }

    /// Marks `id` as the conductor's own session (or not) and announces it.
    pub fn set_conductor(&self, id: &SessionId, conductor: bool) -> Result<(), Error> {
        let session = self.require(id)?;
        self.update_info(&session, |info| {
            let changed = info.conductor != conductor;
            info.conductor = conductor;
            if conductor {
                info.unread = 0;
            }
            changed
        });
        Ok(())
    }

    pub(crate) fn update_info(&self, session: &Session, f: impl FnOnce(&mut SessionInfo) -> bool) {
        let changed = f(&mut lock(&session.info));
        if changed {
            self.notify(session);
        }
    }

    /// Records a decision the agent waits on and announces it.
    pub(crate) fn request_decision(&self, session: &Session, pending: PendingDecision) {
        let decision = pending.decision.clone();
        *lock(&session.pending) = Some(pending);
        // The decision asks the person now: an open plain question of an
        // earlier turn would be a second alert for the same wait.
        lock(&session.turn).question.take();
        session.push(|seq| SessionEvent::DecisionRequested {
            seq,
            at: now_millis(),
            decision,
        });
        self.add_unread(session);
        self.set_state(session, SessionState::AwaitingDecision);
        self.notify(session);
    }

    pub fn mark_read(&self, id: &SessionId) -> Result<(), Error> {
        let session = self.require(id)?;
        self.update_info(&session, |info| std::mem::take(&mut info.unread) != 0);
        Ok(())
    }

    /// Removes an ended session now, as its retention timer would later
    /// ([`Self::remove`]: the list entry, its buffers once the last
    /// reference goes, its broker registration), and tells every client.
    /// `NotFound` for an unknown session, `Conflict` for one that has not
    /// exited. Returns what it was.
    pub fn dismiss(&self, id: &SessionId) -> Result<SessionInfo, Error> {
        let session = self.require(id)?;
        let info = session.info();
        if !matches!(info.state, SessionState::Exited { .. }) {
            return Err(api_error(
                ErrorCode::Conflict,
                "the session is still running; stop it first",
            ));
        }
        self.remove(id);
        Ok(info)
    }

    /// "Remove from VCA": [`Self::dismiss`], after stopping a running
    /// session VCA started and waiting (up to [`STOP_WAIT`]) until it has
    /// exited. Removing only once the exit was announced keeps the card
    /// from coming back: an exit announced after the removal would add it
    /// to every client again, and an agent still alive outside a managed
    /// session would be picked up by discovery as a PC window.
    ///
    /// `Unsupported` for a running session VCA did not start (VCA never
    /// ends those), `Conflict` for the conductor's own session (stopping
    /// the conductor ends it), `Unavailable` when the agent did not exit in
    /// time (it was told to stop; a later removal finishes the job).
    /// Returns what it was and whether it was stopped here.
    pub async fn stop_and_dismiss(&self, id: &SessionId) -> Result<(SessionInfo, bool), Error> {
        let session = self.require(id)?;
        let info = session.info();
        if matches!(info.state, SessionState::Exited { .. }) {
            return self.dismiss(id).map(|info| (info, false));
        }
        if info.conductor {
            return Err(api_error(
                ErrorCode::Conflict,
                "this is the conductor's own session; stop the conductor instead",
            ));
        }
        let Some(m) = session.managed() else {
            let reason = if session.cloud().is_some() {
                "this session runs at its cloud provider; stop it first, then remove it"
            } else {
                "this session runs in a terminal window on the PC; close that window, then remove it"
            };
            return Err(api_error(ErrorCode::Unsupported, reason));
        };
        // An agent that exited on its own just now fails the kill; its exit
        // is still announced below.
        if let Err(err) = m.end() {
            tracing::debug!(session = %id, %err, "stopping the session to remove it");
        }
        let exited = tokio::time::timeout(STOP_WAIT, async {
            while !matches!(session.info().state, SessionState::Exited { .. }) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        if exited.is_err() {
            return Err(api_error(
                ErrorCode::Unavailable,
                "the agent did not stop in time; try removing it again",
            ));
        }
        self.dismiss(id).map(|info| (info, true))
    }

    /// Renames a session (`None` or a blank name restores the automatic
    /// name), stores the name and announces the change to every client.
    pub fn rename(&self, id: &SessionId, name: Option<&str>) -> Result<SessionInfo, Error> {
        let name = validate_session_name(name)
            .map_err(|reason| api_error(ErrorCode::InvalidRequest, reason))?;
        let session = self.require(id)?;
        self.learn_order();
        self.inner
            .names
            .set(&session.name_key(), name.clone())
            .map_err(|e| {
                api_error(
                    ErrorCode::Internal,
                    format!("cannot save the session name: {e}"),
                )
            })?;
        set_name(&mut lock(&session.info), name);
        self.learn_order();
        self.notify(&session);
        Ok(session.info())
    }

    /// Re-applies the stored name after the session's name key changed
    /// (an observed agent reported a new session id). A custom name moves
    /// along to the new key; a name already stored for the new key wins.
    pub(crate) fn rekey_name(&self, session: &Session, old_key: &str) {
        self.learn_order();
        let key = session.name_key();
        if key == old_key {
            return;
        }
        let current = {
            let info = lock(&session.info);
            info.custom_name.then(|| info.display_name.clone())
        };
        let name = match self.inner.names.get(&key) {
            Some(name) => Some(name),
            None => {
                if let Some(name) = &current {
                    if let Err(err) = self.inner.names.set(&key, Some(name.clone())) {
                        tracing::warn!(%err, "cannot save the session name");
                    }
                }
                current.clone()
            }
        };
        if name != current {
            set_name(&mut lock(&session.info), name);
            self.notify(session);
        }
    }

    /// Records the automatic name and applies a stored custom name.
    fn apply_saved_name(&self, session: &Session) {
        let saved = self.inner.names.get(&session.name_key());
        set_name(&mut lock(&session.info), saved);
    }

    fn insert(&self, session: Arc<Session>) {
        self.apply_saved_name(&session);
        if self.claude_hooks_missing() && session.agent() == Some(AgentKind::ClaudeCode) {
            lock(&session.info).problem = Some(vca_core::SESSION_PROBLEM_HOOKS_MISSING.to_owned());
        }
        lock(&self.inner.sessions).insert(session.id.clone(), Arc::clone(&session));
        let _ = self
            .inner
            .updates
            .send(Update::Changed(Box::new(session.info())));
        // Clients add a new card last; one with a stored place moves there.
        if self.inner.order.is_custom() {
            self.announce_order();
        }
    }

    pub(crate) fn remove(&self, id: &SessionId) {
        let removed = lock(&self.inner.sessions).remove(id);
        if let Some(session) = removed {
            if !session.name_outlives_session() {
                self.inner.names.forget(&session.name_key());
            }
            // Cloud sessions were never registered with the broker.
            if let (Some(broker), None) = (&self.inner.broker, session.cloud()) {
                broker.unregister_session(id);
                broker.unmonitor_session(id);
            }
            if let Some(m) = session.managed() {
                lock(&m.attach).take();
                let _ = m.end();
            }
            let _ = self.inner.updates.send(Update::Removed(id.clone()));
        }
    }

    fn new_session(&self, id: SessionId, info: SessionInfo, kind: Kind) -> Arc<Session> {
        let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
        static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);
        let session = Arc::new(Session {
            id,
            seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
            info: Mutex::new(info),
            log: Arc::new(Mutex::new(EventLog::new(self.inner.cfg.limits))),
            kind,
            pending: Mutex::new(None),
            deferred: Mutex::new(crate::hooks::DeferredQueue::default()),
            recent_calls: Mutex::new(crate::hooks::RecentCalls::default()),
            decision_gens: Mutex::new(crate::hooks::DecisionGens::default()),
            hook_seen: AtomicBool::new(false),
            agent_session_id: Mutex::new(None),
            transcript_started: AtomicBool::new(false),
            followed: Mutex::new(None),
            open_tools: Mutex::new(crate::observed_watch::OpenTools::default()),
            open_question: Mutex::new(None),
            typed_answer: Mutex::new(None),
            subagents: Mutex::new(vca_adapters::SubagentTracker::default()),
            turn: Mutex::new(crate::asks::TurnState::default()),
            progress: Mutex::new(crate::progress::ProgressState::default()),
            input: input_tx,
            tasks: Mutex::new(Vec::new()),
        });
        let worker = tokio::spawn(input_worker(
            self.clone(),
            Arc::downgrade(&session),
            input_rx,
        ));
        session.add_task(worker);
        session
    }

    fn attach_endpoint(&self, id: &SessionId) -> PathBuf {
        match &self.inner.cfg.attach_dir {
            Some(dir) if cfg!(unix) => dir.join(format!("attach-{id}.sock")),
            _ => vca_pty::attach::default_endpoint(&id.0),
        }
    }

    /// Launches an agent for a client request (SES-04). The project
    /// directory must be inside the workspace roots (SEC-05).
    pub fn launch(&self, req: &LaunchRequest) -> Result<SessionInfo, Error> {
        if req.profile.is_some() {
            return Err(api_error(
                ErrorCode::Unsupported,
                "account profiles are not available yet",
            ));
        }
        if matches!(req.agent, AgentKind::Shell(_)) {
            return Err(shell_needs_open_shell());
        }
        if matches!(req.agent, AgentKind::Generic(_)) && !self.inner.cfg.allow_generic_launch {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                "arbitrary programs can only be started from the PC itself with `vca run`; \
                 open a remote shell instead",
            ));
        }
        let dir = self
            .inner
            .cfg
            .workspace_roots
            .prepare(&req.project_dir, req.create_dir)?;
        let adapter = adapter_for(&req.agent);
        let mut checked = req.clone();
        checked.project_dir = dir.clone();
        // A folder Claude Code was used in before picks up its latest
        // conversation (`--continue`) unless a new one was asked for.
        let continued = (req.agent == AgentKind::ClaudeCode
            && !req.fresh
            && req.profile.is_none()
            && !vca_adapters::claude::ClaudeAdapter::args_choose_conversation(&req.args))
        .then(|| vca_adapters::claude::ClaudeAdapter::new().latest_transcript(&dir))
        .flatten();
        if continued.is_some() {
            checked.args.insert(0, "--continue".to_owned());
        }
        let spec = adapter
            .launch_spec(&checked, None)
            .map_err(|e| api_error(ErrorCode::InvalidRequest, e.to_string()))?;
        let opts = SpawnOptions::new(&spec.program)
            .args(&spec.args)
            .cwd(&spec.cwd)
            .size(DEFAULT_SIZE);
        let opts = spec
            .env
            .iter()
            .fold(opts, |o, (k, v)| o.env(k.as_str(), v.as_str()));
        let name = req
            .display_name
            .clone()
            .unwrap_or_else(|| default_name(&req.agent, &dir));
        let info = self.start_managed(req.agent.clone(), adapter, dir, name, opts, None)?;
        if let Some(locator) = continued {
            self.follow_continued(&info.id, locator);
        }
        Ok(info)
    }

    /// Shows the conversation a `--continue` launch picks up right away:
    /// Claude Code appends to the existing transcript, which the search for
    /// a newly created one would never find. A conversation another session
    /// already shows is left to it.
    fn follow_continued(&self, id: &SessionId, locator: SessionLocator) {
        let Some(session) = self.get(id) else {
            return;
        };
        let owned_elsewhere = locator.agent_session_id.as_ref().is_some_and(|agent_id| {
            self.all()
                .iter()
                .filter(|s| s.id != session.id)
                .any(|s| s.agent_session_id().as_deref() == Some(agent_id.as_str()))
        });
        if owned_elsewhere {
            return;
        }
        if let Some(agent_id) = &locator.agent_session_id {
            self.learn_agent_session_id(&session, agent_id);
        }
        tracing::info!(
            session = %session.id,
            transcript = ?locator.transcript_path,
            "continuing the folder's latest conversation"
        );
        self.follow_transcript(&session, locator);
    }

    /// Runs a command typed in a local terminal under management. The local
    /// channel is owner-only, so the workspace roots do not apply here: the
    /// caller could run the command directly anyway.
    pub fn run(&self, req: &RunRequest) -> Result<SessionInfo, Error> {
        if req.program.trim().is_empty() {
            return Err(api_error(ErrorCode::InvalidRequest, "empty command"));
        }
        let agent = req
            .agent
            .clone()
            .unwrap_or_else(|| detect_agent(&req.program));
        if matches!(agent, AgentKind::Shell(_)) {
            return Err(shell_needs_open_shell());
        }
        let adapter = adapter_for(&agent);
        let size = req
            .size
            .filter(|s| s.cols > 0 && s.rows > 0)
            .map_or(DEFAULT_SIZE, |s| PtySize {
                cols: s.cols,
                rows: s.rows,
            });
        let opts = SpawnOptions::new(&req.program)
            .args(&req.args)
            .cwd(&req.cwd)
            .size(size);
        // The caller's terminal may itself run inside a Claude Code
        // session: its identity is not forwarded (see `child_env`).
        let opts = vca_core::child_env::filter_vars(&req.env)
            .iter()
            .filter(|(k, _)| k != SESSION_ENV && k != CONTROL_ENV && k != HOOK_TOKEN_ENV)
            .fold(opts, |o, (k, v)| o.env(k.as_str(), v.as_str()));
        let name = req
            .display_name
            .clone()
            .unwrap_or_else(|| default_name(&agent, &req.cwd));
        self.start_managed(agent, adapter, req.cwd.clone(), name, opts, None)
    }

    /// Starts a remote shell (checked by [`crate::shell::Shells`]). Unlike
    /// agents, shells do not get the hook variables: agents started inside
    /// the shell must not report as the shell session.
    pub(crate) fn start_shell(
        &self,
        agent: AgentKind,
        project_dir: PathBuf,
        name: String,
        opts: SpawnOptions,
        shell: ShellInfo,
    ) -> Result<SessionInfo, Error> {
        let adapter = adapter_for(&agent);
        self.start_managed(agent, adapter, project_dir, name, opts, Some(shell))
    }

    /// The workspace allow-list (SEC-05).
    pub(crate) fn workspace_roots(&self) -> &WorkspaceRoots {
        &self.inner.cfg.workspace_roots
    }

    fn start_managed(
        &self,
        agent: AgentKind,
        adapter: Arc<dyn AgentAdapter>,
        project_dir: PathBuf,
        name: String,
        opts: SpawnOptions,
        shell: Option<ShellInfo>,
    ) -> Result<SessionInfo, Error> {
        let id = SessionId::new();
        // Never run as the Claude Code session vcad itself may have been
        // started from (see `vca_core::child_env`).
        let opts = vca_core::child_env::inherited_removals()
            .into_iter()
            .fold(opts, SpawnOptions::env_remove);
        let hook_token = crate::security::stepup::random_token();
        let opts = if shell.is_some() {
            opts.env_remove(SESSION_ENV)
                .env_remove(CONTROL_ENV)
                .env_remove(HOOK_TOKEN_ENV)
                .env(REMOTE_SHELL_ENV, "1")
        } else {
            opts.env(SESSION_ENV, id.0.as_str())
                .env(HOOK_TOKEN_ENV, hook_token.as_str())
                .env_remove(REMOTE_SHELL_ENV)
                .env(
                    CONTROL_ENV,
                    crate::control::endpoint_env_value(&self.inner.cfg.control_endpoint),
                )
        };
        let (pty, job) = ProcessJob::spawn(opts.clone())
            .map_err(|e| api_error(ErrorCode::InvalidRequest, e.to_string()))?;
        let endpoint = self.attach_endpoint(&id);
        let attach = AttachServer::bind(&endpoint, pty.clone()).map_err(|e| {
            let _ = pty.kill();
            api_error(ErrorCode::Internal, format!("attach server: {e}"))
        })?;
        let now = now_millis();
        let info = SessionInfo {
            id: id.clone(),
            origin: SessionOrigin::Local {
                agent,
                project_dir,
                pid: pty.pid(),
            },
            display_name: name,
            state: SessionState::Starting,
            control: ControlTier::ManagedPty,
            profile: None,
            created_at: now,
            last_activity: now,
            unread: 0,
            preview: None,
            auto_name: None,
            custom_name: false,
            shell,
            pending_decision: None,
            subagents: 0,
            conductor: false,
            terminal_prompt: None,
            question: None,
            progress: None,
            problem: None,
        };
        let session = self.new_session(
            id,
            info,
            Kind::Managed(Box::new(Managed {
                pty: Mutex::new(pty.clone()),
                attach: Mutex::new(Some(attach)),
                endpoint,
                adapter,
                spawn: opts,
                hook_token,
                generation: AtomicU64::new(0),
                last_output: Mutex::new(Instant::now()),
                job: Mutex::new(job),
            })),
        );
        self.insert(Arc::clone(&session));
        self.watch_pty(&session, pty, 0);
        let finds_transcript = matches!(
            session.agent(),
            Some(AgentKind::ClaudeCode | AgentKind::Codex)
        );
        if !session.is_shell() && finds_transcript {
            if let Some(m) = session.managed() {
                let cwd = m.spawn.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
                self.watch_for_transcript(
                    &session,
                    Arc::clone(&m.adapter),
                    cwd,
                    now,
                    TRANSCRIPT_FALLBACK_DELAY,
                );
            }
        }
        session.add_task(tokio::spawn(heuristics_loop(
            self.clone(),
            Arc::downgrade(&session),
            self.inner.cfg.heuristics_interval,
        )));
        Ok(session.info())
    }

    /// Starts the output and exit tasks of a managed session's PTY.
    fn watch_pty(&self, session: &Arc<Session>, pty: PtySession, generation: u64) {
        if let (Some(broker), Some(pid)) = (&self.inner.broker, pty.pid()) {
            broker.register_session(session.id.clone(), pid);
        }
        let weak = Arc::downgrade(session);
        let mut sub = pty.subscribe_from(0);
        let sized = pty.clone();
        session.add_task(tokio::spawn(async move {
            // Viewers render at the PTY's size, carried in the output
            // (see `term_size`).
            let mut marker = crate::term_size::SizeMarker::default();
            while let Some(ev) = sub.recv().await {
                let vca_pty::OutputEvent::Data(chunk) = ev else {
                    continue;
                };
                let Some(session) = weak.upgrade() else {
                    return;
                };
                let now = Instant::now();
                if let Some(m) = session.managed() {
                    *lock(&m.last_output) = now;
                }
                let at = now_millis();
                lock(&session.info).last_activity = at;
                let data = marker.prefix(sized.size(), now, &chunk.data);
                session.push(|seq| SessionEvent::Output { seq, at, data });
            }
        }));
        let mgr = self.clone();
        let weak = Arc::downgrade(session);
        session.add_task(tokio::spawn(async move {
            let status = pty.wait().await;
            let Some(session) = weak.upgrade() else {
                return;
            };
            let Some(m) = session.managed() else {
                return;
            };
            if m.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let code = status.ok().and_then(|s| s.code);
            if let Some(broker) = &mgr.inner.broker {
                broker.unregister_session(&session.id);
                broker
                    .admission()
                    .release_by_key(&crate::hooks::session_admission_key(&session.id));
            }
            lock(&session.pending).take();
            lock(&session.deferred).clear();
            mgr.set_state(&session, SessionState::Exited { code });
            mgr.schedule_removal(&session);
        }));
    }

    pub(crate) fn schedule_removal(&self, session: &Arc<Session>) {
        let mgr = self.clone();
        let id = session.id.clone();
        let retention = self.inner.cfg.exited_retention;
        let weak = Arc::downgrade(session);
        tokio::spawn(async move {
            tokio::time::sleep(retention).await;
            let still_exited = weak
                .upgrade()
                .is_some_and(|s| matches!(s.info().state, SessionState::Exited { .. }));
            if still_exited {
                mgr.remove(&id);
            }
        });
    }

    /// Text sent while a prompt is open could answer it: only a person who
    /// typed it may do that. A decision held back until the agent reports a
    /// prompt (`awaits_prompt`) is no prompt on screen: in a mode that settles
    /// calls on its own it stays held for the whole of a long command.
    fn check_open_decision(session: &Session, input: &Input) -> Result<(), Error> {
        let unmarked = matches!(
            input,
            Input::Text {
                at_prompt: false,
                ..
            }
        );
        let open = lock(&session.pending).is_some()
            || lock(&session.deferred).iter().any(|d| !d.awaits_prompt);
        if unmarked && open {
            return Err(api_error(
                ErrorCode::Conflict,
                "the agent is waiting for a decision in this session; answer it first",
            ));
        }
        Ok(())
    }

    /// The same check just before Enter, after the text was written: the
    /// prompt may have opened during the pause. Enter stays unpressed.
    fn check_before_enter(session: &Session, input: &Input) -> Result<(), Error> {
        Self::check_open_decision(session, input).map_err(|_| {
            api_error(
                ErrorCode::Conflict,
                "a decision opened while typing: the text is in the agent's input, not sent; \
                 answer the decision first",
            )
        })
    }

    /// Queues input for delivery. The returned receiver resolves once the
    /// input reached the session (or failed).
    pub fn send_input(
        &self,
        id: &SessionId,
        input: Input,
    ) -> Result<oneshot::Receiver<Result<(), Error>>, Error> {
        let session = self.require(id)?;
        Self::check_open_decision(&session, &input)?;
        if matches!(session.info().state, SessionState::Exited { .. }) {
            return Err(api_error(ErrorCode::Conflict, "the session has exited"));
        }
        let control = session.info().control;
        if matches!(control, ControlTier::ObserveOnly) {
            return Err(api_error(
                ErrorCode::Unsupported,
                "this session can only be observed; start it with `vca run` to control it",
            ));
        }
        let (done, rx) = oneshot::channel();
        session
            .input
            .try_send(InputJob { input, done })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => api_error(
                    ErrorCode::Unavailable,
                    format!(
                        "{INPUT_QUEUE} messages are already waiting for this session; \
                         send again once they were delivered"
                    ),
                ),
                mpsc::error::TrySendError::Closed(_) => {
                    api_error(ErrorCode::Unavailable, "session input closed")
                }
            })?;
        Ok(rx)
    }

    async fn deliver(&self, session: &Session, input: Input) -> Result<(), Error> {
        // A session that exited while this waited in line takes nothing:
        // every queued input fails instead of reaching whatever process
        // got its terminal or PID next.
        if matches!(session.info().state, SessionState::Exited { .. }) {
            return Err(api_error(ErrorCode::Conflict, "the session has exited"));
        }
        // Again here: a decision may have opened while this waited in line.
        Self::check_open_decision(session, &input)?;
        if let Input::Answer {
            decision_id,
            answers,
        } = &input
        {
            // The form clears itself once the agent took the answers.
            crate::questions::answer(self, session, decision_id, answers).await?;
            lock(&session.info).last_activity = now_millis();
            return Ok(());
        }
        let answered = matches!(input, Input::Choose { .. });
        let input = match input {
            Input::Choose {
                decision_id,
                option_id,
            } => {
                let pending = lock(&session.pending).clone();
                let Some(pending) = pending.filter(|p| p.decision.id == decision_id) else {
                    return Err(api_error(
                        ErrorCode::Conflict,
                        "that decision is no longer pending",
                    ));
                };
                if !pending.decision.options.iter().any(|o| o.id == option_id) {
                    return Err(api_error(ErrorCode::InvalidRequest, "unknown option"));
                }
                lock(&session.pending).take();
                Input::Text {
                    text: option_id,
                    submit: pending.submit,
                    at_prompt: true,
                }
            }
            Input::Answer { .. } => unreachable!("handled above"),
            other => other,
        };
        match &session.kind {
            Kind::Managed(m) => {
                let pty = lock(&m.pty).clone();
                let unavailable =
                    |e: vca_pty::PtyError| api_error(ErrorCode::Unavailable, e.to_string());
                if pty.send_body(&input).await.map_err(unavailable)? {
                    Self::check_before_enter(session, &input)?;
                    pty.press_enter().await.map_err(unavailable)?;
                }
                if let Input::Text { text, submit, .. } = &input {
                    tracing::debug!(session = %session.info().id, chars = text.chars().count(), submit = *submit, "typed into the pty");
                }
            }
            Kind::Observed(o) => {
                let Some(target) = &o.terminal else {
                    return Err(api_error(ErrorCode::Unsupported, "no input channel"));
                };
                let Input::Text { text, submit, .. } = &input else {
                    return Err(api_error(
                        ErrorCode::Unsupported,
                        "only text can be typed into this terminal",
                    ));
                };
                let chars = text.chars().count();
                let typed = match vca_adapters::terminal::inject(target, text, false).await {
                    Ok(report) if *submit => {
                        tokio::time::sleep(OBSERVED_SUBMIT_DELAY).await;
                        Self::check_before_enter(session, &input)?;
                        vca_adapters::terminal::inject(target, "", true)
                            .await
                            .map(|_| report)
                    }
                    other => other,
                };
                match typed {
                    Ok(report) => {
                        tracing::info!(session = %session.info().id, chars, submit = *submit, report = %report, "typed into the terminal");
                    }
                    Err(e) => {
                        // Never silently: the client sees the failure too.
                        tracing::warn!(session = %session.info().id, chars, "typing into the terminal failed: {e}");
                        return Err(api_error(ErrorCode::Unavailable, e.to_string()));
                    }
                }
            }
            Kind::Cloud(c) => {
                let Input::Text { text, .. } = &input else {
                    return Err(api_error(
                        ErrorCode::Unsupported,
                        "cloud sessions only accept text messages",
                    ));
                };
                c.send_text(text).await?;
            }
        }
        lock(&session.info).last_activity = now_millis();
        if answered {
            // Every client drops the decision now, whoever answered it; the
            // agent's own hooks and output refine the state from here.
            self.set_state(session, SessionState::Running);
            self.notify(session);
            // A decision that became due while this one was on screen.
            self.promote_waiting(session);
        }
        Ok(())
    }

    /// Pause, resume, stop or restart a session (SES-06).
    pub async fn control(&self, id: &SessionId, action: SessionAction) -> Result<(), Error> {
        let session = self.require(id)?;
        if let Some(cloud) = session.cloud() {
            // Cloud sessions run at the provider: no broker, no process.
            return match action {
                SessionAction::Stop => cloud.stop().await,
                SessionAction::Pause | SessionAction::Resume | SessionAction::Restart => Err(
                    api_error(ErrorCode::Unsupported, "cloud sessions can only be stopped"),
                ),
            };
        }
        let broker_err =
            |e: vca_broker::BrokerError| api_error(ErrorCode::Unavailable, e.to_string());
        match action {
            SessionAction::Pause | SessionAction::Resume => {
                // VCA never suspends a process it did not start.
                if session.managed().is_none() {
                    return Err(api_error(
                        ErrorCode::Unsupported,
                        "sessions VCA did not start cannot be paused from VCA",
                    ));
                }
                let Some(broker) = &self.inner.broker else {
                    return Err(api_error(
                        ErrorCode::Unsupported,
                        "the resource broker is off",
                    ));
                };
                let scope = self.inner.cfg.pause_scope;
                if action == SessionAction::Pause {
                    broker.pause_session(id, scope).await.map_err(broker_err)?;
                    self.set_state(&session, SessionState::Paused);
                } else {
                    broker.resume_session(id, scope).await.map_err(broker_err)?;
                    self.set_state(&session, SessionState::Running);
                }
                Ok(())
            }
            SessionAction::Stop => match &session.kind {
                // Only processes VCA started are ended: the session's job on
                // Windows, then the PTY child. The broker's pid-based tree
                // termination is deliberately not used here.
                Kind::Managed(m) => {
                    m.end()
                        .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
                    Ok(())
                }
                // VCA never terminates processes it did not start.
                Kind::Observed(_) | Kind::Cloud(_) => Err(api_error(
                    ErrorCode::Unsupported,
                    "sessions VCA did not start cannot be stopped from VCA",
                )),
            },
            SessionAction::Restart => self.restart(&session).await,
        }
    }

    async fn restart(&self, session: &Arc<Session>) -> Result<(), Error> {
        let Some(m) = session.managed() else {
            return Err(api_error(
                ErrorCode::Unsupported,
                "only managed sessions can be restarted",
            ));
        };
        let generation = m.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let old = lock(&m.pty).clone();
        let _ = m.end();
        let _ = tokio::time::timeout(Duration::from_secs(5), old.wait()).await;
        lock(&m.attach).take();
        let (pty, job) = ProcessJob::spawn(m.spawn.clone())
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
        let attach = AttachServer::bind(&m.endpoint, pty.clone())
            .map_err(|e| api_error(ErrorCode::Internal, format!("attach server: {e}")))?;
        *lock(&m.job) = job;
        *lock(&m.pty) = pty.clone();
        *lock(&m.attach) = Some(attach);
        lock(&session.pending).take();
        lock(&session.deferred).clear();
        session.hook_seen.store(false, Ordering::SeqCst);
        self.update_info(session, |info| {
            if let SessionOrigin::Local { pid, .. } = &mut info.origin {
                *pid = pty.pid();
            }
            info.state = SessionState::Starting;
            true
        });
        session.push(|seq| SessionEvent::StateChanged {
            seq,
            at: now_millis(),
            state: SessionState::Starting,
        });
        self.watch_pty(session, pty, generation);
        Ok(())
    }

    /// Finds and follows the transcript of a managed session whose agent
    /// never sent a hook event: the newest transcript `adapter` finds for
    /// `cwd` that was started at or after `since` and belongs to no other
    /// session. Stops once a transcript is followed (by this or by a hook,
    /// which then never follows a second one) or the session ends.
    pub(crate) fn watch_for_transcript(
        &self,
        session: &Arc<Session>,
        adapter: Arc<dyn AgentAdapter>,
        cwd: PathBuf,
        since: vca_core::UnixMillis,
        delay: Duration,
    ) {
        let weak = Arc::downgrade(session);
        let mgr = self.clone();
        session.add_task(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            loop {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                // Hooks name the transcript themselves; without them, keep
                // looking: the agent may start a new conversation (a new
                // file) in the same process.
                let done = (session.transcript_started.load(Ordering::SeqCst)
                    && session.hook_seen.load(Ordering::SeqCst))
                    || matches!(session.info().state, SessionState::Exited { .. });
                if done {
                    return;
                }
                let followed = lock(&session.followed).as_ref().map(|(p, _)| p.clone());
                let mut taken: Vec<String> = mgr
                    .all()
                    .into_iter()
                    .filter(|s| s.id != session.id)
                    .filter_map(|s| s.agent_session_id())
                    .collect();
                // Not the one already followed.
                taken.extend(
                    followed
                        .as_deref()
                        .and_then(Path::file_stem)
                        .and_then(|s| s.to_str())
                        .map(str::to_owned),
                );
                let (a, dir) = (Arc::clone(&adapter), cwd.clone());
                let found = tokio::task::spawn_blocking(move || {
                    a.new_transcript_since(&dir, since, &taken)
                })
                .await
                .ok()
                .flatten();
                if let Some(locator) = found {
                    if locator.transcript_path != followed
                        || !session.transcript_started.load(Ordering::SeqCst)
                    {
                        if let Some(id) = &locator.agent_session_id {
                            mgr.learn_agent_session_id(&session, id);
                        }
                        tracing::info!(
                            session = %session.id,
                            transcript = ?locator.transcript_path,
                            "no hook event arrived; following the transcript found for the session"
                        );
                        mgr.follow_transcript(&session, locator);
                    }
                }
                drop(session);
                tokio::time::sleep(TRANSCRIPT_FALLBACK_POLL).await;
            }
        }));
    }

    /// Records the agent's own session id (from a hook or the transcript
    /// file); a custom name follows it.
    pub(crate) fn learn_agent_session_id(&self, session: &Session, agent_id: &str) {
        let changed = lock(&session.agent_session_id).as_deref() != Some(agent_id);
        if changed {
            let old_key = session.name_key();
            *lock(&session.agent_session_id) = Some(agent_id.to_owned());
            self.rekey_name(session, &old_key);
        }
    }

    /// Streams the agent's transcript into the session's chat events.
    pub(crate) fn follow_transcript(&self, session: &Arc<Session>, locator: SessionLocator) {
        self.follow(session, locator, false);
    }

    /// Follows the new conversation of an observed session (`/clear` or a
    /// new conversation in the same process writes a new file) instead of
    /// the old one. The caller has made sure the file is this session's:
    /// discovery checks the process, the folder and that no other session
    /// owns it (see `crate::discovery`).
    pub(crate) fn follow_new_conversation(&self, session: &Arc<Session>, locator: SessionLocator) {
        if let Some(id) = &locator.agent_session_id {
            self.learn_agent_session_id(session, id);
        }
        tracing::info!(
            session = %session.id,
            transcript = ?locator.transcript_path,
            "the observed agent started a new conversation; following its transcript"
        );
        self.follow(session, locator, true);
    }

    fn follow(&self, session: &Arc<Session>, locator: SessionLocator, new_conversation: bool) {
        let switching = if session.transcript_started.swap(true, Ordering::SeqCst) {
            // Already following one. Another file of a managed session is
            // its agent's new conversation: follow that instead, so the chat
            // and the questions it shows are the current ones. An observed
            // session only switches when discovery vouches for the file.
            let current = lock(&session.followed).as_ref().map(|(p, _)| p.clone());
            let may_switch = session.managed().is_some() || new_conversation;
            match (&locator.transcript_path, current) {
                (Some(new), Some(old)) if may_switch && *new != old => true,
                _ => return,
            }
        } else {
            false
        };
        let Some(agent) = session.agent() else {
            return;
        };
        if switching {
            if let Some((_, old)) = lock(&session.followed).take() {
                old.abort();
            }
        }
        let path = locator.transcript_path.clone();
        let weak = Arc::downgrade(session);
        let mgr = self.clone();
        let task = tokio::spawn(async move {
            let adapter = adapter_for(&agent);
            let mut stream = match adapter.tail_history(&locator, true).await {
                Ok(s) => s,
                Err(err) => {
                    tracing::debug!(%err, "cannot follow transcript");
                    if let Some(s) = weak.upgrade() {
                        s.transcript_started.store(false, Ordering::SeqCst);
                    }
                    return;
                }
            };
            let mut expiry = tokio::time::interval(crate::subagents::EXPIRY_CHECK);
            expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            if switching {
                // What was derived from the old file (open calls, a question)
                // no longer holds.
                if let Some(session) = weak.upgrade() {
                    let reset = HistoryItem::Rewritten;
                    crate::observed_watch::observe_item(&mgr, &session, &reset, Instant::now());
                    crate::questions::observe_item(&mgr, &session, &reset);
                    crate::asks::observe_item(&mgr, &session, &reset);
                    mgr.update_subagents(&session, |t| t.observe(&reset, now_millis()));
                }
            }
            loop {
                let item = tokio::select! {
                    item = stream.recv() => item,
                    _ = expiry.tick() => {
                        let Some(session) = weak.upgrade() else {
                            return;
                        };
                        mgr.update_subagents(&session, |t| t.expire(now_millis()));
                        continue;
                    }
                };
                let Some(item) = item else {
                    return;
                };
                let Some(session) = weak.upgrade() else {
                    return;
                };
                mgr.observe_deferred(&session, &item);
                crate::observed_watch::observe_item(&mgr, &session, &item, Instant::now());
                crate::questions::observe_item(&mgr, &session, &item);
                crate::asks::observe_item(&mgr, &session, &item);
                crate::progress::observe_item(&mgr, &session, &item);
                mgr.update_subagents(&session, |t| t.observe(&item, now_millis()));
                // Tool results only pair calls; bookkeeping items have no
                // chat bubble.
                if matches!(
                    item,
                    HistoryItem::ToolResult { .. }
                        | HistoryItem::Subagent { .. }
                        | HistoryItem::Question { .. }
                        | HistoryItem::Rewritten
                        | HistoryItem::PermissionMode { .. }
                        | HistoryItem::TurnEnd { .. }
                        | HistoryItem::Thinking { .. }
                ) {
                    continue;
                }
                session.push(|seq| {
                    item.to_event(seq, now_millis())
                        .expect("messages and tool calls are chat events")
                });
            }
        });
        if let Some(path) = path {
            *lock(&session.followed) = Some((path, task.abort_handle()));
        }
        session.add_task(task);
    }

    /// Stops every managed session (used at shutdown).
    pub async fn stop_all(&self, grace: Duration) {
        let sessions = self.all();
        let managed: Vec<PtySession> = sessions
            .iter()
            .filter_map(|s| s.managed())
            .map(|m| {
                let _ = m.end();
                lock(&m.pty).clone()
            })
            .collect();
        let _ = tokio::time::timeout(grace, async {
            for pty in &managed {
                let _ = pty.wait().await;
            }
        })
        .await;
    }

    pub(crate) fn insert_observed(
        &self,
        info: SessionInfo,
        observed: Observed,
        agent_session_id: Option<String>,
    ) -> Arc<Session> {
        // Observed sessions are not registered with the broker, so neither
        // autonomous containment nor user actions can reach processes VCA
        // did not start. They are only added to the monitor-only list, which
        // shows their usage and gives no way to act on them.
        if let (Some(broker), SessionOrigin::Local { pid: Some(pid), .. }) =
            (&self.inner.broker, &info.origin)
        {
            broker.monitor_session(info.id.clone(), *pid);
        }
        let session = self.new_session(info.id.clone(), info, Kind::Observed(observed));
        // Before insertion, so that a stored name is found under the
        // agent's session id.
        *lock(&session.agent_session_id) = agent_session_id;
        self.insert(Arc::clone(&session));
        session
    }

    /// Inserts a cloud session card. Like observed sessions, it is not
    /// registered with the broker.
    pub(crate) fn insert_cloud(&self, info: SessionInfo, handle: CloudHandle) -> Arc<Session> {
        let session = self.new_session(info.id.clone(), info, Kind::Cloud(Box::new(handle)));
        self.insert(Arc::clone(&session));
        session
    }

    /// Stops polling every cloud session (at shutdown). The remote
    /// sessions keep running at their providers.
    pub fn stop_cloud_polling(&self) {
        for s in self.all().iter().filter(|s| s.cloud().is_some()) {
            for t in lock(&s.tasks).drain(..) {
                t.abort();
            }
        }
    }

    /// Applies an output heuristic result to a managed session.
    fn apply_heuristic(&self, session: &Arc<Session>, found: Option<HeuristicMatch>, busy: bool) {
        let state = session.info().state;
        if matches!(state, SessionState::Exited { .. } | SessionState::Paused) {
            return;
        }
        match found {
            Some(m) if m.state == SessionState::AwaitingDecision && !m.options.is_empty() => {
                let prompt = m.prompt.unwrap_or_default();
                // A hook-reported decision already covers this prompt.
                let same = lock(&session.pending)
                    .as_ref()
                    .is_some_and(|p| !p.heuristic || p.decision.prompt == prompt);
                if !same {
                    let decision = Decision {
                        id: format!("h-{}-{}", session.id, lock(&session.log).next_seq()),
                        prompt,
                        options: m.options.clone(),
                        inferred: false,
                        questions: Vec::new(),
                        command: None,
                    };
                    self.request_decision(
                        session,
                        PendingDecision {
                            submit: options_need_submit(&decision.options),
                            decision,
                            heuristic: true,
                            at: now_millis(),
                            tool_use_id: None,
                        },
                    );
                }
            }
            other => {
                {
                    let mut pending = lock(&session.pending);
                    if pending.as_ref().is_some_and(|p| p.heuristic) {
                        pending.take();
                    }
                }
                let next = match other {
                    Some(m) if m.state != SessionState::AwaitingDecision => m.state,
                    _ if busy => SessionState::Running,
                    _ if matches!(state, SessionState::Running | SessionState::Starting) => {
                        SessionState::Idle
                    }
                    _ => state.clone(),
                };
                // A quiet prompt is only "awaiting input" once output stopped.
                let next = if busy && next == SessionState::AwaitingInput {
                    SessionState::Running
                } else {
                    next
                };
                self.set_state(session, next);
            }
        }
    }
}

async fn input_worker(
    mgr: SessionManager,
    session: std::sync::Weak<Session>,
    mut rx: mpsc::Receiver<InputJob>,
) {
    while let Some(job) = rx.recv().await {
        let Some(s) = session.upgrade() else {
            let _ = job
                .done
                .send(Err(api_error(ErrorCode::NotFound, "session removed")));
            return;
        };
        let result = mgr.deliver(&s, job.input).await;
        let _ = job.done.send(result);
    }
}

/// Keeps the card preview and (without hooks) the state up to date.
async fn heuristics_loop(mgr: SessionManager, session: std::sync::Weak<Session>, every: Duration) {
    let mut tick = tokio::time::interval(every.max(Duration::from_millis(100)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(session) = session.upgrade() else {
            return;
        };
        let Some(m) = session.managed() else {
            return;
        };
        let pty = lock(&m.pty).clone();
        let Some(snap) = pty.screen_snapshot() else {
            continue;
        };
        // Shell output is not put on the card, which every device sees;
        // only admin devices may watch a shell.
        let preview = if session.is_shell() {
            String::new()
        } else {
            truncate_chars(&snap.tail(PREVIEW_LINES).join("\n"), PREVIEW_CHARS, true)
        };
        let preview = (!preview.is_empty()).then_some(preview);
        mgr.update_info(&session, |info| {
            if info.preview != preview {
                info.preview = preview;
                true
            } else {
                false
            }
        });
        let text = snap.text();
        mgr.note_terminal_prompt(&session, &text);
        if session.hook_seen.load(Ordering::SeqCst) {
            continue;
        }
        let busy = lock(&m.last_output).elapsed() < RECENT_OUTPUT;
        let found = m.adapter.guess_state(&text);
        mgr.apply_heuristic(&session, found, busy);
        // The state may have changed what the menu means.
        mgr.note_terminal_prompt(&session, &text);
    }
}

/// What a selection menu on the screen asks, for
/// [`SessionInfo::terminal_prompt`]. The trust question counts always; any
/// other menu only before the agent has a transcript and when it is not
/// already a decision VCA can answer.
pub(crate) fn terminal_prompt_for(
    screen: &str,
    has_transcript: bool,
    decision: bool,
) -> Option<TerminalPrompt> {
    let menu = vca_adapters::heuristics::menu_prompt(screen)?;
    if menu.trust_folder {
        return Some(TerminalPrompt::TrustFolder);
    }
    (!has_transcript && !decision).then_some(TerminalPrompt::Menu)
}

impl SessionManager {
    /// Updates [`SessionInfo::terminal_prompt`] from the screen text.
    pub(crate) fn note_terminal_prompt(&self, session: &Session, screen: &str) {
        let decision = lock(&session.pending).is_some()
            || matches!(session.info().state, SessionState::AwaitingDecision);
        let prompt = terminal_prompt_for(
            screen,
            session.transcript_started.load(Ordering::SeqCst),
            decision,
        );
        self.update_info(session, |info| {
            let changed = info.terminal_prompt != prompt;
            info.terminal_prompt = prompt;
            changed
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_agents_from_commands() {
        assert_eq!(detect_agent("claude"), AgentKind::ClaudeCode);
        assert_eq!(detect_agent("/usr/bin/codex"), AgentKind::Codex);
        assert_eq!(detect_agent(r"C:\bin\claude.cmd"), AgentKind::ClaudeCode);
        assert_eq!(
            detect_agent("vca-fake-agent"),
            AgentKind::Generic("vca-fake-agent".into())
        );
    }

    #[test]
    fn numbered_options_select_without_enter() {
        let opt = |id: &str| DecisionOption {
            id: id.into(),
            label: id.into(),
        };
        assert!(!options_need_submit(&[opt("1"), opt("2")]));
        assert!(options_need_submit(&[opt("y"), opt("n")]));
    }

    #[test]
    fn default_names() {
        assert_eq!(
            default_name(&AgentKind::ClaudeCode, Path::new("/home/u/shop")),
            "shop · Claude Code"
        );
    }

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
                allow_generic_launch: true,
            },
            None,
        )
    }

    /// Any attempt to act on a process fails the test.
    struct NoProcessControl;

    impl vca_broker::contain::ProcessControl for NoProcessControl {
        fn set_priority(
            &self,
            root: u32,
            _: Scope,
            _: vca_broker::contain::Priority,
        ) -> vca_broker::contain::ContainResult {
            panic!("reprioritized {root}")
        }
        fn suspend(&self, root: u32, _: Scope) -> vca_broker::contain::ContainResult {
            panic!("suspended {root}")
        }
        fn resume(&self, root: u32, _: Scope) -> vca_broker::contain::ContainResult {
            panic!("resumed {root}")
        }
        fn terminate(
            &self,
            root: u32,
            _: Scope,
            _: Duration,
        ) -> vca_broker::contain::ContainResult {
            panic!("terminated {root}")
        }
    }

    fn manager_with_broker(names: &Path) -> SessionManager {
        let broker = Broker::start_with_control(
            vca_broker::BrokerConfig::default(),
            Arc::new(NoProcessControl),
        );
        let plain = manager(names);
        SessionManager::new(plain.inner.cfg.clone(), Some(Arc::new(broker)))
    }

    #[tokio::test]
    async fn observed_sessions_cannot_be_paused_resumed_or_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager_with_broker(&dir.path().join("names.json"));
        let session = observe(&mgr, 4_242_424, None, ObservedIdentity::default());
        for action in [
            SessionAction::Pause,
            SessionAction::Resume,
            SessionAction::Stop,
            SessionAction::Restart,
        ] {
            let err = mgr.control(&session.id, action).await.unwrap_err();
            assert_eq!(err.code(), ErrorCode::Unsupported, "{action:?}");
        }
        assert_eq!(session.info().state, SessionState::Running);
    }

    /// "Remove from VCA" never ends a process VCA did not start, and never
    /// the conductor's own session; an ended one goes at once.
    #[tokio::test]
    async fn remove_from_vca_refuses_what_vca_cannot_stop() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager_with_broker(&dir.path().join("names.json"));
        let window = observe(&mgr, 4_242_424, None, ObservedIdentity::default());
        let err = mgr.stop_and_dismiss(&window.id).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unsupported);
        assert!(err.to_string().contains("terminal window"), "{err}");
        assert_eq!(window.info().state, SessionState::Running);
        assert!(mgr.get(&window.id).is_some());

        let conductor = observe(&mgr, 4_242_425, None, ObservedIdentity::default());
        mgr.set_conductor(&conductor.id, true).unwrap();
        let err = mgr.stop_and_dismiss(&conductor.id).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Conflict);
        assert!(mgr.get(&conductor.id).is_some());

        mgr.set_state(&window, SessionState::Exited { code: Some(0) });
        let mut updates = mgr.subscribe_updates();
        let (info, stopped) = mgr.stop_and_dismiss(&window.id).await.unwrap();
        assert_eq!(info.id, window.id);
        assert!(!stopped);
        assert!(mgr.get(&window.id).is_none());
        assert!(matches!(updates.try_recv(), Ok(Update::Removed(id)) if id == window.id));

        let err = mgr
            .stop_and_dismiss(&SessionId::from("nope"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotFound);
    }

    /// A task still holding a removed session cannot bring its card back.
    #[tokio::test]
    async fn a_removed_session_is_never_announced_again() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let session = observe(&mgr, 4_242_426, None, ObservedIdentity::default());
        mgr.set_state(&session, SessionState::Exited { code: Some(0) });
        mgr.dismiss(&session.id).unwrap();
        let mut updates = mgr.subscribe_updates();
        mgr.update_info(&session, |info| {
            info.unread += 1;
            true
        });
        mgr.notify(&session);
        assert!(updates.try_recv().is_err());
        // Listed sessions are announced as usual.
        let other = observe(&mgr, 4_242_427, None, ObservedIdentity::default());
        while updates.try_recv().is_ok() {}
        mgr.notify(&other);
        assert!(matches!(updates.try_recv(), Ok(Update::Changed(_))));
    }

    #[tokio::test]
    async fn observed_sessions_stay_off_the_broker_registry() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager_with_broker(&dir.path().join("names.json"));
        let session = observe(&mgr, 4_242_424, None, ObservedIdentity::default());
        let broker = mgr.broker().unwrap();
        let refused = broker
            .pause_session(&session.id, Scope::WholeTree)
            .await
            .unwrap_err();
        assert!(
            matches!(refused, vca_broker::BrokerError::UnknownSession(_)),
            "{refused:?}"
        );
        // Removing the card ends the monitoring too.
        mgr.remove(&session.id);
    }

    /// Inserts an observed session the way discovery does.
    fn observe(
        mgr: &SessionManager,
        pid: u32,
        agent_session_id: Option<&str>,
        identity: ObservedIdentity,
    ) -> Arc<Session> {
        let project_dir = PathBuf::from("/code/shop");
        let info = SessionInfo {
            id: SessionId::new(),
            origin: SessionOrigin::Local {
                agent: AgentKind::ClaudeCode,
                project_dir: project_dir.clone(),
                pid: Some(pid),
            },
            display_name: default_name(&AgentKind::ClaudeCode, &project_dir),
            state: SessionState::Running,
            control: ControlTier::ObserveOnly,
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
            identity,
        };
        mgr.insert_observed(info, observed, agent_session_id.map(str::to_owned))
    }

    fn api_code(err: Error) -> ErrorCode {
        match err {
            Error::Api { code, .. } => code,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn rename_validates_announces_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 4242, Some("abc"), ObservedIdentity::default());
        let info = s.info();
        assert_eq!(info.display_name, "shop · Claude Code");
        assert_eq!(info.auto_name.as_deref(), Some("shop · Claude Code"));
        assert!(!info.custom_name);

        let mut updates = mgr.subscribe_updates();
        let renamed = mgr.rename(&s.id, Some("  API work  ")).unwrap();
        assert_eq!(renamed.display_name, "API work");
        assert_eq!(renamed.auto_name.as_deref(), Some("shop · Claude Code"));
        assert!(renamed.custom_name);
        match updates.recv().await.unwrap() {
            Update::Changed(info) => assert_eq!(info.display_name, "API work"),
            other => panic!("{other:?}"),
        }
        assert_eq!(mgr.list()[0].display_name, "API work");

        for bad in ["a\nb", "tab\there", &"x".repeat(81)] {
            assert_eq!(
                api_code(mgr.rename(&s.id, Some(bad)).unwrap_err()),
                ErrorCode::InvalidRequest,
                "{bad:?}"
            );
        }
        assert_eq!(s.info().display_name, "API work");
        assert_eq!(
            api_code(mgr.rename(&SessionId::from("nope"), Some("x")).unwrap_err()),
            ErrorCode::NotFound
        );

        // A blank name and `None` both reset.
        let reset = mgr.rename(&s.id, Some("   ")).unwrap();
        assert_eq!(reset.display_name, "shop · Claude Code");
        assert!(!reset.custom_name);
        mgr.rename(&s.id, Some("again")).unwrap();
        let reset = mgr.rename(&s.id, None).unwrap();
        assert_eq!(reset.display_name, "shop · Claude Code");
        assert!(!reset.custom_name);
    }

    #[tokio::test]
    async fn observed_names_survive_a_restart_and_rediscovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("session-names.json");
        let by_process = ObservedIdentity {
            transcript_path: None,
            process_start: Some(1_700_000_000),
        };
        let by_transcript = ObservedIdentity {
            transcript_path: Some(PathBuf::from("/home/u/.claude/projects/x/t.jsonl")),
            process_start: Some(5),
        };
        {
            let mgr = manager(&path);
            let a = observe(&mgr, 100, Some("claude-session-1"), by_process.clone());
            let b = observe(&mgr, 200, None, by_process.clone());
            let c = observe(&mgr, 300, None, by_transcript.clone());
            mgr.rename(&a.id, Some("by agent id")).unwrap();
            mgr.rename(&b.id, Some("by process")).unwrap();
            mgr.rename(&c.id, Some("by transcript")).unwrap();
        }
        assert!(path.exists());

        // A new vcad with the same data dir discovers the same agents
        // under new session ids (and the agent session under a new pid).
        let mgr = manager(&path);
        let a = observe(
            &mgr,
            999,
            Some("claude-session-1"),
            ObservedIdentity::default(),
        );
        let b = observe(&mgr, 200, None, by_process.clone());
        let c = observe(&mgr, 301, None, by_transcript);
        assert_eq!(a.info().display_name, "by agent id");
        assert!(a.info().custom_name);
        assert_eq!(b.info().display_name, "by process");
        assert_eq!(c.info().display_name, "by transcript");

        // Same pid, different start time: another process.
        let other = observe(
            &mgr,
            200,
            None,
            ObservedIdentity {
                transcript_path: None,
                process_start: Some(1_700_000_999),
            },
        );
        assert_eq!(other.info().display_name, "shop · Claude Code");
        assert!(!other.info().custom_name);
        // Different identities never share a key.
        assert_ne!(a.name_key(), other.name_key());
    }

    #[tokio::test]
    async fn ended_sessions_keep_only_resumable_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names.json");
        let mgr = manager(&path);
        let by_process = ObservedIdentity {
            transcript_path: None,
            process_start: Some(7),
        };
        let resumable = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        let process = observe(&mgr, 2, None, by_process.clone());
        mgr.rename(&resumable.id, Some("kept")).unwrap();
        mgr.rename(&process.id, Some("dropped")).unwrap();
        mgr.remove(&resumable.id);
        mgr.remove(&process.id);
        drop(mgr);

        let mgr = manager(&path);
        assert_eq!(
            observe(&mgr, 3, Some("abc"), ObservedIdentity::default())
                .info()
                .display_name,
            "kept"
        );
        assert_eq!(
            observe(&mgr, 2, None, by_process).info().display_name,
            "shop · Claude Code"
        );
    }

    #[tokio::test]
    async fn custom_name_follows_a_new_agent_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("old"), ObservedIdentity::default());
        mgr.rename(&s.id, Some("mine")).unwrap();
        let old_key = s.name_key();
        *lock(&s.agent_session_id) = Some("new".into());
        mgr.rekey_name(&s, &old_key);
        assert_eq!(s.info().display_name, "mine");
        drop(mgr);
        let mgr = manager(&dir.path().join("names.json"));
        let again = observe(&mgr, 2, Some("new"), ObservedIdentity::default());
        assert_eq!(again.info().display_name, "mine");
    }

    async fn hook(mgr: &SessionManager, s: &Session, json: &str) {
        let event = vca_adapters::claude::ClaudeHookPayload::parse(json)
            .unwrap()
            .to_agent_event();
        mgr.hook_for(&s.id, event).await;
    }

    fn requested_ids(s: &Session) -> Vec<String> {
        lock(s.log())
            .replay_after(0)
            .events
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::DecisionRequested { decision, .. } => Some(decision.id),
                _ => None,
            })
            .collect()
    }

    fn permission(tool_use_id: Option<&str>, command: &str) -> String {
        let id = tool_use_id
            .map(|i| format!(r#","tool_use_id":"{i}""#))
            .unwrap_or_default();
        format!(
            r#"{{"session_id":"abc","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{{"command":"{command}"}}{id}}}"#
        )
    }

    const PERMISSION_NOTICE: &str = r#"{"session_id":"abc","hook_event_name":"Notification","message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"}"#;

    /// Waits until hook decisions outlived their grace period.
    async fn settle() {
        tokio::time::sleep(crate::hooks::DECISION_GRACE + Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn repeated_hook_delivery_is_one_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        let json = permission(None, "rm -rf x");
        hook(&mgr, &s, &json).await;
        hook(&mgr, &s, &json).await;
        settle().await;
        let ids = requested_ids(&s);
        assert_eq!(ids.len(), 1, "{ids:?}");
        assert!(!ids[0].contains("rm"));
        assert_eq!(s.info().unread, 1);
        // With the agent's own id too.
        let own = permission(Some("toolu_1"), "ls");
        lock(&s.pending).take();
        hook(&mgr, &s, &own).await;
        hook(&mgr, &s, &own).await;
        settle().await;
        assert_eq!(requested_ids(&s).len(), 2);
    }

    #[tokio::test]
    async fn permission_notification_after_hook_is_not_a_second_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        hook(&mgr, &s, &permission(None, "ls")).await;
        hook(&mgr, &s, PERMISSION_NOTICE).await;
        hook(&mgr, &s, PERMISSION_NOTICE).await;
        // The notice alone, delivered twice, is also one decision.
        let s2 = observe(&mgr, 2, Some("abd"), ObservedIdentity::default());
        hook(&mgr, &s2, PERMISSION_NOTICE).await;
        hook(&mgr, &s2, PERMISSION_NOTICE).await;
        settle().await;
        assert_eq!(requested_ids(&s).len(), 1);
        assert_eq!(requested_ids(&s2).len(), 1);
        // Once shown, a further notice is an ordinary notification.
        hook(&mgr, &s, PERMISSION_NOTICE).await;
        assert_eq!(requested_ids(&s).len(), 1);
    }

    #[tokio::test]
    async fn same_command_after_an_answer_is_a_new_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        let json = permission(None, "cargo test");
        hook(&mgr, &s, &json).await;
        settle().await;
        let first = lock(&s.pending).clone().unwrap().decision.id;
        // Answering takes the pending decision once.
        assert!(lock(&s.pending).take().is_some());
        assert!(lock(&s.pending).take().is_none());
        hook(&mgr, &s, &json).await;
        settle().await;
        let second = lock(&s.pending).clone().unwrap().decision.id;
        assert_ne!(first, second);
        hook(&mgr, &s, &json).await;
        assert_eq!(requested_ids(&s), vec![first, second]);
    }

    #[tokio::test]
    async fn heuristic_does_not_duplicate_a_hook_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        hook(&mgr, &s, &permission(None, "ls")).await;
        settle().await;
        let found = HeuristicMatch {
            state: SessionState::AwaitingDecision,
            rule: "t".into(),
            prompt: Some("Do you want to proceed?".into()),
            options: vec![DecisionOption {
                id: "1".into(),
                label: "Yes".into(),
            }],
        };
        mgr.apply_heuristic(&s, Some(found.clone()), false);
        mgr.apply_heuristic(&s, Some(found), false);
        assert_eq!(requested_ids(&s).len(), 1);
    }

    /// Claude Code's first-run question, as a fake screen buffer.
    const TRUST_SCREEN: &str = " Accessing workspace:\n\n C:\\data\\conductor\n\n \
        Quick safety check: Is this a project you created or one you trust? (Like your own\n \
        code, a well-known open source project, or work from your team).\n\n \
        ❯ 1. Yes, I trust this folder\n   2. No, exit\n\n Enter to confirm · Esc to cancel\n";
    const THEME_SCREEN: &str = " Choose the text style that looks best with your terminal\n\n \
        ❯ 1. Dark mode ✔\n   2. Light mode\n";

    #[tokio::test]
    async fn menus_without_a_transcript_need_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, None, ObservedIdentity::default());
        assert_eq!(s.info().terminal_prompt, None);

        mgr.note_terminal_prompt(&s, TRUST_SCREEN);
        assert_eq!(s.info().terminal_prompt, Some(TerminalPrompt::TrustFolder));
        let json = serde_json::to_string(&s.info()).unwrap();
        assert!(
            json.contains(r#""terminal_prompt":"trust_folder""#),
            "{json}"
        );

        mgr.note_terminal_prompt(&s, THEME_SCREEN);
        assert_eq!(s.info().terminal_prompt, Some(TerminalPrompt::Menu));
        // The question is answered: the prompt leaves.
        mgr.note_terminal_prompt(&s, "╭───╮\n│ > │\n╰───╯\n");
        assert_eq!(s.info().terminal_prompt, None);
        assert!(
            !serde_json::to_string(&s.info())
                .unwrap()
                .contains("terminal_prompt")
        );

        // With a transcript, an ordinary menu is the agent's own business
        // (decisions cover permission menus); the trust question still counts.
        s.transcript_started.store(true, Ordering::SeqCst);
        mgr.note_terminal_prompt(&s, THEME_SCREEN);
        assert_eq!(s.info().terminal_prompt, None);
        mgr.note_terminal_prompt(&s, TRUST_SCREEN);
        assert_eq!(s.info().terminal_prompt, Some(TerminalPrompt::TrustFolder));
        // A plain numbered list is no menu.
        assert_eq!(
            terminal_prompt_for("Plan:\n1. Tests\n2. Fix\n", false, false),
            None
        );
        // A menu VCA already turned into a decision is answered there.
        assert_eq!(terminal_prompt_for(THEME_SCREEN, false, true), None);
    }

    const IDLE_NOTICE: &str = r#"{"session_id":"abc","hook_event_name":"Notification","message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#;

    fn notifications(s: &Session) -> usize {
        lock(s.log())
            .replay_after(0)
            .events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Notification { .. }))
            .count()
    }

    #[tokio::test]
    async fn the_conductor_session_alerts_only_for_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        // An ordinary session: the notice is an event and an unread.
        hook(&mgr, &s, IDLE_NOTICE).await;
        assert_eq!(notifications(&s), 1);
        assert_eq!(s.info().unread, 1);
        let json = serde_json::to_string(&s.info()).unwrap();
        assert!(!json.contains("conductor"), "left out when false: {json}");

        // Marked as the conductor's session: unread clears and stays 0.
        let mut updates = mgr.subscribe_updates();
        mgr.set_conductor(&s.id, true).unwrap();
        match updates.try_recv() {
            Ok(Update::Changed(info)) => assert!(info.conductor && info.unread == 0),
            other => panic!("{other:?}"),
        }
        let json = serde_json::to_string(&s.info()).unwrap();
        assert!(json.contains(r#""conductor":true"#), "{json}");
        hook(&mgr, &s, IDLE_NOTICE).await;
        assert_eq!(notifications(&s), 1, "no notification event");
        assert_eq!(s.info().unread, 0);

        // A permission decision still reaches the person.
        hook(&mgr, &s, &permission(None, "rm -rf build")).await;
        settle().await;
        let info = s.info();
        assert_eq!(requested_ids(&s).len(), 1);
        assert!(info.pending_decision.is_some());
        assert_eq!(info.state, SessionState::AwaitingDecision);
        assert_eq!(info.unread, 0);
    }

    /// A Claude Code hook payload for tool call `id` running `command`.
    fn tool_hook(event: &str, mode: &str, id: &str, command: &str) -> String {
        format!(
            r#"{{"session_id":"abc","hook_event_name":"{event}","permission_mode":"{mode}","tool_name":"Bash","tool_input":{{"command":"{command}"}},"tool_use_id":"{id}"}}"#
        )
    }

    /// Whether the chat log shows a "waiting for a decision" marker.
    fn awaited(s: &Session) -> bool {
        lock(s.log()).replay_after(0).events.iter().any(|e| {
            matches!(
                e,
                SessionEvent::StateChanged {
                    state: SessionState::AwaitingDecision,
                    ..
                }
            )
        })
    }

    /// Nothing about a decision reached the person.
    fn silent(s: &Session) {
        let info = s.info();
        assert!(requested_ids(s).is_empty(), "{:?}", requested_ids(s));
        assert_eq!(notifications(s), 0);
        assert!(!awaited(s), "no decision marker in the chat");
        assert!(info.pending_decision.is_none());
        assert_eq!(info.unread, 0);
        assert_eq!(info.state, SessionState::Running);
        assert!(lock(&s.deferred).is_none());
    }

    #[tokio::test]
    async fn an_auto_approved_call_raises_no_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        for mode in ["auto", "default"] {
            let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
            let mut updates = mgr.subscribe_updates();
            hook(&mgr, &s, &tool_hook("PreToolUse", mode, "toolu_a", "ls")).await;
            hook(
                &mgr,
                &s,
                &tool_hook("PermissionRequest", mode, "toolu_a", "ls"),
            )
            .await;
            // No permission notification: Claude Code sends one only for a
            // prompt that stayed on screen for about six seconds.
            tokio::time::sleep(Duration::from_millis(800)).await;
            hook(&mgr, &s, &tool_hook("PostToolUse", mode, "toolu_a", "ls")).await;
            settle().await;
            silent(&s);
            // No card update ever showed a decision either.
            while let Ok(update) = updates.try_recv() {
                if let Update::Changed(info) = update {
                    assert!(info.pending_decision.is_none(), "{mode}");
                    assert_ne!(info.state, SessionState::AwaitingDecision, "{mode}");
                }
            }
            mgr.remove(&s.id);
        }
    }

    #[tokio::test]
    async fn a_real_prompt_is_raised_once_after_the_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        let request = tool_hook("PermissionRequest", "default", "toolu_b", "rm -rf build");
        hook(
            &mgr,
            &s,
            &tool_hook("PreToolUse", "default", "toolu_b", "rm -rf build"),
        )
        .await;
        hook(&mgr, &s, &request).await;
        hook(&mgr, &s, &request).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(requested_ids(&s).is_empty(), "not within the grace period");
        assert_eq!(notifications(&s), 0);
        assert!(!awaited(&s));
        assert_eq!(s.info().unread, 0);
        // No PostToolUse for 5 s: the person has to answer.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(requested_ids(&s), ["claude:abc:toolu_b"]);
        assert_eq!(notifications(&s), 0);
        assert!(awaited(&s));
        let info = s.info();
        assert_eq!(info.state, SessionState::AwaitingDecision);
        assert_eq!(info.unread, 1);
        let pending = info.pending_decision.unwrap().decision;
        assert_eq!(pending.prompt, "Allow Bash: rm -rf build?");
        assert_eq!(pending.options.len(), 3);
        // Later repeats stay one decision; the answer clears it.
        hook(&mgr, &s, &request).await;
        settle().await;
        assert_eq!(requested_ids(&s).len(), 1);
        hook(
            &mgr,
            &s,
            &tool_hook("PostToolUse", "default", "toolu_b", "rm -rf build"),
        )
        .await;
        let info = s.info();
        assert!(info.pending_decision.is_none());
        assert_eq!(info.state, SessionState::Running);
    }

    #[tokio::test]
    async fn the_calls_transcript_result_drops_a_decision_in_its_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        hook(
            &mgr,
            &s,
            &tool_hook("PermissionRequest", "auto", "toolu_c", "ls"),
        )
        .await;
        let result = |id: &str| HistoryItem::ToolResult {
            at: None,
            id: Some(id.into()),
            text: String::new(),
            is_error: false,
        };
        // Another call's result does not end it; its own does.
        mgr.observe_deferred(&s, &result("toolu_other"));
        assert!(lock(&s.deferred).is_some());
        tokio::time::sleep(Duration::from_millis(500)).await;
        mgr.observe_deferred(&s, &result("toolu_c"));
        settle().await;
        silent(&s);
    }

    #[tokio::test]
    async fn permission_modes_that_approve_on_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let input = r#"{"questions":[{"question":"Ship it?","header":"Ship","options":[{"label":"Yes","description":""},{"label":"No","description":""}],"multiSelect":false}]}"#;
        let ask = |mode: &str| {
            format!(
                r#"{{"session_id":"abc","hook_event_name":"PreToolUse","permission_mode":"{mode}","tool_name":"AskUserQuestion","tool_use_id":"toolu_q","tool_input":{input}}}"#
            )
        };
        // auto / bypass: PreToolUse raises nothing.
        let quiet = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        hook(&mgr, &quiet, &ask("bypassPermissions")).await;
        hook(&mgr, &quiet, &ask("auto")).await;
        // auto mode can still hand a call to the person: once the agent
        // reports its prompt it is a decision like any other.
        let escalated = observe(&mgr, 2, Some("abe"), ObservedIdentity::default());
        let request = tool_hook("PermissionRequest", "auto", "toolu_e", "git push")
            .replace(r#""abc""#, r#""abe""#);
        hook(&mgr, &escalated, &request).await;
        hook(
            &mgr,
            &escalated,
            &PERMISSION_NOTICE.replace(r#""abc""#, r#""abe""#),
        )
        .await;
        // default: the question form is raised after the grace period.
        let asked = observe(&mgr, 3, Some("abf"), ObservedIdentity::default());
        hook(
            &mgr,
            &asked,
            &ask("default").replace(r#""abc""#, r#""abf""#),
        )
        .await;
        assert!(requested_ids(&asked).is_empty());
        settle().await;
        silent(&quiet);
        assert_eq!(requested_ids(&escalated), ["claude:abe:toolu_e"]);
        assert_eq!(escalated.info().state, SessionState::AwaitingDecision);
        assert_eq!(requested_ids(&asked), ["claude:abf:toolu_q"]);
        assert!(
            asked
                .info()
                .pending_decision
                .unwrap()
                .decision
                .is_question()
        );
    }

    // ------------------------------------------------------- card order

    /// An observed Claude Code session in `folder`, created at `at`.
    fn observe_at(
        mgr: &SessionManager,
        folder: &str,
        conversation: Option<&str>,
        at: UnixMillis,
    ) -> Arc<Session> {
        let project_dir = PathBuf::from(folder);
        let info = SessionInfo {
            id: SessionId::new(),
            origin: SessionOrigin::Local {
                agent: AgentKind::ClaudeCode,
                project_dir: project_dir.clone(),
                pid: Some(4000 + u32::try_from(at % 1000).unwrap()),
            },
            display_name: default_name(&AgentKind::ClaudeCode, &project_dir),
            state: SessionState::Running,
            control: ControlTier::ObserveOnly,
            profile: None,
            created_at: at,
            last_activity: at,
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
            identity: ObservedIdentity::default(),
        };
        mgr.insert_observed(info, observed, conversation.map(str::to_owned))
    }

    fn names_of(mgr: &SessionManager) -> Vec<String> {
        mgr.list()
            .into_iter()
            .map(|s| match s.origin {
                SessionOrigin::Local { project_dir, .. } => project_dir
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                SessionOrigin::Cloud { remote_id, .. } => remote_id,
            })
            .collect()
    }

    #[tokio::test]
    async fn the_card_order_outlives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let names = dir.path().join("names.json");
        let mgr = manager(&names);
        let api = observe_at(&mgr, "/code/api", Some("conv-api"), 1);
        let web = observe_at(&mgr, "/code/web", None, 2);
        let docs = observe_at(&mgr, "/code/docs", Some("conv-docs"), 3);
        // The default order: oldest first.
        assert_eq!(names_of(&mgr), ["api", "web", "docs"]);
        assert!(!mgr.session_order().custom);

        let mut pushes = mgr.subscribe_order();
        let order = mgr
            .set_session_order(&[docs.id.clone(), api.id.clone(), web.id.clone()])
            .unwrap();
        assert!(order.custom);
        assert_eq!(
            order.session_ids,
            [docs.id.clone(), api.id.clone(), web.id.clone()]
        );
        // Every client hears it.
        assert_eq!(pushes.recv().await.unwrap(), order);
        assert_eq!(names_of(&mgr), ["docs", "api", "web"]);
        assert!(dir.path().join(crate::order::ORDER_FILE).exists());
        drop(mgr);

        // vcad restarts: new session ids, sessions found in another order,
        // and a session nobody placed yet.
        let mgr = manager(&names);
        let mut pushes = mgr.subscribe_order();
        observe_at(&mgr, "/code/new", None, 10);
        let web = observe_at(&mgr, "/code/web", None, 11);
        observe_at(&mgr, "/code/api", Some("conv-api"), 12);
        observe_at(&mgr, "/code/docs", Some("conv-docs"), 13);
        assert_eq!(names_of(&mgr), ["docs", "api", "web", "new"]);
        // A session taking its stored place is announced, so clients move it.
        let mut last = None;
        while let Ok(order) = pushes.try_recv() {
            last = Some(order);
        }
        let last = last.expect("an order push");
        assert!(last.custom);
        assert_eq!(last.session_ids.len(), 4);

        // A rename keeps the place, also after the next restart.
        mgr.rename(&web.id, Some("Front end")).unwrap();
        assert_eq!(names_of(&mgr), ["docs", "api", "web", "new"]);
        drop(mgr);
        let mgr = manager(&names);
        let web = observe_at(&mgr, "/code/web", None, 20);
        mgr.rename(&web.id, Some("Front end")).unwrap();
        observe_at(&mgr, "/code/api", Some("conv-api"), 21);
        assert_eq!(names_of(&mgr), ["api", "web"]);
    }

    #[tokio::test]
    async fn set_order_ignores_unknown_ids_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let a = observe_at(&mgr, "/p/a", None, 1);
        let b = observe_at(&mgr, "/p/b", None, 2);
        let c = observe_at(&mgr, "/p/c", None, 3);
        // Only c is named (twice), plus an unknown id: c first, then the rest
        // in the current order.
        let order = mgr
            .set_session_order(&[SessionId::from("gone"), c.id.clone(), c.id.clone()])
            .unwrap();
        assert_eq!(
            order.session_ids,
            [c.id.clone(), a.id.clone(), b.id.clone()]
        );
        // Sessions that share a folder and a name can still trade places.
        let twin1 = observe_at(&mgr, "/p/twin", None, 4);
        let twin2 = observe_at(&mgr, "/p/twin", None, 5);
        let order = mgr
            .set_session_order(&[twin2.id.clone(), twin1.id.clone()])
            .unwrap();
        assert_eq!(order.session_ids[..2], [twin2.id.clone(), twin1.id.clone()]);
    }

    #[tokio::test]
    async fn reset_goes_back_to_the_default_order() {
        let dir = tempfile::tempdir().unwrap();
        let names = dir.path().join("names.json");
        let mgr = manager(&names);
        let a = observe_at(&mgr, "/p/a", None, 1);
        let b = observe_at(&mgr, "/p/b", None, 2);
        mgr.set_session_order(&[b.id.clone(), a.id.clone()])
            .unwrap();
        assert_eq!(names_of(&mgr), ["b", "a"]);
        let mut pushes = mgr.subscribe_order();
        let order = mgr.reset_session_order().unwrap();
        assert!(!order.custom);
        assert_eq!(order.session_ids, [a.id.clone(), b.id.clone()]);
        assert_eq!(pushes.recv().await.unwrap(), order);
        drop(mgr);
        let mgr = manager(&names);
        observe_at(&mgr, "/p/b", None, 3);
        observe_at(&mgr, "/p/a", None, 4);
        assert_eq!(names_of(&mgr), ["b", "a"]);
        assert!(!mgr.session_order().custom);
    }

    use vca_adapters::HookDecision;

    fn open_decision(id: &str) -> PendingDecision {
        PendingDecision {
            decision: Decision {
                id: id.into(),
                prompt: "Allow Bash: rm -rf build?".into(),
                ..Decision::default()
            },
            submit: false,
            heuristic: false,
            at: now_millis(),
            tool_use_id: Some("call-a".into()),
        }
    }

    fn text(submit: bool, at_prompt: bool) -> Input {
        Input::Text {
            text: "1".into(),
            submit,
            at_prompt,
        }
    }

    fn refused(mgr: &SessionManager, id: &SessionId, input: Input) -> bool {
        matches!(mgr.send_input(id, input), Err(e) if e.code() == ErrorCode::Conflict)
    }

    /// Dictation and other programs cannot reach an open permission prompt.
    #[tokio::test]
    async fn a_submitted_text_cannot_answer_an_open_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("a"), ObservedIdentity::default());
        assert!(!refused(&mgr, &s.id, text(true, false)), "nothing is open");

        *lock(&s.pending) = Some(open_decision("d1"));
        assert!(refused(&mgr, &s.id, text(true, false)));
        // A person typing at the prompt.
        assert!(!refused(&mgr, &s.id, text(true, true)));
        lock(&s.pending).take();

        // A decision in its grace period counts as open too.
        mgr.defer_decision(&s, open_decision("d2"), None, None, false);
        assert!(refused(&mgr, &s.id, text(true, false)));
    }

    /// A real process, stopped at the end of the test.
    fn managed(mgr: &SessionManager, dir: &Path, script: &str) -> Arc<Session> {
        let (program, flag) = if cfg!(windows) {
            ("cmd", "/c")
        } else {
            ("sh", "-c")
        };
        let info = mgr
            .run(&RunRequest {
                program: program.into(),
                args: vec![flag.into(), script.into()],
                cwd: dir.to_owned(),
                env: Vec::new(),
                size: None,
                agent: Some(AgentKind::ClaudeCode),
                display_name: None,
            })
            .unwrap();
        mgr.get(&info.id).unwrap()
    }

    fn hook_token(s: &Session) -> String {
        s.managed().unwrap().hook_token.clone()
    }

    fn messages(s: &Session, needle: &str) -> usize {
        let (events, _) = lock(s.log()).page_before(None, 1000);
        events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Message { text, .. } if text.contains(needle)))
            .count()
    }

    /// Without hooks a managed session never learned its transcript, so
    /// no message, turn end or reply ever arrived.
    #[tokio::test]
    async fn a_managed_session_without_hooks_finds_its_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let project = dir.path().join("work");
        std::fs::create_dir_all(&project).unwrap();
        let wait = if cfg!(windows) { "pause" } else { "read x" };
        let s = managed(&mgr, &project, wait);
        let since = s.info().created_at;
        let config = dir.path().join("claude");
        let normalized: PathBuf = project.components().collect();
        let folder =
            config
                .join("projects")
                .join(vca_adapters::claude::transcript::encode_project_dir(
                    &normalized,
                ));
        std::fs::create_dir_all(&folder).unwrap();
        // Another session's transcript in the same folder is not ours.
        let other = mgr.insert_observed(
            SessionInfo {
                id: SessionId::new(),
                ..s.info()
            },
            Observed {
                terminal: None,
                identity: ObservedIdentity::default(),
            },
            Some("other-session".into()),
        );
        std::fs::write(
            folder.join("other-session.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"not mine"}}"#.to_owned() + "\n",
        )
        .unwrap();
        let transcript = folder.join("agent-7.jsonl");
        let line = r#"{"type":"user","message":{"role":"user","content":"hello conductor"}}"#;
        std::fs::write(&transcript, format!("{line}\n")).unwrap();

        let adapter = Arc::new(vca_adapters::claude::ClaudeAdapter::with_config_dir(
            &config,
        ));
        mgr.watch_for_transcript(&s, adapter, project.clone(), since, Duration::ZERO);
        for _ in 0..200 {
            if messages(&s, "hello conductor") > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(messages(&s, "hello conductor"), 1);
        assert_eq!(messages(&s, "not mine"), 0);
        assert_eq!(s.agent_session_id().as_deref(), Some("agent-7"));
        assert!(s.transcript_started.load(Ordering::SeqCst));

        // A hook arriving later with the same transcript does not follow it
        // a second time.
        let hook = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "agent-7",
            "transcript_path": transcript.to_string_lossy(),
            "prompt": "x",
        });
        mgr.handle_hook(
            Some(s.id.clone()),
            Some(&hook_token(&s)),
            event(&hook.to_string()),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(messages(&s, "hello conductor"), 1, "followed twice");

        // The agent starts a new conversation (a new file) in the same
        // process: the chat follows it instead of staying on the old one.
        let second = folder.join("agent-8.jsonl");
        let line = r#"{"type":"user","message":{"role":"user","content":"second chat"}}"#;
        std::fs::write(&second, format!("{line}\n")).unwrap();
        let hook = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "agent-8",
            "transcript_path": second.to_string_lossy(),
            "prompt": "y",
        });
        mgr.handle_hook(
            Some(s.id.clone()),
            Some(&hook_token(&s)),
            event(&hook.to_string()),
        )
        .await;
        for _ in 0..300 {
            if messages(&s, "second chat") > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            messages(&s, "second chat"),
            1,
            "new conversation not followed"
        );
        assert_eq!(s.agent_session_id().as_deref(), Some("agent-8"));
        assert_eq!(messages(&s, "hello conductor"), 1);
        drop(other);
        mgr.stop_all(Duration::from_secs(2)).await;
    }

    fn event(json: &str) -> vca_adapters::AgentHookEvent {
        vca_adapters::claude::ClaudeHookPayload::parse(json)
            .unwrap()
            .to_agent_event()
    }

    const STOP: &str = r#"{"hook_event_name":"Stop","session_id":"agent-1"}"#;

    /// Any local process could erase or forge another session's decision.
    #[tokio::test]
    async fn a_hook_event_for_a_managed_session_needs_its_token() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let wait = if cfg!(windows) { "pause" } else { "read x" };
        let s = managed(&mgr, dir.path(), wait);
        *lock(&s.pending) = Some(open_decision("d1"));
        let token = hook_token(&s);
        assert_eq!(token.len(), 64);

        // Without the token, or with another one: nothing happens, and the
        // caller gets the reply that lets an agent go on.
        for forged in [None, Some("0".repeat(64))] {
            let reply = mgr
                .handle_hook(Some(s.id.clone()), forged.as_deref(), event(STOP))
                .await;
            assert_eq!(reply, HookDecision::NoOpinion);
            assert!(lock(&s.pending).is_some(), "the decision stays");
            assert_eq!(s.agent_session_id(), None, "nothing was learned");
        }
        // The session's own hook works.
        mgr.handle_hook(Some(s.id.clone()), Some(&token), event(STOP))
            .await;
        assert!(lock(&s.pending).is_none());
        assert_eq!(s.agent_session_id().as_deref(), Some("agent-1"));

        // Knowing the agent id is no way around the token.
        *lock(&s.pending) = Some(open_decision("d2"));
        mgr.handle_hook(None, None, event(STOP)).await;
        assert!(lock(&s.pending).is_some());
        mgr.stop_all(Duration::from_secs(2)).await;
    }

    /// Hooks are installed for the whole user: a session VCA did not start
    /// has no token, and its events must keep working.
    #[tokio::test]
    async fn hooks_of_observed_sessions_work_by_agent_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("agent-1"), ObservedIdentity::default());
        *lock(&s.pending) = Some(open_decision("d1"));
        // Naming the session is not enough: the agent's id must match.
        let other = r#"{"hook_event_name":"Stop","session_id":"agent-2"}"#;
        mgr.handle_hook(Some(s.id.clone()), None, event(other))
            .await;
        assert!(lock(&s.pending).is_some());
        mgr.handle_hook(None, None, event(STOP)).await;
        assert!(lock(&s.pending).is_none());
    }

    /// An unknown agent must not be blocked by anything here.
    #[tokio::test]
    async fn a_hook_of_an_unknown_session_lets_the_agent_go_on() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let reply = mgr
            .handle_hook(Some("no-such-session".into()), None, event(STOP))
            .await;
        assert_eq!(reply, HookDecision::NoOpinion);
    }

    /// Web viewers render at the PTY's size, which the output carries
    /// (`term_size`): the first output chunk starts with it.
    #[tokio::test]
    async fn output_carries_the_pty_size() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let (program, flag) = if cfg!(windows) {
            ("cmd", "/c")
        } else {
            ("sh", "-c")
        };
        let info = mgr
            .run(&RunRequest {
                program: program.into(),
                args: vec![flag.into(), "echo sized".into()],
                cwd: dir.path().to_owned(),
                env: Vec::new(),
                size: Some(crate::control::TermSize {
                    cols: 100,
                    rows: 30,
                }),
                agent: Some(AgentKind::ClaudeCode),
                display_name: None,
            })
            .unwrap();
        let s = mgr.get(&info.id).unwrap();
        let mut first = None;
        for _ in 0..200 {
            let (events, _) = lock(s.log()).page_before(None, 1000);
            first = events.into_iter().find_map(|e| match e {
                SessionEvent::Output { data, .. } => Some(data),
                _ => None,
            });
            if first.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let first = first.expect("no output");
        assert!(
            first.starts_with(b"\x1b]7377;100;30\x07"),
            "{:?}",
            String::from_utf8_lossy(&first)
        );
        mgr.stop_all(Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn managed_sessions_get_their_token_and_shells_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let script = if cfg!(windows) {
            "echo token=%VCA_HOOK_TOKEN%"
        } else {
            "echo token=$VCA_HOOK_TOKEN"
        };
        let s = managed(&mgr, dir.path(), script);
        let token = hook_token(&s);
        let mut text = String::new();
        for _ in 0..200 {
            let (events, _) = lock(s.log()).page_before(None, 1000);
            text = events
                .iter()
                .filter_map(|e| match e {
                    SessionEvent::Output { data, .. } => {
                        Some(String::from_utf8_lossy(data).into_owned())
                    }
                    _ => None,
                })
                .collect();
            if text.contains(&token) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(text.contains(&token), "{text}");
        mgr.stop_all(Duration::from_secs(2)).await;
    }

    fn tool(event_name: &str, id: &str) -> String {
        tool_hook(event_name, "default", id, "cargo test")
    }

    /// With parallel calls, one going on says nothing about the other: the
    /// prompt of the waiting one must not vanish.
    #[tokio::test]
    async fn another_call_does_not_clear_a_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        mgr.request_decision(&s, open_decision("d1"));
        hook(&mgr, &s, &tool("PreToolUse", "call-b")).await;
        hook(&mgr, &s, &tool("PostToolUse", "call-b")).await;
        assert!(lock(&s.pending).is_some(), "still waiting");
        assert_eq!(s.info().state, SessionState::AwaitingDecision);
        // Its own call does.
        hook(&mgr, &s, &tool("PostToolUse", "call-a")).await;
        assert!(lock(&s.pending).is_none());
        assert_eq!(s.info().state, SessionState::Running);
    }

    #[tokio::test]
    async fn another_call_does_not_drop_a_held_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        let held = open_decision("d1");
        mgr.defer_decision(&s, held, Some("call-a".into()), None, true);
        hook(&mgr, &s, &tool("PreToolUse", "call-b")).await;
        assert!(lock(&s.deferred).is_some(), "still held");
        hook(&mgr, &s, &tool("PostToolUse", "call-a")).await;
        assert!(lock(&s.deferred).is_none());
    }

    /// The turn ended: nothing is waiting any more, whichever call it was.
    #[tokio::test]
    async fn the_end_of_the_turn_clears_a_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("abc"), ObservedIdentity::default());
        mgr.request_decision(&s, open_decision("d1"));
        hook(&mgr, &s, r#"{"hook_event_name":"Stop","session_id":"abc"}"#).await;
        assert!(lock(&s.pending).is_none());
    }

    #[tokio::test]
    async fn text_without_enter_cannot_answer_an_open_decision_either() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("a"), ObservedIdentity::default());
        *lock(&s.pending) = Some(open_decision("d1"));
        for (body, submit) in [("1", false), ("y", false), ("y\r", false), ("y", true)] {
            let unmarked = Input::Text {
                text: body.into(),
                submit,
                at_prompt: false,
            };
            assert!(refused(&mgr, &s.id, unmarked), "{body:?}");
        }
        // Keys a person taps on the terminal view.
        assert!(!refused(&mgr, &s.id, text(false, true)));
    }

    /// A long command in a mode that settles calls on its own: nothing is on
    /// screen, so nothing is refused.
    #[tokio::test]
    async fn a_decision_held_for_the_agents_prompt_refuses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("a"), ObservedIdentity::default());
        mgr.defer_decision(&s, open_decision("d1"), None, None, true);
        assert!(!refused(&mgr, &s.id, text(true, false)));
    }

    /// Esc at the prompt: no hook names the call, but its result appears in
    /// the transcript.
    #[tokio::test]
    async fn the_result_of_the_call_clears_a_shown_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, 1, Some("a"), ObservedIdentity::default());
        mgr.request_decision(&s, open_decision("d1"));
        let result = |id: &str| HistoryItem::ToolResult {
            at: None,
            id: Some(id.into()),
            text: String::new(),
            is_error: true,
        };
        mgr.observe_deferred(&s, &result("call-b"));
        assert!(lock(&s.pending).is_some(), "another call says nothing");
        mgr.observe_deferred(&s, &result("call-a"));
        assert!(lock(&s.pending).is_none());
        assert_eq!(s.info().state, SessionState::Running);
        assert!(!refused(&mgr, &s.id, text(true, false)));
    }

    /// A prompt that opens between the text and Enter: the text stays in the
    /// input line and Enter is not pressed.
    #[tokio::test]
    async fn enter_is_not_pressed_on_a_decision_that_just_opened() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let wait = if cfg!(windows) { "pause" } else { "read x" };
        let s = managed(&mgr, dir.path(), wait);
        *lock(&s.pending) = Some(open_decision("d1"));
        let err = SessionManager::check_before_enter(&s, &text(true, false)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Conflict);
        assert!(
            crate::util::error_text(&err).contains("not sent"),
            "{err:?}"
        );
        assert!(SessionManager::check_before_enter(&s, &text(true, true)).is_ok());
        mgr.stop_all(Duration::from_secs(2)).await;
    }
}
