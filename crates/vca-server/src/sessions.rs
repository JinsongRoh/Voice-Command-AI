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
    AgentKind, ControlTier, Decision, DecisionOption, Error, Input, LaunchRequest, SessionAction,
    SessionEvent, SessionId, SessionInfo, SessionOrigin, SessionState, now_millis,
};
use vca_pty::{AttachServer, PtySession, PtySize, SpawnOptions};

use crate::control::{CONTROL_ENV, RunRequest, SESSION_ENV};
use crate::events::{EventLog, LogLimits};
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
}

/// Change notifications for session cards.
#[derive(Debug, Clone)]
pub enum Update {
    Changed(SessionInfo),
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
}

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
    /// Bumped on every (re)start so stale exit notifications are ignored.
    generation: AtomicU64,
    last_output: Mutex<Instant>,
}

pub(crate) struct Observed {
    pub terminal: Option<TerminalTarget>,
}

pub(crate) enum Kind {
    Managed(Box<Managed>),
    Observed(Observed),
}

/// One session card and everything behind it.
pub struct Session {
    pub(crate) id: SessionId,
    info: Mutex<SessionInfo>,
    log: Arc<Mutex<EventLog>>,
    pub(crate) kind: Kind,
    pub(crate) pending: Mutex<Option<PendingDecision>>,
    /// True once a hook reported on this session; output heuristics then
    /// no longer drive its state.
    pub(crate) hook_seen: AtomicBool,
    pub(crate) agent_session_id: Mutex<Option<String>>,
    transcript_started: AtomicBool,
    input: mpsc::UnboundedSender<InputJob>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
    }
}

impl Session {
    pub fn info(&self) -> SessionInfo {
        lock(&self.info).clone()
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
            Kind::Observed(_) => None,
        }
    }

    pub(crate) fn managed(&self) -> Option<&Managed> {
        match &self.kind {
            Kind::Managed(m) => Some(m),
            Kind::Observed(_) => None,
        }
    }

    pub(crate) fn push(&self, make: impl FnOnce(u64) -> SessionEvent) -> SessionEvent {
        lock(&self.log).push(make)
    }

    fn add_task(&self, task: JoinHandle<()>) {
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

/// Whether a heuristic decision is answered with Enter after the option.
fn options_need_submit(options: &[DecisionOption]) -> bool {
    !options
        .iter()
        .all(|o| !o.id.is_empty() && o.id.chars().all(|c| c.is_ascii_digit()))
}

impl SessionManager {
    pub fn new(cfg: ManagerConfig, broker: Option<Arc<Broker>>) -> Self {
        let (updates, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                cfg,
                sessions: Mutex::new(HashMap::new()),
                updates,
                broker,
            }),
        }
    }

    pub fn broker(&self) -> Option<&Arc<Broker>> {
        self.inner.broker.as_ref()
    }

    pub fn subscribe_updates(&self) -> broadcast::Receiver<Update> {
        self.inner.updates.subscribe()
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut out: Vec<SessionInfo> = lock(&self.inner.sessions)
            .values()
            .map(|s| s.info())
            .collect();
        out.sort_by_key(|s| s.created_at);
        out
    }

    pub fn get(&self, id: &SessionId) -> Option<Arc<Session>> {
        lock(&self.inner.sessions).get(id).cloned()
    }

    pub(crate) fn all(&self) -> Vec<Arc<Session>> {
        lock(&self.inner.sessions).values().cloned().collect()
    }

    fn require(&self, id: &SessionId) -> Result<Arc<Session>, Error> {
        self.get(id).ok_or_else(|| not_found(id))
    }

    pub(crate) fn notify(&self, session: &Session) {
        let _ = self.inner.updates.send(Update::Changed(session.info()));
    }

    /// Changes a session's state, logging and announcing it.
    pub(crate) fn set_state(&self, session: &Session, state: SessionState) {
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
        }
        session.push(|seq| SessionEvent::StateChanged {
            seq,
            at: now_millis(),
            state,
        });
        self.notify(session);
    }

    pub(crate) fn add_unread(&self, session: &Session) {
        lock(&session.info).unread += 1;
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

    fn insert(&self, session: Arc<Session>) {
        lock(&self.inner.sessions).insert(session.id.clone(), Arc::clone(&session));
        let _ = self.inner.updates.send(Update::Changed(session.info()));
    }

    pub(crate) fn remove(&self, id: &SessionId) {
        let removed = lock(&self.inner.sessions).remove(id);
        if let Some(session) = removed {
            if let Some(broker) = &self.inner.broker {
                broker.unregister_session(id);
            }
            if let Some(m) = session.managed() {
                lock(&m.attach).take();
                let _ = lock(&m.pty).kill();
            }
            let _ = self.inner.updates.send(Update::Removed(id.clone()));
        }
    }

    fn new_session(&self, id: SessionId, info: SessionInfo, kind: Kind) -> Arc<Session> {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let session = Arc::new(Session {
            id,
            info: Mutex::new(info),
            log: Arc::new(Mutex::new(EventLog::new(self.inner.cfg.limits))),
            kind,
            pending: Mutex::new(None),
            hook_seen: AtomicBool::new(false),
            agent_session_id: Mutex::new(None),
            transcript_started: AtomicBool::new(false),
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
        let dir = self
            .inner
            .cfg
            .workspace_roots
            .prepare(&req.project_dir, req.create_dir)?;
        let adapter = vca_adapters::adapter_for(&req.agent);
        let mut checked = req.clone();
        checked.project_dir = dir.clone();
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
        self.start_managed(req.agent.clone(), adapter, dir, name, opts)
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
        let adapter = vca_adapters::adapter_for(&agent);
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
        let opts = req
            .env
            .iter()
            .filter(|(k, _)| k != SESSION_ENV && k != CONTROL_ENV)
            .fold(opts, |o, (k, v)| o.env(k.as_str(), v.as_str()));
        let name = req
            .display_name
            .clone()
            .unwrap_or_else(|| default_name(&agent, &req.cwd));
        self.start_managed(agent, adapter, req.cwd.clone(), name, opts)
    }

    fn start_managed(
        &self,
        agent: AgentKind,
        adapter: Arc<dyn AgentAdapter>,
        project_dir: PathBuf,
        name: String,
        opts: SpawnOptions,
    ) -> Result<SessionInfo, Error> {
        let id = SessionId::new();
        let opts = opts.env(SESSION_ENV, id.0.as_str()).env(
            CONTROL_ENV,
            crate::control::endpoint_env_value(&self.inner.cfg.control_endpoint),
        );
        let pty = PtySession::spawn(opts.clone())
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
                generation: AtomicU64::new(0),
                last_output: Mutex::new(Instant::now()),
            })),
        );
        self.insert(Arc::clone(&session));
        self.watch_pty(&session, pty, 0);
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
        session.add_task(tokio::spawn(async move {
            while let Some(ev) = sub.recv().await {
                let vca_pty::OutputEvent::Data(chunk) = ev else {
                    continue;
                };
                let Some(session) = weak.upgrade() else {
                    return;
                };
                if let Some(m) = session.managed() {
                    *lock(&m.last_output) = Instant::now();
                }
                let at = now_millis();
                lock(&session.info).last_activity = at;
                session.push(|seq| SessionEvent::Output {
                    seq,
                    at,
                    data: chunk.data.to_vec(),
                });
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
                broker.admission().release_by_key(&session.id.0);
            }
            lock(&session.pending).take();
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

    /// Queues input for delivery. The returned receiver resolves once the
    /// input reached the session (or failed).
    pub fn send_input(
        &self,
        id: &SessionId,
        input: Input,
    ) -> Result<oneshot::Receiver<Result<(), Error>>, Error> {
        let session = self.require(id)?;
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
            .send(InputJob { input, done })
            .map_err(|_| api_error(ErrorCode::Unavailable, "session input closed"))?;
        Ok(rx)
    }

    async fn deliver(&self, session: &Session, input: Input) -> Result<(), Error> {
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
                }
            }
            other => other,
        };
        match &session.kind {
            Kind::Managed(m) => {
                let pty = lock(&m.pty).clone();
                pty.send_input(&input)
                    .await
                    .map_err(|e| api_error(ErrorCode::Unavailable, e.to_string()))?;
            }
            Kind::Observed(o) => {
                let Some(target) = &o.terminal else {
                    return Err(api_error(ErrorCode::Unsupported, "no input channel"));
                };
                let Input::Text { text, submit } = &input else {
                    return Err(api_error(
                        ErrorCode::Unsupported,
                        "only text can be typed into this terminal",
                    ));
                };
                vca_adapters::terminal::inject(target, text, *submit)
                    .await
                    .map_err(|e| api_error(ErrorCode::Unavailable, e.to_string()))?;
            }
        }
        lock(&session.info).last_activity = now_millis();
        Ok(())
    }

    /// Pause, resume, stop or restart a session (SES-06).
    pub async fn control(&self, id: &SessionId, action: SessionAction) -> Result<(), Error> {
        let session = self.require(id)?;
        let broker_err =
            |e: vca_broker::BrokerError| api_error(ErrorCode::Unavailable, e.to_string());
        match action {
            SessionAction::Pause | SessionAction::Resume => {
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
                // Only the process VCA started is killed. Closing its
                // pseudo-console ends the console processes attached to it;
                // the broker's tree termination is deliberately not used
                // until it can only reach processes VCA owns.
                Kind::Managed(m) => {
                    let pty = lock(&m.pty).clone();
                    pty.kill()
                        .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
                    Ok(())
                }
                // VCA never terminates processes it did not start.
                Kind::Observed(_) => Err(api_error(
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
        let _ = old.kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), old.wait()).await;
        lock(&m.attach).take();
        let pty = PtySession::spawn(m.spawn.clone())
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
        let attach = AttachServer::bind(&m.endpoint, pty.clone())
            .map_err(|e| api_error(ErrorCode::Internal, format!("attach server: {e}")))?;
        *lock(&m.pty) = pty.clone();
        *lock(&m.attach) = Some(attach);
        lock(&session.pending).take();
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

    /// Streams the agent's transcript into the session's chat events.
    pub(crate) fn follow_transcript(&self, session: &Arc<Session>, locator: SessionLocator) {
        if session.transcript_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(agent) = session.agent() else {
            return;
        };
        let weak = Arc::downgrade(session);
        let task = tokio::spawn(async move {
            let adapter = vca_adapters::adapter_for(&agent);
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
            while let Some(item) = stream.recv().await {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                if matches!(item, HistoryItem::ToolResult { .. }) {
                    continue;
                }
                session.push(|seq| item.to_event(seq, now_millis()));
            }
        });
        session.add_task(task);
    }

    /// Stops every managed session (used at shutdown).
    pub async fn stop_all(&self, grace: Duration) {
        let managed: Vec<PtySession> = self
            .all()
            .iter()
            .filter_map(|s| s.managed().map(|m| lock(&m.pty).clone()))
            .collect();
        for pty in &managed {
            let _ = pty.kill();
        }
        let _ = tokio::time::timeout(grace, async {
            for pty in &managed {
                let _ = pty.wait().await;
            }
        })
        .await;
    }

    pub(crate) fn insert_observed(&self, info: SessionInfo, observed: Observed) -> Arc<Session> {
        // Observed sessions are not registered with the broker, so neither
        // autonomous containment nor user actions can reach processes VCA
        // did not start.
        let session = self.new_session(info.id.clone(), info, Kind::Observed(observed));
        self.insert(Arc::clone(&session));
        session
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
                let same = lock(&session.pending)
                    .as_ref()
                    .is_some_and(|p| p.decision.prompt == prompt);
                if !same {
                    let decision = Decision {
                        id: format!("h-{}-{}", session.id, lock(&session.log).next_seq()),
                        prompt,
                        options: m.options.clone(),
                    };
                    self.request_decision(
                        session,
                        PendingDecision {
                            submit: options_need_submit(&decision.options),
                            decision,
                            heuristic: true,
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
    mut rx: mpsc::UnboundedReceiver<InputJob>,
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
        let preview = truncate_chars(&snap.tail(PREVIEW_LINES).join("\n"), PREVIEW_CHARS, true);
        let preview = (!preview.is_empty()).then_some(preview);
        mgr.update_info(&session, |info| {
            if info.preview != preview {
                info.preview = preview;
                true
            } else {
                false
            }
        });
        if session.hook_seen.load(Ordering::SeqCst) {
            continue;
        }
        let busy = lock(&m.last_output).elapsed() < RECENT_OUTPUT;
        let found = m.adapter.guess_state(&snap.text());
        mgr.apply_heuristic(&session, found, busy);
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
}
