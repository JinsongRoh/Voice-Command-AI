//! How many sub-agents a session is running, read from its transcript.
//!
//! Claude Code (the main transcript; sub-agent conversations are
//! sidechains and never reach it):
//!
//! * a sub-agent starts with a `Task` or `Agent` tool call;
//! * a foreground sub-agent ends when the call's result arrives;
//! * a background sub-agent's result arrives at once ("Async agent launched
//!   successfully", with `agentId: <id>`); it ends with a
//!   `<task-notification>` naming the agent (`<task-id>` is the agent id,
//!   `<tool-use-id>` the launching call) with a final status. "Agent … is
//!   waiting on its own background work" is not final;
//! * `SendMessage` to a finished agent resumes it in the background (its
//!   result carries `"resumedAgentId"`); it ends the same way;
//! * a `SubagentStop` hook can end a background agent early (see
//!   [`SubagentTracker::hook_stopped`]).
//!
//! Codex: [`HistoryItem::Subagent`] items from `SubAgentActivity` events
//! and final answers (see [`crate::codex::rollout`]).
//!
//! A sub-agent whose end never shows up (a lost notification, a crash)
//! expires after [`SUBAGENT_EXPIRY_MS`], counted from the transcript
//! timestamp of its start, so replayed old history does not count either.

use vca_core::UnixMillis;

use crate::claude::transcript::is_subagent_tool;
use crate::types::HistoryItem;

/// How long a sub-agent counts at most without an end.
pub const SUBAGENT_EXPIRY_MS: UnixMillis = 6 * 60 * 60 * 1000;

/// Most sub-agents tracked per session; the oldest are dropped.
const MAX_TRACKED: usize = 64;

/// Background agents remembered after a `SubagentStop` hook ended them.
const MAX_HOOK_ENDED: usize = 32;

/// One sub-agent that has not ended yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    /// The launching tool call (Claude Code).
    call_id: Option<String>,
    /// The agent's own id, once known (background agents, Codex).
    agent_id: Option<String>,
    background: bool,
    started: UnixMillis,
}

impl Running {
    fn is(&self, task_id: Option<&str>, call_id: Option<&str>) -> bool {
        (task_id.is_some() && self.agent_id.as_deref() == task_id)
            || (call_id.is_some() && self.call_id.as_deref() == call_id)
    }
}

/// Sub-agents of one session that are still working.
#[derive(Debug, Default, Clone)]
pub struct SubagentTracker {
    running: Vec<Running>,
    /// Background agents a `SubagentStop` hook ended. A later non-final
    /// notification for one of them (it waits on its own background work)
    /// brings it back.
    hook_ended: Vec<Running>,
}

impl SubagentTracker {
    /// Number of sub-agents working now.
    pub fn count(&self) -> u32 {
        u32::try_from(self.running.len()).unwrap_or(u32::MAX)
    }

    /// Applies one transcript item; `now` is the current time. Expired
    /// sub-agents are dropped as well.
    pub fn observe(&mut self, item: &HistoryItem, now: UnixMillis) {
        let started = item.at().unwrap_or(now);
        match item {
            HistoryItem::ToolCall {
                id: Some(id), name, ..
            } if is_subagent_tool(name) => {
                if !self.running.iter().any(|r| r.is(None, Some(id))) {
                    self.push(Running {
                        call_id: Some(id.clone()),
                        agent_id: None,
                        background: false,
                        started,
                    });
                }
            }
            HistoryItem::ToolResult {
                id: Some(id), text, ..
            } => self.tool_result(id, text, started),
            HistoryItem::Message { text, .. } => {
                if let Some(notice) = TaskNotice::parse(text) {
                    self.notice(&notice);
                }
            }
            HistoryItem::Subagent {
                id: Some(id),
                running: true,
                ..
            } => {
                if !self.running.iter().any(|r| r.is(Some(id), None)) {
                    self.push(Running {
                        call_id: None,
                        agent_id: Some(id.clone()),
                        background: true,
                        started,
                    });
                }
            }
            HistoryItem::Subagent {
                id: Some(id),
                running: false,
                ..
            } => self.running.retain(|r| !r.is(Some(id), None)),
            HistoryItem::Subagent {
                id: None,
                running: false,
                ..
            } => self.running.clear(),
            HistoryItem::Rewritten => self.reset(),
            _ => {}
        }
        self.expire(now);
    }

    /// A `SubagentStop` hook reported that the agent `agent_id` stopped.
    /// Only background agents are matched (foreground ones end with their
    /// tool result, and their id is not known before).
    pub fn hook_stopped(&mut self, agent_id: &str) {
        let Some(pos) = self
            .running
            .iter()
            .position(|r| r.background && r.agent_id.as_deref() == Some(agent_id))
        else {
            return;
        };
        let ended = self.running.remove(pos);
        if self.hook_ended.len() >= MAX_HOOK_ENDED {
            self.hook_ended.remove(0);
        }
        self.hook_ended.push(ended);
    }

    /// Drops sub-agents that started more than [`SUBAGENT_EXPIRY_MS`] ago.
    pub fn expire(&mut self, now: UnixMillis) {
        let alive = |r: &Running| now.saturating_sub(r.started) < SUBAGENT_EXPIRY_MS;
        self.running.retain(alive);
        self.hook_ended.retain(alive);
    }

    /// Forgets everything (the session ended or its transcript was
    /// rewritten).
    pub fn reset(&mut self) {
        self.running.clear();
        self.hook_ended.clear();
    }

    fn push(&mut self, running: Running) {
        if self.running.len() >= MAX_TRACKED {
            self.running.remove(0);
        }
        self.running.push(running);
    }

    fn tool_result(&mut self, call_id: &str, text: &str, at: UnixMillis) {
        if let Some(agent) = resumed_agent_id(text) {
            // `SendMessage` woke a finished agent; it runs in the background.
            if !self.running.iter().any(|r| r.is(Some(&agent), None)) {
                self.hook_ended.retain(|r| !r.is(Some(&agent), None));
                self.push(Running {
                    call_id: None,
                    agent_id: Some(agent),
                    background: true,
                    started: at,
                });
            }
            return;
        }
        let Some(pos) = self
            .running
            .iter()
            .position(|r| !r.background && r.is(None, Some(call_id)))
        else {
            return;
        };
        if text.trim_start().starts_with(ASYNC_LAUNCH) {
            let r = &mut self.running[pos];
            r.background = true;
            r.agent_id = launched_agent_id(text);
        } else {
            self.running.remove(pos);
        }
    }

    fn notice(&mut self, notice: &TaskNotice<'_>) {
        let (task, call) = (notice.task_id, notice.tool_use_id);
        if notice.is_final() {
            self.running.retain(|r| !(r.background && r.is(task, call)));
            self.hook_ended.retain(|r| !r.is(task, call));
        } else if let Some(pos) = self.hook_ended.iter().position(|r| r.is(task, call)) {
            // Stopped per the hook, yet still waiting on its own work.
            let back = self.hook_ended.remove(pos);
            if !self.running.iter().any(|r| r == &back) {
                self.push(back);
            }
        }
    }
}

/// Opening of the result of a background sub-agent launch.
const ASYNC_LAUNCH: &str = "Async agent launched";

/// `agentId: a54a…` in an async launch result.
fn launched_agent_id(text: &str) -> Option<String> {
    let rest = &text[text.find("agentId:")? + "agentId:".len()..];
    let id: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!id.is_empty()).then_some(id)
}

/// `"resumedAgentId":"a54a…"` in a `SendMessage` result.
fn resumed_agent_id(text: &str) -> Option<String> {
    const KEY: &str = "\"resumedAgentId\"";
    let rest = text[text.find(KEY)? + KEY.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let id = &rest[..rest.find('"')?];
    (!id.is_empty()).then(|| id.to_owned())
}

/// The fields of a Claude Code `<task-notification>` that matter here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskNotice<'a> {
    task_id: Option<&'a str>,
    tool_use_id: Option<&'a str>,
    status: Option<&'a str>,
    summary: &'a str,
}

impl<'a> TaskNotice<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let body = &text[text.find("<task-notification")?..];
        Some(Self {
            task_id: tag_text(body, "task-id"),
            tool_use_id: tag_text(body, "tool-use-id"),
            status: tag_text(body, "status"),
            summary: tag_text(body, "summary").unwrap_or(""),
        })
    }

    /// Whether the notice ends the agent. "Waiting on its own background
    /// work" comes with status `completed` but is not an end.
    fn is_final(&self) -> bool {
        let summary = self.summary.to_ascii_lowercase();
        if summary.contains("waiting on") {
            return false;
        }
        match self.status.map(str::to_ascii_lowercase).as_deref() {
            Some(
                "completed" | "success" | "succeeded" | "done" | "finished" | "failed" | "error"
                | "errored" | "killed" | "stopped" | "cancelled" | "canceled",
            ) => true,
            Some(_) => false,
            None => ["finished", "completed", "failed", "stopped", "was killed"]
                .iter()
                .any(|w| summary.contains(w)),
        }
    }
}

/// Trimmed inner text of the first `<name>…</name>` element.
fn tag_text<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let rest = &text[start..];
    let end = rest.find(&format!("</{name}>")).unwrap_or(rest.len());
    Some(rest[..end].trim()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::transcript::parse_line;

    const T0: UnixMillis = 1_793_527_200_000; // 2026-11-01T10:00:00Z
    const MIN: UnixMillis = 60_000;

    // Shapes below are trimmed copies of real Claude Code 2.1 records.
    fn agent_call(id: &str, description: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"{id}","name":"Agent","input":{{"description":"{description}","prompt":"..."}}}}]}},"timestamp":"2026-11-01T10:00:00.000Z"}}"#
        )
    }

    fn result(id: &str, text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"tool_use_id": id, "type": "tool_result", "content": [{"type": "text", "text": text}]}
            ]},
            "toolUseResult": {},
            "timestamp": "2026-11-01T10:01:00.000Z",
        })
        .to_string()
    }

    fn launched(id: &str, agent: &str) -> String {
        result(
            id,
            &format!(
                "Async agent launched successfully. (This tool result is internal metadata.)\nagentId: {agent} (internal ID - do not mention to user. Use SendMessage with to: '{agent}' to continue this agent.)\nThe agent is working in the background."
            ),
        )
    }

    fn notice(agent: &str, call: &str, status: &str, summary: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": format!(
                "<task-notification>\n<task-id>{agent}</task-id>\n<tool-use-id>{call}</tool-use-id>\n<output-file>C:\\tmp\\{agent}.output</output-file>\n<status>{status}</status>\n<summary>{summary}</summary>\n</task-notification>"
            )},
            "origin": {"kind": "task-notification"},
            "timestamp": "2026-11-01T10:30:00.000Z",
        })
        .to_string()
    }

    fn feed(t: &mut SubagentTracker, line: &str, now: UnixMillis) -> u32 {
        for item in parse_line(line) {
            t.observe(&item, now);
        }
        t.count()
    }

    #[test]
    fn foreground_agent_counts_until_its_result() {
        let mut t = SubagentTracker::default();
        assert_eq!(feed(&mut t, &agent_call("toolu_1", "Review"), T0), 1);
        // Other tools do not count.
        let bash = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(feed(&mut t, bash, T0), 1);
        assert_eq!(feed(&mut t, &result("toolu_2", "ok"), T0), 1);
        assert_eq!(feed(&mut t, &result("toolu_1", "All good."), T0 + MIN), 0);
        // The legacy tool name counts too; an error result ends it as well.
        let task = agent_call("toolu_3", "Old").replace("\"Agent\"", "\"Task\"");
        assert_eq!(feed(&mut t, &task, T0), 1);
        assert_eq!(
            feed(
                &mut t,
                &result("toolu_3", "[Request interrupted by user]"),
                T0
            ),
            0
        );
    }

    #[test]
    fn background_agent_counts_until_a_final_notification() {
        let mut t = SubagentTracker::default();
        assert_eq!(feed(&mut t, &agent_call("toolu_b", "Build"), T0), 1);
        assert_eq!(
            feed(&mut t, &launched("toolu_b", "aa3dcd3de4ec12040"), T0),
            1
        );
        // The main session moves on while the agent works.
        let prompt = r#"{"type":"user","message":{"role":"user","content":"next"},"origin":{"kind":"human"}}"#;
        assert_eq!(feed(&mut t, prompt, T0), 1);
        let waiting = notice(
            "aa3dcd3de4ec12040",
            "toolu_b",
            "completed",
            "Agent \"Build\" is waiting on its own background work",
        );
        assert_eq!(feed(&mut t, &waiting, T0 + MIN), 1, "waiting is not final");
        // Notifications of background shell commands do not end agents.
        let shell = notice(
            "bkcx5bhcq",
            "toolu_x",
            "completed",
            "Background command \"Wait\" completed (exit code 0)",
        );
        assert_eq!(feed(&mut t, &shell, T0 + MIN), 1);
        let done = notice(
            "aa3dcd3de4ec12040",
            "toolu_b",
            "completed",
            "Agent \"Build\" finished",
        );
        assert_eq!(feed(&mut t, &done, T0 + 2 * MIN), 0);
    }

    #[test]
    fn parallel_agents_count_separately_and_stopped_ones_end() {
        let mut t = SubagentTracker::default();
        feed(&mut t, &agent_call("toolu_a", "One"), T0);
        feed(&mut t, &agent_call("toolu_b", "Two"), T0);
        assert_eq!(t.count(), 2);
        feed(&mut t, &launched("toolu_a", "a111"), T0);
        assert_eq!(feed(&mut t, &launched("toolu_b", "a222"), T0), 2);
        // Matched by the task id alone.
        let stopped = notice("a111", "", "killed", "Agent \"One\" was stopped by Claude");
        assert_eq!(feed(&mut t, &stopped, T0), 1);
        // Matched by the tool use id alone.
        let failed = notice("", "toolu_b", "failed", "Agent \"Two\" failed");
        assert_eq!(feed(&mut t, &failed, T0), 0);
    }

    #[test]
    fn a_resumed_agent_counts_until_its_notification() {
        let mut t = SubagentTracker::default();
        let send = result(
            "toolu_s",
            r#"{"success":true,"message":"Resuming agent abb781e","resumedAgentId":"abb781e215dc265c4"}"#,
        );
        assert_eq!(feed(&mut t, &send, T0), 1);
        assert_eq!(feed(&mut t, &send, T0), 1, "counted once");
        let done = notice("abb781e215dc265c4", "toolu_s", "stopped", "Task stopped");
        assert_eq!(feed(&mut t, &done, T0), 0);
    }

    #[test]
    fn agents_without_an_end_expire() {
        let mut t = SubagentTracker::default();
        feed(&mut t, &agent_call("toolu_b", "Lost"), T0);
        feed(&mut t, &launched("toolu_b", "a333"), T0);
        t.expire(T0 + SUBAGENT_EXPIRY_MS - MIN);
        assert_eq!(t.count(), 1, "a long agent still counts");
        t.expire(T0 + SUBAGENT_EXPIRY_MS + MIN);
        assert_eq!(t.count(), 0);
        // Replayed old history never counts.
        assert_eq!(
            feed(&mut t, &agent_call("toolu_c", "Old"), T0 + 7 * 60 * MIN),
            0
        );
    }

    #[test]
    fn a_rewrite_or_reset_clears_the_count() {
        let mut t = SubagentTracker::default();
        feed(&mut t, &agent_call("toolu_a", "One"), T0);
        feed(&mut t, &agent_call("toolu_b", "Two"), T0);
        t.observe(&HistoryItem::Rewritten, T0);
        assert_eq!(t.count(), 0);
        feed(&mut t, &agent_call("toolu_c", "Three"), T0);
        t.reset();
        assert_eq!(t.count(), 0);
    }

    #[test]
    fn subagent_stop_hook_ends_a_background_agent() {
        let mut t = SubagentTracker::default();
        feed(&mut t, &agent_call("toolu_b", "Build"), T0);
        feed(&mut t, &launched("toolu_b", "a444"), T0);
        t.hook_stopped("unknown");
        assert_eq!(t.count(), 1);
        t.hook_stopped("a444");
        assert_eq!(t.count(), 0);
        // It turned out to wait on its own background work.
        let waiting = notice(
            "a444",
            "toolu_b",
            "completed",
            "Agent is waiting on its own background work",
        );
        assert_eq!(feed(&mut t, &waiting, T0), 1);
        let done = notice("a444", "toolu_b", "completed", "Agent finished");
        assert_eq!(feed(&mut t, &done, T0), 0);
        // A foreground agent is never ended by the hook.
        feed(&mut t, &agent_call("toolu_f", "Fg"), T0);
        t.hook_stopped("toolu_f");
        assert_eq!(t.count(), 1);
    }

    #[test]
    fn codex_items() {
        let mut t = SubagentTracker::default();
        let item = |id: Option<&str>, running| HistoryItem::Subagent {
            at: Some(T0),
            id: id.map(str::to_owned),
            running,
        };
        t.observe(&item(Some("/root/a"), true), T0);
        t.observe(&item(Some("/root/b"), true), T0);
        t.observe(&item(Some("/root/a"), true), T0);
        assert_eq!(t.count(), 2);
        t.observe(&item(Some("/root/a"), false), T0);
        assert_eq!(t.count(), 1);
        t.observe(&item(None, false), T0);
        assert_eq!(t.count(), 0);
    }

    #[test]
    fn parses_ids_and_notices() {
        assert_eq!(
            launched_agent_id("Async agent launched.\nagentId: a7a5 (internal)").as_deref(),
            Some("a7a5")
        );
        assert_eq!(launched_agent_id("Async agent launched."), None);
        assert_eq!(
            resumed_agent_id(r#"{"resumedAgentId": "x1"}"#).as_deref(),
            Some("x1")
        );
        let n = TaskNotice::parse(
            "<task-notification><task-id>a</task-id><status>running</status></task-notification>",
        )
        .unwrap();
        assert_eq!(n.task_id, Some("a"));
        assert_eq!(n.tool_use_id, None);
        assert!(!n.is_final());
        let n = TaskNotice::parse(
            "<task-notification><summary>Agent \"x\" finished</summary></task-notification>",
        )
        .unwrap();
        assert!(n.is_final());
        assert!(TaskNotice::parse("hello").is_none());
    }
}
