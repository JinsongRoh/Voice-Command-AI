//! Turn progress: is the agent working on the person's prompt, since when,
//! and what does it do right now ([`SessionInfo::progress`]).
//!
//! Hooks report a turn first (`UserPromptSubmit` about 0.2 s after Enter,
//! `Stop` at its end), but they may be missing: another tool can rewrite
//! the agent's settings, and observed sessions only have them when the
//! user installed them. The transcript reports the same turn a moment
//! later, so it drives the session state when hooks do not:
//!
//! * the person's prompt (a user message) → `running`;
//! * the turn's end ([`HistoryItem::TurnEnd`]) → `idle`;
//! * a thinking block, a tool call or the agent's text → the turn's
//!   current activity ("Thinking…", "Reading main.rs", ...).
//!
//! Only Claude Code transcripts drive it: they are the ones that record
//! the end of a turn (`system` / `turn_duration`).
//!
//! Hooks stay authoritative: a transcript record is ignored for the turn
//! when a hook already reported a turn event within [`HOOK_SLACK`] before
//! the record was written (the same prompt, the same end, or a newer
//! turn), so the state never flips twice for one event and a late
//! transcript line never undoes a hook. Records older than
//! [`crate::asks::FRESH_TURN`] (a transcript read from the start) only end
//! a turn; they never start one.
//!
//! The same prompt signal (hook or transcript, counted once) answers a
//! client's input with [`vca_core::DeliveryStatus::Acknowledged`] ("Seen by
//! the agent"), see [`prompt_watch`].
//!
//! [`SessionInfo::progress`]: vca_core::SessionInfo::progress

use tokio::sync::watch;
use vca_adapters::{AgentHookEvent, HistoryItem};
use vca_core::{
    AgentKind, MessageRole, SessionState, TurnActivity, TurnProgress, UnixMillis, now_millis,
};

use crate::sessions::{Session, SessionManager};
use crate::util::lock;

/// A transcript record written within this time after a hook turn event
/// (ms) is that event's own copy (or older than a newer turn the hooks
/// reported) and leaves the turn alone.
pub(crate) const HOOK_SLACK: UnixMillis = 10_000;

/// A record this far in the future is skewed and does not count as fresh.
const FUTURE_SLACK: UnixMillis = 5_000;

/// Per-session bookkeeping behind [`SessionInfo::progress`](vca_core::SessionInfo::progress).
#[derive(Debug)]
pub(crate) struct ProgressState {
    /// Bumped once per prompt of the person that the agent took (its hook
    /// or its transcript record): input acknowledgements wait on it.
    prompts: watch::Sender<u64>,
    /// When a hook last reported a prompt or the end of a turn (server
    /// clock, ms; 0: never).
    hook_turn_at: UnixMillis,
    /// The transcript's last prompt has no turn end yet, and when it was
    /// written.
    transcript_prompt: Option<UnixMillis>,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            prompts: watch::channel(0).0,
            hook_turn_at: 0,
            transcript_prompt: None,
        }
    }
}

impl ProgressState {
    /// A hook already reported the turn event of a record written at `at`.
    fn hooks_cover(&self, at: UnixMillis) -> bool {
        self.hook_turn_at > 0 && self.hook_turn_at.saturating_add(HOOK_SLACK) >= at
    }

    fn bump(&self) {
        self.prompts.send_modify(|n| *n = n.wrapping_add(1));
    }
}

/// Was a record written at `at` seen live?
fn fresh(at: UnixMillis, now: UnixMillis) -> bool {
    at <= now.saturating_add(FUTURE_SLACK) && now.saturating_sub(at) <= crate::asks::FRESH_TURN
}

/// The prompt counter of a session and its current value: an input sent
/// now is acknowledged once the counter passes that value.
pub(crate) fn prompt_watch(session: &Session) -> (watch::Receiver<u64>, u64) {
    let rx = lock(&session.progress).prompts.subscribe();
    let mark = *rx.borrow();
    (rx, mark)
}

/// The agent took the person's prompt: the session runs. Never over a
/// decision, a pause or an exit.
fn set_running(mgr: &SessionManager, session: &Session) {
    if lock(&session.pending).is_some() {
        return;
    }
    let state = session.info().state;
    if matches!(
        state,
        SessionState::Idle
            | SessionState::AwaitingInput
            | SessionState::Starting
            | SessionState::Error { .. }
    ) {
        mgr.set_state(session, SessionState::Running);
    }
}

/// The turn ended: a running session is idle.
fn set_idle(mgr: &SessionManager, session: &Session) {
    let state = session.info().state;
    if matches!(state, SessionState::Running | SessionState::Starting) {
        mgr.set_state(session, SessionState::Idle);
    }
}

/// Changes the turn shown on the card (and announces it when it changed).
fn update_turn(mgr: &SessionManager, session: &Session, f: impl FnOnce(&mut Option<TurnProgress>)) {
    mgr.update_info(session, |info| {
        let before = info.progress.clone();
        f(&mut info.progress);
        info.progress != before
    });
}

/// A new turn, started at `at`.
fn start_turn(mgr: &SessionManager, session: &Session, at: UnixMillis) {
    update_turn(mgr, session, |turn| {
        *turn = Some(TurnProgress {
            started_at: at,
            ended_at: None,
            activity: None,
        });
    });
}

/// Ends the running turn at `at`.
fn end_turn(mgr: &SessionManager, session: &Session, at: UnixMillis) {
    update_turn(mgr, session, |turn| {
        if let Some(t) = turn.as_mut().filter(|t| t.ended_at.is_none()) {
            t.ended_at = Some(at.max(t.started_at));
            t.activity = None;
        }
    });
}

/// Sets what the running turn does now.
fn set_activity(mgr: &SessionManager, session: &Session, activity: Option<TurnActivity>) {
    update_turn(mgr, session, |turn| {
        if let Some(t) = turn.as_mut().filter(|t| t.ended_at.is_none()) {
            t.activity = activity;
        }
    });
}

fn turn_running(session: &Session) -> bool {
    session
        .info()
        .progress
        .is_some_and(|t| t.ended_at.is_none())
}

/// Feeds one transcript item (called by the transcript tailer).
pub(crate) fn observe_item(mgr: &SessionManager, session: &Session, item: &HistoryItem) {
    // Only Claude Code transcripts record the end of a turn: from others a
    // prompt would start a turn that never ends.
    if session.agent() != Some(AgentKind::ClaudeCode) {
        return;
    }
    let now = now_millis();
    let at = item.at().unwrap_or(now);
    let live = fresh(at, now);
    match item {
        HistoryItem::Message {
            role: MessageRole::User,
            ..
        } => {
            let covered = {
                let mut st = lock(&session.progress);
                st.transcript_prompt = Some(at);
                let covered = st.hooks_cover(at);
                if live && !covered {
                    st.bump();
                }
                covered
            };
            if live && !covered {
                start_turn(mgr, session, at.min(now));
                set_running(mgr, session);
            }
        }
        HistoryItem::TurnEnd { .. } => {
            let covered = {
                let mut st = lock(&session.progress);
                st.transcript_prompt = None;
                st.hooks_cover(at)
            };
            if !covered {
                end_turn(mgr, session, at.min(now));
                // Also for an old turn end: a session found while its
                // agent waits is idle, not "running" forever.
                set_idle(mgr, session);
            }
        }
        HistoryItem::Thinking { .. }
        | HistoryItem::ToolCall { .. }
        | HistoryItem::ToolResult { .. }
        | HistoryItem::Message {
            role: MessageRole::Assistant,
            ..
        } if live => {
            // A turn the transcript started before VCA looked (vcad started
            // mid-turn): it shows once the agent is seen at work.
            let running = turn_running(session);
            let resumed = {
                let st = lock(&session.progress);
                st.transcript_prompt
                    .filter(|_| !running && !st.hooks_cover(at))
            };
            if let Some(started) = resumed {
                start_turn(mgr, session, started.min(now));
                set_running(mgr, session);
            }
            let activity = match item {
                HistoryItem::Thinking { .. } => Some(TurnActivity::Thinking),
                HistoryItem::ToolCall { name, summary, .. } => Some(TurnActivity::Tool {
                    name: name.clone(),
                    summary: summary.clone(),
                }),
                // A tool's result keeps the call on screen until the next step.
                HistoryItem::ToolResult { .. } => return,
                _ => None,
            };
            set_activity(mgr, session, activity);
        }
        _ => {}
    }
}

/// Applies a hook event: the prompt starts a turn, its end ends it, and a
/// reported tool call is the turn's activity.
pub(crate) fn on_hook(mgr: &SessionManager, session: &Session, event: &AgentHookEvent) {
    let now = now_millis();
    match event.event_name.as_str() {
        "UserPromptSubmit" => {
            {
                let mut st = lock(&session.progress);
                st.hook_turn_at = now;
                st.bump();
            }
            start_turn(mgr, session, now);
        }
        "Stop" | "StopFailure" | "SessionEnd" => {
            lock(&session.progress).hook_turn_at = now;
            end_turn(mgr, session, now);
        }
        "PreToolUse" => {
            if let Some(tool) = &event.tool {
                let summary =
                    vca_adapters::claude::transcript::tool_summary(&tool.name, &tool.input);
                set_activity(
                    mgr,
                    session,
                    Some(TurnActivity::Tool {
                        name: tool.name.clone(),
                        summary,
                    }),
                );
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use vca_broker::contain::Scope;
    use vca_core::{AgentKind, ControlTier, SessionEvent, SessionId, SessionInfo, SessionOrigin};

    use super::*;
    use crate::events::LogLimits;
    use crate::sessions::{ManagerConfig, Observed, ObservedIdentity, Session};
    use crate::workspace::WorkspaceRoots;

    fn manager(dir: &Path) -> SessionManager {
        SessionManager::new(
            ManagerConfig {
                limits: LogLimits::default(),
                exited_retention: Duration::from_secs(60),
                pause_scope: Scope::ChildrenOnly,
                heuristics_interval: Duration::from_secs(1),
                control_endpoint: PathBuf::from("unused"),
                attach_dir: None,
                workspace_roots: WorkspaceRoots::new(Vec::new()),
                session_names: Some(dir.join("names.json")),
                allow_generic_launch: false,
            },
            None,
        )
    }

    /// An observed Claude Code session as discovery inserts it: `running`.
    fn observed(mgr: &SessionManager) -> Arc<Session> {
        let dir = PathBuf::from("/code/shop");
        let info = SessionInfo {
            id: SessionId::new(),
            origin: SessionOrigin::Local {
                agent: AgentKind::ClaudeCode,
                project_dir: dir.clone(),
                pid: Some(4_300_001),
            },
            display_name: "shop".into(),
            state: SessionState::Running,
            control: ControlTier::ObserveOnly,
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
            identity: ObservedIdentity::default(),
        };
        mgr.insert_observed(info, observed, Some("abc".into()))
    }

    fn prompt(at: UnixMillis) -> HistoryItem {
        HistoryItem::Message {
            at: Some(at),
            role: MessageRole::User,
            text: "fix the build".into(),
        }
    }

    fn states(s: &Session) -> Vec<SessionState> {
        lock(s.log())
            .replay_after(0)
            .events
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::StateChanged { state, .. } => Some(state),
                _ => None,
            })
            .collect()
    }

    async fn hook(mgr: &SessionManager, s: &Session, json: &str) {
        let event = vca_adapters::claude::ClaudeHookPayload::parse(json)
            .unwrap()
            .to_agent_event();
        mgr.hook_for(&s.id, event).await;
    }

    #[tokio::test]
    async fn the_transcript_alone_drives_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        let s = observed(&mgr);
        let (prompts, mark) = prompt_watch(&s);

        // Read from the start: an old turn ended long ago. The session was
        // found idle, not "running" forever; nothing old starts a turn.
        let old = now_millis() - 3_600_000;
        observe_item(&mgr, &s, &prompt(old));
        observe_item(&mgr, &s, &HistoryItem::TurnEnd { at: Some(old + 5) });
        assert_eq!(s.info().state, SessionState::Idle);
        assert_eq!(s.info().progress, None);
        assert_eq!(*prompts.borrow(), mark);

        // The person types a prompt: running, the turn starts, the input is
        // seen. (Records a moment ago: future times are clamped to now.)
        let now = now_millis() - 1_000;
        observe_item(&mgr, &s, &prompt(now));
        assert_eq!(s.info().state, SessionState::Running);
        let turn = s.info().progress.unwrap();
        assert_eq!(turn.started_at, now);
        assert_eq!(turn.ended_at, None);
        assert_ne!(*prompts.borrow(), mark);

        observe_item(&mgr, &s, &HistoryItem::Thinking { at: Some(now + 1) });
        assert_eq!(
            s.info().progress.unwrap().activity,
            Some(TurnActivity::Thinking)
        );
        observe_item(
            &mgr,
            &s,
            &HistoryItem::ToolCall {
                at: Some(now + 2),
                id: Some("t1".into()),
                name: "Read".into(),
                summary: "src/main.rs".into(),
            },
        );
        let tool = Some(TurnActivity::Tool {
            name: "Read".into(),
            summary: "src/main.rs".into(),
        });
        assert_eq!(s.info().progress.unwrap().activity, tool);
        // The result keeps the call shown until the next step.
        observe_item(
            &mgr,
            &s,
            &HistoryItem::ToolResult {
                at: Some(now + 3),
                id: Some("t1".into()),
                text: "fn main() {}".into(),
                is_error: false,
            },
        );
        assert_eq!(s.info().progress.unwrap().activity, tool);

        observe_item(&mgr, &s, &HistoryItem::TurnEnd { at: Some(now + 9) });
        assert_eq!(s.info().state, SessionState::Idle);
        let turn = s.info().progress.unwrap();
        assert_eq!(turn.ended_at, Some(now + 9));
        assert_eq!(turn.activity, None);
        assert_eq!(
            states(&s),
            [
                SessionState::Idle,
                SessionState::Running,
                SessionState::Idle
            ]
        );
    }

    #[tokio::test]
    async fn hooks_win_and_the_transcript_copy_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        let s = observed(&mgr);
        mgr.set_state(&s, SessionState::Idle);
        let (prompts, mark) = prompt_watch(&s);

        hook(
            &mgr,
            &s,
            r#"{"session_id":"abc","hook_event_name":"UserPromptSubmit","prompt":"go"}"#,
        )
        .await;
        assert_eq!(s.info().state, SessionState::Running);
        let after_hook = *prompts.borrow();
        assert_ne!(after_hook, mark);
        let started = s.info().progress.unwrap().started_at;

        // A short turn: the hook reports its end before the tailer reads
        // the prompt record.
        hook(
            &mgr,
            &s,
            r#"{"session_id":"abc","hook_event_name":"Stop","last_assistant_message":"Done."}"#,
        )
        .await;
        assert_eq!(s.info().state, SessionState::Idle);
        let now = now_millis();
        observe_item(&mgr, &s, &prompt(now));
        observe_item(&mgr, &s, &HistoryItem::TurnEnd { at: Some(now + 1) });

        // One prompt, one start, one end: no second flip, no second ack.
        assert_eq!(*prompts.borrow(), after_hook);
        assert_eq!(s.info().state, SessionState::Idle);
        let turn = s.info().progress.unwrap();
        assert_eq!(turn.started_at, started);
        assert!(turn.ended_at.is_some());
        assert_eq!(
            states(&s),
            [
                SessionState::Idle,
                SessionState::Running,
                SessionState::Idle
            ]
        );
    }

    #[tokio::test]
    async fn a_decision_is_never_overwritten_by_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        let s = observed(&mgr);
        mgr.set_state(&s, SessionState::AwaitingDecision);
        observe_item(&mgr, &s, &prompt(now_millis()));
        assert_eq!(s.info().state, SessionState::AwaitingDecision);
        observe_item(
            &mgr,
            &s,
            &HistoryItem::TurnEnd {
                at: Some(now_millis()),
            },
        );
        assert_eq!(s.info().state, SessionState::AwaitingDecision);
    }

    #[tokio::test]
    async fn the_hooks_missing_problem_follows_the_check_and_the_first_hook() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        let s = observed(&mgr);
        assert_eq!(s.info().problem, None);
        mgr.set_claude_hooks_missing(true);
        assert_eq!(
            s.info().problem.as_deref(),
            Some(vca_core::SESSION_PROBLEM_HOOKS_MISSING)
        );
        // A session that reports through hooks has them (its agent read the
        // settings before they were rewritten).
        hook(
            &mgr,
            &s,
            r#"{"session_id":"abc","hook_event_name":"SessionStart"}"#,
        )
        .await;
        assert_eq!(s.info().problem, None);
        let other = observed(&mgr);
        assert_eq!(
            other.info().problem.as_deref(),
            Some(vca_core::SESSION_PROBLEM_HOOKS_MISSING)
        );
        mgr.set_claude_hooks_missing(false);
        assert_eq!(other.info().problem, None);
    }
}
