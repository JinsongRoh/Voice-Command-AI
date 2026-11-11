//! Google Jules adapter (Jules REST API, `v1alpha`).
//!
//! Endpoints (documented at <https://developers.google.com/jules/api>):
//! `POST /sessions`, `GET /sessions`, `GET /sessions/{id}`,
//! `POST /sessions/{id}:sendMessage`, `POST /sessions/{id}:approvePlan`,
//! `GET /sessions/{id}/activities`. Authentication uses the
//! `X-Goog-Api-Key` header. The API is in alpha; every response field is
//! parsed as optional.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use vca_core::{CloudProvider, MessageRole, SessionState, now_millis};

use crate::adapter::CloudAgentAdapter;
use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::http::{self, join};
use crate::repo::GithubRepo;
use crate::types::{
    Activity, ActivityPage, CloudCapabilities, CloudLaunch, CloudSession, CloudSessionId, Cursor,
    parse_time,
};

pub const DEFAULT_BASE_URL: &str = "https://jules.googleapis.com/v1alpha";
const API_KEY_HEADER: &str = "X-Goog-Api-Key";
const MAX_LIST_PAGES: usize = 5;

/// Jules adapter.
#[derive(Debug, Clone)]
pub struct JulesAdapter {
    client: reqwest::Client,
    base_url: String,
}

impl Default for JulesAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl JulesAdapter {
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
        let key = cred.require()?;
        Ok(self
            .client
            .request(method, join(&self.base_url, path))
            .header(API_KEY_HEADER, key))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JSession {
    name: Option<String>,
    id: Option<String>,
    title: Option<String>,
    prompt: Option<String>,
    state: Option<String>,
    url: Option<String>,
    create_time: Option<String>,
    update_time: Option<String>,
    source_context: Option<Value>,
    outputs: Vec<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JSessionList {
    sessions: Vec<JSession>,
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JActivity {
    name: Option<String>,
    id: Option<String>,
    description: Option<String>,
    create_time: Option<String>,
    originator: Option<String>,
    agent_messaged: Option<Value>,
    user_messaged: Option<Value>,
    plan_generated: Option<Value>,
    plan_approved: Option<Value>,
    progress_updated: Option<Value>,
    session_completed: Option<Value>,
    session_failed: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JActivityList {
    activities: Vec<JActivity>,
    next_page_token: Option<String>,
}

/// Maps Jules session states (`QUEUED`, `PLANNING`, `AWAITING_PLAN_APPROVAL`,
/// `AWAITING_USER_FEEDBACK`, `IN_PROGRESS`, `PAUSED`, `FAILED`, `COMPLETED`).
fn map_state(s: &str) -> SessionState {
    match s {
        "QUEUED" => SessionState::Starting,
        "PLANNING" | "IN_PROGRESS" => SessionState::Running,
        "AWAITING_PLAN_APPROVAL" => SessionState::AwaitingDecision,
        "AWAITING_USER_FEEDBACK" => SessionState::AwaitingInput,
        "PAUSED" => SessionState::Paused,
        "FAILED" => SessionState::Error {
            message: "Jules session failed".into(),
        },
        "COMPLETED" => SessionState::Exited { code: Some(0) },
        _ => SessionState::Running,
    }
}

impl JSession {
    fn into_cloud(self) -> Result<CloudSession> {
        let id = self
            .id
            .or_else(|| {
                self.name
                    .as_deref()
                    .and_then(|n| n.rsplit('/').next())
                    .map(str::to_owned)
            })
            .ok_or_else(|| CloudError::Decode("Jules session without id".into()))?;
        let mut s = CloudSession::new(CloudProvider::Jules, id);
        s.title = self.title.or(self.prompt);
        if let Some(st) = &self.state {
            s.state = map_state(st);
        }
        s.provider_status = self.state;
        s.url = self.url;
        s.created_at = self.create_time.as_deref().and_then(parse_time);
        s.updated_at = self.update_time.as_deref().and_then(parse_time);
        s.repository = self
            .source_context
            .as_ref()
            .and_then(|c| c.get("source"))
            .and_then(Value::as_str)
            .map(|src| {
                src.strip_prefix("sources/github/")
                    .unwrap_or(src)
                    .to_owned()
            });
        s.pull_request_url = self
            .outputs
            .iter()
            .find_map(|o| o.pointer("/pullRequest/url").and_then(Value::as_str))
            .map(str::to_owned);
        Ok(s)
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

impl JActivity {
    fn into_activity(self) -> Option<Activity> {
        let id = self.id.or(self.name)?;
        let at = self
            .create_time
            .as_deref()
            .and_then(parse_time)
            .unwrap_or_else(now_millis);
        if let Some(m) = &self.agent_messaged {
            let text = str_at(m, "agentMessage").unwrap_or_default();
            return Some(Activity::message(id, at, MessageRole::Assistant, text));
        }
        if let Some(m) = &self.user_messaged {
            let text = str_at(m, "userMessage").unwrap_or_default();
            return Some(Activity::message(id, at, MessageRole::User, text));
        }
        if let Some(p) = &self.plan_generated {
            let steps: Vec<String> = p
                .pointer("/plan/steps")
                .and_then(Value::as_array)
                .map(|steps| {
                    steps
                        .iter()
                        .enumerate()
                        .map(|(i, st)| {
                            format!("{}. {}", i + 1, str_at(st, "title").unwrap_or("(step)"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            return Some(Activity::notification(
                id,
                at,
                "Plan ready",
                steps.join("\n"),
            ));
        }
        if self.plan_approved.is_some() {
            return Some(Activity::notification(id, at, "Plan approved", ""));
        }
        if let Some(p) = &self.progress_updated {
            let title = str_at(p, "title").unwrap_or("Progress");
            let body = str_at(p, "description").unwrap_or_default();
            return Some(Activity::message(
                id,
                at,
                MessageRole::System,
                join_lines(title, body),
            ));
        }
        if self.session_completed.is_some() {
            return Some(Activity::state(
                id,
                at,
                SessionState::Exited { code: Some(0) },
            ));
        }
        if let Some(f) = &self.session_failed {
            let reason = str_at(f, "reason").unwrap_or("Jules session failed");
            return Some(Activity::state(
                id,
                at,
                SessionState::Error {
                    message: reason.to_owned(),
                },
            ));
        }
        // Unknown activity kind: surface its description so nothing is lost.
        let text = self.description?;
        let role = if self.originator.as_deref() == Some("user") {
            MessageRole::User
        } else {
            MessageRole::System
        };
        Some(Activity::message(id, at, role, text))
    }
}

fn join_lines(a: &str, b: &str) -> String {
    if b.is_empty() {
        a.to_owned()
    } else {
        format!("{a}\n{b}")
    }
}

#[async_trait]
impl CloudAgentAdapter for JulesAdapter {
    fn provider(&self) -> CloudProvider {
        CloudProvider::Jules
    }

    fn capabilities(&self) -> CloudCapabilities {
        CloudCapabilities {
            create: true,
            list: true,
            follow_up: true,
            progress: true,
            progress_is_status_only: false,
            stop: false,
            approve_plan: true,
            direct_mode: true,
            unverified: false,
        }
    }

    async fn list_sessions(&self, cred: &Credential) -> Result<Vec<CloudSession>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut req = self
                .req(reqwest::Method::GET, "/sessions", cred)?
                .query(&[("pageSize", "50")]);
            if let Some(t) = &token {
                req = req.query(&[("pageToken", t)]);
            }
            let page: JSessionList = http::send_json(req).await?;
            for s in page.sessions {
                out.push(s.into_cloud()?);
            }
            token = page.next_page_token.filter(|t| !t.is_empty());
            if token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    async fn get_session(&self, cred: &Credential, id: &CloudSessionId) -> Result<CloudSession> {
        let req = self.req(reqwest::Method::GET, &format!("/sessions/{id}"), cred)?;
        http::send_json::<JSession>(req).await?.into_cloud()
    }

    async fn create_session(&self, cred: &Credential, req: &CloudLaunch) -> Result<CloudSession> {
        if req.prompt.trim().is_empty() {
            return Err(CloudError::InvalidRequest("prompt is empty".into()));
        }
        let mut body = json!({ "prompt": req.prompt });
        if let Some(repo) = &req.repository {
            let repo = GithubRepo::parse(repo).ok_or_else(|| {
                CloudError::InvalidRequest(format!("not a GitHub repository: {repo}"))
            })?;
            let mut ctx =
                json!({ "source": format!("sources/github/{}/{}", repo.owner, repo.name) });
            if let Some(branch) = &req.branch {
                ctx["githubRepoContext"] = json!({ "startingBranch": branch });
            }
            body["sourceContext"] = ctx;
        }
        if let Some(t) = &req.title {
            body["title"] = json!(t);
        }
        if req.require_plan_approval {
            body["requirePlanApproval"] = json!(true);
        }
        if req.auto_create_pr {
            body["automationMode"] = json!("AUTO_CREATE_PR");
        }
        let r = self
            .req(reqwest::Method::POST, "/sessions", cred)?
            .json(&body);
        http::send_json::<JSession>(r).await?.into_cloud()
    }

    async fn send_message(&self, cred: &Credential, id: &CloudSessionId, text: &str) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/sessions/{id}:sendMessage"),
                cred,
            )?
            .json(&json!({ "prompt": text }));
        http::send_empty(r).await
    }

    async fn approve_plan(&self, cred: &Credential, id: &CloudSessionId) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/sessions/{id}:approvePlan"),
                cred,
            )?
            .json(&json!({}));
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
                &format!("/sessions/{id}/activities"),
                cred,
            )?
            .query(&[("pageSize", "100")]);
        if let Some(c) = &since {
            r = r.query(&[("pageToken", &c.0)]);
        }
        let page: JActivityList = http::send_json(r).await?;
        let activities = page
            .activities
            .into_iter()
            .filter_map(JActivity::into_activity)
            .collect();
        let next_token = page.next_page_token.filter(|t| !t.is_empty());
        let has_more = next_token.is_some();
        let state = if has_more {
            None
        } else {
            Some(self.get_session(cred, id).await?.state)
        };
        Ok(ActivityPage {
            activities,
            // At the end of the feed, resume from the page just read.
            next: next_token.map(Cursor).or(since),
            has_more,
            state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_activity_kinds() {
        let a: JActivity = serde_json::from_value(json!({
            "name": "sessions/1/activities/a1", "id": "a1",
            "createTime": "2025-01-01T00:00:00Z", "originator": "agent",
            "planGenerated": {"plan": {"id": "p", "steps": [{"title": "Read"}, {"title": "Fix"}]}},
            "unknownField": 1
        }))
        .unwrap();
        let act = a.into_activity().unwrap();
        assert_eq!(act.id, "a1");
        assert_eq!(
            act.kind,
            crate::types::ActivityKind::Notification {
                title: "Plan ready".into(),
                body: "1. Read\n2. Fix".into()
            }
        );
        let failed: JActivity =
            serde_json::from_value(json!({"id": "a2", "sessionFailed": {"reason": "boom"}}))
                .unwrap();
        assert!(matches!(
            failed.into_activity().unwrap().kind,
            crate::types::ActivityKind::StateChanged {
                state: SessionState::Error { .. }
            }
        ));
    }

    #[test]
    fn session_from_name_only() {
        let s: JSession = serde_json::from_value(json!({
            "name": "sessions/42", "state": "AWAITING_PLAN_APPROVAL",
            "sourceContext": {"source": "sources/github/octo/hello"},
            "outputs": [{"pullRequest": {"url": "https://github.com/octo/hello/pull/1"}}]
        }))
        .unwrap();
        let c = s.into_cloud().unwrap();
        assert_eq!(c.id.0, "42");
        assert_eq!(c.state, SessionState::AwaitingDecision);
        assert_eq!(c.repository.as_deref(), Some("octo/hello"));
        assert!(c.pull_request_url.is_some());
    }
}
