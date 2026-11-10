//! Claude Code routines adapter (routine fire endpoint, experimental).
//!
//! Only one endpoint is public
//! (<https://platform.claude.com/docs/en/api/claude-code/routines-fire>):
//! `POST https://api.anthropic.com/v1/claude_code/routines/{routine_id}/fire`
//! with a per-routine bearer token (`sk-ant-oat01-...`), header
//! `anthropic-version`, and an optional body `{"text": "..."}` (at most
//! 65,536 characters). It returns `claude_code_session_id` and
//! `claude_code_session_url`. There is no idempotency key: each successful
//! call starts a new session, so callers must not retry blindly.
//!
//! Listing, follow-ups and progress have no public API, so VCA can only
//! start a run and link to it.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use vca_core::{CloudProvider, SessionState, now_millis};

use crate::adapter::CloudAgentAdapter;
use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::http::{self, join};
use crate::types::{
    ActivityPage, CloudCapabilities, CloudLaunch, CloudSession, CloudSessionId, Cursor,
};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Maximum length of the `text` field, in characters.
pub const MAX_TEXT_CHARS: usize = 65_536;

/// Adapter for one saved routine.
#[derive(Debug, Clone)]
pub struct ClaudeRoutineAdapter {
    client: reqwest::Client,
    base_url: String,
    routine_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FireResponse {
    claude_code_session_id: Option<String>,
    claude_code_session_url: Option<String>,
}

impl ClaudeRoutineAdapter {
    /// `routine_id` is the `trig_...` identifier shown with the API trigger.
    pub fn new(routine_id: impl Into<String>) -> Self {
        Self {
            client: http::default_client(),
            base_url: DEFAULT_BASE_URL.into(),
            routine_id: routine_id.into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn routine_id(&self) -> &str {
        &self.routine_id
    }
}

#[async_trait]
impl CloudAgentAdapter for ClaudeRoutineAdapter {
    fn provider(&self) -> CloudProvider {
        CloudProvider::ClaudeCodeRoutines
    }

    fn capabilities(&self) -> CloudCapabilities {
        CloudCapabilities {
            create: true,
            direct_mode: true,
            ..CloudCapabilities::default()
        }
    }

    async fn list_sessions(&self, _cred: &Credential) -> Result<Vec<CloudSession>> {
        Err(CloudError::Unsupported("list"))
    }

    async fn get_session(&self, _cred: &Credential, _id: &CloudSessionId) -> Result<CloudSession> {
        Err(CloudError::Unsupported("progress"))
    }

    /// Fires the routine. `req.prompt` becomes the run's `text` context
    /// (it is passed alongside the routine's saved prompt); repository,
    /// branch and model come from the saved routine and are ignored.
    async fn create_session(&self, cred: &Credential, req: &CloudLaunch) -> Result<CloudSession> {
        let token = cred.require()?;
        if req.prompt.chars().count() > MAX_TEXT_CHARS {
            return Err(CloudError::InvalidRequest(format!(
                "routine text exceeds {MAX_TEXT_CHARS} characters"
            )));
        }
        let url = join(
            &self.base_url,
            &format!("/v1/claude_code/routines/{}/fire", self.routine_id),
        );
        let mut r = self
            .client
            .post(url)
            .bearer_auth(token)
            .header("anthropic-version", ANTHROPIC_VERSION);
        if !req.prompt.is_empty() {
            r = r.json(&json!({ "text": req.prompt }));
        }
        let resp: FireResponse = http::send_json(r).await?;
        let id = resp
            .claude_code_session_id
            .ok_or_else(|| CloudError::Decode("missing claude_code_session_id".into()))?;
        let mut s = CloudSession::new(CloudProvider::ClaudeCodeRoutines, id);
        s.url = resp.claude_code_session_url;
        s.title = req.title.clone();
        s.state = SessionState::Running;
        s.provider_status = Some("fired".into());
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
        _id: &CloudSessionId,
        _since: Option<Cursor>,
    ) -> Result<ActivityPage> {
        Err(CloudError::Unsupported("progress"))
    }
}
