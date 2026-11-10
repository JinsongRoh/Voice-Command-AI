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
    /// The sub-agent a `SubagentStop` event reports as stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    /// The agent's id of the tool call the event is about, when it names
    /// one. Pairs a decision with the call's later progress (hooks and
    /// transcript results).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// The agent's permission mode when the event fired (Claude Code's
    /// `permission_mode`: `default`, `acceptEdits`, `plan`, `auto`,
    /// `dontAsk`, `bypassPermissions`). `None` when the payload has none
    /// (not every event carries it) and from older `vca` binaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// The full text of the agent's final message of the turn (Claude
    /// Code's `Stop.last_assistant_message`, Codex's turn-complete
    /// message), for telling a turn that ends in a question to the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
}

impl AgentHookEvent {
    /// True for a permission prompt the agent's own notification reports
    /// as shown to the person (Claude Code's `Notification` hook of type
    /// `permission_prompt`, sent once the prompt has waited about six
    /// seconds).
    pub fn is_prompt_notification(&self) -> bool {
        self.event_name == "Notification" && self.decision.is_some()
    }
}

/// Whether a permission mode settles tool calls without showing the person
/// a prompt (`auto`: a classifier decides; `dontAsk`: denies what is not
/// allowed; `bypassPermissions`: allows everything). A `PermissionRequest`
/// in such a mode is only real once the agent's prompt notification
/// confirms it.
pub fn mode_settles_without_prompt(mode: &str) -> bool {
    matches!(mode, "auto" | "dontAsk" | "bypassPermissions")
}

/// Whether a permission mode shows the person a prompt for a call that
/// needs permission (`default`, `acceptEdits`, `plan`).
pub fn mode_prompts(mode: &str) -> bool {
    matches!(mode, "default" | "acceptEdits" | "plan" | "manual")
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
