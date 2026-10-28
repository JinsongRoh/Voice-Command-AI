//! Provider-neutral cloud session types and their mapping onto `vca-core`.

use std::fmt;

use serde::{Deserialize, Serialize};
use vca_core::{
    CloudProvider, ControlTier, MessageRole, SessionEvent, SessionId, SessionInfo, SessionOrigin,
    SessionState, UnixMillis,
};

/// What a provider's official API allows (CLD-07). The UI hides missing
/// capabilities instead of failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CloudCapabilities {
    /// Start a new session.
    pub create: bool,
    /// List existing sessions.
    pub list: bool,
    /// Send a follow-up message to a running session.
    pub follow_up: bool,
    /// Read progress (messages, state changes) of a session.
    pub progress: bool,
    /// Progress is limited to coarse status (no conversation).
    pub progress_is_status_only: bool,
    /// Stop or cancel a running session.
    pub stop: bool,
    /// Approve a plan the agent proposed.
    pub approve_plan: bool,
    /// The adapter works without the user's machine (direct mode, CLD-06).
    pub direct_mode: bool,
    /// Endpoint paths or shapes could not be verified against public docs.
    pub unverified: bool,
}

/// Identifier assigned by the provider.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CloudSessionId(pub String);

impl CloudSessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CloudSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for CloudSessionId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for CloudSessionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// A session as reported by a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSession {
    pub provider: CloudProvider,
    pub id: CloudSessionId,
    pub title: Option<String>,
    /// Normalized state.
    pub state: SessionState,
    /// The provider's own status string, for display and debugging.
    pub provider_status: Option<String>,
    /// Link that opens the session in the provider's app.
    pub url: Option<String>,
    /// Repository the session works on, when known (`owner/repo` or URL).
    pub repository: Option<String>,
    /// Pull request opened by the session, when known.
    pub pull_request_url: Option<String>,
    pub created_at: Option<UnixMillis>,
    pub updated_at: Option<UnixMillis>,
}

impl CloudSession {
    pub(crate) fn new(provider: CloudProvider, id: impl Into<CloudSessionId>) -> Self {
        Self {
            provider,
            id: id.into(),
            title: None,
            state: SessionState::Starting,
            provider_status: None,
            url: None,
            repository: None,
            pull_request_url: None,
            created_at: None,
            updated_at: None,
        }
    }

    /// A stable local session ID derived from the provider and remote ID.
    pub fn default_session_id(&self) -> SessionId {
        SessionId(format!(
            "cloud:{}:{}",
            provider_slug(self.provider),
            self.id
        ))
    }

    /// Builds the session card (CLD-02): origin `Cloud`, control tier `Cloud`.
    pub fn to_session_info(&self, id: SessionId, now: UnixMillis) -> SessionInfo {
        let created = self.created_at.unwrap_or(now);
        SessionInfo {
            id,
            origin: SessionOrigin::Cloud {
                provider: self.provider,
                remote_id: self.id.0.clone(),
                url: self.url.clone(),
            },
            display_name: self
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| format!("{} {}", provider_name(self.provider), self.id)),
            state: self.state.clone(),
            control: ControlTier::Cloud,
            profile: None,
            created_at: created,
            last_activity: self.updated_at.unwrap_or(created),
            unread: 0,
            preview: None,
        }
    }
}

/// Request to start a cloud session (CLD-03).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CloudLaunch {
    pub prompt: String,
    /// Repository in the provider's format. Usually `owner/repo` or a
    /// GitHub URL; adapters normalize what they can.
    pub repository: Option<String>,
    /// Starting branch or ref.
    pub branch: Option<String>,
    pub title: Option<String>,
    /// Ask the agent to wait for plan approval (where supported).
    #[serde(default)]
    pub require_plan_approval: bool,
    /// Ask the agent to open a pull request when done (where supported).
    #[serde(default)]
    pub auto_create_pr: bool,
    /// Provider-specific model name, when the provider lets callers choose.
    pub model: Option<String>,
}

/// Opaque position in a provider's activity feed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(pub String);

/// One item of a session's activity feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    /// Identifier unique within the session; used to de-duplicate items
    /// that are returned again on later polls.
    pub id: String,
    pub at: UnixMillis,
    pub kind: ActivityKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActivityKind {
    Message { role: MessageRole, text: String },
    StateChanged { state: SessionState },
    ToolCall { name: String, summary: String },
    Notification { title: String, body: String },
}

impl Activity {
    pub(crate) fn message(
        id: impl Into<String>,
        at: UnixMillis,
        role: MessageRole,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            at,
            kind: ActivityKind::Message {
                role,
                text: text.into(),
            },
        }
    }

    pub(crate) fn state(id: impl Into<String>, at: UnixMillis, state: SessionState) -> Self {
        Self {
            id: id.into(),
            at,
            kind: ActivityKind::StateChanged { state },
        }
    }

    pub(crate) fn tool_call(
        id: impl Into<String>,
        at: UnixMillis,
        name: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            at,
            kind: ActivityKind::ToolCall {
                name: name.into(),
                summary: summary.into(),
            },
        }
    }

    pub(crate) fn notification(
        id: impl Into<String>,
        at: UnixMillis,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            at,
            kind: ActivityKind::Notification {
                title: title.into(),
                body: body.into(),
            },
        }
    }

    /// Converts the activity into a session event with the given sequence number.
    pub fn to_event(&self, seq: u64) -> SessionEvent {
        match &self.kind {
            ActivityKind::Message { role, text } => SessionEvent::Message {
                seq,
                at: self.at,
                role: *role,
                text: text.clone(),
            },
            ActivityKind::StateChanged { state } => SessionEvent::StateChanged {
                seq,
                at: self.at,
                state: state.clone(),
            },
            ActivityKind::ToolCall { name, summary } => SessionEvent::ToolCall {
                seq,
                at: self.at,
                name: name.clone(),
                summary: summary.clone(),
            },
            ActivityKind::Notification { title, body } => SessionEvent::Notification {
                seq,
                at: self.at,
                title: title.clone(),
                body: body.clone(),
            },
        }
    }
}

/// One page of activity.
///
/// Contract for adapters: items are in chronological order and may repeat
/// items from earlier pages (the poller de-duplicates by [`Activity::id`]).
/// When `has_more` is true the caller fetches again immediately with
/// `next`; otherwise `next` is the cursor to resume from on the next poll
/// (it may point at the last page again so that new items appended to it
/// are seen).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActivityPage {
    pub activities: Vec<Activity>,
    pub next: Option<Cursor>,
    pub has_more: bool,
    /// Latest known session state, when the provider reports it with the page.
    pub state: Option<SessionState>,
}

/// Short machine name of a provider, used in derived IDs.
pub fn provider_slug(p: CloudProvider) -> &'static str {
    match p {
        CloudProvider::Jules => "jules",
        CloudProvider::CursorCloud => "cursor",
        CloudProvider::Devin => "devin",
        CloudProvider::CopilotCloud => "copilot",
        CloudProvider::ClaudeManagedAgents => "claude-managed",
        CloudProvider::ClaudeCodeRoutines => "claude-routines",
        CloudProvider::CodexCloud => "codex",
    }
}

/// Human-readable provider name.
pub fn provider_name(p: CloudProvider) -> &'static str {
    match p {
        CloudProvider::Jules => "Jules",
        CloudProvider::CursorCloud => "Cursor",
        CloudProvider::Devin => "Devin",
        CloudProvider::CopilotCloud => "Copilot",
        CloudProvider::ClaudeManagedAgents => "Claude Managed Agents",
        CloudProvider::ClaudeCodeRoutines => "Claude Code routine",
        CloudProvider::CodexCloud => "Codex cloud",
    }
}

/// Parses an RFC 3339 timestamp into milliseconds since the Unix epoch.
pub(crate) fn parse_time(s: &str) -> Option<UnixMillis> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .and_then(|t| u64::try_from(t.timestamp_millis()).ok())
}

/// Accepts RFC 3339 strings or Unix timestamps (seconds or milliseconds).
pub(crate) fn parse_time_value(v: &serde_json::Value) -> Option<UnixMillis> {
    match v {
        serde_json::Value::String(s) => parse_time(s),
        serde_json::Value::Number(n) => {
            let f = n.as_f64()?;
            if f <= 0.0 {
                return None;
            }
            // Heuristic: values below 1e11 are seconds.
            Some(if f < 1e11 {
                (f * 1000.0) as u64
            } else {
                f as u64
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_info_mapping() {
        let mut s = CloudSession::new(CloudProvider::Jules, "123");
        s.url = Some("https://example.test/s/123".into());
        s.created_at = Some(10);
        s.updated_at = Some(20);
        s.state = SessionState::Running;
        let info = s.to_session_info(s.default_session_id(), 99);
        assert_eq!(info.id.0, "cloud:jules:123");
        assert_eq!(info.control, ControlTier::Cloud);
        assert_eq!(info.display_name, "Jules 123");
        assert_eq!(info.created_at, 10);
        assert_eq!(info.last_activity, 20);
        match info.origin {
            SessionOrigin::Cloud {
                provider,
                remote_id,
                url,
            } => {
                assert_eq!(provider, CloudProvider::Jules);
                assert_eq!(remote_id, "123");
                assert!(url.is_some());
            }
            other => panic!("unexpected origin {other:?}"),
        }
    }

    #[test]
    fn activity_to_event() {
        let a = Activity::message("a1", 5, MessageRole::Assistant, "hi");
        assert_eq!(
            a.to_event(7),
            SessionEvent::Message {
                seq: 7,
                at: 5,
                role: MessageRole::Assistant,
                text: "hi".into()
            }
        );
    }

    #[test]
    fn time_parsing() {
        assert_eq!(parse_time("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(
            parse_time("2025-01-01T00:00:00.5+00:00"),
            Some(1_735_689_600_500)
        );
        assert_eq!(
            parse_time_value(&serde_json::json!(1_735_689_600)),
            Some(1_735_689_600_000)
        );
        assert_eq!(
            parse_time_value(&serde_json::json!(1_735_689_600_500u64)),
            Some(1_735_689_600_500)
        );
        assert_eq!(parse_time("garbage"), None);
    }
}
