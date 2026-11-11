//! GitHub Copilot cloud agent adapter (Agent tasks REST API, public preview).
//!
//! Endpoint paths and shapes were verified against GitHub's published
//! OpenAPI description (`github/rest-api-description`,
//! `descriptions/api.github.com/api.github.com.yaml`, operations
//! `agent-tasks/*`; docs: <https://docs.github.com/rest/agent-tasks/agent-tasks>):
//!
//! - `POST /agents/repos/{owner}/{repo}/tasks` — start a task
//!   (`prompt`, optional `model`, `custom_agent`, `create_pull_request`,
//!   `base_ref`, `head_ref`); 201 with the task.
//! - `GET /agents/tasks` — list the user's tasks (`tasks` array).
//! - `GET /agents/tasks/{task_id}` — one task including `sessions`.
//!
//! The API has no follow-up message or cancel operation, so those
//! capabilities are off. Progress is derived from the task's sessions and
//! state. The endpoints are in public preview and "subject to change";
//! all paths are kept in the constants below.
//!
//! Authentication: personal access token (classic or fine-grained with
//! "Agent tasks" repository permission) or OAuth / GitHub App user token.
//! GitHub App installation tokens are not supported by GitHub.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use vca_core::{CloudProvider, MessageRole, SessionState};

use crate::adapter::CloudAgentAdapter;
use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::http::{self, join};
use crate::repo::GithubRepo;
use crate::types::{
    Activity, ActivityPage, CloudCapabilities, CloudLaunch, CloudSession, CloudSessionId, Cursor,
    parse_time,
};

pub const DEFAULT_BASE_URL: &str = "https://api.github.com";
/// REST API version header value.
pub const API_VERSION: &str = "2022-11-28";

mod paths {
    pub fn create(owner: &str, repo: &str) -> String {
        format!("/agents/repos/{owner}/{repo}/tasks")
    }
    pub const LIST: &str = "/agents/tasks";
    pub fn get(task_id: &str) -> String {
        format!("/agents/tasks/{task_id}")
    }
}

/// Copilot cloud agent adapter.
#[derive(Debug, Clone)]
pub struct CopilotAdapter {
    client: reqwest::Client,
    base_url: String,
}

impl Default for CopilotAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotAdapter {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            client: http::default_client(),
            base_url: base_url.into(),
        }
    }

    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        cred: &Credential,
    ) -> Result<reqwest::RequestBuilder> {
        let token = cred.require()?;
        Ok(self
            .client
            .request(method, join(&self.base_url, path))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION))
    }

    async fn get_task(&self, cred: &Credential, id: &CloudSessionId) -> Result<GTask> {
        let r = self.req(reqwest::Method::GET, &paths::get(id.as_str()), cred)?;
        http::send_json(r).await
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GTask {
    id: Option<String>,
    url: Option<String>,
    html_url: Option<String>,
    name: Option<String>,
    state: Option<String>,
    artifacts: Vec<GArtifact>,
    created_at: Option<String>,
    updated_at: Option<String>,
    sessions: Vec<GSession>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GArtifact {
    #[serde(rename = "type")]
    kind: Option<String>,
    data: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GSession {
    id: Option<String>,
    state: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    completed_at: Option<String>,
    prompt: Option<String>,
    head_ref: Option<String>,
    error: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GTaskList {
    tasks: Vec<GTask>,
}

/// Maps `queued`, `in_progress`, `completed`, `failed`, `idle`,
/// `waiting_for_user`, `timed_out`, `cancelled`.
fn map_state(s: &str) -> SessionState {
    match s {
        "queued" => SessionState::Starting,
        "in_progress" => SessionState::Running,
        "completed" => SessionState::Exited { code: Some(0) },
        "failed" => SessionState::Error {
            message: "Copilot task failed".into(),
        },
        "idle" => SessionState::Idle,
        "waiting_for_user" => SessionState::AwaitingInput,
        "timed_out" => SessionState::Error {
            message: "Copilot task timed out".into(),
        },
        "cancelled" => SessionState::Exited { code: None },
        _ => SessionState::Running,
    }
}

/// Extracts `owner/repo` from a task API URL
/// (`https://api.github.com/agents/repos/{owner}/{repo}/tasks/{id}`).
fn repo_from_task_url(url: &str) -> Option<String> {
    let rest = url.split("/agents/repos/").nth(1)?;
    let mut parts = rest.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(format!("{owner}/{repo}"))
}

impl GTask {
    fn to_cloud(&self) -> Result<CloudSession> {
        let id = self
            .id
            .clone()
            .ok_or_else(|| CloudError::Decode("Copilot task without id".into()))?;
        let mut s = CloudSession::new(CloudProvider::CopilotCloud, id);
        s.title = self.name.clone();
        if let Some(st) = &self.state {
            s.state = map_state(st);
        }
        s.provider_status = self.state.clone();
        s.url = self.html_url.clone();
        s.repository = self.url.as_deref().and_then(repo_from_task_url);
        s.created_at = self.created_at.as_deref().and_then(parse_time);
        s.updated_at = self.updated_at.as_deref().and_then(parse_time);
        // Pull request artifacts carry only the database ID (not the number),
        // so no pull request URL can be derived; the task page links to it.
        Ok(s)
    }

    fn activities(&self) -> Vec<Activity> {
        let mut out = Vec::new();
        for (i, sess) in self.sessions.iter().enumerate() {
            let sid = sess.id.clone().unwrap_or_else(|| format!("idx-{i}"));
            let created = sess.created_at.as_deref().and_then(parse_time).unwrap_or(0);
            if let Some(p) = &sess.prompt {
                out.push(Activity::message(
                    format!("{sid}:prompt"),
                    created,
                    MessageRole::User,
                    p.clone(),
                ));
            }
            if let Some(head) = &sess.head_ref {
                out.push(Activity::notification(
                    format!("{sid}:branch"),
                    created,
                    "Working branch",
                    head.clone(),
                ));
            }
            if let Some(st) = &sess.state {
                let at = sess
                    .completed_at
                    .as_deref()
                    .or(sess.updated_at.as_deref())
                    .and_then(parse_time)
                    .unwrap_or(created);
                out.push(Activity::state(
                    format!("{sid}:state:{st}"),
                    at,
                    map_state(st),
                ));
            }
            if let Some(msg) = sess
                .error
                .as_ref()
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
            {
                out.push(Activity::notification(
                    format!("{sid}:error"),
                    created,
                    "Copilot error",
                    msg,
                ));
            }
        }
        for a in &self.artifacts {
            if a.kind.as_deref() == Some("pull")
                && let Some(id) = a.data.get("id").and_then(Value::as_i64)
            {
                out.push(Activity::notification(
                    format!("artifact:pull:{id}"),
                    self.updated_at.as_deref().and_then(parse_time).unwrap_or(0),
                    "Pull request ready",
                    self.html_url.clone().unwrap_or_default(),
                ));
            }
        }
        out
    }
}

#[async_trait]
impl CloudAgentAdapter for CopilotAdapter {
    fn provider(&self) -> CloudProvider {
        CloudProvider::CopilotCloud
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
            direct_mode: true,
            unverified: false,
        }
    }

    async fn list_sessions(&self, cred: &Credential) -> Result<Vec<CloudSession>> {
        let r = self.req(reqwest::Method::GET, paths::LIST, cred)?.query(&[
            ("per_page", "50"),
            ("sort", "updated_at"),
            ("direction", "desc"),
        ]);
        let list: GTaskList = http::send_json(r).await?;
        list.tasks.iter().map(GTask::to_cloud).collect()
    }

    async fn get_session(&self, cred: &Credential, id: &CloudSessionId) -> Result<CloudSession> {
        self.get_task(cred, id).await?.to_cloud()
    }

    async fn create_session(&self, cred: &Credential, req: &CloudLaunch) -> Result<CloudSession> {
        if req.prompt.trim().is_empty() {
            return Err(CloudError::InvalidRequest("prompt is empty".into()));
        }
        let repo = req
            .repository
            .as_deref()
            .and_then(GithubRepo::parse)
            .ok_or_else(|| {
                CloudError::InvalidRequest("Copilot tasks need an owner/repo repository".into())
            })?;
        let mut body = json!({
            "prompt": req.prompt,
            "create_pull_request": req.auto_create_pr,
        });
        if let Some(b) = &req.branch {
            body["base_ref"] = json!(b);
        }
        if let Some(m) = &req.model {
            body["model"] = json!(m);
        }
        let r = self
            .req(
                reqwest::Method::POST,
                &paths::create(&repo.owner, &repo.name),
                cred,
            )?
            .json(&body);
        let task: GTask = http::send_json(r).await?;
        let mut s = task.to_cloud()?;
        s.repository.get_or_insert_with(|| repo.slug());
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
        cred: &Credential,
        id: &CloudSessionId,
        _since: Option<Cursor>,
    ) -> Result<ActivityPage> {
        let task = self.get_task(cred, id).await?;
        let state = task.state.as_deref().map(map_state);
        Ok(ActivityPage {
            activities: task.activities(),
            next: None,
            has_more: false,
            state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_from_url() {
        assert_eq!(
            repo_from_task_url("https://api.github.com/agents/repos/octocat/hello-world/tasks/a1")
                .as_deref(),
            Some("octocat/hello-world")
        );
        assert_eq!(repo_from_task_url("https://x/y"), None);
    }

    #[test]
    fn parses_documented_example() {
        // Example response of GET /agents/repos/{owner}/{repo}/tasks/{task_id}
        // from GitHub's OpenAPI description.
        let task: GTask = serde_json::from_value(json!({
            "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "url": "https://api.github.com/agents/repos/octocat/hello-world/tasks/a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "html_url": "https://github.com/octocat/hello-world/copilot/tasks/a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "name": "Fix the login button on the homepage",
            "creator": {"id": 1}, "creator_type": "user", "owner": {"id": 1},
            "repository": {"id": 1296269}, "state": "completed", "session_count": 1,
            "artifacts": [{"provider": "github", "type": "pull", "data": {"id": 42}}],
            "archived_at": null,
            "created_at": "2025-01-01T00:00:00Z", "updated_at": "2025-01-01T01:00:00Z",
            "sessions": [{
                "id": "s1a2b3c4-d5e6-7890-abcd-ef1234567890",
                "name": "Fix the login button on the homepage",
                "user": {"id": 1}, "owner": {"id": 1}, "repository": {"id": 1296269},
                "task_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890", "state": "completed",
                "created_at": "2025-01-01T00:00:00Z", "updated_at": "2025-01-01T01:00:00Z",
                "completed_at": "2025-01-01T01:00:00Z",
                "prompt": "Fix the login button on the homepage",
                "head_ref": "copilot/fix-1", "base_ref": "main", "model": "claude-sonnet-4.6"
            }]
        }))
        .unwrap();
        let s = task.to_cloud().unwrap();
        assert_eq!(s.state, SessionState::Exited { code: Some(0) });
        assert_eq!(s.repository.as_deref(), Some("octocat/hello-world"));
        let acts = task.activities();
        assert_eq!(acts.len(), 4);
        assert_eq!(acts[0].id, "s1a2b3c4-d5e6-7890-abcd-ef1234567890:prompt");
    }
}
