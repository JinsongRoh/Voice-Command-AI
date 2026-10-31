//! Devin adapter (Devin API v3, organization scope).
//!
//! Endpoints (documented at <https://docs.devin.ai/api-reference/v3/>), all
//! under `/v3/organizations/{org_id}`: `POST /sessions`, `GET /sessions`,
//! `GET /sessions/{devin_id}`, `POST /sessions/{devin_id}/messages`,
//! `GET /sessions/{devin_id}/messages` (cursor paginated) and
//! `DELETE /sessions/{devin_id}` (terminate). Authentication uses a service
//! user key (`cog_...`) as a bearer token.
//!
//! The paths are confirmed from the public API reference index; the exact
//! response field names could not be read here, so the parser accepts the
//! known alternatives (`session_id` / `devin_id` / `id`, `items` /
//! `sessions` / `data`, and so on). Field names marked "unverified" below
//! should be re-checked against the reference.

use async_trait::async_trait;
use serde_json::{Value, json};
use vca_core::{CloudProvider, MessageRole, SessionState, now_millis};

use crate::adapter::CloudAgentAdapter;
use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::http::{self, join};
use crate::repo::GithubRepo;
use crate::types::{
    Activity, ActivityPage, CloudCapabilities, CloudLaunch, CloudSession, CloudSessionId, Cursor,
    parse_time_value,
};

pub const DEFAULT_BASE_URL: &str = "https://api.devin.ai";
const MAX_LIST_PAGES: usize = 5;

/// Devin adapter for one organization.
#[derive(Debug, Clone)]
pub struct DevinAdapter {
    client: reqwest::Client,
    base_url: String,
    org_id: String,
}

impl DevinAdapter {
    pub fn new(org_id: impl Into<String>) -> Self {
        Self::with_base_url(DEFAULT_BASE_URL, org_id)
    }

    pub fn with_base_url(base_url: impl Into<String>, org_id: impl Into<String>) -> Self {
        Self {
            client: http::default_client(),
            base_url: base_url.into(),
            org_id: org_id.into(),
        }
    }

    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        cred: &Credential,
    ) -> Result<reqwest::RequestBuilder> {
        let key = cred.require()?;
        let url = join(
            &self.base_url,
            &format!("/v3/organizations/{}{}", self.org_id, path),
        );
        Ok(self.client.request(method, url).bearer_auth(key))
    }
}

fn first_str<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

fn first_array<'a>(v: &'a Value, keys: &[&str]) -> &'a [Value] {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Next-page cursor in the known shapes (unverified field names).
fn next_cursor(v: &Value) -> Option<String> {
    let has_next = v
        .get("has_next_page")
        .or_else(|| v.get("has_more"))
        .and_then(Value::as_bool);
    if has_next == Some(false) {
        return None;
    }
    first_str(v, &["end_cursor", "next_cursor", "cursor", "next"])
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

/// Maps Devin's `status` and `status_detail` (v3) or `status_enum` (v1).
fn map_status(status: Option<&str>, detail: Option<&str>) -> SessionState {
    match detail {
        Some("waiting_for_user" | "blocked") => return SessionState::AwaitingInput,
        Some("waiting_for_approval") => return SessionState::AwaitingDecision,
        Some("finished") => return SessionState::Exited { code: Some(0) },
        Some("expired") => return SessionState::Exited { code: None },
        Some("suspended" | "suspend_requested" | "inactivity") => return SessionState::Paused,
        _ => {}
    }
    match status {
        Some("new" | "claimed" | "resuming") => SessionState::Starting,
        Some("running" | "working" | "resumed") => SessionState::Running,
        Some("blocked") => SessionState::AwaitingInput,
        Some("suspended" | "suspend_requested") => SessionState::Paused,
        Some("exit" | "finished" | "stopped") => SessionState::Exited { code: Some(0) },
        Some("expired") => SessionState::Exited { code: None },
        Some("error") => SessionState::Error {
            message: "Devin session error".into(),
        },
        _ => SessionState::Running,
    }
}

fn session_from_value(v: &Value) -> Result<CloudSession> {
    let id = first_str(v, &["session_id", "devin_id", "id"])
        .ok_or_else(|| CloudError::Decode("Devin session without id".into()))?;
    let mut s = CloudSession::new(CloudProvider::Devin, id);
    s.title = first_str(v, &["title", "name"]).map(str::to_owned);
    let status = first_str(v, &["status"]);
    let detail = first_str(v, &["status_detail", "status_enum"]);
    s.state = map_status(status, detail);
    s.provider_status = detail.or(status).map(str::to_owned);
    s.url = first_str(v, &["url"]).map(str::to_owned);
    s.created_at = v.get("created_at").and_then(parse_time_value);
    s.updated_at = v.get("updated_at").and_then(parse_time_value);
    s.pull_request_url = first_array(v, &["pull_requests"])
        .iter()
        .find_map(|p| first_str(p, &["pr_url", "url"]))
        .or_else(|| {
            v.get("pull_request")
                .and_then(|p| first_str(p, &["url", "pr_url"]))
        })
        .map(str::to_owned);
    Ok(s)
}

fn message_from_value(v: &Value, idx: usize) -> Activity {
    let id = first_str(v, &["event_id", "message_id", "id"])
        .map(str::to_owned)
        .unwrap_or_else(|| format!("idx-{idx}"));
    let at = ["created_at", "timestamp"]
        .iter()
        .find_map(|k| v.get(*k).and_then(parse_time_value))
        .unwrap_or_else(now_millis);
    let role = match first_str(v, &["source", "origin", "role", "type"]) {
        Some("user" | "user_message" | "initial_user_message") => MessageRole::User,
        _ => MessageRole::Assistant,
    };
    let text = first_str(v, &["message", "text", "content"]).unwrap_or_default();
    Activity::message(id, at, role, text)
}

#[async_trait]
impl CloudAgentAdapter for DevinAdapter {
    fn provider(&self) -> CloudProvider {
        CloudProvider::Devin
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
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut r = self
                .req(reqwest::Method::GET, "/sessions", cred)?
                .query(&[("first", "50")]);
            if let Some(c) = &cursor {
                r = r.query(&[("after", c)]);
            }
            let page: Value = http::send_json(r).await?;
            let items = if let Some(arr) = page.as_array() {
                arr.as_slice()
            } else {
                first_array(&page, &["items", "sessions", "data"])
            };
            for item in items {
                out.push(session_from_value(item)?);
            }
            cursor = next_cursor(&page);
            if cursor.is_none() || items.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    async fn get_session(&self, cred: &Credential, id: &CloudSessionId) -> Result<CloudSession> {
        let r = self.req(reqwest::Method::GET, &format!("/sessions/{id}"), cred)?;
        session_from_value(&http::send_json::<Value>(r).await?)
    }

    async fn create_session(&self, cred: &Credential, req: &CloudLaunch) -> Result<CloudSession> {
        if req.prompt.trim().is_empty() {
            return Err(CloudError::InvalidRequest("prompt is empty".into()));
        }
        let mut body = json!({ "prompt": req.prompt });
        if let Some(t) = &req.title {
            body["title"] = json!(t);
        }
        if let Some(repo) = &req.repository {
            // Unverified: `repos` as a list of `owner/repo` strings.
            let slug = GithubRepo::parse(repo)
                .map(|r| r.slug())
                .unwrap_or_else(|| repo.clone());
            body["repos"] = json!([slug]);
        }
        let r = self
            .req(reqwest::Method::POST, "/sessions", cred)?
            .json(&body);
        let mut s = session_from_value(&http::send_json::<Value>(r).await?)?;
        if s.title.is_none() {
            s.title = req.title.clone();
        }
        Ok(s)
    }

    async fn send_message(&self, cred: &Credential, id: &CloudSessionId, text: &str) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/sessions/{id}/messages"),
                cred,
            )?
            .json(&json!({ "message": text }));
        http::send_empty(r).await
    }

    async fn stop(&self, cred: &Credential, id: &CloudSessionId) -> Result<()> {
        let r = self.req(reqwest::Method::DELETE, &format!("/sessions/{id}"), cred)?;
        http::send_empty(r).await
    }

    async fn activity(
        &self,
        cred: &Credential,
        id: &CloudSessionId,
        since: Option<Cursor>,
    ) -> Result<ActivityPage> {
        let mut r = self.req(
            reqwest::Method::GET,
            &format!("/sessions/{id}/messages"),
            cred,
        )?;
        if let Some(c) = &since {
            r = r.query(&[("after", &c.0)]);
        }
        let page: Value = http::send_json(r).await?;
        let items = if let Some(arr) = page.as_array() {
            arr.as_slice()
        } else {
            first_array(&page, &["items", "messages", "data"])
        };
        let activities: Vec<Activity> = items
            .iter()
            .enumerate()
            .map(|(i, m)| message_from_value(m, i))
            .collect();
        let next = next_cursor(&page);
        let has_more = next.is_some() && !activities.is_empty();
        let state = if has_more {
            None
        } else {
            Some(self.get_session(cred, id).await?.state)
        };
        Ok(ActivityPage {
            activities,
            next: next.map(Cursor).or(since),
            has_more,
            state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(
            map_status(Some("running"), Some("waiting_for_user")),
            SessionState::AwaitingInput
        );
        assert_eq!(
            map_status(Some("exit"), None),
            SessionState::Exited { code: Some(0) }
        );
        assert_eq!(
            map_status(None, Some("blocked")),
            SessionState::AwaitingInput
        );
        assert!(matches!(
            map_status(Some("error"), None),
            SessionState::Error { .. }
        ));
    }

    #[test]
    fn tolerant_session_parse() {
        let s = session_from_value(&json!({
            "devin_id": "devin-abc", "status": "running", "url": "https://app.devin.ai/sessions/abc",
            "created_at": 1_735_689_600, "pull_requests": [{"pr_url": "https://github.com/o/r/pull/2"}]
        }))
        .unwrap();
        assert_eq!(s.id.0, "devin-abc");
        assert_eq!(s.created_at, Some(1_735_689_600_000));
        assert!(s.pull_request_url.is_some());
        assert!(session_from_value(&json!({"status": "x"})).is_err());
    }
}
