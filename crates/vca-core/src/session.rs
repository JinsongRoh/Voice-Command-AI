use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::UnixMillis;

/// Stable identifier of a session managed or observed by VCA.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Coding agents that run on the user's machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "name", rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    GeminiCli,
    Antigravity,
    Grok,
    CursorCli,
    CopilotCli,
    Aider,
    Opencode,
    QwenCode,
    /// Any other command, identified by its executable name.
    Generic(String),
}

/// Coding agents hosted by a cloud provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    Jules,
    CursorCloud,
    Devin,
    CopilotCloud,
    ClaudeManagedAgents,
    ClaudeCodeRoutines,
    CodexCloud,
}

/// Where a session lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionOrigin {
    Local {
        agent: AgentKind,
        project_dir: PathBuf,
        /// Root process of the session, when known.
        pid: Option<u32>,
    },
    Cloud {
        provider: CloudProvider,
        /// Identifier assigned by the provider.
        remote_id: String,
        /// Link that opens the session in the provider's own app.
        url: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    Running,
    Idle,
    AwaitingInput,
    AwaitingDecision,
    Paused,
    Exited { code: Option<i32> },
    Error { message: String },
}

impl SessionState {
    /// True when the user is expected to act.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            Self::AwaitingInput | Self::AwaitingDecision | Self::Error { .. }
        )
    }
}

/// The strongest mechanism VCA can use to deliver input to a session
/// (see architecture §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlTier {
    ManagedPty,
    NativeChannel,
    TerminalApi,
    ObserveOnly,
    Cloud,
}

/// What a client sees on a session card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub origin: SessionOrigin,
    pub display_name: String,
    pub state: SessionState,
    pub control: ControlTier,
    pub profile: Option<String>,
    pub created_at: UnixMillis,
    pub last_activity: UnixMillis,
    /// Number of unread notifications for this session.
    pub unread: u32,
    /// Short excerpt of recent output for the session card, so clients do
    /// not need to subscribe to every session.
    #[serde(default)]
    pub preview: Option<String>,
}

/// Request to start a new local session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub agent: AgentKind,
    pub project_dir: PathBuf,
    /// Create `project_dir` if it does not exist.
    #[serde(default)]
    pub create_dir: bool,
    pub profile: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub display_name: Option<String>,
}

/// Request to start a new cloud session (CLD-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudLaunchRequest {
    pub provider: CloudProvider,
    pub repository: String,
    pub prompt: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    Pause,
    Resume,
    Stop,
    Restart,
}
