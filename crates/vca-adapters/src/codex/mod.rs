//! OpenAI Codex CLI adapter.
//!
//! * Sessions: `<CODEX_HOME>/sessions/**/rollout-*.jsonl` (see [`rollout`]),
//!   where `CODEX_HOME` defaults to `~/.codex`.
//! * Notifications: `notify` in `<CODEX_HOME>/config.toml` (see [`notify`]).
//!   The notify program only reports finished turns; Codex has no
//!   pre-tool gate through `notify`, so admission control for Codex needs
//!   the managed PTY or Codex's newer lifecycle hooks (not integrated yet).
//! * Profiles: one `CODEX_HOME` per profile.

pub mod notify;
pub mod rollout;

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use vca_core::{AgentKind, LaunchRequest};

use crate::adapter::{AgentAdapter, SessionLocator, basic_launch_spec};
use crate::claude::{attach_processes, blocking};
use crate::error::{AdapterError, Result};
use crate::process::{ProcessMatcher, ProcessTable};
use crate::tail::{self, HistoryStream, JsonlTail};
use crate::types::{
    Capabilities, DiscoveredSession, HistoryItem, HookOptions, HookScope, HookStatus, LaunchSpec,
    Profile, system_time_millis,
};

pub use notify::{CodexNotifyPayload, NotifyArgs, parse_notify_args};

pub const HOME_ENV: &str = "CODEX_HOME";

/// Codex adapter for one `CODEX_HOME`.
#[derive(Debug, Clone)]
pub struct CodexAdapter {
    home: PathBuf,
    poll_interval: Duration,
    /// Upper bound on rollout files inspected during discovery (newest first).
    max_sessions: usize,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAdapter {
    /// Uses `CODEX_HOME`, or `~/.codex`.
    pub fn new() -> Self {
        Self::with_home(default_home())
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            poll_interval: Duration::from_millis(1000),
            max_sessions: 500,
        }
    }

    pub fn for_profile(profile: &Profile) -> Self {
        Self::with_home(&profile.config_dir)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn config_path(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    /// Arguments that resume a stored session in a new process.
    pub fn resume_args(session_id: &str) -> Vec<String> {
        vec!["resume".into(), session_id.into()]
    }

    /// All rollout files with their modification time, newest first.
    fn rollout_files(&self) -> Vec<(PathBuf, Option<u64>)> {
        let mut out = Vec::new();
        let mut stack = vec![self.home.join("sessions")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                let Ok(ft) = entry.file_type() else { continue };
                if ft.is_dir() {
                    stack.push(path);
                } else if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
                {
                    let mtime = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(system_time_millis);
                    out.push((path, mtime));
                }
            }
        }
        out.sort_by_key(|(_, m)| std::cmp::Reverse(*m));
        out
    }

    fn stored_sessions(&self) -> Vec<DiscoveredSession> {
        self.rollout_files()
            .into_iter()
            .take(self.max_sessions)
            .map(|(path, mtime)| {
                let meta = rollout::read_meta(&path).unwrap_or_default();
                let id = meta.session_id.clone().or_else(|| {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .and_then(rollout::id_from_file_name)
                });
                DiscoveredSession {
                    agent: AgentKind::Codex,
                    agent_session_id: id,
                    project_dir: meta.cwd,
                    pid: None,
                    last_activity: mtime.or(meta.started_at),
                    title: meta.first_prompt,
                    transcript_path: Some(path),
                }
            })
            .collect()
    }

    fn locate(&self, session: &SessionLocator) -> Result<PathBuf> {
        if let Some(path) = &session.transcript_path {
            return Ok(path.clone());
        }
        let id = session
            .agent_session_id
            .as_deref()
            .ok_or_else(|| AdapterError::NotFound("no session id or transcript path".into()))?;
        self.rollout_files()
            .into_iter()
            .map(|(p, _)| p)
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(&format!("{id}.jsonl")))
            })
            .ok_or_else(|| AdapterError::NotFound(format!("rollout of session {id}")))
    }
}

/// `CODEX_HOME`, or `~/.codex`.
pub fn default_home() -> PathBuf {
    std::env::var_os(HOME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

/// Session id on a Codex command line (`codex resume <id>`).
fn session_id_from_cmd(cmd: &[String]) -> Option<String> {
    let pos = cmd.iter().position(|a| a == "resume")?;
    cmd.get(pos + 1).filter(|v| !v.starts_with('-')).cloned()
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transcripts: true,
            hooks: true,
            pre_tool_gate: false,
            resume: true,
            profile_isolation: true,
            output_heuristics: true,
            verified: true,
        }
    }

    fn process_matcher(&self) -> ProcessMatcher {
        ProcessMatcher::new(&["codex"], &["@openai/codex"])
    }

    async fn discover(&self, procs: &ProcessTable) -> Result<Vec<DiscoveredSession>> {
        let this = self.clone();
        let mut sessions = blocking(move || Ok(this.stored_sessions())).await?;
        let running = procs.find(&self.process_matcher());
        attach_processes(AgentKind::Codex, &mut sessions, &running, |p| {
            session_id_from_cmd(&p.cmd)
        });
        Ok(sessions)
    }

    fn launch_spec(&self, req: &LaunchRequest, profile: Option<&Profile>) -> Result<LaunchSpec> {
        let env = profile.map(|p| self.profile_env(p)).unwrap_or_default();
        basic_launch_spec("codex", req, env)
    }

    async fn read_history(&self, session: &SessionLocator) -> Result<Vec<HistoryItem>> {
        let path = self.locate(session)?;
        blocking(move || Ok(tail::read_all(&path, rollout::parse_line)?)).await
    }

    async fn tail_history(
        &self,
        session: &SessionLocator,
        from_start: bool,
    ) -> Result<HistoryStream> {
        let path = self.locate(session)?;
        let tail = if from_start {
            JsonlTail::new(path, rollout::parse_line)
        } else {
            JsonlTail::from_end(path, rollout::parse_line)?
        };
        Ok(tail::spawn_tail(tail, self.poll_interval))
    }

    /// Codex reads `notify` from `config.toml` in `CODEX_HOME` only, so
    /// every scope maps to that file.
    async fn install_hooks(&self, _scope: &HookScope, opts: &HookOptions) -> Result<HookStatus> {
        let (path, opts) = (self.config_path(), opts.clone());
        blocking(move || notify::install(&path, &opts)).await
    }

    async fn uninstall_hooks(&self, _scope: &HookScope) -> Result<HookStatus> {
        let path = self.config_path();
        blocking(move || notify::uninstall(&path, &HookOptions::default())).await
    }

    async fn hooks_status(&self, _scope: &HookScope, opts: &HookOptions) -> Result<HookStatus> {
        let (path, opts) = (self.config_path(), opts.clone());
        blocking(move || notify::status(&path, &opts)).await
    }

    fn profile_env(&self, profile: &Profile) -> Vec<(String, String)> {
        vec![(
            HOME_ENV.to_owned(),
            profile.config_dir.to_string_lossy().into_owned(),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_id_from_cmd() {
        let cmd: Vec<String> = ["codex", "resume", "abc"].map(String::from).to_vec();
        assert_eq!(session_id_from_cmd(&cmd).as_deref(), Some("abc"));
        assert_eq!(session_id_from_cmd(&["codex".to_owned()]), None);
    }

    #[test]
    fn profile_env_sets_codex_home() {
        let env = CodexAdapter::new().profile_env(&Profile::new("p", "/x"));
        assert_eq!(env, vec![("CODEX_HOME".to_owned(), "/x".to_owned())]);
    }
}
