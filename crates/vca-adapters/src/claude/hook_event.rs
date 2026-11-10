//! Claude Code hook payloads (stdin of `vca hook claude <event>`) and replies.
//!
//! Format checked against <https://code.claude.com/docs/en/hooks> (October
//! 2026): every event receives `session_id`, `transcript_path`, `cwd` and
//! `hook_event_name`, plus event-specific fields. Unknown fields are kept in
//! [`ClaudeHookPayload::extra`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use vca_core::{AgentKind, Decision, DecisionOption, SessionState};

use crate::claude::question::question_prompt;
use crate::claude::transcript::{ASK_USER_QUESTION, parse_questions, tool_summary};
use crate::hook::{AgentHookEvent, HookDecision, HookNotification, HookResponse, HookToolUse};
use crate::types::truncate;

/// The hook events VCA understands.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClaudeHookEventName {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PostToolUseFailure,
    Notification,
    Stop,
    StopFailure,
    SubagentStop,
    SessionEnd,
    Other(String),
}

impl ClaudeHookEventName {
    pub fn parse(name: &str) -> Self {
        match name {
            "SessionStart" => Self::SessionStart,
            "UserPromptSubmit" => Self::UserPromptSubmit,
            "PreToolUse" => Self::PreToolUse,
            "PermissionRequest" => Self::PermissionRequest,
            "PostToolUse" => Self::PostToolUse,
            "PostToolUseFailure" => Self::PostToolUseFailure,
            "Notification" => Self::Notification,
            "Stop" => Self::Stop,
            "StopFailure" => Self::StopFailure,
            "SubagentStop" => Self::SubagentStop,
            "SessionEnd" => Self::SessionEnd,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PermissionRequest => "PermissionRequest",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Notification => "Notification",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
            Self::SubagentStop => "SubagentStop",
            Self::SessionEnd => "SessionEnd",
            Self::Other(s) => s,
        }
    }

    /// Kebab-case form used as the CLI argument, for example `pre-tool-use`.
    pub fn cli_arg(&self) -> String {
        let mut out = String::new();
        for (i, c) in self.as_str().chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    out.push('-');
                }
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Inverse of [`Self::cli_arg`].
    pub fn from_cli_arg(arg: &str) -> Self {
        let camel: String = arg
            .split('-')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect();
        Self::parse(&camel)
    }
}

/// Raw hook input. All fields are optional so that payloads from older or
/// newer Claude Code versions still parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeHookPayload {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<PathBuf>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Notification text.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// `permission_prompt`, `idle_prompt`, `elicitation_dialog`, ...
    #[serde(default)]
    pub notification_type: Option<String>,
    /// UserPromptSubmit text.
    #[serde(default)]
    pub prompt: Option<String>,
    /// SessionStart source: `startup`, `resume`, `clear`, `compact`, `fork`.
    #[serde(default)]
    pub source: Option<String>,
    /// SessionEnd reason.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub stop_hook_active: Option<bool>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ClaudeHookPayload {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn event(&self) -> ClaudeHookEventName {
        ClaudeHookEventName::parse(&self.hook_event_name)
    }

    /// The shell command of a `Bash` (or `PowerShell`) tool call, for
    /// admission control. `None` for other tools.
    pub fn shell_command(&self) -> Option<&str> {
        match self.tool_name.as_deref()? {
            "Bash" | "PowerShell" => self.tool_input.as_ref()?.get("command")?.as_str(),
            _ => None,
        }
    }

    fn tool(&self) -> Option<HookToolUse> {
        let name = self.tool_name.clone()?;
        Some(HookToolUse {
            command: self.shell_command().map(str::to_owned),
            input: self.tool_input.clone().unwrap_or(Value::Null),
            name,
        })
    }

    /// Stable, opaque decision id. The agent's own `tool_use_id` wins;
    /// otherwise the id is a hash of session, tool and normalized input
    /// (`x-` prefix), or of the notification text (`n-` prefix) when the
    /// payload names no tool. Never time-based, never contains raw input.
    fn decision_id(&self) -> String {
        let session = self.session_id.as_deref().unwrap_or("unknown");
        if let Some(id) = &self.tool_use_id {
            return format!("claude:{session}:{id}");
        }
        match &self.tool_name {
            Some(tool) => {
                let input = self.tool_input.as_ref().map(canonical_json);
                let hash = stable_hash(&["claude", session, tool, input.as_deref().unwrap_or("")]);
                format!("claude:{session}:{DERIVED_TOOL_PREFIX}{hash}")
            }
            None => {
                let hash = stable_hash(&[
                    "claude",
                    session,
                    self.notification_type.as_deref().unwrap_or(""),
                    self.message.as_deref().unwrap_or(""),
                ]);
                format!("claude:{session}:{DERIVED_NOTICE_PREFIX}{hash}")
            }
        }
    }

    /// Translates the payload into VCA terms.
    pub fn to_agent_event(&self) -> AgentHookEvent {
        let event = self.event();
        let mut out = AgentHookEvent {
            agent: AgentKind::ClaudeCode,
            event_name: self.hook_event_name.clone(),
            agent_session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            transcript_path: self.transcript_path.clone(),
            state: None,
            notification: None,
            decision: None,
            tool: self.tool(),
            blocking: matches!(
                event,
                ClaudeHookEventName::PreToolUse
                    | ClaudeHookEventName::PermissionRequest
                    | ClaudeHookEventName::UserPromptSubmit
            ),
            subagent_id: None,
            tool_use_id: self.tool_use_id.clone(),
            permission_mode: self.permission_mode.clone(),
            last_message: None,
        };
        match event {
            ClaudeHookEventName::SessionStart => out.state = Some(SessionState::Idle),
            ClaudeHookEventName::UserPromptSubmit
            | ClaudeHookEventName::PreToolUse
            | ClaudeHookEventName::PostToolUse
            | ClaudeHookEventName::PostToolUseFailure => out.state = Some(SessionState::Running),
            ClaudeHookEventName::PermissionRequest => {
                out.state = Some(SessionState::AwaitingDecision);
                out.decision = Some(self.permission_decision());
            }
            ClaudeHookEventName::Notification => self.apply_notification(&mut out),
            ClaudeHookEventName::Stop => {
                out.state = Some(SessionState::Idle);
                out.last_message = self.last_assistant_message.clone();
                out.notification = Some(HookNotification {
                    title: "Task complete".into(),
                    body: self
                        .last_assistant_message
                        .as_deref()
                        .map(|m| truncate(m, 300))
                        .unwrap_or_default(),
                });
            }
            ClaudeHookEventName::StopFailure => {
                let message = self
                    .extra
                    .get("error")
                    .map(|e| {
                        e.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| e.to_string())
                    })
                    .or_else(|| self.message.clone())
                    .unwrap_or_else(|| "The turn ended with an error".into());
                out.notification = Some(HookNotification {
                    title: "Agent error".into(),
                    body: message.clone(),
                });
                out.state = Some(SessionState::Error { message });
            }
            ClaudeHookEventName::SessionEnd => {
                out.state = Some(SessionState::Exited { code: None })
            }
            ClaudeHookEventName::SubagentStop => {
                out.subagent_id = self
                    .extra
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            ClaudeHookEventName::Other(_) => {}
        }
        // A question form asks the person, like a permission prompt does.
        // `PreToolUse` runs before the agent's permission flow; in a mode
        // that settles calls without the person it raises nothing (a form
        // that still asks arrives as `PermissionRequest` or in the
        // transcript).
        let asks = match event {
            ClaudeHookEventName::PreToolUse => !self.auto_approves(),
            ClaudeHookEventName::PermissionRequest => true,
            _ => false,
        };
        if asks {
            if let Some(decision) = self.question_decision() {
                out.state = Some(SessionState::AwaitingDecision);
                out.decision = Some(decision);
            }
        }
        out
    }

    /// True when the payload's `permission_mode` lets the agent settle tool
    /// calls without asking the person: `auto` (a classifier decides) and
    /// `bypassPermissions`. Such a mode can still escalate a call to the
    /// person through a `PermissionRequest`.
    pub fn auto_approves(&self) -> bool {
        self.permission_mode
            .as_deref()
            .is_some_and(permission_mode_auto_approves)
    }

    /// The question form of an `AskUserQuestion` call. Its id is the one
    /// the transcript gives the same call, so both report one decision.
    fn question_decision(&self) -> Option<Decision> {
        if self.tool_name.as_deref() != Some(ASK_USER_QUESTION) {
            return None;
        }
        let questions = parse_questions(self.tool_input.as_ref()?)?;
        Some(Decision {
            id: self.decision_id(),
            prompt: question_prompt(&questions),
            options: Vec::new(),
            inferred: false,
            questions,
            command: None,
        })
    }

    fn apply_notification(&self, out: &mut AgentHookEvent) {
        let message = self.message.clone().unwrap_or_default();
        let kind = self.notification_type.as_deref().unwrap_or("");
        let lower = message.to_lowercase();
        let state = match kind {
            "permission_prompt" => Some(SessionState::AwaitingDecision),
            "idle_prompt"
            | "elicitation_dialog"
            | "elicitation_url_dialog"
            | "agent_needs_input" => Some(SessionState::AwaitingInput),
            // Payloads without a type (older versions): use the text.
            "" if lower.contains("permission") => Some(SessionState::AwaitingDecision),
            "" if lower.contains("waiting for your input")
                || lower.contains("waiting for input") =>
            {
                Some(SessionState::AwaitingInput)
            }
            _ => None,
        };
        if state == Some(SessionState::AwaitingDecision) {
            out.decision = Some(Decision {
                id: self.decision_id(),
                prompt: message.clone(),
                options: claude_permission_options(),
                inferred: false,
                questions: Vec::new(),
                command: None,
            });
        }
        out.state = state;
        out.notification = Some(HookNotification {
            title: self
                .title
                .clone()
                .unwrap_or_else(|| "Claude Code".to_owned()),
            body: message,
        });
    }

    fn permission_decision(&self) -> Decision {
        let tool = self.tool_name.as_deref().unwrap_or("a tool");
        let detail = self
            .tool_input
            .as_ref()
            .map(|input| tool_summary(tool, input))
            .filter(|s| !s.is_empty());
        let prompt = match detail {
            Some(d) => format!("Allow {tool}: {d}?"),
            None => format!("Allow {tool}?"),
        };
        Decision {
            id: self.decision_id(),
            prompt,
            options: claude_permission_options(),
            inferred: false,
            questions: Vec::new(),
            command: self.shell_command().map(str::to_owned),
        }
    }
}

/// Whether Claude Code's permission mode (`default`, `acceptEdits`,
/// `plan`, `auto`, `bypassPermissions`, ...) settles tool calls without
/// asking the person.
pub fn permission_mode_auto_approves(mode: &str) -> bool {
    matches!(mode, "auto" | "bypassPermissions")
}

const DERIVED_TOOL_PREFIX: &str = "x-";
const DERIVED_NOTICE_PREFIX: &str = "n-";

/// True for ids derived from content (the agent supplied no id), which
/// repeat for identical requests and so need answered-state separation.
pub fn is_derived_decision_id(id: &str) -> bool {
    id.rsplit(':').next().is_some_and(|last| {
        last.starts_with(DERIVED_TOOL_PREFIX) || last.starts_with(DERIVED_NOTICE_PREFIX)
    })
}

/// True for derived ids built from a payload that named no tool (a bare
/// permission notification); such a prompt duplicates an already pending
/// tool decision rather than being a new one.
pub fn is_notice_decision_id(id: &str) -> bool {
    id.rsplit(':')
        .next()
        .is_some_and(|last| last.starts_with(DERIVED_NOTICE_PREFIX))
}

/// JSON with object keys sorted, so key order never changes the hash.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// Deterministic 128-bit FNV-1a style hash (two lanes) as 32 hex chars.
/// Stable across processes and Rust versions, unlike `DefaultHasher`.
pub fn stable_hash(parts: &[&str]) -> String {
    let mut lanes = [0xcbf2_9ce4_8422_2325_u64, 0x8422_2325_cbf2_9ce4_u64];
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0x1f)) {
            for (i, lane) in lanes.iter_mut().enumerate() {
                *lane ^= u64::from(byte) ^ (i as u64);
                *lane = lane.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    format!("{:016x}{:016x}", lanes[0], lanes[1])
}

/// The choices of Claude Code's permission dialog. Option ids are the keys
/// to type into the terminal (the dialog's numbered entries).
pub fn claude_permission_options() -> Vec<DecisionOption> {
    [("1", "Yes"), ("2", "Yes, and don't ask again"), ("3", "No")]
        .into_iter()
        .map(|(id, label)| DecisionOption {
            id: id.into(),
            label: label.into(),
        })
        .collect()
}

/// Renders a [`HookDecision`] as Claude Code hook output for `event`.
///
/// * `PreToolUse`: JSON `hookSpecificOutput.permissionDecision`
///   (`allow` / `deny` / `ask`), exit code 0.
/// * `PermissionRequest`: JSON `hookSpecificOutput.decision.behavior`
///   (`allow` / `deny`); `ask` leaves the normal dialog in place.
/// * `UserPromptSubmit`: `{"decision":"block"}` for deny.
/// * Everything else, and [`HookDecision::NoOpinion`]: no output, exit 0.
pub fn render_response(event: &ClaudeHookEventName, decision: &HookDecision) -> HookResponse {
    let json_out = |value: Value| HookResponse {
        stdout: value.to_string(),
        stderr: String::new(),
        exit_code: 0,
    };
    match (event, decision) {
        (_, HookDecision::NoOpinion) => HookResponse::ok(),
        (ClaudeHookEventName::PreToolUse, d) => {
            let (kind, reason) = match d {
                HookDecision::Allow { reason } => ("allow", reason.clone()),
                HookDecision::Deny { reason } => ("deny", Some(reason.clone())),
                HookDecision::Ask { reason } => ("ask", reason.clone()),
                HookDecision::NoOpinion => unreachable!("handled above"),
            };
            let mut specific = json!({
                "hookEventName": "PreToolUse",
                "permissionDecision": kind,
            });
            if let Some(reason) = reason {
                specific["permissionDecisionReason"] = Value::String(reason);
            }
            json_out(json!({ "hookSpecificOutput": specific }))
        }
        (ClaudeHookEventName::PermissionRequest, HookDecision::Allow { .. }) => json_out(json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "allow" }
            }
        })),
        (ClaudeHookEventName::PermissionRequest, HookDecision::Deny { reason }) => {
            json_out(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "deny", "message": reason }
                }
            }))
        }
        (ClaudeHookEventName::UserPromptSubmit, HookDecision::Deny { reason }) => {
            json_out(json!({ "decision": "block", "reason": reason }))
        }
        _ => HookResponse::ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRE_TOOL_USE: &str = r#"{
      "session_id": "abc123",
      "prompt_id": "550e8400-e29b-41d4-a716-446655440000",
      "transcript_path": "/home/user/.claude/projects/-home-user-my-project/abc123.jsonl",
      "cwd": "/home/user/my-project",
      "permission_mode": "default",
      "hook_event_name": "PreToolUse",
      "tool_name": "Bash",
      "tool_input": {"command": "npm test", "description": "Run test suite", "timeout": 120000},
      "tool_use_id": "toolu_01ABC123"
    }"#;

    #[test]
    fn parses_pre_tool_use_and_extracts_command() {
        let p = ClaudeHookPayload::parse(PRE_TOOL_USE).unwrap();
        assert_eq!(p.event(), ClaudeHookEventName::PreToolUse);
        assert_eq!(p.shell_command(), Some("npm test"));
        assert!(p.extra.contains_key("prompt_id"));
        let ev = p.to_agent_event();
        assert_eq!(ev.state, Some(SessionState::Running));
        assert!(ev.blocking);
        assert_eq!(ev.tool.unwrap().command.as_deref(), Some("npm test"));
    }

    fn perm(input: &str, extra: &str) -> Decision {
        ClaudeHookPayload::parse(&format!(
            r#"{{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{input}{extra}}}"#
        ))
        .unwrap()
        .to_agent_event()
        .decision
        .unwrap()
    }

    #[test]
    fn decision_ids_are_stable_opaque_and_prefer_agent_id() {
        let a = perm(r#"{"command":"rm -rf x","description":"d"}"#, "");
        let b = perm(r#"{"description":"d","command":"rm -rf x"}"#, "");
        assert_eq!(a.id, b.id, "key order and repeats do not change the id");
        assert!(!a.id.contains("rm"));
        assert!(is_derived_decision_id(&a.id));
        assert_ne!(a.id, perm(r#"{"command":"ls"}"#, "").id);
        let own = perm(r#"{"command":"ls"}"#, r#","tool_use_id":"toolu_9""#);
        assert_eq!(own.id, "claude:s1:toolu_9");
        assert!(!is_derived_decision_id(&own.id));
        let n = ClaudeHookPayload::parse(
            r#"{"session_id":"s1","hook_event_name":"Notification","message":"needs permission","notification_type":"permission_prompt"}"#,
        )
        .unwrap()
        .to_agent_event()
        .decision
        .unwrap();
        assert!(is_notice_decision_id(&n.id));
    }

    #[test]
    fn notification_types_map_to_states() {
        let perm = ClaudeHookPayload::parse(
            r#"{"session_id":"s","hook_event_name":"Notification","message":"Claude needs your permission to use Bash","title":"Permission needed","notification_type":"permission_prompt"}"#,
        )
        .unwrap()
        .to_agent_event();
        assert_eq!(perm.state, Some(SessionState::AwaitingDecision));
        assert_eq!(perm.decision.unwrap().options.len(), 3);
        assert_eq!(perm.notification.unwrap().title, "Permission needed");

        let idle = ClaudeHookPayload::parse(
            r#"{"session_id":"s","hook_event_name":"Notification","message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#,
        )
        .unwrap()
        .to_agent_event();
        assert_eq!(idle.state, Some(SessionState::AwaitingInput));

        let legacy = ClaudeHookPayload::parse(
            r#"{"session_id":"s","hook_event_name":"Notification","message":"Claude is waiting for your input"}"#,
        )
        .unwrap()
        .to_agent_event();
        assert_eq!(legacy.state, Some(SessionState::AwaitingInput));

        let auth = ClaudeHookPayload::parse(
            r#"{"hook_event_name":"Notification","message":"Signed in","notification_type":"auth_success"}"#,
        )
        .unwrap()
        .to_agent_event();
        assert_eq!(auth.state, None);
        assert!(auth.notification.is_some());
    }

    #[test]
    fn stop_and_session_end() {
        let stop = ClaudeHookPayload::parse(
            r#"{"session_id":"s","hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"All tests pass.","background_tasks":[]}"#,
        )
        .unwrap()
        .to_agent_event();
        assert_eq!(stop.state, Some(SessionState::Idle));
        assert_eq!(stop.notification.unwrap().body, "All tests pass.");

        let end = ClaudeHookPayload::parse(r#"{"hook_event_name":"SessionEnd","reason":"other"}"#)
            .unwrap()
            .to_agent_event();
        assert_eq!(end.state, Some(SessionState::Exited { code: None }));
    }

    #[test]
    fn subagent_stop_names_the_agent() {
        let ev = ClaudeHookPayload::parse(
            r#"{"session_id":"s","hook_event_name":"SubagentStop","agent_id":"a54a","agent_type":"general-purpose","stop_hook_active":false}"#,
        )
        .unwrap()
        .to_agent_event();
        assert_eq!(ev.subagent_id.as_deref(), Some("a54a"));
        assert_eq!(ev.state, None);
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""subagent_id":"a54a""#));
        // Other events, and events from older `vca hook` builds, name none.
        let stop = ClaudeHookPayload::parse(r#"{"hook_event_name":"Stop"}"#)
            .unwrap()
            .to_agent_event();
        assert_eq!(stop.subagent_id, None);
        let mut old = serde_json::to_value(&stop).unwrap();
        old.as_object_mut().unwrap().remove("subagent_id");
        let back: AgentHookEvent = serde_json::from_value(old).unwrap();
        assert_eq!(back.subagent_id, None);
    }

    #[test]
    fn permission_request_becomes_decision() {
        let ev = ClaudeHookPayload::parse(
            r#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf node_modules"},"permission_suggestions":[]}"#,
        )
        .unwrap()
        .to_agent_event();
        assert_eq!(ev.state, Some(SessionState::AwaitingDecision));
        assert_eq!(
            ev.decision.unwrap().prompt,
            "Allow Bash: rm -rf node_modules?"
        );
    }

    #[test]
    fn renders_pre_tool_use_replies() {
        let ev = ClaudeHookEventName::PreToolUse;
        let deny = render_response(
            &ev,
            &HookDecision::Deny {
                reason: "Too many builds running".into(),
            },
        );
        assert_eq!(deny.exit_code, 0);
        let v: Value = serde_json::from_str(&deny.stdout).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "Too many builds running"
        );
        let allow = render_response(&ev, &HookDecision::Allow { reason: None });
        let v: Value = serde_json::from_str(&allow.stdout).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(
            v["hookSpecificOutput"]
                .get("permissionDecisionReason")
                .is_none()
        );
        assert_eq!(
            render_response(&ev, &HookDecision::NoOpinion),
            HookResponse::ok()
        );
    }

    #[test]
    fn renders_permission_request_replies() {
        let ev = ClaudeHookEventName::PermissionRequest;
        let v: Value = serde_json::from_str(
            &render_response(
                &ev,
                &HookDecision::Deny {
                    reason: "no".into(),
                },
            )
            .stdout,
        )
        .unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            render_response(&ev, &HookDecision::Ask { reason: None }),
            HookResponse::ok()
        );
    }

    #[test]
    fn cli_arg_round_trip() {
        for name in [
            "PreToolUse",
            "Stop",
            "UserPromptSubmit",
            "SessionEnd",
            "PermissionRequest",
        ] {
            let ev = ClaudeHookEventName::parse(name);
            assert_eq!(ClaudeHookEventName::from_cli_arg(&ev.cli_arg()), ev);
        }
        assert_eq!(ClaudeHookEventName::PreToolUse.cli_arg(), "pre-tool-use");
    }

    #[test]
    fn ask_user_question_hooks_are_question_decisions() {
        let input = r#"{"questions":[{"question":"Ship it?","header":"Ship","options":[{"label":"Yes","description":"now"},{"label":"No","description":""}],"multiSelect":false}]}"#;
        for event in ["PreToolUse", "PermissionRequest"] {
            let json = format!(
                r#"{{"session_id":"s1","hook_event_name":"{event}","tool_name":"AskUserQuestion","tool_use_id":"toolu_q","tool_input":{input}}}"#
            );
            let out = ClaudeHookPayload::parse(&json).unwrap().to_agent_event();
            assert_eq!(out.state, Some(SessionState::AwaitingDecision), "{event}");
            let d = out.decision.expect("a decision");
            assert_eq!(d.id, "claude:s1:toolu_q");
            assert_eq!(d.prompt, "[Ship] Ship it?");
            assert!(d.options.is_empty());
            assert!(d.is_question());
            assert_eq!(d.questions[0].options[0].description, "now");
        }
        // After the answer the agent runs again.
        let post = format!(
            r#"{{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"AskUserQuestion","tool_use_id":"toolu_q","tool_input":{input}}}"#
        );
        let out = ClaudeHookPayload::parse(&post).unwrap().to_agent_event();
        assert_eq!(out.state, Some(SessionState::Running));
        assert!(out.decision.is_none());
    }

    #[test]
    fn events_carry_the_tool_use_id() {
        let p = ClaudeHookPayload::parse(PRE_TOOL_USE).unwrap();
        assert_eq!(
            p.to_agent_event().tool_use_id.as_deref(),
            Some("toolu_01ABC123")
        );
        let ev = ClaudeHookPayload::parse(r#"{"hook_event_name":"Stop"}"#)
            .unwrap()
            .to_agent_event();
        assert_eq!(ev.tool_use_id, None);
        // Left out when absent; events from older builds parse.
        let json = serde_json::to_value(&ev).unwrap();
        assert!(json.get("tool_use_id").is_none());
        let back: AgentHookEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back.tool_use_id, None);
    }

    #[test]
    fn pre_tool_use_never_asks_in_modes_that_approve_on_their_own() {
        let input = r#"{"questions":[{"question":"Ship it?","header":"Ship","options":[{"label":"Yes","description":""}],"multiSelect":false}]}"#;
        let event = |name: &str, mode: &str, tool: &str| {
            ClaudeHookPayload::parse(&format!(
                r#"{{"session_id":"s1","hook_event_name":"{name}","permission_mode":"{mode}","tool_name":"{tool}","tool_use_id":"toolu_m","tool_input":{input}}}"#
            ))
            .unwrap()
            .to_agent_event()
        };
        for mode in ["auto", "bypassPermissions"] {
            assert!(permission_mode_auto_approves(mode));
            let pre = event("PreToolUse", mode, "AskUserQuestion");
            assert!(pre.decision.is_none(), "{mode}");
            assert_eq!(pre.state, Some(SessionState::Running), "{mode}");
            // Such a mode can still hand a call to the person.
            let perm = event("PermissionRequest", mode, "Bash");
            assert_eq!(perm.state, Some(SessionState::AwaitingDecision), "{mode}");
            assert!(perm.decision.is_some(), "{mode}");
        }
        for mode in ["default", "acceptEdits", "plan"] {
            assert!(!permission_mode_auto_approves(mode));
            let pre = event("PreToolUse", mode, "AskUserQuestion");
            assert!(pre.decision.is_some_and(|d| d.is_question()), "{mode}");
        }
        // A Bash PreToolUse is never a decision, in any mode.
        for mode in ["default", "auto", "plan"] {
            let pre = event("PreToolUse", mode, "Bash");
            assert!(pre.decision.is_none(), "{mode}");
            assert_eq!(pre.state, Some(SessionState::Running), "{mode}");
        }
    }
}
