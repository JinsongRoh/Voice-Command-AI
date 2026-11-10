//! Claude Code adapter.
//!
//! * Transcripts: `<config dir>/projects/<encoded project>/<session>.jsonl`,
//!   where the config dir is `CLAUDE_CONFIG_DIR` or `~/.claude`.
//! * Hooks: `settings.json` (see [`settings`]); payloads in [`hook_event`].
//! * Profiles: one `CLAUDE_CONFIG_DIR` per profile.

pub mod hook_event;
pub mod question;
pub mod settings;
pub mod transcript;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use vca_core::{AgentKind, LaunchRequest};

use crate::adapter::{AgentAdapter, SessionLocator, basic_launch_spec};
use crate::error::{AdapterError, Result};
use crate::meta_cache::{self, SharedCache};
use crate::process::{ProcInfo, ProcessMatcher, ProcessTable};
use crate::tail::{self, HistoryStream, JsonlTail};
use crate::types::{
    Capabilities, DiscoveredSession, HistoryItem, HookOptions, HookScope, HookStatus, LaunchSpec,
    Profile, system_time_millis,
};

pub use hook_event::{
    ClaudeHookEventName, ClaudeHookPayload, is_derived_decision_id, is_notice_decision_id,
    permission_mode_auto_approves, render_response,
};
pub use question::{
    AnswerError, KeyStep, answer_keys, form_on_screen, form_still_open, question_prompt,
    text_on_screen,
};
pub use settings::{ClaudeHookSpec, default_hooks};
pub use transcript::{ASK_USER_QUESTION, ToolPermission, parse_questions, tool_permission};

/// Environment variable that relocates Claude Code's configuration.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Claude Code adapter for one configuration directory.
#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    config_dir: PathBuf,
    hooks: Vec<ClaudeHookSpec>,
    poll_interval: Duration,
    /// What discovery read from each stored transcript (shared by clones).
    stored: SharedCache<StoredMeta>,
}

/// What discovery needs from one stored transcript.
#[derive(Debug, Clone, Default)]
struct StoredMeta {
    session_id: Option<String>,
    project_dir: PathBuf,
    last_timestamp: Option<u64>,
    title: Option<String>,
}

impl StoredMeta {
    /// Reads a transcript's metadata. The project folder is decoded (a walk
    /// of the file system) only when the transcript records no directory,
    /// and only when the file changed.
    fn read(path: &Path, folder: &str) -> Self {
        let meta = transcript::read_meta(path).unwrap_or_default();
        Self {
            title: meta.title(),
            session_id: meta.session_id,
            project_dir: meta
                .cwd
                .unwrap_or_else(|| transcript::decode_project_dir(folder)),
            last_timestamp: meta.last_timestamp,
        }
    }
}

/// Running processes whose stored sessions are checked with an exact
/// `stat` (see [`meta_cache::HOT_WINDOW`]): their project folders and the
/// session ids on their command lines.
#[derive(Debug, Default)]
struct Hot {
    folders: HashSet<String>,
    ids: HashSet<String>,
}

impl Hot {
    fn of(procs: &[&ProcInfo]) -> Self {
        let mut hot = Self::default();
        for p in procs {
            if let Some(cwd) = &p.cwd {
                let normalized: PathBuf = cwd.components().collect();
                hot.folders
                    .insert(transcript::encode_project_dir(&normalized));
            }
            if let Some(id) = session_id_from_cmd(&p.cmd) {
                hot.ids.insert(id);
            }
        }
        hot
    }
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
            stored: SharedCache::default(),
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

    /// The conversation `claude --continue` picks up in `cwd`: the most
    /// recently written transcript of the folder (subagent files aside).
    pub fn latest_transcript(&self, cwd: &Path) -> Option<SessionLocator> {
        let normalized: PathBuf = cwd.components().collect();
        let folder = self
            .projects_dir()
            .join(transcript::encode_project_dir(&normalized));
        let path = std::fs::read_dir(&folder)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .filter(|p| {
                !p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("agent-"))
            })
            .filter_map(|p| {
                let meta = std::fs::metadata(&p).ok()?;
                (meta.is_file() && meta.len() > 0).then_some((meta.modified().ok()?, p))
            })
            .max_by_key(|(modified, _)| *modified)
            .map(|(_, p)| p)?;
        Some(SessionLocator {
            agent_session_id: path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
            transcript_path: Some(path),
            project_dir: Some(cwd.to_owned()),
        })
    }

    /// Whether `args` already choose the conversation (continue, resume,
    /// a fixed id) or run without one (print mode).
    pub fn args_choose_conversation(args: &[String]) -> bool {
        const FLAGS: [&str; 8] = [
            "-c",
            "--continue",
            "-r",
            "--resume",
            "--session-id",
            "-p",
            "--print",
            "--fork-session",
        ];
        args.iter().any(|a| {
            FLAGS
                .iter()
                .any(|f| a == f || a.starts_with(&format!("{f}=")))
        })
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

    /// Lists stored sessions (blocking). A transcript is read again only
    /// when its size or modification time changed since the last pass;
    /// entries of deleted transcripts are dropped.
    fn stored_sessions(&self, hot: &Hot) -> Vec<DiscoveredSession> {
        let mut cache = self.stored.lock();
        cache.begin();
        let out = self.list_stored(&mut cache, hot);
        cache.finish();
        out
    }

    fn list_stored(
        &self,
        cache: &mut meta_cache::MetaCache<StoredMeta>,
        hot: &Hot,
    ) -> Vec<DiscoveredSession> {
        let Ok(projects) = std::fs::read_dir(self.projects_dir()) else {
            return Vec::new();
        };
        let now = SystemTime::now();
        let mut out = Vec::new();
        for project in projects.filter_map(|e| e.ok()) {
            let project_path = project.path();
            if !project_path.is_dir() {
                continue;
            }
            let folder = project.file_name().to_string_lossy().into_owned();
            let Ok(files) = std::fs::read_dir(&project_path) else {
                continue;
            };
            let hot_folder = hot.folders.contains(&folder);
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
                let Some(listed) = meta_cache::stamp(&file, false) else {
                    continue;
                };
                let exact = hot_folder || hot.ids.contains(stem) || listed.is_recent(now);
                let stamp = if exact {
                    meta_cache::stamp(&file, true).unwrap_or(listed)
                } else {
                    listed
                };
                let meta = cache.get(&path, stamp, || StoredMeta::read(&path, &folder));
                let mtime = stamp.modified().and_then(system_time_millis);
                out.push(DiscoveredSession {
                    agent: AgentKind::ClaudeCode,
                    agent_session_id: Some(meta.session_id.unwrap_or_else(|| stem.to_owned())),
                    project_dir: Some(meta.project_dir),
                    pid: None,
                    last_activity: mtime.max(meta.last_timestamp),
                    title: meta.title,
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

/// Whether a stored session belongs to a process started in `cwd`.
///
/// The directory recorded inside a transcript is where the session last was
/// (`cd` into a subfolder, a Git Bash style `/e/app` path), not where the
/// process started, so it often differs from the process's working directory.
/// The folder the transcript is stored in is named after the directory the
/// agent was started in, which makes it the reliable second check.
fn same_project(session: &DiscoveredSession, cwd: &Path) -> bool {
    if session.project_dir.as_deref() == Some(cwd) {
        return true;
    }
    let Some(folder) = session
        .transcript_path
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::file_name)
    else {
        return false;
    };
    // Components drop a trailing separator, which the folder name lacks.
    let normalized: PathBuf = cwd.components().collect();
    transcript::encode_project_dir(&normalized).as_str() == folder.to_string_lossy()
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
            .find(|s| s.pid.is_none() && p.cwd.as_deref().is_some_and(|cwd| same_project(s, cwd)));
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

    /// Claude Code stores a session as `<session id>.jsonl` in the projects
    /// folder named after the directory it was started in, created with
    /// the first message.
    fn new_transcript_since(
        &self,
        cwd: &Path,
        since: vca_core::UnixMillis,
        taken: &[String],
    ) -> Option<SessionLocator> {
        let normalized: PathBuf = cwd.components().collect();
        let folder = self
            .projects_dir()
            .join(transcript::encode_project_dir(&normalized));
        let files = std::fs::read_dir(&folder)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .filter(|p| {
                !p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|id| taken.iter().any(|t| t == id))
            });
        let path = crate::adapter::newest_file_since(files, since)?;
        Some(SessionLocator {
            agent_session_id: path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
            transcript_path: Some(path),
            project_dir: Some(cwd.to_owned()),
        })
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
        let matcher = self.process_matcher();
        let running = procs.find(&matcher);
        let hot = Hot::of(&running);
        let this = self.clone();
        let mut sessions = tokio::task::spawn_blocking(move || this.stored_sessions(&hot))
            .await
            .map_err(|e| AdapterError::Io(std::io::Error::other(e)))?;
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
        // A hook that runs a `vca` that is gone counts as missing.
        blocking(move || settings::status_on_disk(&path, &hooks, &opts)).await
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

    fn proc_in(pid: u32, cwd: &str) -> ProcInfo {
        ProcInfo {
            pid,
            parent: None,
            name: "claude".into(),
            cmd: vec!["claude".into()],
            exe: None,
            cwd: Some(PathBuf::from(cwd)),
            start_time: 1,
        }
    }

    fn stored(id: &str, folder: &str, recorded_cwd: &str) -> DiscoveredSession {
        DiscoveredSession {
            agent: AgentKind::ClaudeCode,
            agent_session_id: Some(id.into()),
            project_dir: Some(PathBuf::from(recorded_cwd)),
            pid: None,
            last_activity: Some(10),
            title: Some("t".into()),
            transcript_path: Some(PathBuf::from(format!(
                "/home/u/.claude/projects/{folder}/{id}.jsonl"
            ))),
        }
    }

    /// Time to read a whole transcript: `VCA_MEASURE_FILE=<path> cargo test
    /// -p vca-adapters measure_full_read -- --ignored --nocapture`; without
    /// the variable a synthetic transcript of about 20 MB is written.
    #[test]
    #[ignore = "measurement, not a check"]
    fn measure_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = match std::env::var_os("VCA_MEASURE_FILE") {
            Some(p) => PathBuf::from(p),
            None => {
                let path = dir.path().join("big.jsonl");
                let filler = "lorem ipsum dolor sit amet ".repeat(40);
                let mut out = String::new();
                for i in 0..20_000 {
                    let (kind, role) = if i % 2 == 0 {
                        ("user", "user")
                    } else {
                        ("assistant", "assistant")
                    };
                    out.push_str(&format!(
                        r#"{{"type":"{kind}","uuid":"u{i}","timestamp":"2026-10-31T12:00:00.000Z","cwd":"/w","message":{{"role":"{role}","content":[{{"type":"text","text":"{i} {filler}"}}]}}}}"#
                    ));
                    out.push('\n');
                }
                std::fs::write(&path, out).unwrap();
                path
            }
        };
        let bytes = std::fs::metadata(&path).unwrap().len();
        let started = std::time::Instant::now();
        let items = tail::read_all(&path, transcript::parse_line).unwrap();
        println!(
            "read_all: {} bytes -> {} items in {:?}",
            bytes,
            items.len(),
            started.elapsed()
        );
    }

    #[test]
    fn process_matches_session_that_moved_into_a_subfolder() {
        // The transcript's last recorded cwd is a subfolder, but the process
        // (and the folder Claude Code filed the transcript under) is the root.
        let mut sessions = vec![stored("s1", "-work-app", "/work/app/src/ui")];
        let p = proc_in(7, "/work/app/");
        attach_processes(AgentKind::ClaudeCode, &mut sessions, &[&p], |_| None);
        assert_eq!(sessions.len(), 1, "no extra transcript-less session");
        assert_eq!(sessions[0].pid, Some(7));
        assert!(sessions[0].transcript_path.is_some());
    }

    #[test]
    fn process_does_not_take_an_unrelated_project_session() {
        let mut sessions = vec![stored("s1", "-work-other", "/work/other")];
        let p = proc_in(7, "/work/app");
        attach_processes(AgentKind::ClaudeCode, &mut sessions, &[&p], |_| None);
        assert_eq!(sessions.len(), 2);
        let other = sessions.iter().find(|s| s.title.is_some()).unwrap();
        assert_eq!(other.pid, None);
    }

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
            fresh: false,
        };
        let spec = a
            .launch_spec(&req, Some(&Profile::new("work", "/p/work")))
            .unwrap();
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, ["--model", "opus"]);
        assert_eq!(spec.cwd, PathBuf::from("/src/app"));
        assert_eq!(spec.env.len(), 1);
    }

    #[test]
    fn continue_picks_the_folders_latest_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let a = ClaudeAdapter::with_config_dir(tmp.path());
        let cwd = PathBuf::from(if cfg!(windows) { r"E:\w\app" } else { "/w/app" });
        assert!(
            a.latest_transcript(&cwd).is_none(),
            "a new folder starts fresh"
        );
        let folder = transcript::encode_project_dir(&cwd);
        let old = write_transcript(tmp.path(), &folder, "old", "Old");
        std::thread::sleep(std::time::Duration::from_millis(30));
        let new = write_transcript(tmp.path(), &folder, "new", "New");
        // Subagent files and empty files are not conversations.
        std::fs::write(new.with_file_name("agent-x.jsonl"), "{}\n").unwrap();
        std::fs::write(new.with_file_name("empty.jsonl"), "").unwrap();
        let found = a.latest_transcript(&cwd).unwrap();
        assert_eq!(found.agent_session_id.as_deref(), Some("new"));
        assert_eq!(found.transcript_path.as_deref(), Some(new.as_path()));
        // Writing to the older one makes it the latest again.
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&old, "{}\n{}\n").unwrap();
        let found = a.latest_transcript(&cwd).unwrap();
        assert_eq!(found.agent_session_id.as_deref(), Some("old"));
    }

    #[test]
    fn explicit_conversation_flags_are_respected() {
        let args = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        assert!(!ClaudeAdapter::args_choose_conversation(&args(
            "--model opus"
        )));
        for given in [
            "-c",
            "--continue",
            "--resume abc",
            "-r",
            "--session-id=x",
            "-p hi",
            "--print",
        ] {
            assert!(
                ClaudeAdapter::args_choose_conversation(&args(given)),
                "{given}"
            );
        }
    }

    fn write_transcript(dir: &Path, folder: &str, id: &str, title: &str) -> PathBuf {
        let path = dir
            .join("projects")
            .join(folder)
            .join(format!("{id}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = format!(
            r#"{{"type":"custom-title","customTitle":"{title}","sessionId":"{id}","cwd":"/w/{folder}","timestamp":"2026-11-01T10:00:00.000Z"}}"#
        );
        std::fs::write(&path, format!("{line}\n")).unwrap();
        path
    }

    fn titles(found: &[DiscoveredSession]) -> Vec<String> {
        let mut t: Vec<String> = found.iter().filter_map(|s| s.title.clone()).collect();
        t.sort();
        t
    }

    #[test]
    fn stored_transcripts_are_read_again_only_when_changed() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let a = write_transcript(dir.path(), "app", "s-a", "first");
        let b = write_transcript(dir.path(), "app", "s-b", "other");
        let adapter = ClaudeAdapter::with_config_dir(dir.path());
        // A clone (as discovery's blocking task uses) shares the cache.
        let reads = || adapter.stored.lock().reads();
        let pass = || adapter.clone().stored_sessions(&Hot::default());

        assert_eq!(titles(&pass()), ["first", "other"]);
        assert_eq!(reads(), 2);
        // Nothing changed: nothing is read.
        assert_eq!(titles(&pass()), ["first", "other"]);
        assert_eq!(reads(), 2);

        // A changed transcript is read again, and only that one.
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"renamed","sessionId":"s-a"}}"#
        )
        .unwrap();
        drop(f);
        assert_eq!(titles(&pass()), ["other", "renamed"]);
        assert_eq!(reads(), 3);

        // A deleted transcript leaves the list and the cache.
        std::fs::remove_file(&b).unwrap();
        assert_eq!(titles(&pass()), ["renamed"]);
        assert_eq!(adapter.stored.lock().len(), 1);
        assert_eq!(reads(), 3);

        // A new one is found at once.
        write_transcript(dir.path(), "shop", "s-c", "new");
        assert_eq!(titles(&pass()), ["new", "renamed"]);
        assert_eq!(reads(), 4);
    }

    #[test]
    fn per_pass_reads_do_not_grow_with_stored_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300 {
            write_transcript(dir.path(), &format!("p{}", i % 7), &format!("s{i}"), "t");
        }
        let adapter = ClaudeAdapter::with_config_dir(dir.path());
        let hot = Hot::default();
        assert_eq!(adapter.stored_sessions(&hot).len(), 300);
        let after_first = adapter.stored.lock().reads();
        assert_eq!(after_first, 300);
        for _ in 0..5 {
            assert_eq!(adapter.stored_sessions(&hot).len(), 300);
        }
        // Later passes read nothing: the work per pass is a directory
        // listing, not 300 file reads.
        assert_eq!(adapter.stored.lock().reads(), after_first);
    }

    #[test]
    fn a_transcript_without_a_directory_falls_back_to_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("projects")
            .join("-nowhere-x")
            .join("s1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"type\":\"summary\",\"summary\":\"s\"}\n").unwrap();
        let adapter = ClaudeAdapter::with_config_dir(dir.path());
        let found = adapter.stored_sessions(&Hot::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent_session_id.as_deref(), Some("s1"));
        assert!(found[0].project_dir.is_some());
        // Cached with the decoded folder: the second pass reads nothing.
        adapter.stored_sessions(&Hot::default());
        assert_eq!(adapter.stored.lock().reads(), 1);
    }

    #[test]
    fn running_processes_mark_their_folders_and_ids_hot() {
        let mut p = proc_in(7, "/work/app/");
        p.cmd = vec!["claude".into(), "--resume".into(), "abc".into()];
        let hot = Hot::of(&[&p]);
        assert!(hot.folders.contains("-work-app"));
        assert!(hot.ids.contains("abc"));
    }
}
