//! OpenAI Codex cloud adapter, driven through the official `codex cloud` CLI.
//!
//! Codex cloud has no public REST API. The adapter runs the CLI as a
//! subprocess on the user's machine (so it is not available in direct
//! mode) and relies on the CLI's own sign-in; the [`Credential`] argument
//! is ignored. Commands used (verified against the `codex-rs/cloud-tasks`
//! sources of <https://github.com/openai/codex>):
//!
//! - `codex cloud exec --env ENV_ID [--branch B] QUERY` prints the task URL.
//! - `codex cloud list --json [--env ENV_ID] [--limit N] [--cursor C]`
//!   prints `{"tasks": [{"id","url","title","status","updated_at",...}], "cursor": ...}`.
//! - `codex cloud status TASK_ID` prints `[STATUS] title` and exits with 1
//!   while the task is not `READY`.
//! - `codex cloud diff TASK_ID` prints the unified diff.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use vca_core::{CloudProvider, SessionState, now_millis};

use crate::adapter::CloudAgentAdapter;
use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::http::truncate;
use crate::types::{
    Activity, ActivityPage, CloudCapabilities, CloudLaunch, CloudSession, CloudSessionId, Cursor,
    parse_time,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Codex cloud adapter.
#[derive(Debug, Clone)]
pub struct CodexCloudAdapter {
    program: PathBuf,
    environment: Option<String>,
    timeout: Duration,
}

impl Default for CodexCloudAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct XList {
    tasks: Vec<XTask>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct XTask {
    id: Option<String>,
    url: Option<String>,
    title: Option<String>,
    status: Option<String>,
    updated_at: Option<String>,
    environment_label: Option<String>,
    environment_id: Option<String>,
}

/// Maps `pending`, `ready`, `applied`, `error` (case-insensitive).
fn map_status(s: &str) -> SessionState {
    match s.to_ascii_lowercase().as_str() {
        "pending" => SessionState::Running,
        "ready" | "applied" => SessionState::Exited { code: Some(0) },
        "error" => SessionState::Error {
            message: "Codex cloud task failed".into(),
        },
        _ => SessionState::Running,
    }
}

/// Removes ANSI escape sequences (in case color is forced on).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Task ID from a task URL or a bare ID (mirrors the CLI's own parsing).
fn task_id_from(raw: &str) -> Option<String> {
    let t = raw.trim();
    let t = t.split('#').next()?.split('?').next()?;
    let id = t.trim_end_matches('/').rsplit('/').next()?.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// Parses the first line of `codex cloud status`: `[STATUS] title`.
fn parse_status_output(stdout: &str) -> Option<(String, String)> {
    let clean = strip_ansi(stdout);
    let line = clean
        .lines()
        .find(|l| l.trim_start().starts_with('['))?
        .trim();
    let end = line.find(']')?;
    let status = line[1..end].trim().to_owned();
    let title = line[end + 1..].trim().to_owned();
    Some((status, title))
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl CodexCloudAdapter {
    pub fn new() -> Self {
        Self {
            program: PathBuf::from("codex"),
            environment: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Path to the `codex` executable (default: `codex` on `PATH`; on
    /// Windows an npm install may need `codex.cmd`).
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Codex cloud environment used by `exec` (and as the `list` filter).
    pub fn with_environment(mut self, env_id: impl Into<String>) -> Self {
        self.environment = Some(env_id.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn run(&self, args: Vec<OsString>) -> Result<Output> {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.arg("cloud")
            .args(args)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CloudError::Unsupported("codex CLI not installed")
            } else {
                CloudError::Command(format!("could not start codex: {e}"))
            }
        })?;
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| CloudError::Command("codex timed out".into()))?
            .map_err(|e| CloudError::Command(e.to_string()))?;
        Ok(Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn failure(out: &Output) -> CloudError {
        let msg = strip_ansi(out.stderr.trim());
        let lower = msg.to_ascii_lowercase();
        if lower.contains("login")
            || lower.contains("sign in")
            || lower.contains("not logged")
            || lower.contains("unauthorized")
        {
            return CloudError::Auth(truncate(&msg, 300));
        }
        if lower.contains("not found") {
            return CloudError::NotFound(truncate(&msg, 300));
        }
        CloudError::Command(format!("exit {:?}: {}", out.code, truncate(&msg, 300)))
    }

    /// Returns the unified diff of a task (`codex cloud diff`).
    pub async fn diff(&self, id: &CloudSessionId) -> Result<String> {
        let out = self.run(vec!["diff".into(), id.as_str().into()]).await?;
        if out.code != Some(0) {
            return Err(Self::failure(&out));
        }
        Ok(out.stdout)
    }

    async fn status(&self, id: &CloudSessionId) -> Result<(String, String)> {
        let out = self.run(vec!["status".into(), id.as_str().into()]).await?;
        // Exit code 1 only means "not ready yet" when a status line was printed.
        if let Some(parsed) = parse_status_output(&out.stdout)
            && matches!(out.code, Some(0 | 1))
        {
            return Ok(parsed);
        }
        Err(Self::failure(&out))
    }
}

impl XTask {
    fn into_cloud(self) -> Option<CloudSession> {
        let id = self
            .id
            .or_else(|| self.url.as_deref().and_then(task_id_from))?;
        let mut s = CloudSession::new(CloudProvider::CodexCloud, id);
        s.title = self.title;
        if let Some(st) = &self.status {
            s.state = map_status(st);
        }
        s.provider_status = self.status;
        s.url = self.url;
        s.updated_at = self.updated_at.as_deref().and_then(parse_time);
        s.repository = self.environment_label.or(self.environment_id);
        Some(s)
    }
}

#[async_trait]
impl CloudAgentAdapter for CodexCloudAdapter {
    fn provider(&self) -> CloudProvider {
        CloudProvider::CodexCloud
    }

    fn capabilities(&self) -> CloudCapabilities {
        CloudCapabilities {
            create: true,
            list: true,
            follow_up: false,
            progress: true,
            progress_is_status_only: true,
            stop: false,
            approve_plan: false,
            direct_mode: false,
            unverified: false,
        }
    }

    async fn list_sessions(&self, _cred: &Credential) -> Result<Vec<CloudSession>> {
        let mut args: Vec<OsString> = vec![
            "list".into(),
            "--json".into(),
            "--limit".into(),
            "20".into(),
        ];
        if let Some(env) = &self.environment {
            args.push("--env".into());
            args.push(env.into());
        }
        let out = self.run(args).await?;
        if out.code != Some(0) {
            return Err(Self::failure(&out));
        }
        let list: XList = serde_json::from_str(out.stdout.trim())
            .map_err(|e| CloudError::Decode(e.to_string()))?;
        Ok(list
            .tasks
            .into_iter()
            .filter_map(XTask::into_cloud)
            .collect())
    }

    async fn get_session(&self, _cred: &Credential, id: &CloudSessionId) -> Result<CloudSession> {
        let (status, title) = self.status(id).await?;
        let mut s = CloudSession::new(CloudProvider::CodexCloud, id.clone());
        s.state = map_status(&status);
        s.provider_status = Some(status);
        s.title = Some(title).filter(|t| !t.is_empty());
        Ok(s)
    }

    async fn create_session(&self, _cred: &Credential, req: &CloudLaunch) -> Result<CloudSession> {
        if req.prompt.trim().is_empty() {
            return Err(CloudError::InvalidRequest("prompt is empty".into()));
        }
        let env = self.environment.as_ref().ok_or_else(|| {
            CloudError::InvalidRequest("no Codex cloud environment configured".into())
        })?;
        let mut args: Vec<OsString> = vec!["exec".into(), "--env".into(), env.into()];
        if let Some(b) = &req.branch {
            args.push("--branch".into());
            args.push(b.into());
        }
        // `--` keeps prompts that start with '-' from being read as flags.
        args.push("--".into());
        args.push(req.prompt.clone().into());
        let out = self.run(args).await?;
        if out.code != Some(0) {
            return Err(Self::failure(&out));
        }
        let url = strip_ansi(&out.stdout)
            .lines()
            .map(str::trim)
            .rfind(|l| l.starts_with("http"))
            .map(str::to_owned)
            .ok_or_else(|| CloudError::Decode("codex cloud exec printed no task URL".into()))?;
        let id =
            task_id_from(&url).ok_or_else(|| CloudError::Decode(format!("no task id in {url}")))?;
        let mut s = CloudSession::new(CloudProvider::CodexCloud, id);
        s.url = Some(url);
        s.title = req.title.clone();
        s.state = SessionState::Running;
        s.created_at = Some(now_millis());
        Ok(s)
    }

    async fn send_message(
        &self,
        _cred: &Credential,
        _id: &CloudSessionId,
        _text: &str,
    ) -> Result<()> {
        Err(CloudError::Unsupported("follow_up"))
    }

    async fn activity(
        &self,
        _cred: &Credential,
        id: &CloudSessionId,
        _since: Option<Cursor>,
    ) -> Result<ActivityPage> {
        let (status, _title) = self.status(id).await?;
        let state = map_status(&status);
        let at = now_millis();
        let mut activities = vec![Activity::state(
            format!("status:{status}"),
            at,
            state.clone(),
        )];
        if matches!(state, SessionState::Exited { .. }) {
            activities.push(Activity::notification(
                format!("ready:{status}"),
                at,
                "Codex task ready",
                "The diff is ready to review or apply.",
            ));
        }
        Ok(ActivityPage {
            activities,
            next: None,
            has_more: false,
            state: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_lines() {
        let out = "\u{1b}[32m[READY]\u{1b}[0m Fix the bug\n  env  •  2m ago\n  +3/-1 • 1 file\n";
        assert_eq!(
            parse_status_output(out),
            Some(("READY".into(), "Fix the bug".into()))
        );
        assert_eq!(parse_status_output("nothing"), None);
    }

    #[test]
    fn task_ids() {
        assert_eq!(
            task_id_from("https://chatgpt.com/codex/tasks/task_i_123456?foo=bar").as_deref(),
            Some("task_i_123456")
        );
        assert_eq!(task_id_from("task_i_abc").as_deref(), Some("task_i_abc"));
        assert_eq!(task_id_from("  "), None);
    }

    #[test]
    fn list_json_shape() {
        let json = r#"{"tasks":[{"id":"task_e_1","url":"https://chatgpt.com/codex/tasks/task_e_1","title":"T","status":"pending","updated_at":"2025-01-01T00:00:00Z","environment_id":"env","environment_label":"owner/repo","summary":{"files_changed":0,"lines_added":0,"lines_removed":0},"is_review":false,"attempt_total":1}],"cursor":null}"#;
        let list: XList = serde_json::from_str(json).unwrap();
        let s = list.tasks.into_iter().next().unwrap().into_cloud().unwrap();
        assert_eq!(s.id.0, "task_e_1");
        assert_eq!(s.state, SessionState::Running);
        assert_eq!(s.repository.as_deref(), Some("owner/repo"));
    }
}
