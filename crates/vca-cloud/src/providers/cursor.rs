//! Cursor Cloud Agents adapter (Cloud Agents API `v0`).
//!
//! Endpoints (documented at <https://docs.cursor.com/en/background-agent/api/overview>):
//! `POST /v0/agents`, `GET /v0/agents`, `GET /v0/agents/{id}`,
//! `POST /v0/agents/{id}/followup`, `GET /v0/agents/{id}/conversation`,
//! `POST /v0/agents/{id}/stop`. Authentication uses a dedicated Cloud Agents
//! API key, sent with HTTP Basic (key as user name, empty password) by
//! default, or as a bearer token.

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

pub const DEFAULT_BASE_URL: &str = "https://api.cursor.com";
const MAX_LIST_PAGES: usize = 5;

/// How the API key is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorAuth {
    /// `Authorization: Basic base64(key:)`, as in Cursor's curl examples.
    #[default]
    Basic,
    /// `Authorization: Bearer key`.
    Bearer,
}

/// Cursor Cloud Agents adapter.
#[derive(Debug, Clone)]
pub struct CursorAdapter {
    client: reqwest::Client,
    base_url: String,
    auth: CursorAuth,
}

impl Default for CursorAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorAdapter {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            client: http::default_client(),
            base_url: base_url.into(),
            auth: CursorAuth::default(),
        }
    }

    pub fn with_auth(mut self, auth: CursorAuth) -> Self {
        self.auth = auth;
        self
    }

    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        cred: &Credential,
    ) -> Result<reqwest::RequestBuilder> {
        let key = cred.require()?;
        let r = self.client.request(method, join(&self.base_url, path));
        Ok(match self.auth {
            CursorAuth::Basic => r.basic_auth(key, Some("")),
            CursorAuth::Bearer => r.bearer_auth(key),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CAgent {
    id: Option<String>,
    name: Option<String>,
    status: Option<String>,
    source: Option<Value>,
    target: Option<Value>,
    summary: Option<String>,
    created_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CAgentList {
    agents: Vec<CAgent>,
    next_cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CConversation {
    messages: Vec<CMessage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CMessage {
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

/// Maps `CREATING`, `RUNNING`, `FINISHED`, `ERROR`, `EXPIRED`.
fn map_status(s: &str) -> SessionState {
    match s {
        "CREATING" => SessionState::Starting,
        "RUNNING" => SessionState::Running,
        "FINISHED" => SessionState::Exited { code: Some(0) },
        "ERROR" => SessionState::Error {
            message: "Cursor agent failed".into(),
        },
        "EXPIRED" => SessionState::Exited { code: None },
        _ => SessionState::Running,
    }
}

impl CAgent {
    fn into_cloud(self) -> Result<CloudSession> {
        let id = self
            .id
            .ok_or_else(|| CloudError::Decode("Cursor agent without id".into()))?;
        let mut s = CloudSession::new(CloudProvider::CursorCloud, id);
        s.title = self.name.or(self.summary);
        if let Some(st) = &self.status {
            s.state = map_status(st);
        }
        s.provider_status = self.status;
        let target = self.target.unwrap_or_default();
        s.url = target.get("url").and_then(Value::as_str).map(str::to_owned);
        s.pull_request_url = target
            .get("prUrl")
            .and_then(Value::as_str)
            .map(str::to_owned);
        s.repository = self
            .source
            .as_ref()
            .and_then(|v| v.get("repository"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        s.created_at = self.created_at.as_deref().and_then(parse_time);
        Ok(s)
    }
}

#[async_trait]
impl CloudAgentAdapter for CursorAdapter {
    fn provider(&self) -> CloudProvider {
        CloudProvider::CursorCloud
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
                .req(reqwest::Method::GET, "/v0/agents", cred)?
                .query(&[("limit", "100")]);
            if let Some(c) = &cursor {
                r = r.query(&[("cursor", c)]);
            }
            let page: CAgentList = http::send_json(r).await?;
            for a in page.agents {
                out.push(a.into_cloud()?);
            }
            cursor = page.next_cursor.filter(|c| !c.is_empty());
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    async fn get_session(&self, cred: &Credential, id: &CloudSessionId) -> Result<CloudSession> {
        let r = self.req(reqwest::Method::GET, &format!("/v0/agents/{id}"), cred)?;
        http::send_json::<CAgent>(r).await?.into_cloud()
    }

    async fn create_session(&self, cred: &Credential, req: &CloudLaunch) -> Result<CloudSession> {
        if req.prompt.trim().is_empty() {
            return Err(CloudError::InvalidRequest("prompt is empty".into()));
        }
        let repo = req
            .repository
            .as_deref()
            .ok_or_else(|| CloudError::InvalidRequest("Cursor agents need a repository".into()))?;
        let repo_url = GithubRepo::parse(repo)
            .map(|r| r.https_url())
            .unwrap_or_else(|| repo.to_owned());
        let mut source = json!({ "repository": repo_url });
        if let Some(b) = &req.branch {
            source["ref"] = json!(b);
        }
        let mut body = json!({
            "prompt": { "text": req.prompt },
            "source": source,
            "target": { "autoCreatePr": req.auto_create_pr },
        });
        if let Some(m) = &req.model {
            body["model"] = json!(m);
        }
        let r = self
            .req(reqwest::Method::POST, "/v0/agents", cred)?
            .json(&body);
        let mut s = http::send_json::<CAgent>(r).await?.into_cloud()?;
        if s.title.is_none() {
            s.title = req.title.clone();
        }
        Ok(s)
    }

    async fn send_message(&self, cred: &Credential, id: &CloudSessionId, text: &str) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/v0/agents/{id}/followup"),
                cred,
            )?
            .json(&json!({ "prompt": { "text": text } }));
        http::send_empty(r).await
    }

    async fn stop(&self, cred: &Credential, id: &CloudSessionId) -> Result<()> {
        let r = self.req(
            reqwest::Method::POST,
            &format!("/v0/agents/{id}/stop"),
            cred,
        )?;
        http::send_empty(r).await
    }

    async fn activity(
        &self,
        cred: &Credential,
        id: &CloudSessionId,
        _since: Option<Cursor>,
    ) -> Result<ActivityPage> {
        // The conversation endpoint returns the whole conversation; the
        // poller de-duplicates by message ID.
        let r = self.req(
            reqwest::Method::GET,
            &format!("/v0/agents/{id}/conversation"),
            cred,
        )?;
        let conv: CConversation = http::send_json(r).await?;
        let at = now_millis();
        let activities = conv
            .messages
            .into_iter()
            .enumerate()
            .map(|(i, m)| {
                let role = match m.kind.as_deref() {
                    Some("user_message") => MessageRole::User,
                    _ => MessageRole::Assistant,
                };
                let id = m.id.unwrap_or_else(|| format!("idx-{i}"));
                Activity::message(id, at, role, m.text.unwrap_or_default())
            })
            .collect();
        let state = self.get_session(cred, id).await?.state;
        Ok(ActivityPage {
            activities,
            next: None,
            has_more: false,
            state: Some(state),
        })
    }
}
