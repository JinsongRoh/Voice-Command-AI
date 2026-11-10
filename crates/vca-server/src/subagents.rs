//! The sub-agent count on session cards ([`vca_core::SessionInfo::subagents`]).
//!
//! The transcript tailer of observed and managed sessions feeds every
//! history item to the session's [`SubagentTracker`] (the rules live in
//! [`vca_adapters::subagents`]); a `SubagentStop` hook can end a background
//! agent early. The card is announced only when the count changes. The
//! count is cleared when the session exits ([`SessionManager::set_state`])
//! and when the transcript is rewritten, and agents whose end never shows
//! up expire after [`vca_adapters::subagents::SUBAGENT_EXPIRY_MS`] (checked
//! every [`EXPIRY_CHECK`]).

use std::time::Duration;

use vca_adapters::SubagentTracker;
use vca_core::SessionState;

use crate::sessions::{Session, SessionManager};
use crate::util::lock;

/// How often the tailer drops sub-agents that ran out of time.
pub(crate) const EXPIRY_CHECK: Duration = Duration::from_secs(60);

impl SessionManager {
    /// Applies `f` to the session's sub-agents and announces the card if
    /// the count changed. An exited session counts none.
    pub(crate) fn update_subagents(&self, session: &Session, f: impl FnOnce(&mut SubagentTracker)) {
        let count = {
            let mut tracker = lock(&session.subagents);
            f(&mut tracker);
            tracker.count()
        };
        self.update_info(session, |info| {
            let count = if matches!(info.state, SessionState::Exited { .. }) {
                0
            } else {
                count
            };
            let changed = info.subagents != count;
            info.subagents = count;
            changed
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use tokio::sync::broadcast::error::TryRecvError;
    use vca_adapters::{AgentHookEvent, HistoryItem, SessionLocator};
    use vca_broker::contain::Scope;
    use vca_core::{AgentKind, ControlTier, SessionId, SessionInfo, SessionOrigin, now_millis};

    use super::*;
    use crate::events::LogLimits;
    use crate::sessions::{ManagerConfig, Observed, ObservedIdentity, Update};
    use crate::workspace::WorkspaceRoots;

    fn manager(names: &Path) -> SessionManager {
        SessionManager::new(
            ManagerConfig {
                limits: LogLimits::default(),
                exited_retention: Duration::from_secs(60),
                pause_scope: Scope::ChildrenOnly,
                heuristics_interval: Duration::from_secs(1),
                control_endpoint: PathBuf::from("unused"),
                attach_dir: None,
                workspace_roots: WorkspaceRoots::new(Vec::new()),
                session_names: Some(names.to_owned()),
                allow_generic_launch: false,
            },
            None,
        )
    }

    fn observe(
        mgr: &SessionManager,
        agent: AgentKind,
        transcript: Option<PathBuf>,
    ) -> Arc<Session> {
        let info = SessionInfo {
            id: SessionId::new(),
            origin: SessionOrigin::Local {
                agent,
                project_dir: PathBuf::from("/code/shop"),
                pid: Some(4_242_424),
            },
            display_name: "shop".into(),
            state: SessionState::Running,
            control: ControlTier::TerminalApi,
            profile: None,
            created_at: now_millis(),
            last_activity: now_millis(),
            unread: 0,
            preview: None,
            auto_name: None,
            custom_name: false,
            shell: None,
            pending_decision: None,
            subagents: 0,
            conductor: false,
            terminal_prompt: None,
            question: None,
            progress: None,
            problem: None,
        };
        let observed = Observed {
            terminal: None,
            identity: ObservedIdentity {
                transcript_path: transcript,
                process_start: None,
            },
        };
        mgr.insert_observed(info, observed, Some("abc".into()))
    }

    // Claude Code 2.1 shapes without timestamps: they start now, so they
    // never count as expired history.
    fn agent_call(id: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"{id}","name":"Agent","input":{{"description":"Review","prompt":"..."}}}}]}}}}"#
        )
    }

    fn result(id: &str, text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"tool_use_id": id, "type": "tool_result", "content": [{"type": "text", "text": text}]}
            ]},
            "toolUseResult": {},
        })
        .to_string()
    }

    fn launched(id: &str, agent: &str) -> String {
        result(
            id,
            &format!(
                "Async agent launched successfully.\nagentId: {agent} (internal ID)\nThe agent is working in the background."
            ),
        )
    }

    fn notice(agent: &str, status: &str, summary: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": format!(
                "<task-notification>\n<task-id>{agent}</task-id>\n<status>{status}</status>\n<summary>{summary}</summary>\n</task-notification>"
            )},
            "origin": {"kind": "task-notification"},
        })
        .to_string()
    }

    fn feed(mgr: &SessionManager, s: &Session, line: &str) -> u32 {
        for item in vca_adapters::claude::transcript::parse_line(line) {
            mgr.update_subagents(s, |t| t.observe(&item, now_millis()));
        }
        s.info().subagents
    }

    /// Counts announced so far.
    fn announced(rx: &mut tokio::sync::broadcast::Receiver<Update>) -> Vec<u32> {
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(Update::Changed(info)) => out.push(info.subagents),
                Ok(Update::Removed(_)) => {}
                Err(TryRecvError::Lagged(_)) => {}
                Err(_) => return out,
            }
        }
    }

    #[tokio::test]
    async fn counts_foreground_background_and_parallel_agents() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, AgentKind::ClaudeCode, None);
        let mut rx = mgr.subscribe_updates();

        assert_eq!(feed(&mgr, &s, &agent_call("toolu_f")), 1);
        assert_eq!(feed(&mgr, &s, &result("toolu_f", "Done.")), 0);

        assert_eq!(feed(&mgr, &s, &agent_call("toolu_a")), 1);
        assert_eq!(feed(&mgr, &s, &agent_call("toolu_b")), 2);
        assert_eq!(feed(&mgr, &s, &launched("toolu_a", "a111")), 2);
        assert_eq!(feed(&mgr, &s, &launched("toolu_b", "a222")), 2);
        let waiting = notice(
            "a111",
            "completed",
            "Agent \"One\" is waiting on its own background work",
        );
        assert_eq!(feed(&mgr, &s, &waiting), 2);
        assert_eq!(
            feed(
                &mgr,
                &s,
                &notice("a111", "completed", "Agent \"One\" finished")
            ),
            1
        );
        assert_eq!(
            feed(
                &mgr,
                &s,
                &notice("a222", "killed", "Agent \"Two\" was stopped")
            ),
            0
        );
        // A card update only when the number changed.
        assert_eq!(announced(&mut rx), [1, 0, 1, 2, 1, 0]);
    }

    #[tokio::test]
    async fn subagent_stop_hook_ends_a_background_agent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, AgentKind::ClaudeCode, None);
        feed(&mgr, &s, &agent_call("toolu_a"));
        assert_eq!(feed(&mgr, &s, &launched("toolu_a", "a111")), 1);
        let hook = AgentHookEvent {
            agent: AgentKind::ClaudeCode,
            event_name: "SubagentStop".into(),
            agent_session_id: Some("abc".into()),
            cwd: None,
            transcript_path: None,
            state: None,
            notification: None,
            decision: None,
            tool: None,
            blocking: false,
            subagent_id: Some("a111".into()),
            tool_use_id: None,
            permission_mode: None,
            last_message: None,
        };
        mgr.handle_hook(None, None, hook).await;
        assert_eq!(s.info().subagents, 0);
    }

    #[tokio::test]
    async fn expiry_and_session_end_clear_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, AgentKind::ClaudeCode, None);
        feed(&mgr, &s, &agent_call("toolu_a"));
        feed(&mgr, &s, &launched("toolu_a", "a111"));
        feed(&mgr, &s, &agent_call("toolu_b"));
        assert_eq!(s.info().subagents, 2);
        let later = now_millis() + vca_adapters::subagents::SUBAGENT_EXPIRY_MS + 1_000;
        mgr.update_subagents(&s, |t| t.expire(later));
        assert_eq!(s.info().subagents, 0, "lost notifications expire");

        feed(&mgr, &s, &agent_call("toolu_c"));
        assert_eq!(s.info().subagents, 1);
        mgr.set_state(&s, SessionState::Exited { code: Some(0) });
        assert_eq!(s.info().subagents, 0);
        assert_eq!(lock(&s.subagents).count(), 0);
        // Late transcript lines of an exited session do not count.
        assert_eq!(feed(&mgr, &s, &agent_call("toolu_d")), 0);
    }

    #[tokio::test]
    async fn a_rewrite_resets_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, AgentKind::ClaudeCode, None);
        feed(&mgr, &s, &agent_call("toolu_a"));
        assert_eq!(s.info().subagents, 1);
        mgr.update_subagents(&s, |t| t.observe(&HistoryItem::Rewritten, now_millis()));
        assert_eq!(s.info().subagents, 0);
    }

    #[tokio::test]
    async fn the_count_is_optional_on_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let idle = observe(&mgr, AgentKind::ClaudeCode, None).info();
        let json = serde_json::to_string(&idle).unwrap();
        assert!(!json.contains("subagents"), "left out when 0");
        let parsed: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.subagents, 0);
        let busy = SessionInfo {
            subagents: 3,
            ..idle
        };
        let json = serde_json::to_string(&busy).unwrap();
        assert!(json.contains(r#""subagents":3"#));
        assert_eq!(serde_json::from_str::<SessionInfo>(&json).unwrap(), busy);
    }

    /// End to end through the real transcript tailer on a fixture file,
    /// including a rewrite of the file.
    #[tokio::test]
    async fn the_tailer_counts_agents_of_a_transcript_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.jsonl");
        let (call_a, call_b) = (agent_call("toolu_a"), agent_call("toolu_b"));
        std::fs::write(&path, format!("{call_a}\n{call_b}\n")).unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr, AgentKind::ClaudeCode, Some(path.clone()));
        mgr.follow_transcript(
            &s,
            SessionLocator {
                agent_session_id: None,
                transcript_path: Some(path.clone()),
                project_dir: None,
            },
        );
        let wait_for = |want: u32| {
            let s = Arc::clone(&s);
            async move {
                for _ in 0..100 {
                    if s.info().subagents == want {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                false
            }
        };
        assert!(wait_for(2).await, "two agents run");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", result("toolu_a", "Done.")).unwrap();
        drop(file);
        assert!(wait_for(1).await, "the result ends one");
        // The file is rewritten shorter: the count starts over.
        std::fs::write(&path, format!("{call_b}\n")).unwrap();
        assert!(wait_for(0).await, "a rewrite resets the count");
        // Only an agent started after the rewrite counts.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", agent_call("toolu_c")).unwrap();
        drop(file);
        assert!(wait_for(1).await, "new agents count again");
    }
}
