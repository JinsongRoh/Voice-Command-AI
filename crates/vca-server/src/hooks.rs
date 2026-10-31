//! Agent hook events from `vca hook`: session state, notifications,
//! decisions, transcript discovery and admission control (RES-02).
//!
//! Hooks never grant permissions the agent would otherwise ask for: the
//! reply is either "no opinion" (the agent's own flow applies) or a denial
//! because the machine has no capacity for a heavy job.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use vca_adapters::{AgentHookEvent, HookDecision, SessionLocator};
use vca_broker::AdmissionRequest;
use vca_core::{AdmissionVerdict, SessionEvent, SessionId, SessionState, now_millis};

use crate::sessions::{PendingDecision, Session, SessionManager};
use crate::util::lock;

/// Longest time a PreToolUse hook waits in the admission queue. Claude
/// Code's default hook timeout is 60 s and `vca hook` gives up after 55 s.
const MAX_QUEUE_WAIT: Duration = Duration::from_secs(50);

/// Events after which a tool's admission slot is released.
const RELEASE_EVENTS: &[&str] = &[
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

impl SessionManager {
    /// Finds the session a hook event belongs to: the managed session named
    /// by `VCA_SESSION_ID`, else the session with the same agent session id.
    fn find_hook_session(
        &self,
        hint: Option<&SessionId>,
        event: &AgentHookEvent,
    ) -> Option<Arc<Session>> {
        if let Some(s) = hint.and_then(|id| self.get(id)) {
            return Some(s);
        }
        let agent_id = event.agent_session_id.as_deref()?;
        self.all().into_iter().find(|s| {
            lock(&s.agent_session_id).as_deref() == Some(agent_id)
                && s.agent().as_ref() == Some(&event.agent)
        })
    }

    /// Applies a hook event and returns the reply for the agent.
    pub async fn handle_hook(
        &self,
        hint: Option<SessionId>,
        event: AgentHookEvent,
    ) -> HookDecision {
        let session = self.find_hook_session(hint.as_ref(), &event);
        if let Some(session) = &session {
            self.apply_hook(session, &event);
        }
        let key = session
            .as_ref()
            .map(|s| s.id.0.clone())
            .or_else(|| event.agent_session_id.clone());
        let Some(broker) = self.broker() else {
            return HookDecision::NoOpinion;
        };
        if RELEASE_EVENTS.contains(&event.event_name.as_str()) {
            if let Some(key) = &key {
                broker.admission().release_by_key(key);
            }
            return HookDecision::NoOpinion;
        }
        let (true, Some(tool)) = (event.event_name == "PreToolUse", &event.tool) else {
            return HookDecision::NoOpinion;
        };
        let Some(command) = &tool.command else {
            return HookDecision::NoOpinion;
        };
        let mut request = AdmissionRequest::new(command.clone());
        if let Some(s) = &session {
            request = request.session(s.id.clone());
        }
        if let Some(key) = key {
            request = request.key(key);
        }
        let admission = broker.admission();
        let submitted = admission.submit(request);
        let verdict = match submitted.ticket {
            Some(ticket) => {
                let wait = Duration::from_secs(admission.policy().admission.queue_timeout_secs)
                    .min(MAX_QUEUE_WAIT);
                admission.wait(ticket, wait).await
            }
            None => submitted.verdict,
        };
        match verdict {
            AdmissionVerdict::Grant { .. } => HookDecision::NoOpinion,
            AdmissionVerdict::Deny { reason } => busy(&reason),
            // `wait` always resolves a queued job; be safe anyway.
            AdmissionVerdict::Queue { .. } => busy("still queued"),
        }
    }

    fn apply_hook(&self, session: &Arc<Session>, event: &AgentHookEvent) {
        session.hook_seen.store(true, Ordering::SeqCst);
        if let Some(agent_id) = &event.agent_session_id {
            let mut current = lock(&session.agent_session_id);
            if current.as_deref() != Some(agent_id.as_str()) {
                *current = Some(agent_id.clone());
            }
        }
        if session.managed().is_some()
            && (event.transcript_path.is_some() || event.agent_session_id.is_some())
        {
            self.follow_transcript(
                session,
                SessionLocator {
                    agent_session_id: event.agent_session_id.clone(),
                    transcript_path: event.transcript_path.clone(),
                    project_dir: event.cwd.clone(),
                },
            );
        }
        if let Some(n) = &event.notification {
            let (title, body) = (n.title.clone(), n.body.clone());
            session.push(|seq| SessionEvent::Notification {
                seq,
                at: now_millis(),
                title,
                body,
            });
            self.add_unread(session);
            self.notify(session);
        }
        if let Some(decision) = &event.decision {
            let already = lock(&session.pending)
                .as_ref()
                .is_some_and(|p| p.decision.id == decision.id);
            if !already {
                self.request_decision(
                    session,
                    PendingDecision {
                        decision: decision.clone(),
                        submit: false,
                        heuristic: false,
                    },
                );
            }
        }
        match &event.state {
            // Managed sessions learn about exits from the PTY itself.
            Some(SessionState::Exited { .. }) if session.managed().is_some() => {}
            Some(state) => {
                if !matches!(state, SessionState::AwaitingDecision) {
                    lock(&session.pending).take();
                }
                self.set_state(session, state.clone());
            }
            None => {}
        }
    }
}

fn busy(reason: &str) -> HookDecision {
    HookDecision::Deny {
        reason: format!(
            "VCA resource broker: the machine is busy ({reason}). \
             Wait for running builds or tests to finish, then retry."
        ),
    }
}
