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
    /// A plain shell opened with [`crate::ClientMsg::OpenShell`]. The content
    /// is the allow-listed shell name: `powershell`, `pwsh`, `cmd`, `bash`,
    /// `zsh` or `sh` (`{"type":"shell","name":"powershell"}`).
    Shell(String),
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
    /// The name VCA picked on its own (the agent's title, the launch name
    /// or "<folder> · <agent>"). `display_name` differs from it when the
    /// user renamed the session. `None` from servers that predate renaming.
    #[serde(default)]
    pub auto_name: Option<String>,
    /// True when `display_name` is a name the user chose.
    #[serde(default)]
    pub custom_name: bool,
    /// Set for remote shell sessions (agent [`AgentKind::Shell`]): their
    /// timeouts and who opened them.
    #[serde(default)]
    pub shell: Option<ShellInfo>,
    /// The decision the agent is waiting on, so clients can list it without
    /// subscribing to the session. `None` from older servers.
    #[serde(default)]
    pub pending_decision: Option<PendingDecisionInfo>,
    /// Sub-agents the session's agent is running now (Claude Code `Task` /
    /// `Agent` calls, Codex sub-agents), from its transcript. Left out of
    /// the JSON when 0; 0 from older servers.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subagents: u32,
    /// True for the conductor's own session (see `docs/conductor.md`): its
    /// turn-ends, unread counts and notifications are not alerts (its
    /// replies reach the conductor view); its permission decisions still
    /// are. Left out of the JSON when false; false from older servers.
    #[serde(default, skip_serializing_if = "is_false")]
    pub conductor: bool,
    /// A selection menu on the terminal screen of a managed session that
    /// has no transcript yet (for example Claude Code's first-run "trust
    /// this folder" question): only the terminal answers it. `None` from
    /// older servers and left out of the JSON when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_prompt: Option<TerminalPrompt>,
    /// The agent ended its turn with a question to the person (a plain
    /// question in its final message, not a form or a permission prompt).
    /// Cleared once the conversation moves on. `None` from older servers
    /// and left out of the JSON when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<AgentQuestion>,
    /// The agent's current (or last) turn: when it started, whether it
    /// ended and what the agent is doing now, from its hooks and its
    /// transcript. Clients show it as the chat's "Working · 42s" line.
    /// `None` from older servers and left out of the JSON when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<TurnProgress>,
    /// Something keeps VCA from following this session well, for example
    /// [`SESSION_PROBLEM_HOOKS_MISSING`]. `None` from older servers and left
    /// out of the JSON when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// [`SessionInfo::problem`]: the agent should report to VCA through hooks
/// (Claude Code) but its hooks are missing from its settings, so progress
/// shows late (only from the transcript). The same word as the conductor's
/// problem ([`crate::CONDUCTOR_PROBLEM_HOOKS_MISSING`]).
pub const SESSION_PROBLEM_HOOKS_MISSING: &str = "hooks_missing";

/// An agent's turn as clients see it ([`SessionInfo::progress`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnProgress {
    /// When the person's prompt started the turn (server clock).
    pub started_at: UnixMillis,
    /// When the turn ended; `None` while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<UnixMillis>,
    /// What the agent does right now, when known; `None` between steps
    /// (for example while it writes its answer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<TurnActivity>,
}

/// What an agent does within a turn ([`TurnProgress::activity`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnActivity {
    /// It recorded a thinking block. Its text is never sent (it is
    /// redacted or private): only the fact.
    Thinking,
    /// Its newest tool call: the tool and its one-line summary (the file,
    /// the command, the pattern).
    Tool { name: String, summary: String },
}

/// A question an agent's final message of a turn asks the person
/// ([`SessionInfo::question`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQuestion {
    /// Stable id of this question (one per turn), for alert bookkeeping.
    pub id: String,
    /// The question: the last sentence of the message that asks something,
    /// shortened.
    pub text: String,
    pub at: UnixMillis,
}

/// What a [`SessionInfo::terminal_prompt`] asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalPrompt {
    /// The agent asks whether to trust the folder it starts in.
    TrustFolder,
    /// Any other selection menu.
    Menu,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A decision a session is waiting on, with when it was asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDecisionInfo {
    pub decision: crate::event::Decision,
    pub at: UnixMillis,
}

/// Timeouts and owner of a remote shell session. The server stops the shell
/// at `idle_deadline` (no input from any client for `idle_timeout_secs`) or
/// at `hard_deadline` (absolute lifetime), whichever comes first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellInfo {
    pub idle_timeout_secs: u32,
    /// Moves forward with every input; never later than `hard_deadline`.
    pub idle_deadline: UnixMillis,
    pub hard_deadline: UnixMillis,
    /// Name of the device that opened the shell (`local` for this computer).
    pub opened_by: String,
    /// The allow-listed shell it runs (`powershell`, `bash`, ...), and the
    /// folder it started in: an [`crate::ClientMsg::OpenShell`] with both
    /// (and a fresh code) opens a new shell like an ended one. Empty from
    /// older servers.
    #[serde(default)]
    pub shell: String,
    #[serde(default)]
    pub project_dir: PathBuf,
}

/// Request to open a remote shell ([`crate::ClientMsg::OpenShell`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellRequest {
    /// `powershell`, `pwsh`, `cmd` (Windows), `bash`, `zsh`, `sh` (Unix) or
    /// `default`. Never a path.
    pub shell: String,
    /// Must be inside the workspace roots, like a launch.
    pub project_dir: PathBuf,
    #[serde(default)]
    pub display_name: Option<String>,
    /// A fresh TOTP code (single use).
    pub totp: String,
}

impl fmt::Debug for ShellRequest {
    /// Never prints the TOTP code.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellRequest")
            .field("shell", &self.shell)
            .field("project_dir", &self.project_dir)
            .field("display_name", &self.display_name)
            .field("totp", &"[redacted]")
            .finish()
    }
}

/// Longest session name accepted by [`validate_session_name`], in
/// characters.
pub const MAX_SESSION_NAME_CHARS: usize = 80;

/// Checks a user-chosen session name. Returns the trimmed name, `None` for
/// a blank name (meaning "use the automatic name"), or the reason it is
/// not accepted.
pub fn validate_session_name(name: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if name.chars().any(char::is_control) {
        return Err("the name must not contain control characters");
    }
    if name.chars().count() > MAX_SESSION_NAME_CHARS {
        return Err("the name is longer than 80 characters");
    }
    Ok(Some(name.to_owned()))
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
    /// Start a new conversation even when the folder has earlier ones.
    /// Otherwise an agent that can continue (Claude Code) picks up the
    /// folder's most recent conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_validated() {
        assert_eq!(validate_session_name(None), Ok(None));
        assert_eq!(validate_session_name(Some("   ")), Ok(None));
        assert_eq!(
            validate_session_name(Some("  API work ")),
            Ok(Some("API work".into()))
        );
        assert!(validate_session_name(Some("a\nb")).is_err());
        assert!(validate_session_name(Some("a\u{7}b")).is_err());
        let long = "가".repeat(MAX_SESSION_NAME_CHARS);
        assert_eq!(validate_session_name(Some(&long)), Ok(Some(long.clone())));
        assert!(validate_session_name(Some(&format!("{long}x"))).is_err());
    }

    #[test]
    fn session_info_from_old_servers_still_parses() {
        let json = r#"{"id":"s","origin":{"type":"cloud","provider":"jules","remote_id":"1",
            "url":null},"display_name":"n","state":{"type":"running"},"control":"cloud",
            "profile":null,"created_at":1,"last_activity":1,"unread":0}"#;
        let info: SessionInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.auto_name, None);
        assert!(!info.custom_name);
        assert_eq!(info.shell, None);
    }

    #[test]
    fn shell_kind_and_info_shapes() {
        let kind = AgentKind::Shell("powershell".into());
        assert_eq!(
            serde_json::to_value(&kind).unwrap(),
            serde_json::json!({"type": "shell", "name": "powershell"})
        );
        let info = ShellInfo {
            idle_timeout_secs: 900,
            idle_deadline: 10,
            hard_deadline: 20,
            opened_by: "phone".into(),
            shell: "pwsh".into(),
            project_dir: PathBuf::from("C:/code"),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["idle_timeout_secs"], 900);
        assert_eq!(json["opened_by"], "phone");
        assert_eq!(json["shell"], "pwsh");
        assert_eq!(json["project_dir"], "C:/code");
        // Older servers send neither.
        let old: ShellInfo = serde_json::from_value(serde_json::json!({
            "idle_timeout_secs": 1, "idle_deadline": 1, "hard_deadline": 2, "opened_by": "x"
        }))
        .unwrap();
        assert!(old.shell.is_empty() && old.project_dir.as_os_str().is_empty());
        let req: ShellRequest = serde_json::from_value(serde_json::json!({
            "shell": "cmd", "project_dir": "C:/code", "totp": "123456"
        }))
        .unwrap();
        assert_eq!(req.display_name, None);
        assert!(!format!("{req:?}").contains("123456"));
    }
}
