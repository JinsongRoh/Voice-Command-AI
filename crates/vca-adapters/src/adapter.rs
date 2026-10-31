//! The [`AgentAdapter`] trait and the registry of built-in adapters.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use vca_core::{AgentKind, LaunchRequest};

use crate::error::{AdapterError, Result};
use crate::heuristics::{HeuristicMatch, OutputHeuristics};
use crate::process::{ProcessMatcher, ProcessTable};
use crate::tail::HistoryStream;
use crate::types::{
    Capabilities, DiscoveredSession, HistoryItem, HookOptions, HookScope, HookStatus, LaunchSpec,
    Profile,
};

/// Identifies a stored session for reading its history. Any field may be
/// missing; adapters use what they need.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLocator {
    pub agent_session_id: Option<String>,
    pub transcript_path: Option<PathBuf>,
    pub project_dir: Option<PathBuf>,
}

impl From<&DiscoveredSession> for SessionLocator {
    fn from(s: &DiscoveredSession) -> Self {
        Self {
            agent_session_id: s.agent_session_id.clone(),
            transcript_path: s.transcript_path.clone(),
            project_dir: s.project_dir.clone(),
        }
    }
}

/// Integration with one local coding agent (architecture §5).
///
/// Adapters are independent of the server: they do not own PTYs or
/// sessions. They discover sessions, describe how to launch the agent,
/// read its history and manage its hook configuration.
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> AgentKind;

    fn capabilities(&self) -> Capabilities;

    /// How running processes of this agent are recognized.
    fn process_matcher(&self) -> ProcessMatcher;

    /// Finds sessions that are running (in `procs`) or stored on disk.
    /// Take one [`ProcessTable::snapshot`] and share it across adapters;
    /// see [`discover_all`].
    async fn discover(&self, procs: &ProcessTable) -> Result<Vec<DiscoveredSession>>;

    /// Builds the command line for a new managed session. `profile`
    /// selects an isolated account configuration.
    fn launch_spec(&self, req: &LaunchRequest, profile: Option<&Profile>) -> Result<LaunchSpec>;

    /// Reads the full stored history of a session.
    async fn read_history(&self, _session: &SessionLocator) -> Result<Vec<HistoryItem>> {
        Err(self.unsupported("reading history"))
    }

    /// Follows a session's transcript and streams new items. With
    /// `from_start`, existing history is streamed first.
    async fn tail_history(
        &self,
        _session: &SessionLocator,
        _from_start: bool,
    ) -> Result<HistoryStream> {
        Err(self.unsupported("following history"))
    }

    /// Installs (or repairs) the agent-side hooks. Idempotent.
    async fn install_hooks(&self, _scope: &HookScope, _opts: &HookOptions) -> Result<HookStatus> {
        Err(self.unsupported("hooks"))
    }

    /// Removes the hooks installed by VCA, leaving all others untouched.
    async fn uninstall_hooks(&self, _scope: &HookScope) -> Result<HookStatus> {
        Err(self.unsupported("hooks"))
    }

    /// Reports which hooks are installed.
    async fn hooks_status(&self, _scope: &HookScope, _opts: &HookOptions) -> Result<HookStatus> {
        Err(self.unsupported("hooks"))
    }

    /// Environment variables that isolate `profile` (for example a
    /// per-profile configuration directory). Empty when unsupported.
    fn profile_env(&self, _profile: &Profile) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Guesses the session state from recent terminal output.
    fn guess_state(&self, recent_output: &str) -> Option<HeuristicMatch> {
        OutputHeuristics::default_set().analyze(recent_output)
    }

    #[doc(hidden)]
    fn unsupported(&self, what: &str) -> AdapterError {
        AdapterError::Unsupported(format!("{what} for {:?}", self.kind()))
    }
}

/// Builds a launch spec for `program` from a request.
pub(crate) fn basic_launch_spec(
    program: &str,
    req: &LaunchRequest,
    env: Vec<(String, String)>,
) -> Result<LaunchSpec> {
    if req.project_dir.as_os_str().is_empty() {
        return Err(AdapterError::InvalidConfig("project_dir is empty".into()));
    }
    Ok(LaunchSpec {
        program: program.to_owned(),
        args: req.args.clone(),
        env,
        cwd: req.project_dir.clone(),
    })
}

/// All built-in adapters, using the current user's default directories.
pub fn builtin_adapters() -> Vec<Arc<dyn AgentAdapter>> {
    let mut out: Vec<Arc<dyn AgentAdapter>> = vec![
        Arc::new(crate::claude::ClaudeAdapter::new()),
        Arc::new(crate::codex::CodexAdapter::new()),
    ];
    out.extend(
        crate::generic::AgentSpec::builtin()
            .into_iter()
            .map(|spec| {
                Arc::new(crate::generic::GenericAdapter::new(spec)) as Arc<dyn AgentAdapter>
            }),
    );
    out
}

/// Returns the built-in adapter for `kind`. Unknown generic commands get a
/// [`crate::generic::GenericAdapter`] for that executable name.
pub fn adapter_for(kind: &AgentKind) -> Arc<dyn AgentAdapter> {
    match kind {
        AgentKind::ClaudeCode => Arc::new(crate::claude::ClaudeAdapter::new()),
        AgentKind::Codex => Arc::new(crate::codex::CodexAdapter::new()),
        other => {
            let spec = crate::generic::AgentSpec::builtin()
                .into_iter()
                .find(|s| &s.kind == other)
                .unwrap_or_else(|| match other {
                    AgentKind::Generic(name) => crate::generic::AgentSpec::generic(name),
                    _ => unreachable!("every non-generic kind has a builtin spec"),
                });
            Arc::new(crate::generic::GenericAdapter::new(spec))
        }
    }
}

/// Takes one process snapshot and runs discovery on every adapter.
/// Adapter failures are logged and skipped.
pub async fn discover_all(adapters: &[Arc<dyn AgentAdapter>]) -> Vec<DiscoveredSession> {
    let procs = match tokio::task::spawn_blocking(ProcessTable::snapshot).await {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(%err, "process snapshot failed");
            ProcessTable::default()
        }
    };
    let mut out = Vec::new();
    for adapter in adapters {
        match adapter.discover(&procs).await {
            Ok(found) => out.extend(found),
            Err(err) => tracing::warn!(%err, agent = ?adapter.kind(), "discovery failed"),
        }
    }
    out
}
