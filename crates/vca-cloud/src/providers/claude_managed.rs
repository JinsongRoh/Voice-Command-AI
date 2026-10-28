//! Claude Managed Agents adapter (Claude API `/v1/sessions`, beta).
//!
//! Endpoints (documented at <https://platform.claude.com/docs/en/api/beta/sessions>):
//! `POST /v1/sessions` (with `initial_events`), `GET /v1/sessions`,
//! `GET /v1/sessions/{id}`, `POST /v1/sessions/{id}/events`,
//! `GET /v1/sessions/{id}/events` (`page` / `next_page` cursor).
//! Headers: `x-api-key`, `anthropic-version`, `anthropic-beta:
//! managed-agents-2026-04-01`.
//!
//! Sessions reference an agent and an environment that the user created
//! beforehand (for example with `ant apply`); their IDs are adapter
//! settings. Usage is billed to the Claude API key, not to a claude.ai
//! subscription. "Stop" sends a `user.interrupt` event, which ends the
//! current turn and leaves the session idle.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use vca_core::{CloudProvider, MessageRole, SessionState, now_millis};

use crate::adapter::CloudAgentAdapter;
use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::http::{self, join};
use crate::types::{
    Activity, ActivityPage, CloudCapabilities, CloudLaunch, CloudSession, CloudSessionId, Cursor,
    parse_time,
};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
pub const BETA: &str = "managed-agents-2026-04-01";

/// Claude Managed Agents adapter.
#[derive(Debug, Clone)]
pub struct ClaudeManagedAdapter {
    client: reqwest::Client,
    base_url: String,
    agent_id: String,
    agent_version: Option<u64>,
    environment_id: String,
    workspace_id: Option<String>,
}

impl ClaudeManagedAdapter {
    pub fn new(agent_id: impl Into<String>, environment_id: impl Into<String>) -> Self {
        Self {
            client: http::default_client(),
            base_url: DEFAULT_BASE_URL.into(),
            agent_id: agent_id.into(),
            agent_version: None,
            environment_id: environment_id.into(),
            workspace_id: None,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Pins sessions to one agent version.
    pub fn with_agent_version(mut self, version: u64) -> Self {
        self.agent_version = Some(version);
        self
    }

    /// Console workspace ID, used to build "open in Console" links.
    pub fn with_workspace(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        cred: &Credential,
    ) -> Result<reqwest::RequestBuilder> {
        let key = cred.require()?;
        Ok(self
            .client
            .request(method, join(&self.base_url, path))
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", BETA))
    }

    fn console_url(&self, id: &str) -> Option<String> {
        self.workspace_id
            .as_ref()
            .map(|ws| format!("https://platform.claude.com/workspaces/{ws}/sessions/{id}"))
    }

    fn to_cloud(&self, s: MSession) -> Result<CloudSession> {
        let id =
            s.id.ok_or_else(|| CloudError::Decode("session without id".into()))?;
        let mut c = CloudSession::new(CloudProvider::ClaudeManagedAgents, id.clone());
        c.title = s.title;
        if let Some(st) = &s.status {
            c.state = map_status(st);
        }
        c.provider_status = s.status;
        c.created_at = s.created_at.as_deref().and_then(parse_time);
        c.updated_at = s.updated_at.as_deref().and_then(parse_time);
        c.url = self.console_url(&id);
        c.repository = s.resources.iter().find_map(|r| {
            (r.get("type").and_then(Value::as_str) == Some("github_repository"))
                .then(|| r.get("url").and_then(Value::as_str).map(str::to_owned))
                .flatten()
        });
        Ok(c)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MSession {
    id: Option<String>,
    title: Option<String>,
    status: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    resources: Vec<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MPage<T> {
    data: Vec<T>,
    next_page: Option<String>,
}

/// Maps `idle`, `running`, `rescheduling`, `terminated`.
fn map_status(s: &str) -> SessionState {
    match s {
        "idle" => SessionState::Idle,
        "running" => SessionState::Running,
        "rescheduling" => SessionState::Starting,
        "terminated" => SessionState::Exited { code: None },
        _ => SessionState::Running,
    }
}

fn text_of(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Converts one session event; returns `None` for kinds VCA does not show
/// (spans, usage snapshots, thinking markers, ...).
fn event_to_activity(e: &Value) -> Option<Activity> {
    let id = e.get("id")?.as_str()?.to_owned();
    let kind = e.get("type")?.as_str()?;
    let at = e
        .get("processed_at")
        .and_then(Value::as_str)
        .and_then(parse_time)
        .unwrap_or_else(now_millis);
    let a = match kind {
        "agent.message" => {
            Activity::message(id, at, MessageRole::Assistant, text_of(e.get("content")))
        }
        "user.message" => Activity::message(id, at, MessageRole::User, text_of(e.get("content"))),
        "agent.tool_use" | "agent.mcp_tool_use" | "agent.custom_tool_use" => {
            let name = e.get("name").and_then(Value::as_str).unwrap_or("tool");
            let summary = e
                .get("input")
                .map(|i| crate::http::truncate(&i.to_string(), 200))
                .unwrap_or_default();
            Activity::tool_call(id, at, name, summary)
        }
        "session.status_running" => Activity::state(id, at, SessionState::Running),
        "session.status_rescheduled" => Activity::state(id, at, SessionState::Starting),
        "session.status_idle" => {
            let reason = e.pointer("/stop_reason/type").and_then(Value::as_str);
            let state = match reason {
                Some("requires_action") => SessionState::AwaitingDecision,
                Some("retries_exhausted") => SessionState::Error {
                    message: "retries exhausted".into(),
                },
                _ => SessionState::Idle,
            };
            Activity::state(id, at, state)
        }
        "session.status_terminated" => Activity::state(id, at, SessionState::Exited { code: None }),
        "session.error" => {
            let msg = e
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("session error");
            Activity::notification(id, at, "Session error", msg)
        }
        _ => return None,
    };
    Some(a)
}

fn user_message(text: &str) -> Value {
    json!({ "type": "user.message", "content": [{ "type": "text", "text": text }] })
}

#[async_trait]
impl CloudAgentAdapter for ClaudeManagedAdapter {
    fn provider(&self) -> CloudProvider {
        CloudProvider::ClaudeManagedAgents
    }

    fn capabilities(&self) -> CloudCapabilities {
        CloudCapabilities {
            create: true,
            list: true,
            follow_up: true,
            progress: true,
            progress_is_status_only: false,
            stop: true,
            approve_plan: false,
            direct_mode: true,
            unverified: false,
        }
    }

    async fn list_sessions(&self, cred: &Credential) -> Result<Vec<CloudSession>> {
        let r = self
            .req(reqwest::Method::GET, "/v1/sessions", cred)?
            .query(&[("limit", "50")]);
        let page: MPage<MSession> = http::send_json(r).await?;
        page.data.into_iter().map(|s| self.to_cloud(s)).collect()
    }

    async fn get_session(&self, cred: &Credential, id: &CloudSessionId) -> Result<CloudSession> {
        let r = self.req(reqwest::Method::GET, &format!("/v1/sessions/{id}"), cred)?;
        self.to_cloud(http::send_json(r).await?)
    }

    async fn create_session(&self, cred: &Credential, req: &CloudLaunch) -> Result<CloudSession> {
        if req.prompt.trim().is_empty() {
            return Err(CloudError::InvalidRequest("prompt is empty".into()));
        }
        let agent = match self.agent_version {
            Some(v) => json!({ "type": "agent", "id": self.agent_id, "version": v }),
            None => json!(self.agent_id),
        };
        let mut body = json!({
            "agent": agent,
            "environment_id": self.environment_id,
            "initial_events": [user_message(&req.prompt)],
        });
        if let Some(t) = &req.title {
            body["title"] = json!(t);
        }
        // Repositories are mounted through `resources`, which needs a GitHub
        // token; VCA leaves repository access to the agent configuration.
        let r = self
            .req(reqwest::Method::POST, "/v1/sessions", cred)?
            .json(&body);
        self.to_cloud(http::send_json(r).await?)
    }

    async fn send_message(&self, cred: &Credential, id: &CloudSessionId, text: &str) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/v1/sessions/{id}/events"),
                cred,
            )?
            .json(&json!({ "events": [user_message(text)] }));
        http::send_empty(r).await
    }

    async fn stop(&self, cred: &Credential, id: &CloudSessionId) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/v1/sessions/{id}/events"),
                cred,
            )?
            .json(&json!({ "events": [{ "type": "user.interrupt" }] }));
        http::send_empty(r).await
    }

    async fn activity(
        &self,
        cred: &Credential,
        id: &CloudSessionId,
        since: Option<Cursor>,
    ) -> Result<ActivityPage> {
        let mut r = self
            .req(
                reqwest::Method::GET,
                &format!("/v1/sessions/{id}/events"),
                cred,
            )?
            .query(&[("limit", "100"), ("order", "asc")]);
        if let Some(c) = &since {
            r = r.query(&[("page", &c.0)]);
        }
        let page: MPage<Value> = http::send_json(r).await?;
        let activities = page.data.iter().filter_map(event_to_activity).collect();
        let next = page.next_page.filter(|p| !p.is_empty());
        let has_more = next.is_some();
        Ok(ActivityPage {
            activities,
            next: next.map(Cursor).or(since),
            has_more,
            // Status events are part of the feed; no extra request needed.
            state: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_documented_events() {
        let msg = json!({"type":"agent.message","id":"sevt_1","content":[{"type":"text","text":"I'll review..."}],"processed_at":"2026-01-01T00:00:00Z"});
        let a = event_to_activity(&msg).unwrap();
        assert_eq!(a.id, "sevt_1");
        assert!(
            matches!(a.kind, crate::types::ActivityKind::Message { role: MessageRole::Assistant, ref text } if text == "I'll review...")
        );
        let idle = json!({"type":"session.status_idle","id":"sevt_2","stop_reason":{"type":"requires_action","event_ids":[]},"processed_at":"2026-01-01T00:00:00Z"});
        assert!(matches!(
            event_to_activity(&idle).unwrap().kind,
            crate::types::ActivityKind::StateChanged {
                state: SessionState::AwaitingDecision
            }
        ));
        let span = json!({"type":"span.model_request_start","id":"sevt_3"});
        assert!(event_to_activity(&span).is_none());
    }
}
