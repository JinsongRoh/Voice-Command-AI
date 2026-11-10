//! Observed sessions: running agents that VCA did not start (SES-01).
//!
//! A pass takes one process snapshot, asks every adapter which processes
//! are its agent, and keeps one observed session per agent process that
//! runs in a terminal. Passes run on a timer and on request
//! ([`Discovery::scan`]), which also reports what was found and why
//! something is not listed. Discovery is read-only: it never attaches to,
//! signals or types into a process.

mod hidden;
mod report;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use vca_adapters::{AgentAdapter, ProcessScanner, ProcessTable, SessionLocator, TerminalTarget};
use vca_core::error::ErrorCode;
use vca_core::protocol::{FoundAgent, FoundStatus};
use vca_core::{
    AgentKind, ControlTier, Error, ServerMsg, SessionId, SessionInfo, SessionOrigin, SessionState,
    UnixMillis, now_millis,
};

use self::hidden::{HIDDEN_FILE, HiddenEntry, HiddenStore};
use self::report::{Host, agent_label, classify_host, is_plain_shell, is_terminal_agent};
use crate::sessions::{Kind, Observed, ObservedIdentity, SessionManager, default_name};
use crate::util::lock;

/// A scan within this time of the last pass answers with that pass.
pub(crate) const SCAN_DEBOUNCE: Duration = Duration::from_secs(3);

/// The process list and terminal lookup; replaced in tests.
#[async_trait]
pub(crate) trait ProcessSource: Send + Sync {
    async fn snapshot(&self) -> ProcessTable;
    /// The terminal that hosts `pid`, found through terminal scripting
    /// interfaces (read-only listing commands).
    async fn terminal(&self, pid: u32, procs: &ProcessTable) -> Option<TerminalTarget>;
}

/// The real process table of this computer. One scanner lives as long as
/// discovery, so a process's command line and directory are read once, not
/// on every pass.
#[derive(Default)]
struct SystemProcesses {
    scanner: Arc<std::sync::Mutex<ProcessScanner>>,
}

#[async_trait]
impl ProcessSource for SystemProcesses {
    async fn snapshot(&self) -> ProcessTable {
        let scanner = Arc::clone(&self.scanner);
        tokio::task::spawn_blocking(move || lock(&scanner).snapshot())
            .await
            .unwrap_or_default()
    }

    async fn terminal(&self, pid: u32, procs: &ProcessTable) -> Option<TerminalTarget> {
        vca_adapters::terminal::detect(pid, procs).await
    }
}

/// Settings of discovery.
#[derive(Debug, Clone)]
pub struct DiscoverySettings {
    pub enabled: bool,
    pub interval: Duration,
    pub detect_terminals: bool,
    /// Where the hidden list is kept; `None` keeps it in memory.
    pub data_dir: Option<PathBuf>,
}

/// One row of the last report with what identifies its process.
#[derive(Debug, Clone)]
struct Row {
    found: FoundAgent,
    process_start: Option<u64>,
}

struct LastPass {
    at: Instant,
    scanned_at: UnixMillis,
    rows: Vec<Row>,
}

#[derive(Default)]
struct PassState {
    /// Observed session of each agent process.
    known: HashMap<u32, SessionId>,
    last: Option<LastPass>,
}

struct Inner {
    mgr: SessionManager,
    adapters: Vec<Arc<dyn AgentAdapter>>,
    source: Box<dyn ProcessSource>,
    settings: DiscoverySettings,
    hidden: HiddenStore,
    debounce: Duration,
    /// One pass at a time; also guards the report.
    state: tokio::sync::Mutex<PassState>,
}

/// Discovery of running agent sessions. Cheap to clone.
#[derive(Clone)]
pub struct Discovery {
    inner: Arc<Inner>,
}

impl Discovery {
    pub fn new(
        mgr: SessionManager,
        adapters: Vec<Arc<dyn AgentAdapter>>,
        settings: DiscoverySettings,
    ) -> Self {
        // The scanner reads the command line of candidate processes only:
        // the built-in names plus every adapter's executable names.
        let source = SystemProcesses::default();
        lock(&source.scanner).watch_names(
            adapters
                .iter()
                .flat_map(|a| a.process_matcher().names)
                .collect::<Vec<_>>(),
        );
        Self::with_source(mgr, adapters, settings, Box::new(source), SCAN_DEBOUNCE)
    }

    pub(crate) fn with_source(
        mgr: SessionManager,
        adapters: Vec<Arc<dyn AgentAdapter>>,
        settings: DiscoverySettings,
        source: Box<dyn ProcessSource>,
        debounce: Duration,
    ) -> Self {
        let hidden = HiddenStore::open(settings.data_dir.as_ref().map(|d| d.join(HIDDEN_FILE)));
        Self {
            inner: Arc::new(Inner {
                mgr,
                adapters,
                source,
                settings,
                hidden,
                debounce,
                state: tokio::sync::Mutex::new(PassState::default()),
            }),
        }
    }

    /// True when discovery runs (the `[discovery]` setting).
    pub fn enabled(&self) -> bool {
        self.inner.settings.enabled
    }

    /// Starts the periodic passes, when enabled.
    pub fn start(&self) -> Option<tokio::task::JoinHandle<()>> {
        if !self.enabled() {
            return None;
        }
        let this = self.clone();
        Some(tokio::spawn(async move {
            let interval = this.inner.settings.interval.max(Duration::from_millis(500));
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let _watch = crate::observed_watch::spawn(this.inner.mgr.clone());
            loop {
                tick.tick().await;
                let mut state = this.inner.state.lock().await;
                this.pass(&mut state).await;
            }
        }))
    }

    fn off() -> Error {
        Error::api(
            ErrorCode::Unavailable,
            "session discovery is turned off on this computer",
        )
    }

    /// Runs a pass now (or reuses one from the last few seconds) and
    /// reports the agent processes found.
    pub async fn scan(&self) -> Result<ServerMsg, Error> {
        if !self.enabled() {
            return Err(Self::off());
        }
        let mut state = self.inner.state.lock().await;
        let fresh = state
            .last
            .as_ref()
            .is_some_and(|l| l.at.elapsed() < self.inner.debounce);
        if !fresh {
            self.pass(&mut state).await;
        }
        Ok(report(&state))
    }

    /// Takes a session off the list and keeps it off while its process
    /// runs. The process is not touched.
    pub async fn hide(
        &self,
        session_id: Option<SessionId>,
        pid: Option<u32>,
    ) -> Result<ServerMsg, Error> {
        if !self.enabled() {
            return Err(Self::off());
        }
        let pid = match (session_id, pid) {
            (Some(id), _) => self.observed_pid(&id)?,
            (None, Some(pid)) => pid,
            (None, None) => {
                return Err(Error::api(
                    ErrorCode::InvalidRequest,
                    "give a session id or a process id",
                ));
            }
        };
        let mut state = self.inner.state.lock().await;
        // A process the last pass did not see: look again, at most once per
        // debounce period.
        let fresh = state
            .last
            .as_ref()
            .is_some_and(|l| l.at.elapsed() < self.inner.debounce);
        if !has_row(&state, pid) && !fresh {
            self.pass(&mut state).await;
        }
        let Some(row) = state
            .last
            .as_ref()
            .and_then(|l| l.rows.iter().find(|r| r.found.pid == pid))
            .cloned()
        else {
            return Err(Error::api(
                ErrorCode::NotFound,
                format!("no AI session runs as process {pid}"),
            ));
        };
        self.inner
            .hidden
            .add(HiddenEntry {
                pid,
                process_start: row.process_start,
                agent: row.found.agent.clone(),
                project_dir: row.found.project_dir.clone(),
                session_id: row.found.session_id.clone(),
                hidden_at: now_millis(),
            })
            .map_err(|err| {
                tracing::warn!(%err, "cannot save {HIDDEN_FILE}");
                Error::api(ErrorCode::Internal, "the hidden list could not be saved")
            })?;
        if let Some(id) = state.known.remove(&pid) {
            self.drop_session(&id);
        }
        if let Some(last) = state.last.as_mut()
            && let Some(r) = last.rows.iter_mut().find(|r| r.found.pid == pid)
            && r.found.status != FoundStatus::UnsupportedAgent
        {
            r.found.status = FoundStatus::HiddenByUser;
            r.found.session_id = None;
        }
        Ok(report(&state))
    }

    /// Lists hidden sessions again (one, or `all`) and runs a pass so they
    /// come back at once.
    pub async fn unhide(
        &self,
        session_id: Option<SessionId>,
        pid: Option<u32>,
        all: bool,
    ) -> Result<ServerMsg, Error> {
        if !self.enabled() {
            return Err(Self::off());
        }
        if !all && session_id.is_none() && pid.is_none() {
            return Err(Error::api(
                ErrorCode::InvalidRequest,
                "give a session id, a process id or all",
            ));
        }
        let mut state = self.inner.state.lock().await;
        let removed = self
            .inner
            .hidden
            .remove_where(|e| {
                all || pid.is_some_and(|p| p == e.pid)
                    || session_id
                        .as_ref()
                        .is_some_and(|id| e.session_id.as_ref() == Some(id))
            })
            .map_err(|err| {
                tracing::warn!(%err, "cannot save {HIDDEN_FILE}");
                Error::api(ErrorCode::Internal, "the hidden list could not be saved")
            })?;
        if removed == 0 && !all {
            return Err(Error::api(
                ErrorCode::NotFound,
                "that session is not hidden",
            ));
        }
        // Nothing changed: a recent pass is as good as a new one.
        let fresh = state
            .last
            .as_ref()
            .is_some_and(|l| l.at.elapsed() < self.inner.debounce);
        if removed > 0 || !fresh {
            self.pass(&mut state).await;
        }
        Ok(report(&state))
    }

    /// The process of an observed session (the only kind that can be
    /// hidden).
    fn observed_pid(&self, id: &SessionId) -> Result<u32, Error> {
        let session = self
            .inner
            .mgr
            .get(id)
            .ok_or_else(|| Error::api(ErrorCode::NotFound, format!("no session {id}")))?;
        if !matches!(session.kind, Kind::Observed(_)) {
            return Err(Error::api(
                ErrorCode::InvalidRequest,
                "only sessions found on the computer can be hidden",
            ));
        }
        session
            .pid()
            .ok_or_else(|| Error::api(ErrorCode::InvalidRequest, "the session has no process"))
    }

    /// Takes an observed session card away (the process keeps running).
    fn drop_session(&self, id: &SessionId) {
        let mgr = &self.inner.mgr;
        if let Some(s) = mgr.get(id)
            && matches!(s.kind, Kind::Observed(_))
        {
            mgr.remove(id);
        }
    }

    /// One discovery pass: updates the observed sessions and the report.
    async fn pass(&self, state: &mut PassState) {
        let inner = &*self.inner;
        let mgr = &inner.mgr;
        let procs = inner.source.snapshot().await;
        let mut found = Vec::new();
        for adapter in &inner.adapters {
            match adapter.discover(&procs).await {
                Ok(list) => found.extend(list),
                Err(err) => tracing::warn!(%err, agent = ?adapter.kind(), "discovery failed"),
            }
        }

        let mut managed_pids = HashSet::new();
        let mut managed_agent_ids = HashSet::new();
        for s in mgr.all() {
            if s.managed().is_none() {
                continue;
            }
            if let Some(pid) = s.pid() {
                managed_pids.insert(pid);
                managed_pids.extend(procs.descendants(pid));
            }
            if let Some(id) = lock(&s.agent_session_id).clone() {
                managed_agent_ids.insert(id);
            }
        }
        let agent_pids: HashMap<u32, AgentKind> = found
            .iter()
            .filter_map(|d| d.pid.map(|p| (p, d.agent.clone())))
            .collect();
        // Running agents per agent kind and folder: a new conversation file
        // in a folder two of them share cannot be told apart.
        let mut per_folder: HashMap<(AgentKind, PathBuf), usize> = HashMap::new();
        for d in found.iter().filter(|d| d.pid.is_some()) {
            if let Some(dir) = &d.project_dir {
                *per_folder
                    .entry((d.agent.clone(), dir.clone()))
                    .or_default() += 1;
            }
        }

        let mut rows: Vec<Row> = Vec::new();
        let mut reported = HashSet::new();
        let mut seen = HashSet::new();
        for d in found {
            let Some(pid) = d.pid else { continue };
            if managed_pids.contains(&pid)
                || d.agent_session_id
                    .as_ref()
                    .is_some_and(|id| managed_agent_ids.contains(id))
                || !reported.insert(pid)
            {
                continue;
            }
            let proc_info = procs.get(pid);
            if proc_info.is_some_and(is_plain_shell) {
                continue;
            }
            let process_start = proc_info.map(|p| p.start_time);
            let host = classify_host(pid, &procs, &agent_pids);
            let project_dir = d
                .project_dir
                .as_ref()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.display().to_string());
            let mut row = Row {
                found: FoundAgent {
                    pid,
                    agent: d.agent.clone(),
                    project_dir,
                    terminal: match &host {
                        Host::Terminal(label) => label.clone(),
                        Host::App(label) => Some(label.clone()),
                        Host::Unknown => None,
                    },
                    title: d.title.clone(),
                    status: FoundStatus::Imported,
                    session_id: None,
                },
                process_start,
            };
            let not_listed = if !is_terminal_agent(&d.agent) {
                Some(FoundStatus::UnsupportedAgent)
            } else if inner.hidden.contains(pid, process_start) {
                Some(FoundStatus::HiddenByUser)
            } else if matches!(host, Host::App(_)) {
                Some(FoundStatus::NotInATerminal)
            } else {
                None
            };
            if let Some(status) = not_listed {
                if let Some(id) = state.known.remove(&pid) {
                    self.drop_session(&id);
                }
                row.found.status = status;
                rows.push(row);
                continue;
            }
            seen.insert(pid);

            let existing = state.known.get(&pid).and_then(|id| mgr.get(id));
            let session = match existing {
                Some(s) => {
                    if let Some(at) = d.last_activity {
                        mgr.update_info(&s, |info| {
                            let changed = at > info.last_activity;
                            info.last_activity = info.last_activity.max(at);
                            changed
                        });
                    }
                    // A transcript that turned up after the process was first
                    // seen (a new conversation writes its file with the first
                    // message) is followed from now on; a no-op when already
                    // followed. Another file for the same process is its new
                    // conversation (`/clear`), when nothing else can own it.
                    match (&d.transcript_path, s.followed_transcript()) {
                        (Some(new), Some(current)) if *new != current => {
                            let shared = d.project_dir.as_ref().is_some_and(|dir| {
                                per_folder
                                    .get(&(d.agent.clone(), dir.clone()))
                                    .is_some_and(|n| *n > 1)
                            });
                            if !shared && new_conversation_of(mgr, &s, process_start, &current, new)
                            {
                                mgr.follow_new_conversation(&s, SessionLocator::from(&d));
                            }
                        }
                        (Some(_), _) => mgr.follow_transcript(&s, SessionLocator::from(&d)),
                        (None, _) => {}
                    }
                    s
                }
                None => {
                    let terminal = if inner.settings.detect_terminals {
                        inner.source.terminal(pid, &procs).await
                    } else {
                        None
                    };
                    let dir = d.project_dir.clone().unwrap_or_else(PathBuf::new);
                    let now = now_millis();
                    let info = SessionInfo {
                        id: SessionId::new(),
                        origin: SessionOrigin::Local {
                            agent: d.agent.clone(),
                            project_dir: dir.clone(),
                            pid: Some(pid),
                        },
                        display_name: d
                            .title
                            .clone()
                            .unwrap_or_else(|| default_name(&d.agent, &dir)),
                        state: SessionState::Running,
                        control: if terminal.is_some() {
                            ControlTier::TerminalApi
                        } else {
                            ControlTier::ObserveOnly
                        },
                        profile: None,
                        created_at: now,
                        last_activity: d.last_activity.unwrap_or(now),
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
                        terminal,
                        identity: ObservedIdentity {
                            transcript_path: d.transcript_path.clone(),
                            process_start,
                        },
                    };
                    let session = mgr.insert_observed(info, observed, d.agent_session_id.clone());
                    if d.transcript_path.is_some() {
                        mgr.follow_transcript(&session, SessionLocator::from(&d));
                    }
                    state.known.insert(pid, session.id.clone());
                    session
                }
            };
            let transcripts = inner
                .adapters
                .iter()
                .find(|a| a.kind() == d.agent)
                .is_some_and(|a| a.capabilities().transcripts);
            row.found.status = if transcripts
                && d.transcript_path.is_none()
                && session.transcript_path().is_none()
            {
                FoundStatus::NoTranscriptYet
            } else {
                FoundStatus::Imported
            };
            row.found.session_id = Some(session.id.clone());
            row.found.title = Some(session.info().display_name);
            if row.found.terminal.is_none()
                && let Kind::Observed(o) = &session.kind
                && let Some(t) = &o.terminal
            {
                row.found.terminal = Some(report::target_label(t).to_owned());
            }
            rows.push(row);
        }

        state.known.retain(|pid, id| {
            if seen.contains(pid) {
                return true;
            }
            if let Some(s) = mgr.get(id) {
                // The pid may be reused while the card stays listed.
                if let Some(broker) = mgr.broker() {
                    broker.unmonitor_session(id);
                }
                mgr.set_state(&s, SessionState::Exited { code: None });
                mgr.schedule_removal(&s);
            }
            false
        });
        inner.hidden.prune(&procs);

        rows.sort_by(|a, b| {
            (
                agent_label(&a.found.agent),
                &a.found.project_dir,
                a.found.pid,
            )
                .cmp(&(
                    agent_label(&b.found.agent),
                    &b.found.project_dir,
                    b.found.pid,
                ))
        });
        state.last = Some(LastPass {
            at: Instant::now(),
            scanned_at: now_millis(),
            rows,
        });
    }
}

/// Is `new` the new conversation of the observed session `session`, whose
/// transcript is `current` now? Discovery assigns a running process the
/// newest conversation file in its folder, so after `/clear` that is the
/// new file. It is taken only when it is the same process (pid and start
/// time), the file lies in the same project folder, is not older than the
/// one followed, and no other session owns it (its conversation id or
/// file). The caller also refuses a folder that two running agents share.
fn new_conversation_of(
    mgr: &SessionManager,
    session: &crate::sessions::Session,
    process_start: Option<u64>,
    current: &std::path::Path,
    new: &std::path::Path,
) -> bool {
    let Kind::Observed(observed) = &session.kind else {
        return false;
    };
    if matches!(session.info().state, SessionState::Exited { .. }) {
        return false;
    }
    let same_process = match (observed.identity.process_start, process_start) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    if !same_process || new.parent() != current.parent() {
        return false;
    }
    let modified = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if let (Some(new_at), Some(current_at)) = (modified(new), modified(current))
        && new_at < current_at
    {
        return false;
    }
    let new_id = new.file_stem().and_then(|s| s.to_str());
    !mgr.all()
        .iter()
        .filter(|s| s.id != session.id)
        .any(|other| {
            other.followed_transcript().as_deref() == Some(new)
                || (new_id.is_some() && other.agent_session_id().as_deref() == new_id)
        })
}

fn has_row(state: &PassState, pid: u32) -> bool {
    state
        .last
        .as_ref()
        .is_some_and(|l| l.rows.iter().any(|r| r.found.pid == pid))
}

fn report(state: &PassState) -> ServerMsg {
    match &state.last {
        Some(last) => ServerMsg::ScanReport {
            found: last.rows.iter().map(|r| r.found.clone()).collect(),
            scanned_at: last.scanned_at,
        },
        None => ServerMsg::ScanReport {
            found: Vec::new(),
            scanned_at: 0,
        },
    }
}
