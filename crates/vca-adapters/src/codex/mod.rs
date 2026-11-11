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
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use vca_core::{AgentKind, LaunchRequest};

use crate::adapter::{AgentAdapter, SessionLocator, basic_launch_spec};
use crate::claude::{attach_processes, blocking};
use crate::error::{AdapterError, Result};
use crate::meta_cache::{self, FileStamp, SharedCache};
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
    /// What discovery read from each rollout (shared by clones).
    stored: SharedCache<rollout::RolloutMeta>,
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
            stored: SharedCache::default(),
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

    /// All rollout files with their stamp in the directory listing,
    /// newest first.
    fn rollout_files(&self) -> Vec<(std::fs::DirEntry, FileStamp)> {
        let mut out = Vec::new();
        let mut stack = vec![self.home.join("sessions")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let Ok(ft) = entry.file_type() else { continue };
                if ft.is_dir() {
                    stack.push(entry.path());
                } else if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
                    && let Some(stamp) = meta_cache::stamp(&entry, false)
                {
                    out.push((entry, stamp));
                }
            }
        }
        out.sort_by_key(|(_, s)| std::cmp::Reverse(s.modified()));
        out
    }

    /// The newest rollouts as sessions (blocking). A rollout is read again
    /// only when its size or modification time changed; entries of rollouts
    /// that were deleted or fell out of the newest [`Self::max_sessions`] are
    /// dropped.
    fn stored_sessions(&self) -> Vec<DiscoveredSession> {
        let now = SystemTime::now();
        let mut cache = self.stored.lock();
        cache.begin();
        let out = self
            .rollout_files()
            .into_iter()
            .take(self.max_sessions)
            .map(|(entry, listed)| {
                let path = entry.path();
                // Codex keeps a rollout open while it runs, and a directory
                // listing may then lag behind; a rollout read before its
                // first prompt is checked exactly until it has one.
                let incomplete = cache.peek(&path).is_some_and(|m| m.first_prompt.is_none());
                let stamp = if listed.is_recent(now) || incomplete {
                    meta_cache::stamp(&entry, true).unwrap_or(listed)
                } else {
                    listed
                };
                let meta = cache.get(&path, stamp, || {
                    rollout::read_meta(&path).unwrap_or_default()
                });
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
                    last_activity: stamp
                        .modified()
                        .and_then(system_time_millis)
                        .or(meta.started_at),
                    title: meta.first_prompt,
                    transcript_path: Some(path),
                }
            })
            .collect();
        cache.finish();
        out
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
            .map(|(e, _)| e.path())
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

    /// Codex stores rollouts by date; the rollout's metadata names the
    /// directory it was started in.
    fn new_transcript_since(
        &self,
        cwd: &Path,
        since: vca_core::UnixMillis,
        taken: &[String],
    ) -> Option<SessionLocator> {
        let normalized: PathBuf = cwd.components().collect();
        // Newest first; a fresh rollout is among the first few.
        let candidates: Vec<(PathBuf, rollout::RolloutMeta)> = self
            .rollout_files()
            .into_iter()
            .take(32)
            .map(|(e, _)| e.path())
            .filter_map(|p| {
                let meta = rollout::read_meta(&p).ok()?;
                let same = meta
                    .cwd
                    .as_deref()
                    .is_some_and(|c| c.components().collect::<PathBuf>() == normalized);
                let claimed = meta
                    .session_id
                    .as_ref()
                    .is_some_and(|id| taken.contains(id));
                (same && !claimed).then_some((p, meta))
            })
            .collect();
        let path =
            crate::adapter::newest_file_since(candidates.iter().map(|(p, _)| p.clone()), since)?;
        let meta = candidates.into_iter().find(|(p, _)| *p == path)?.1;
        Some(SessionLocator {
            agent_session_id: meta.session_id,
            transcript_path: Some(path),
            project_dir: Some(cwd.to_owned()),
        })
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

    fn write_rollout(home: &Path, day: &str, id: &str, prompt: Option<&str>) -> PathBuf {
        let dir = home.join("sessions").join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-11-01T10-00-00-{id}.jsonl"));
        let mut text = format!(
            r#"{{"timestamp":"2026-11-01T10:00:00.000Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/w/{id}","timestamp":"2026-11-01T10:00:00.000Z"}}}}"#
        );
        text.push('\n');
        if let Some(p) = prompt {
            text.push_str(&user_line(p));
        }
        std::fs::write(&path, text).unwrap();
        path
    }

    fn user_line(text: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-11-01T10:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{text}"}}]}}}}"#
        ) + "\n"
    }

    #[test]
    fn rollouts_are_read_again_only_when_changed() {
        use std::io::Write;

        let home = tempfile::tempdir().unwrap();
        let a = write_rollout(home.path(), "2026/11/01", "aaa", None);
        let b = write_rollout(home.path(), "2026/11/02", "bbb", Some("hello"));
        let adapter = CodexAdapter::with_home(home.path());
        let reads = || adapter.stored.lock().reads();
        let pass = || adapter.clone().stored_sessions();

        assert_eq!(pass().len(), 2);
        assert_eq!(reads(), 2);
        assert_eq!(pass().len(), 2);
        assert_eq!(reads(), 2, "unchanged rollouts are not read");

        // The first prompt arrives: that rollout is read again.
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        write!(f, "{}", user_line("fix the build")).unwrap();
        drop(f);
        let found = pass();
        let title = |id: &str| {
            found
                .iter()
                .find(|s| s.agent_session_id.as_deref() == Some(id))
                .and_then(|s| s.title.clone())
        };
        assert_eq!(title("aaa").as_deref(), Some("fix the build"));
        assert_eq!(title("bbb").as_deref(), Some("hello"));
        assert_eq!(reads(), 3);

        // A deleted rollout is evicted.
        std::fs::remove_file(&b).unwrap();
        assert_eq!(pass().len(), 1);
        assert_eq!(adapter.stored.lock().len(), 1);
        assert_eq!(reads(), 3);
    }

    #[test]
    fn per_pass_reads_do_not_grow_with_stored_rollouts() {
        let home = tempfile::tempdir().unwrap();
        for i in 0..120 {
            write_rollout(
                home.path(),
                &format!("2026/10/{:02}", i % 28 + 1),
                &format!("id{i}"),
                Some("p"),
            );
        }
        let adapter = CodexAdapter::with_home(home.path());
        assert_eq!(adapter.stored_sessions().len(), 120);
        assert_eq!(adapter.stored.lock().reads(), 120);
        for _ in 0..5 {
            assert_eq!(adapter.stored_sessions().len(), 120);
        }
        assert_eq!(adapter.stored.lock().reads(), 120);
    }

    #[test]
    fn the_cache_holds_only_the_newest_rollouts() {
        let home = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write_rollout(home.path(), "2026/10/01", &format!("id{i}"), Some("p"));
        }
        let mut adapter = CodexAdapter::with_home(home.path());
        adapter.max_sessions = 3;
        assert_eq!(adapter.stored_sessions().len(), 3);
        assert_eq!(adapter.stored.lock().len(), 3);
    }
}
