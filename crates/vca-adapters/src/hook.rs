//! Agent-neutral representation of hook events and hook replies.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vca_core::{AgentKind, Decision, SessionState};

/// A notification to show to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookNotification {
    pub title: String,
    pub body: String,
}

/// A tool call reported by a hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookToolUse {
    pub name: String,
    pub input: serde_json::Value,
    /// The shell command, for shell tools. This is what the resource
    /// broker classifies for admission control.
    pub command: Option<String>,
}

/// A hook event translated into VCA terms. Produced by `vca hook <agent>`
/// from the agent's payload and forwarded to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentHookEvent {
    pub agent: AgentKind,
    /// Event name in the agent's vocabulary, for example `Notification`.
    pub event_name: String,
    pub agent_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub transcript_path: Option<PathBuf>,
    /// The session state this event implies, if any.
    pub state: Option<SessionState>,
    pub notification: Option<HookNotification>,
    /// A question the agent is waiting on.
    pub decision: Option<Decision>,
    pub tool: Option<HookToolUse>,
    /// True when the agent waits for the hook's reply before continuing
    /// (for example `PreToolUse`), so the reply should come quickly or the
    /// agent stays blocked.
    pub blocking: bool,
}

/// What a hook tells the agent about a pending action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookDecision {
    /// Do not interfere; the agent's normal flow applies.
    NoOpinion,
    /// Let the action run without asking the user.
    Allow { reason: Option<String> },
    /// Block the action. `reason` is shown to the agent.
    Deny { reason: String },
    /// Ask the user to confirm (shown in the agent's own prompt).
    Ask { reason: Option<String> },
}

/// What the hook process prints and how it exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl HookResponse {
    pub fn ok() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        }
    }
}
