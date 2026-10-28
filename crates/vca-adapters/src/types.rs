//! Plain data types shared by all adapters.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vca_core::{AgentKind, MessageRole, SessionEvent, UnixMillis};

/// What an adapter can do for its agent. Used by the server to pick a
/// control tier and by the UI to explain missing features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Sessions stored on disk can be listed and their history read.
    pub transcripts: bool,
    /// Agent-side hooks report completion, waiting and permission events.
    pub hooks: bool,
    /// A hook can allow or deny a tool call before it runs (admission control).
    pub pre_tool_gate: bool,
    /// The agent can resume a stored conversation from the command line.
    pub resume: bool,
    /// Accounts can be isolated with a per-profile environment.
    pub profile_isolation: bool,
    /// State is guessed from terminal output (no structured signal).
    pub output_heuristics: bool,
    /// The integration was checked against the agent's documentation or a
    /// real installation. `false` means best effort and unverified.
    pub verified: bool,
}

/// An account profile, for example "work" or "personal".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    /// Directory that holds this profile's agent configuration and
    /// credentials (for example a `CLAUDE_CONFIG_DIR` or `CODEX_HOME`).
    pub config_dir: PathBuf,
}

impl Profile {
    pub fn new(id: impl Into<String>, config_dir: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            config_dir: config_dir.into(),
        }
    }

    /// Conventional location of a profile directory below a VCA data
    /// directory: `<base>/profiles/<agent>/<id>`.
    pub fn default_dir(base: &Path, agent: &str, id: &str) -> PathBuf {
        base.join("profiles").join(agent).join(id)
    }
}

/// A session found on disk or as a running process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredSession {
    pub agent: AgentKind,
    /// The agent's own session identifier (for example a Claude Code session
    /// UUID or a Codex thread id). `None` for agents without stored sessions.
    pub agent_session_id: Option<String>,
    pub project_dir: Option<PathBuf>,
    /// Root process of the session while it is running.
    pub pid: Option<u32>,
    pub last_activity: Option<UnixMillis>,
    /// Title or short summary, when the agent stores one.
    pub title: Option<String>,
    /// Transcript file the history is read from.
    pub transcript_path: Option<PathBuf>,
}

impl DiscoveredSession {
    pub fn is_running(&self) -> bool {
        self.pid.is_some()
    }
}

/// How the server launches a new managed session. The PTY host runs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Extra environment variables on top of the user's environment.
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// Where hooks are installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "dir", rename_all = "snake_case")]
pub enum HookScope {
    /// The user's global configuration (or a profile's configuration
    /// directory when the adapter was created for a profile).
    User,
    /// A project's shared configuration, for example `.claude/settings.json`.
    Project(PathBuf),
    /// A project's local, uncommitted configuration, for example
    /// `.claude/settings.local.json`.
    ProjectLocal(PathBuf),
}

/// Options for installing hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookOptions {
    /// The command that receives hook events, as an argument vector. The
    /// adapter appends its own arguments, for example `hook claude stop`.
    pub command: Vec<String>,
}

impl Default for HookOptions {
    fn default() -> Self {
        Self {
            command: vec!["vca".to_owned()],
        }
    }
}

/// Installation state of one hook entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEntryStatus {
    /// Event name in the agent's vocabulary, for example `Stop`.
    pub event: String,
    pub state: HookState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookState {
    Installed,
    Missing,
    /// A VCA hook is present but with a different command line.
    Outdated,
}

/// Result of `hooks_status`, `install_hooks` and `uninstall_hooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookStatus {
    /// File the hooks live in.
    pub config_path: PathBuf,
    pub entries: Vec<HookEntryStatus>,
    /// Backup written before the last change, if any.
    pub backup_path: Option<PathBuf>,
}

impl HookStatus {
    pub fn all_installed(&self) -> bool {
        !self.entries.is_empty() && self.entries.iter().all(|e| e.state == HookState::Installed)
    }
}

/// One normalized entry of a session's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HistoryItem {
    Message {
        at: Option<UnixMillis>,
        role: MessageRole,
        text: String,
    },
    ToolCall {
        at: Option<UnixMillis>,
        /// Tool call id, used to pair calls with results.
        id: Option<String>,
        name: String,
        /// One-line description, for example the shell command.
        summary: String,
    },
    ToolResult {
        at: Option<UnixMillis>,
        id: Option<String>,
        text: String,
        is_error: bool,
    },
}

impl HistoryItem {
    pub fn at(&self) -> Option<UnixMillis> {
        match self {
            Self::Message { at, .. } | Self::ToolCall { at, .. } | Self::ToolResult { at, .. } => {
                *at
            }
        }
    }

    /// Converts the item into a [`SessionEvent`] for the chat view. The
    /// server assigns `seq`; `fallback_at` is used when the agent did not
    /// record a timestamp.
    pub fn to_event(&self, seq: u64, fallback_at: UnixMillis) -> SessionEvent {
        let at = self.at().unwrap_or(fallback_at);
        match self {
            Self::Message { role, text, .. } => SessionEvent::Message {
                seq,
                at,
                role: *role,
                text: text.clone(),
            },
            Self::ToolCall { name, summary, .. } => SessionEvent::ToolCall {
                seq,
                at,
                name: name.clone(),
                summary: summary.clone(),
            },
            Self::ToolResult { text, is_error, .. } => SessionEvent::Message {
                seq,
                at,
                role: MessageRole::Tool,
                text: if *is_error {
                    format!("error: {text}")
                } else {
                    text.clone()
                },
            },
        }
    }
}

/// Shortens `text` to at most `max` characters, adding an ellipsis.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(max) {
        None => trimmed.to_owned(),
        Some((idx, _)) => format!("{}…", &trimmed[..idx]),
    }
}

/// Parses an RFC 3339 timestamp into [`UnixMillis`].
pub(crate) fn parse_rfc3339_millis(s: &str) -> Option<UnixMillis> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
}

pub(crate) fn system_time_millis(t: std::time::SystemTime) -> Option<UnixMillis> {
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as UnixMillis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
        assert_eq!(truncate("  short ", 10), "short");
    }

    #[test]
    fn parses_rfc3339() {
        assert_eq!(
            parse_rfc3339_millis("2026-10-28T13:37:00.801Z"),
            Some(1_793_194_620_801)
        );
        assert_eq!(parse_rfc3339_millis("nope"), None);
    }

    #[test]
    fn tool_result_becomes_tool_message() {
        let item = HistoryItem::ToolResult {
            at: None,
            id: None,
            text: "boom".into(),
            is_error: true,
        };
        match item.to_event(7, 42) {
            SessionEvent::Message {
                seq,
                at,
                role,
                text,
            } => {
                assert_eq!((seq, at, role), (7, 42, MessageRole::Tool));
                assert_eq!(text, "error: boom");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
