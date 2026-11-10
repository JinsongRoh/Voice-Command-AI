//! The conductor: one AI session (or VCA's built-in tool-calling loop)
//! that orchestrates the other AI CLI sessions of the same PC through VCA.
//!
//! These are the wire types of the conductor messages in
//! [`crate::ClientMsg`] and [`crate::ServerMsg`]. Safety rules (enforced by
//! the server, see `docs/conductor.md`):
//!
//! * opt-in per PC, enabled only from the PC itself (default off);
//! * the conductor answers another session's permission decision only when
//!   one of the person's [`ConductorDecisionRule`]s (set on the PC, none by
//!   default) covers it, and never approves a risky one; everything else it
//!   escalates to the person ([`ConductorEscalation`]). Its own session's
//!   decisions are always the person's;
//! * every send to another session needs a human's confirmation unless the
//!   person put the session on the expiring allowlist;
//! * never a remote shell, never the conductor itself, only this PC;
//! * hourly caps on conductor actions and wake-ups; every action audited
//!   with origin `conductor`; one kill switch revokes it.

use serde::{Deserialize, Serialize};

use crate::UnixMillis;
use crate::session::SessionId;

/// Who runs the conductor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConductorAgent {
    /// Claude Code with `--mcp-config` (the default).
    #[default]
    ClaudeCode,
    /// Codex CLI with an `mcp_servers` override on its command line.
    Codex,
    /// Gemini CLI with `mcpServers` in the conductor folder's
    /// `.gemini/settings.json`.
    GeminiCli,
    /// VCA's own tool-calling loop against an OpenAI-compatible LLM profile
    /// (`[[speech.llms]]`) that supports tool calling.
    Builtin,
}

impl ConductorAgent {
    pub const ALL: [ConductorAgent; 4] = [
        ConductorAgent::ClaudeCode,
        ConductorAgent::Codex,
        ConductorAgent::GeminiCli,
        ConductorAgent::Builtin,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::GeminiCli => "gemini_cli",
            Self::Builtin => "builtin",
        }
    }
}

/// `Welcome::features` entry of servers that answer the conductor messages.
pub const FEATURE_CONDUCTOR: &str = "conductor";

/// Default of [`ConductorSettings::max_actions_per_hour`].
pub const CONDUCTOR_DEFAULT_ACTIONS_PER_HOUR: u32 = 20;
/// Default of [`ConductorSettings::max_wakes_per_hour`].
pub const CONDUCTOR_DEFAULT_WAKES_PER_HOUR: u32 = 6;
/// Shortest pause between two wake-ups, in seconds.
pub const CONDUCTOR_MIN_WAKE_GAP_SECS: u64 = 120;
/// Default of [`ConductorSettings::confirm_timeout_secs`].
pub const CONDUCTOR_DEFAULT_CONFIRM_SECS: u32 = 120;
/// Default of [`ConductorSettings::builtin_max_calls_per_hour`].
pub const CONDUCTOR_DEFAULT_LLM_CALLS_PER_HOUR: u32 = 30;
/// Default of [`ConductorSettings::builtin_max_tokens`].
pub const CONDUCTOR_DEFAULT_MAX_TOKENS: u32 = 1024;
/// Longest instruction or send text, in characters.
pub const CONDUCTOR_MAX_TEXT_CHARS: usize = 4000;
/// Longest allowlist entry, in minutes (one day).
pub const CONDUCTOR_MAX_ALLOW_MINUTES: u32 = 24 * 60;
/// How long the conductor has to answer or escalate another session's
/// decision once it was told about it, before the decision goes to the
/// person's inbox by itself.
pub const CONDUCTOR_TRIAGE_GRACE_SECS: u64 = 75;
/// Most decision rules kept ([`ConductorSettings::decision_rules`]).
pub const CONDUCTOR_MAX_DECISION_RULES: usize = 50;

/// The answer a [`ConductorDecisionRule`] lets the conductor give.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConductorRuleAnswer {
    /// The plain one-time "Yes" of a decision whose exact shell command
    /// (hook data) starts with one of the rule's commands and passes the
    /// server's safety screen. Never "Yes, and don't ask again".
    Approve,
    /// The plain "No".
    #[default]
    Deny,
}

/// One allow rule of the person: which other-session decisions the
/// conductor may answer on its own. Set only in the desktop app on the PC;
/// none by default, so by default the conductor can only escalate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConductorDecisionRule {
    /// Named in the audit of every answer it allows.
    pub id: String,
    pub answer: ConductorRuleAnswer,
    /// Command prefixes, matched on whole words ("cargo test" covers
    /// "cargo test -p core", not "cargo testing"). Required for `approve`;
    /// for `deny`, empty covers every decision of the rule's sessions.
    pub commands: Vec<String>,
    /// Session names (case-insensitive) or ids it covers; empty: all.
    pub sessions: Vec<String>,
    /// The rule stops applying after this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UnixMillis>,
}

/// The conductor settings of this PC (`<data>/conductor.json`). Changed
/// only from the desktop app on the PC itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConductorSettings {
    /// The opt-in. Off by default.
    pub enabled: bool,
    pub agent: ConductorAgent,
    /// The `[[speech.llms]]` profile of the built-in conductor.
    pub builtin_llm: Option<String>,
    /// "지휘 세션에 알림 전달": batched turn-end, error and decision
    /// summaries are typed into the conductor. Off by default.
    pub wake_events: bool,
    /// Hard cap on conductor actions (sends, interrupts, new sessions,
    /// restores, focus) per hour.
    pub max_actions_per_hour: u32,
    /// Hard cap on wake-ups per hour (at most one per two minutes anyway).
    pub max_wakes_per_hour: u32,
    /// How long a send waits for the person's decision.
    pub confirm_timeout_secs: u32,
    /// Built-in conductor: LLM requests per hour.
    pub builtin_max_calls_per_hour: u32,
    /// Built-in conductor: `max_tokens` of each LLM request.
    pub builtin_max_tokens: u32,
    /// The Command Center is paused ([`crate::ClientMsg::CcPause`]): no
    /// wake-ups reach the conductor; its session keeps running. Changed only
    /// with `CcPause` (a saved settings form keeps the current value).
    pub paused: bool,
    /// The person's allow rules for answering other sessions' decisions.
    /// Empty by default: the conductor then only escalates.
    pub decision_rules: Vec<ConductorDecisionRule>,
}

impl Default for ConductorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            agent: ConductorAgent::ClaudeCode,
            builtin_llm: None,
            wake_events: false,
            max_actions_per_hour: CONDUCTOR_DEFAULT_ACTIONS_PER_HOUR,
            max_wakes_per_hour: CONDUCTOR_DEFAULT_WAKES_PER_HOUR,
            confirm_timeout_secs: CONDUCTOR_DEFAULT_CONFIRM_SECS,
            builtin_max_calls_per_hour: CONDUCTOR_DEFAULT_LLM_CALLS_PER_HOUR,
            builtin_max_tokens: CONDUCTOR_DEFAULT_MAX_TOKENS,
            paused: false,
            decision_rules: Vec::new(),
        }
    }
}

/// Where the conductor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConductorStatus {
    /// Not enabled on this PC, or stopped.
    #[default]
    Off,
    Starting,
    /// Working on something.
    Running,
    /// Idle: waiting for an instruction.
    Ready,
    /// A conductor request waits for the person.
    WaitingForYou,
    /// Revoked by the kill switch (or the global kill switch is tripped)
    /// until started again.
    Revoked,
    /// Paused by the person: running, but it gets no wake-ups.
    Paused,
}

/// A send the conductor wants to make, waiting for the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConductorRequest {
    pub id: String,
    pub session_id: SessionId,
    /// The target session's name when asked.
    pub session_name: String,
    /// The text that would be typed (followed by Enter).
    pub text: String,
    pub created_at: UnixMillis,
    /// Denied automatically after this.
    pub expires_at: UnixMillis,
    /// Why the person is asked, a stable code: `no_instruction`,
    /// `not_named`, `expired` (no instruction grant covers the send) or a
    /// failed auto-send screen (`too_long`, `secret`, `risky_command`,
    /// `link`, `copied_output`, `awaiting_decision`). Absent from older
    /// servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A session whose sends go through without confirmation until
/// `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConductorAllow {
    pub session_id: SessionId,
    pub expires_at: UnixMillis,
}

/// Who put an escalation in the person's inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConductorEscalationSource {
    /// The conductor, with its summary (`escalate_decision`).
    #[default]
    Conductor,
    /// VCA itself: the conductor did not answer or escalate in time, or
    /// could not be told (stopped, paused, revoked, hooks missing, ...).
    Fallback,
}

/// Another session's decision raised to the person. It lives exactly as
/// long as that decision: once the decision is answered (anywhere) or
/// replaced, the escalation goes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConductorEscalation {
    pub id: String,
    pub session_id: SessionId,
    pub session_name: String,
    /// The decision it is about ([`crate::Decision::id`]) and when it was
    /// asked: together they identify it.
    pub decision_id: String,
    pub asked_at: UnixMillis,
    pub source: ConductorEscalationSource,
    /// The conductor's short summary of what is asked and why it needs
    /// the person. Empty for a fallback.
    pub summary: String,
    /// The conductor's recommendation, if it gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
    /// The original request (the decision's prompt), secrets redacted.
    pub request: String,
    /// The exact command of the decision (hook data), secrets redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Why a fallback was raised: `timeout`, `stopped`, `paused`,
    /// `revoked`, `hooks_missing`, `turn_stalled`, `error` or
    /// `not_routed` (passing events to the conductor is off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: UnixMillis,
}

/// One line of the conductor chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConductorTurn {
    /// `user`, `assistant` or `event` (a wake-up summary).
    pub role: String,
    pub text: String,
    pub at: UnixMillis,
}

/// An agent the conductor can run on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConductorAgentInfo {
    pub agent: ConductorAgent,
    /// Its program was found on this PC (always true for `builtin`).
    pub available: bool,
    /// The MCP setup could not be verified end to end.
    pub experimental: bool,
}

/// An LLM profile the built-in conductor can use (OpenAI-compatible only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConductorLlm {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// Everything the conductor views show. Never contains secrets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConductorState {
    pub settings: ConductorSettings,
    pub status: ConductorStatus,
    /// The conductor's own session (CLI agents).
    pub session_id: Option<SessionId>,
    /// The model of the built-in conductor's profile, when known.
    pub model: Option<String>,
    /// The global kill switch is tripped.
    pub kill_switch: bool,
    pub pending: Vec<ConductorRequest>,
    pub allowlist: Vec<ConductorAllow>,
    /// Newest last, at most a few dozen.
    pub chat: Vec<ConductorTurn>,
    pub actions_last_hour: u32,
    pub wakes_last_hour: u32,
    pub agents: Vec<ConductorAgentInfo>,
    pub llms: Vec<ConductorLlm>,
    /// Why the conductor cannot work as it is, when known:
    /// [`CONDUCTOR_PROBLEM_HOOKS_MISSING`] or
    /// [`CONDUCTOR_PROBLEM_TURN_STALLED`]. `None` when nothing is known to
    /// be wrong.
    pub problem: Option<String>,
    /// Other sessions' decisions waiting for the person, raised by the
    /// conductor or by the fallback. Empty while the conductor is off.
    pub escalations: Vec<ConductorEscalation>,
}

/// [`ConductorState::problem`]: the conductor session started, but its
/// agent never reported a hook event, so its turns and replies cannot be
/// seen (the VCA hooks are missing from the agent's settings).
pub const CONDUCTOR_PROBLEM_HOOKS_MISSING: &str = "hooks_missing";

/// [`ConductorState::problem`]: the conductor started a turn (it was asked
/// something, or woken) and no end of that turn was seen for
/// 15 minutes: it may be stuck, or its turn ends are not reported.
pub const CONDUCTOR_PROBLEM_TURN_STALLED: &str = "turn_stalled";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_capped() {
        let s: ConductorSettings = serde_json::from_str("{}").unwrap();
        assert!(!s.enabled);
        assert!(!s.wake_events);
        assert!(s.decision_rules.is_empty(), "no allow rule by default");
        assert_eq!(s.agent, ConductorAgent::ClaudeCode);
        assert_eq!(s.max_actions_per_hour, 20);
        let json = serde_json::to_value(ConductorState::default()).unwrap();
        assert_eq!(json["status"], "off");
        assert_eq!(json["settings"]["agent"], "claude_code");
        let back: ConductorState = serde_json::from_value(json).unwrap();
        assert_eq!(back, ConductorState::default());
        // Older servers send neither rules nor escalations.
        let old: ConductorState =
            serde_json::from_value(serde_json::json!({"status": "ready"})).unwrap();
        assert!(old.escalations.is_empty() && old.settings.decision_rules.is_empty());
        let esc = ConductorEscalation {
            id: "ce-1".into(),
            session_id: "s1".into(),
            decision_id: "d1".into(),
            source: ConductorEscalationSource::Fallback,
            reason: Some("timeout".into()),
            ..ConductorEscalation::default()
        };
        let v = serde_json::to_value(&esc).unwrap();
        assert_eq!(v["source"], "fallback");
        assert!(v.get("recommendation").is_none());
        let rule: ConductorDecisionRule = serde_json::from_value(
            serde_json::json!({"answer": "approve", "commands": ["cargo test"]}),
        )
        .unwrap();
        assert_eq!(rule.answer, ConductorRuleAnswer::Approve);
        assert!(rule.sessions.is_empty());
        let bytes = rmp_serde::to_vec_named(&back).unwrap();
        assert_eq!(
            rmp_serde::from_slice::<ConductorState>(&bytes).unwrap(),
            back
        );
    }
}
