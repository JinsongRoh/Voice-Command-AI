//! Claude Code adapter.
//!
//! * Transcripts: `<config dir>/projects/<encoded project>/<session>.jsonl`,
//!   where the config dir is `CLAUDE_CONFIG_DIR` or `~/.claude`.
//! * Hooks: `settings.json` (see [`settings`]); payloads in [`hook_event`].
//! * Profiles: one `CLAUDE_CONFIG_DIR` per profile.

pub mod hook_event;
pub mod settings;
pub mod transcript;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use vca_core::{AgentKind, LaunchRequest};

use crate::adapter::{AgentAdapter, SessionLocator, basic_launch_spec};
use crate::error::{AdapterError, Result};
use crate::process::{ProcInfo, ProcessMatcher, ProcessTable};
use crate::tail::{self, HistoryStream, JsonlTail};
use crate::types::{
    Capabilities, DiscoveredSession, HistoryItem, HookOptions, HookScope, HookStatus, LaunchSpec,
    Profile, system_time_millis,
};

pub use hook_event::{ClaudeHookEventName, ClaudeHookPayload, render_response};
pub use settings::{ClaudeHookSpec, default_hooks};

/// Environment variable that relocates Claude Code's configuration.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Claude Code adapter for one configuration directory.
#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    config_dir: PathBuf,
    hooks: Vec<ClaudeHookSpec>,
    poll_interval: Duration,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeAdapter {
    /// Uses `CLAUDE_CONFIG_DIR`, or `~/.claude`.
    pub fn new() -> Self {
        Self::with_config_dir(default_config_dir())
    }

    /// Uses an explicit configuration directory (for a profile or tests).
    pub fn with_config_dir(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            hooks: default_hooks(),
            poll_interval: Duration::from_millis(1000),
        }
    }

    /// Adapter for a profile's configuration directory.
    pub fn for_profile(profile: &Profile) -> Self {
        Self::with_config_dir(&profile.config_dir)
    }

    /// Replaces the set of hooks to install.
    pub fn with_hooks(mut self, hooks: Vec<ClaudeHookSpec>) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.config_dir.join("projects")
    }

    /// Expected transcript path of a session (the folder name is derived
    /// from the project directory; very long paths may differ).
    pub fn transcript_path(&self, project_dir: &Path, session_id: &str) -> PathBuf {
        self.projects_dir()
            .join(transcript::encode_project_dir(project_dir))
            .join(format!("{session_id}.jsonl"))
    }

    /// Arguments that resume a stored session in a new process.
    pub fn resume_args(session_id: &str) -> Vec<String> {
        vec!["--resume".into(), session_id.into()]
    }

    fn locate(&self, session: &SessionLocator) -> Result<PathBuf> {
        if let Some(path) = &session.transcript_path {
            return Ok(path.clone());
        }
        let id = session
            .agent_session_id
            .as_deref()
            .ok_or_else(|| AdapterError::NotFound("no session id or transcript path".into()))?;
        if let Some(dir) = &session.project_dir {
            let path = self.transcript_path(dir, id);
            if path.exists() {
                return Ok(path);
            }
        }
        let file = format!("{id}.jsonl");
        std::fs::read_dir(self.projects_dir())?
            .filter_map(|e| e.ok())
            .map(|e| e.path().join(&file))
            .find(|p| p.exists())
            .ok_or_else(|| AdapterError::NotFound(format!("transcript of session {id}")))
    }

    /// Lists stored sessions (blocking).
    fn stored_sessions(&self) -> Vec<DiscoveredSession> {
        let Ok(projects) = std::fs::read_dir(self.projects_dir()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for project in projects.filter_map(|e| e.ok()) {
            let project_path = project.path();
            if !project_path.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&project_path) else {
                continue;
            };
            for file in files.filter_map(|e| e.ok()) {
                let path = file.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
                    || stem.starts_with("agent-")
                {
                    continue;
                }
                let meta = transcript::read_meta(&path).unwrap_or_default();
                let mtime = file
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(system_time_millis);
                let project_dir = meta.cwd.clone().unwrap_or_else(|| {
                    transcript::decode_project_dir(&project.file_name().to_string_lossy())
                });
                out.push(DiscoveredSession {
                    agent: AgentKind::ClaudeCode,
                    agent_session_id: Some(
                        meta.session_id.clone().unwrap_or_else(|| stem.to_owned()),
                    ),
                    project_dir: Some(project_dir),
                    pid: None,
                    last_activity: mtime.max(meta.last_timestamp),
                    title: meta.title(),
                    transcript_path: Some(path),
                });
            }
        }
        out
    }
}

/// `CLAUDE_CONFIG_DIR`, or `~/.claude`.
pub fn default_config_dir() -> PathBuf {
    std::env::var_os(CONFIG_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

/// Session id passed on a Claude Code command line, if any.
fn session_id_from_cmd(cmd: &[String]) -> Option<String> {
    let mut it = cmd.iter();
    while let Some(arg) = it.next() {
        for flag in ["--resume", "-r", "--session-id"] {
            if arg == flag {
                return it.next().filter(|v| !v.starts_with('-')).cloned();
            }
            if let Some(v) = arg.strip_prefix(&format!("{flag}=")) {
                return Some(v.to_owned());
            }
        }
    }
    None
}

/// Assigns running processes to stored sessions: an explicit session id on
/// the command line wins; otherwise the most recently active stored
/// session in the process's working directory. Processes without a match
/// become sessions of their own (a new conversation not yet written).
pub(crate) fn attach_processes(
    agent: AgentKind,
    sessions: &mut Vec<DiscoveredSession>,
    procs: &[&ProcInfo],
    session_id_of: impl Fn(&ProcInfo) -> Option<String>,
) {
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
    let mut index_by_id: HashMap<String, usize> = HashMap::new();
    for (i, s) in sessions.iter().enumerate() {
        if let Some(id) = &s.agent_session_id {
            index_by_id.entry(id.clone()).or_insert(i);
        }
    }
    let mut unmatched = Vec::new();
    // Explicit ids first, so they are not taken by a cwd match.
    let (explicit, implicit): (Vec<&ProcInfo>, Vec<&ProcInfo>) = procs
        .iter()
        .copied()
        .partition(|p| session_id_of(p).is_some());
    for p in explicit {
        let id = session_id_of(p).unwrap_or_default();
        match index_by_id.get(&id) {
            Some(&i) if sessions[i].pid.is_none() => sessions[i].pid = Some(p.pid),
            _ => unmatched.push(p),
        }
    }
    for p in implicit {
        let slot = sessions
            .iter_mut()
            .find(|s| s.pid.is_none() && p.cwd.is_some() && s.project_dir == p.cwd);
        match slot {
            Some(s) => s.pid = Some(p.pid),
            None => unmatched.push(p),
        }
    }
    for p in unmatched {
        sessions.push(DiscoveredSession {
            agent: agent.clone(),
            agent_session_id: session_id_of(p),
            project_dir: p.cwd.clone(),
            pid: Some(p.pid),
            last_activity: Some(p.start_time * 1000),
            title: None,
            transcript_path: None,
        });
    }
}

#[async_trait]
impl AgentAdapter for ClaudeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transcripts: true,
            hooks: true,
            pre_tool_gate: true,
            resume: true,
            profile_isolation: true,
            output_heuristics: true,
            verified: true,
        }
    }

    fn process_matcher(&self) -> ProcessMatcher {
        ProcessMatcher::new(&["claude"], &["@anthropic-ai/claude-code"])
    }

    async fn discover(&self, procs: &ProcessTable) -> Result<Vec<DiscoveredSession>> {
        let this = self.clone();
        let mut sessions = tokio::task::spawn_blocking(move || this.stored_sessions())
            .await
            .map_err(|e| AdapterError::Io(std::io::Error::other(e)))?;
        let matcher = self.process_matcher();
        let running = procs.find(&matcher);
        attach_processes(AgentKind::ClaudeCode, &mut sessions, &running, |p| {
            session_id_from_cmd(&p.cmd)
        });
        Ok(sessions)
    }

    fn launch_spec(&self, req: &LaunchRequest, profile: Option<&Profile>) -> Result<LaunchSpec> {
        let env = profile.map(|p| self.profile_env(p)).unwrap_or_default();
        basic_launch_spec("claude", req, env)
    }

    async fn read_history(&self, session: &SessionLocator) -> Result<Vec<HistoryItem>> {
        let path = self.locate(session)?;
        tokio::task::spawn_blocking(move || tail::read_all(&path, transcript::parse_line))
            .await
            .map_err(|e| AdapterError::Io(std::io::Error::other(e)))?
            .map_err(AdapterError::from)
    }

    async fn tail_history(
        &self,
        session: &SessionLocator,
        from_start: bool,
    ) -> Result<HistoryStream> {
        let path = self.locate(session)?;
        let tail = if from_start {
            JsonlTail::new(path, transcript::parse_line)
        } else {
            JsonlTail::from_end(path, transcript::parse_line)?
        };
        Ok(tail::spawn_tail(tail, self.poll_interval))
    }

    async fn install_hooks(&self, scope: &HookScope, opts: &HookOptions) -> Result<HookStatus> {
        let path = settings::settings_path(&self.config_dir, scope);
        let (hooks, opts) = (self.hooks.clone(), opts.clone());
        blocking(move || settings::install(&path, &hooks, &opts)).await
    }

    async fn uninstall_hooks(&self, scope: &HookScope) -> Result<HookStatus> {
        let path = settings::settings_path(&self.config_dir, scope);
        let hooks = self.hooks.clone();
        blocking(move || settings::uninstall(&path, &hooks, &HookOptions::default())).await
    }

    async fn hooks_status(&self, scope: &HookScope, opts: &HookOptions) -> Result<HookStatus> {
        let path = settings::settings_path(&self.config_dir, scope);
        let (hooks, opts) = (self.hooks.clone(), opts.clone());
        blocking(move || settings::status(&path, &hooks, &opts)).await
    }

    fn profile_env(&self, profile: &Profile) -> Vec<(String, String)> {
        vec![(
            CONFIG_DIR_ENV.to_owned(),
            profile.config_dir.to_string_lossy().into_owned(),
        )]
    }
}

pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AdapterError::Io(std::io::Error::other(e)))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_from_command_line() {
        let cmd = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            session_id_from_cmd(&cmd("claude --resume abc")),
            Some("abc".into())
        );
        assert_eq!(
            session_id_from_cmd(&cmd("claude --session-id=xyz")),
            Some("xyz".into())
        );
        assert_eq!(session_id_from_cmd(&cmd("claude -r --verbose")), None);
        assert_eq!(session_id_from_cmd(&cmd("claude")), None);
    }

    #[test]
    fn profile_env_sets_config_dir() {
        let a = ClaudeAdapter::new();
        let env = a.profile_env(&Profile::new("work", "/p/work"));
        assert_eq!(env, vec![("CLAUDE_CONFIG_DIR".into(), "/p/work".into())]);
        let req = LaunchRequest {
            agent: AgentKind::ClaudeCode,
            project_dir: "/src/app".into(),
            create_dir: false,
            profile: Some("work".into()),
            args: vec!["--model".into(), "opus".into()],
            display_name: None,
        };
        let spec = a
            .launch_spec(&req, Some(&Profile::new("work", "/p/work")))
            .unwrap();
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, ["--model", "opus"]);
        assert_eq!(spec.cwd, PathBuf::from("/src/app"));
        assert_eq!(spec.env.len(), 1);
    }
}
