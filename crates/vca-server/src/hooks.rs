//! Agent hook events from `vca hook`: session state, notifications,
//! decisions, transcript discovery and admission control (RES-02).
//!
//! Hooks never grant permissions the agent would otherwise ask for: the
//! reply is either "no opinion" (the agent's own flow applies) or a denial
//! because the machine has no capacity for a heavy job.
//!
//! Decisions only alert for prompts that really wait on a person. Claude
//! Code may send `PermissionRequest` for a call that its permission mode or
//! classifier then allows on its own, so a decision is held back
//! ([`DeferredDecision`]) and announced only when it is known to wait on
//! the person:
//!
//! * the agent's `Notification` hook of type `permission_prompt` arrives
//!   for the session. Claude Code sends it only once a permission prompt
//!   has waited on screen for about six seconds (hooks reference,
//!   "Notification" matchers, <https://code.claude.com/docs/en/hooks>), so
//!   it is announced at once;
//! * or, in a permission mode that shows prompts (`default`,
//!   `acceptEdits`, `plan`, or no mode reported), the decision stayed
//!   unresolved for [`DECISION_GRACE`].
//!
//! In a mode that settles calls on its own (`auto`, `dontAsk`,
//! `bypassPermissions`) a `PermissionRequest` alone never alerts: a long
//! command the classifier allowed sends `PostToolUse` only when it ends, so
//! no grace period can tell it from a prompt. Such a decision waits for the
//! prompt notification without a timer. When the session's Claude Code
//! settings have no VCA `Notification` hook (see `vca hooks status`) that
//! signal can never come, and the grace period applies as before.
//!
//! If the call runs meanwhile (`PostToolUse`, `PostToolUseFailure`, `Stop`,
//! any hook that puts the session back to work, or the call's result in the
//! transcript), the held decision is dropped without a trace: no chat
//! event, no state change, no unread count, no alert.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use vca_adapters::claude::{is_derived_decision_id, is_notice_decision_id};
use vca_adapters::{AgentHookEvent, HistoryItem, HookDecision, HookNotification, SessionLocator};
use vca_broker::AdmissionRequest;
use vca_core::{AdmissionVerdict, Decision, SessionEvent, SessionId, SessionState, now_millis};

use crate::sessions::{PendingDecision, Session, SessionManager};
use crate::util::lock;

/// Longest time a PreToolUse hook waits in the admission queue. Claude
/// Code's default hook timeout is 60 s and `vca hook` gives up after 55 s.
const MAX_QUEUE_WAIT: Duration = Duration::from_secs(50);

/// How long a decision from a hook or the transcript must stay unresolved
/// before it is shown and alerts. A call the agent's permission mode or
/// classifier allows on its own starts running well within it.
pub(crate) const DECISION_GRACE: Duration = Duration::from_millis(2500);

/// A decision waiting out [`DECISION_GRACE`]: not pending, not shown, no
/// alert yet.
#[derive(Debug, Clone)]
pub(crate) struct DeferredDecision {
    /// Tells this deferral from a later one replacing it.
    token: u64,
    pub(crate) pending: PendingDecision,
    /// The agent's tool call the decision is about, when known.
    tool_use_id: Option<String>,
    /// Alerts that belong to the decision (a permission notification),
    /// held back and dropped with it.
    notifications: Vec<HookNotification>,
    /// No grace timer: announced only by the agent's prompt notification
    /// (a `PermissionRequest` in a mode that settles calls on its own).
    pub(crate) awaits_prompt: bool,
    /// Due to be shown, but another decision is on screen: shown once that
    /// one is answered or cleared, never in its place.
    ready: bool,
}

/// Most alerts held with one decision.
const MAX_HELD_NOTIFICATIONS: usize = 4;

/// Decisions held back on one session, oldest first. Parallel tool calls
/// can each wait on a prompt: one never replaces another.
#[derive(Debug, Default)]
pub(crate) struct DeferredQueue {
    items: VecDeque<DeferredDecision>,
}

impl DeferredQueue {
    /// At most this many decisions are held (the oldest is dropped first).
    pub(crate) const MAX: usize = 8;

    pub(crate) fn is_some(&self) -> bool {
        !self.items.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn is_none(&self) -> bool {
        self.items.is_empty()
    }

    /// The oldest held decision.
    #[cfg(test)]
    pub(crate) fn as_ref(&self) -> Option<&DeferredDecision> {
        self.items.front()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &DeferredDecision> {
        self.items.iter()
    }

    pub(crate) fn clear(&mut self) {
        self.items.clear();
    }

    fn push(&mut self, deferred: DeferredDecision) {
        self.items.push_back(deferred);
        while self.items.len() > Self::MAX {
            self.items.pop_front();
        }
    }
}

/// Recent shell tool calls of a session (from `PreToolUse`), so that a
/// `PermissionRequest`, which names no call, can be tied to its call: the
/// call's result then clears the prompt, and another call does not.
#[derive(Debug, Default)]
pub(crate) struct RecentCalls {
    calls: VecDeque<(String, u64)>,
}

impl RecentCalls {
    const MAX: usize = 16;

    fn fingerprint(tool: &vca_adapters::HookToolUse) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        tool.name.hash(&mut h);
        serde_json::to_string(&tool.input)
            .unwrap_or_default()
            .hash(&mut h);
        h.finish()
    }

    fn started(&mut self, id: &str, tool: &vca_adapters::HookToolUse) {
        self.calls.retain(|(c, _)| c != id);
        self.calls
            .push_back((id.to_owned(), Self::fingerprint(tool)));
        while self.calls.len() > Self::MAX {
            self.calls.pop_front();
        }
    }

    fn ended(&mut self, id: &str) {
        self.calls.retain(|(c, _)| c != id);
    }

    /// The call a permission request for `tool` is about: the newest open
    /// call with the same tool and input. Claimed, so a second identical
    /// request finds the next one.
    fn claim(&mut self, tool: &vca_adapters::HookToolUse) -> Option<String> {
        let print = Self::fingerprint(tool);
        let at = self.calls.iter().rposition(|(_, p)| *p == print)?;
        self.calls.remove(at).map(|(id, _)| id)
    }
}

static NEXT_DEFERRAL: AtomicU64 = AtomicU64::new(1);

/// Where a hook event came from.
enum HookSource {
    Session(Arc<Session>),
    /// No session of ours; the broker still answers for the agent.
    Unknown,
    /// It names a managed session without proving it.
    Refused,
}

/// Events after which a tool's admission slot is released.
const RELEASE_EVENTS: &[&str] = &[
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

impl SessionManager {
    /// Finds the session a hook event belongs to. The event comes over a
    /// channel every process of the owner can open, so it must prove that:
    ///
    /// * a managed session needs its hook token, which only its own process
    ///   tree holds;
    /// * an observed session has no token (VCA did not start it, and the
    ///   hook is installed for the whole user), so the agent session id must
    ///   match. That id is not a secret: see `docs/security-setup.md`.
    fn resolve_hook(
        &self,
        hint: Option<&SessionId>,
        token: Option<&str>,
        event: &AgentHookEvent,
    ) -> HookSource {
        let proved = |s: Arc<Session>| match s.managed() {
            Some(m) if !m.accepts_hook_token(token) => HookSource::Refused,
            _ => HookSource::Session(s),
        };
        if let Some(s) = hint
            .and_then(|id| self.get(id))
            .filter(|s| s.managed().is_some())
        {
            return proved(s);
        }
        let Some(agent_id) = event.agent_session_id.as_deref() else {
            return HookSource::Unknown;
        };
        self.all()
            .into_iter()
            .find(|s| {
                lock(&s.agent_session_id).as_deref() == Some(agent_id)
                    && s.agent().as_ref() == Some(&event.agent)
            })
            .map_or(HookSource::Unknown, proved)
    }

    /// Applies a hook event and returns the reply for the agent. A refused
    /// event changes nothing, and the agent is never held up by it.
    pub async fn handle_hook(
        &self,
        hint: Option<SessionId>,
        token: Option<&str>,
        event: AgentHookEvent,
    ) -> HookDecision {
        match self.resolve_hook(hint.as_ref(), token, &event) {
            HookSource::Session(session) => self.handle_hook_of(Some(session), event).await,
            HookSource::Unknown => self.handle_hook_of(None, event).await,
            HookSource::Refused => {
                tracing::warn!(
                    event = %event.event_name,
                    "ignored a hook event without the session's token"
                );
                HookDecision::NoOpinion
            }
        }
    }

    /// A hook event already known to belong to session `id`.
    #[cfg(test)]
    pub(crate) async fn hook_for(&self, id: &SessionId, event: AgentHookEvent) -> HookDecision {
        self.handle_hook_of(self.get(id), event).await
    }

    async fn handle_hook_of(
        &self,
        session: Option<Arc<Session>>,
        event: AgentHookEvent,
    ) -> HookDecision {
        if let Some(session) = &session {
            // Settings files are read off the async workers.
            let hook_installed = if needs_notification_hook_check(&event) {
                let cwd = event.cwd.clone();
                tokio::task::spawn_blocking(move || {
                    notification_hook_installed(
                        &vca_adapters::claude::default_config_dir(),
                        cwd.as_deref(),
                    )
                })
                .await
                .unwrap_or(false)
            } else {
                false
            };
            self.apply_hook(session, &event, hook_installed);
        }
        // Keys of sessions and of unknown agents never meet: an agent id
        // that equals a session id must not release that session's slot.
        let key = session
            .as_ref()
            .map(|s| session_admission_key(&s.id))
            .or_else(|| {
                event
                    .agent_session_id
                    .as_ref()
                    .map(|a| format!("agent:{a}"))
            });
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

    fn apply_hook(&self, session: &Arc<Session>, event: &AgentHookEvent, hook_installed: bool) {
        if !session.hook_seen.swap(true, Ordering::SeqCst) {
            // Its hooks work: no "hooks missing" banner for it.
            self.refresh_problem(session);
        }
        // The call a decision is about: reported, or (a `PermissionRequest`
        // names none) the recent call with the same tool and input.
        let call_id = {
            let mut recent = lock(&session.recent_calls);
            match (event.event_name.as_str(), &event.tool_use_id, &event.tool) {
                ("PreToolUse", Some(id), Some(tool)) => {
                    recent.started(id, tool);
                    Some(id.clone())
                }
                ("PostToolUse" | "PostToolUseFailure", Some(id), _) => {
                    recent.ended(id);
                    Some(id.clone())
                }
                ("PermissionRequest", None, Some(tool)) => recent.claim(tool),
                (_, id, _) => id.clone(),
            }
        };
        if let Some(agent_id) = &event.agent_session_id {
            // The name key of an observed session follows the agent's
            // session id; a custom name is carried over.
            self.learn_agent_session_id(session, agent_id);
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
        // The agent works again (the call was allowed, or the prompt was
        // answered in its terminal): a decision still in its grace period
        // never needed the person.
        let resumes = event
            .state
            .as_ref()
            .is_some_and(|s| !matches!(s, SessionState::AwaitingDecision));
        // An event about one call says nothing about another: with parallel
        // calls, the next one starting must not drop this one's prompt.
        let settles = |tool_use_id: Option<&str>| match (event.tool_use_id.as_deref(), tool_use_id)
        {
            (Some(reported), Some(asked)) => reported == asked,
            // A turn-level event (the turn ended), or a decision that names
            // no call (from a notification or the screen).
            _ => true,
        };
        if resumes {
            self.drop_deferred(session, |d| settles(d.tool_use_id.as_deref()));
        }
        // The conductor's own notifications ("waiting for your input", ...)
        // are not alerts: its conversation lives in the conductor view and
        // its permission decisions arrive as decisions below.
        let mut notification = event
            .notification
            .clone()
            .filter(|_| !session.is_conductor());
        if let Some(decision) = &event.decision {
            let prompt_shown = event.is_prompt_notification();
            if let Some(decision) = admit_decision(session, decision) {
                let pending = PendingDecision {
                    decision,
                    submit: false,
                    heuristic: false,
                    at: now_millis(),
                    tool_use_id: call_id.clone(),
                };
                let awaits_prompt =
                    prompt_shown || awaits_prompt_notification(event, hook_installed);
                self.defer_decision(
                    session,
                    pending,
                    call_id.clone(),
                    notification.take(),
                    awaits_prompt,
                );
            } else if let Some(deferred) = lock(&session.deferred).items.back_mut() {
                // A repeat (or the permission notification) of the decision
                // being held alerts with it, or not at all.
                if deferred.notifications.len() < MAX_HELD_NOTIFICATIONS {
                    deferred.notifications.extend(notification.take());
                } else {
                    notification.take();
                }
            }
            if prompt_shown {
                // The prompt has been on screen for seconds: the held
                // decision waits on the person.
                self.promote_now(session);
            }
        }
        if let Some(n) = notification {
            session.push(|seq| SessionEvent::Notification {
                seq,
                at: now_millis(),
                title: n.title,
                body: n.body,
            });
            self.add_unread(session);
            self.notify(session);
        }
        if let Some(agent) = &event.subagent_id {
            self.update_subagents(session, |t| t.hook_stopped(agent));
        }
        match &event.state {
            // Managed sessions learn about exits from the PTY itself.
            Some(SessionState::Exited { .. }) if session.managed().is_some() => {}
            // The decision sets the state once it outlived its grace period.
            Some(SessionState::AwaitingDecision) if event.decision.is_some() => {}
            Some(state) => {
                let open = {
                    let mut pending = lock(&session.pending);
                    let settled = !matches!(state, SessionState::AwaitingDecision)
                        && pending
                            .as_ref()
                            .is_some_and(|p| settles(p.tool_use_id.as_deref()));
                    if settled {
                        pending.take();
                    }
                    pending.is_some()
                };
                // A prompt that still waits keeps the session waiting.
                if !open || matches!(state, SessionState::AwaitingDecision) {
                    self.set_state(session, state.clone());
                }
            }
            None => {}
        }
        // A decision that was due while another one was on screen.
        self.promote_waiting(session);
        // Permission mode, and a turn that ended in a question.
        crate::asks::on_hook(self, session, event);
        // The turn's start, end and current tool.
        crate::progress::on_hook(self, session, event);
    }

    /// Holds `pending` back. With `awaits_prompt` it waits for the agent's
    /// prompt notification ([`Self::promote_now`]); otherwise it is
    /// announced after [`DECISION_GRACE`] unless the call ran or was
    /// answered meanwhile.
    pub(crate) fn defer_decision(
        &self,
        session: &Arc<Session>,
        pending: PendingDecision,
        tool_use_id: Option<String>,
        notification: Option<HookNotification>,
        awaits_prompt: bool,
    ) {
        let token = NEXT_DEFERRAL.fetch_add(1, Ordering::Relaxed);
        lock(&session.deferred).push(DeferredDecision {
            token,
            pending,
            tool_use_id,
            notifications: notification.into_iter().collect(),
            awaits_prompt,
            ready: false,
        });
        if awaits_prompt {
            tracing::debug!(
                session = %session.id,
                "permission request held until the agent reports its prompt"
            );
            return;
        }
        let weak = Arc::downgrade(session);
        let mgr = self.clone();
        session.add_task(tokio::spawn(async move {
            tokio::time::sleep(DECISION_GRACE).await;
            if let Some(session) = weak.upgrade() {
                mgr.promote_deferred(&session, token);
            }
        }));
    }

    /// Announces the held decision at once (the agent reported its prompt
    /// on screen). The notification names no call; it is the newest one
    /// (where its alert was attached too): an older decision still held
    /// is a call the agent's mode allowed and that is still running, and
    /// showing it would ask the person about the wrong call.
    fn promote_now(&self, session: &Session) {
        let token = lock(&session.deferred).items.back().map(|d| d.token);
        if let Some(token) = token {
            self.promote_deferred(session, token);
        }
    }

    /// Shows the oldest decision that became due while another one was on
    /// screen, once nothing is on screen any more.
    pub(crate) fn promote_waiting(&self, session: &Session) {
        let free = lock(&session.pending).as_ref().is_none_or(|p| p.heuristic);
        if !free {
            return;
        }
        let token = lock(&session.deferred)
            .iter()
            .find(|d| d.ready)
            .map(|d| d.token);
        if let Some(token) = token {
            self.promote_deferred(session, token);
        }
    }

    /// Announces the deferred decision `token` if it is still unresolved:
    /// its notifications, the decision, `awaiting_decision` and alerts,
    /// exactly as an immediate decision would.
    fn promote_deferred(&self, session: &Session, token: u64) {
        let exited = matches!(session.info().state, SessionState::Exited { .. });
        // `pending` then `deferred` (the order every other path takes both
        // in): the decision moves from the queue to `pending` in one step,
        // so a call that ends meanwhile finds it in one place or the other
        // and clears it, and a prompt that is answered meanwhile finds this
        // one marked ready.
        let (decision, notifications) = {
            let mut pending = lock(&session.pending);
            let mut queue = lock(&session.deferred);
            let Some(at) = queue.items.iter().position(|d| d.token == token) else {
                return;
            };
            if exited {
                queue.items.remove(at);
                return;
            }
            // The transcript may have reported the same form meanwhile.
            let decision = &queue.items[at].pending.decision;
            let shown = pending.as_ref().is_some_and(|p| {
                p.decision.id == decision.id
                    || (decision.is_question()
                        && p.decision.is_question()
                        && p.decision.questions == decision.questions)
            });
            if shown {
                queue.items.remove(at);
                return;
            }
            if pending.as_ref().is_some_and(|p| !p.heuristic) {
                // Another prompt is on screen and still waits: this one is
                // shown after it, never in its place.
                if let Some(mut deferred) = queue.items.remove(at) {
                    deferred.ready = true;
                    queue.items.push_front(deferred);
                }
                return;
            }
            let Some(deferred) = queue.items.remove(at) else {
                return;
            };
            *pending = Some(deferred.pending.clone());
            (deferred.pending, deferred.notifications)
        };
        for n in notifications {
            session.push(|seq| SessionEvent::Notification {
                seq,
                at: now_millis(),
                title: n.title,
                body: n.body,
            });
            self.add_unread(session);
        }
        self.announce_installed(session, decision);
    }

    /// [`SessionManager::request_decision`] for a decision already put in
    /// `pending` (by [`Self::promote_deferred`]): writing it again here
    /// could bring back a call that ended a moment ago.
    fn announce_installed(&self, session: &Session, pending: PendingDecision) {
        let decision = pending.decision;
        lock(&session.turn).question.take();
        session.push(|seq| SessionEvent::DecisionRequested {
            seq,
            at: now_millis(),
            decision,
        });
        self.add_unread(session);
        self.set_state(session, SessionState::AwaitingDecision);
        // The call ended between the two steps: its settle saw the
        // decision in `pending` and took it; the state follows.
        let open = lock(&session.pending).is_some();
        if !open && session.info().state == SessionState::AwaitingDecision {
            self.set_state(session, SessionState::Running);
        }
        self.notify(session);
    }

    /// Drops the deferred decision if `matches` says so. Silent: it was
    /// never announced.
    pub(crate) fn drop_deferred(
        &self,
        session: &Session,
        matches: impl Fn(&DeferredDecision) -> bool,
    ) -> bool {
        let mut queue = lock(&session.deferred);
        let before = queue.items.len();
        queue.items.retain(|d| {
            let drop = matches(d);
            if drop {
                tracing::debug!(
                    session = %session.id,
                    decision = %d.pending.decision.id,
                    awaited_prompt = d.awaits_prompt,
                    "the call went on before the decision was shown; no decision"
                );
            }
            !drop
        });
        queue.items.len() != before
    }

    /// Feeds one transcript item: the result of the call a deferred
    /// decision is about means the call ran without the person.
    ///
    /// The same goes for a decision that was shown: the result of its call
    /// means the prompt is over, however it ended. Esc at the prompt sends no
    /// hook event that names the call, so this is what clears it.
    pub(crate) fn observe_deferred(&self, session: &Session, item: &HistoryItem) {
        if let HistoryItem::ToolResult { id: Some(id), .. } = item {
            self.drop_deferred(session, |d| d.tool_use_id.as_deref() == Some(id.as_str()));
            let settled = {
                let mut pending = lock(&session.pending);
                let same = pending
                    .as_ref()
                    .is_some_and(|p| p.tool_use_id.as_deref() == Some(id.as_str()));
                if same {
                    pending.take();
                }
                same
            };
            if settled && session.info().state == SessionState::AwaitingDecision {
                self.set_state(session, SessionState::Running);
            }
            if settled {
                self.promote_waiting(session);
            }
        }
    }
}

/// Decides whether a hook-reported decision is new. Returns the decision
/// to announce (its id possibly given a reuse generation), or `None` when
/// it repeats one that is pending or was already handled.
///
/// * Agent-supplied ids: announced once per session.
/// * Content-derived ids: a repeat of the pending one is ignored; once the
///   pending one was answered or cleared, an identical request is new and
///   gets the next generation suffix.
/// * A bare permission notification (no tool) duplicates any pending
///   hook decision and is ignored.
pub(crate) fn admit_decision(session: &Session, decision: &Decision) -> Option<Decision> {
    let shown_id = lock(&session.pending)
        .as_ref()
        .filter(|p| !p.heuristic)
        .map(|p| p.decision.id.clone());
    // A decision in its grace period counts as pending.
    let pending_ids: Vec<String> = shown_id
        .into_iter()
        .chain(
            lock(&session.deferred)
                .iter()
                .map(|d| d.pending.decision.id.clone()),
        )
        .collect();
    if is_notice_decision_id(&decision.id) && !pending_ids.is_empty() {
        return None;
    }
    let base = decision.id.clone();
    let mut gens = lock(&session.decision_gens);
    let id = match gens.get(&base) {
        // Forgotten while one of its generations is still pending: a
        // repeat of that one, never a new decision.
        None if pending_ids
            .iter()
            .any(|p| generation_of(&base, p).is_some()) =>
        {
            return None;
        }
        None => {
            gens.insert(base.clone(), 0);
            base
        }
        Some(_) if !is_derived_decision_id(&base) => return None,
        Some(generation) => {
            if pending_ids.contains(&generation_id(&base, generation)) {
                return None;
            }
            gens.insert(base.clone(), generation + 1);
            generation_id(&base, generation + 1)
        }
    };
    Some(Decision {
        id,
        ..decision.clone()
    })
}

/// Hook decision ids seen on one session with their reuse generation (see
/// [`crate::sessions::Session::decision_gens`]). Bounded: past
/// [`DecisionGens::MAX`] ids the oldest is forgotten, never the whole set,
/// so a repeated delivery of a recent decision stays a repeat.
#[derive(Debug, Default)]
pub(crate) struct DecisionGens {
    gens: HashMap<String, u32>,
    /// Ids from the least to the most recently seen (a new generation
    /// counts as seen).
    order: VecDeque<String>,
}

impl DecisionGens {
    pub(crate) const MAX: usize = 512;

    fn get(&self, id: &str) -> Option<u32> {
        self.gens.get(id).copied()
    }

    fn insert(&mut self, id: String, generation: u32) {
        if self.gens.insert(id.clone(), generation).is_some()
            && let Some(at) = self.order.iter().position(|o| *o == id)
        {
            self.order.remove(at);
        }
        self.order.push_back(id);
        while self.order.len() > Self::MAX {
            if let Some(oldest) = self.order.pop_front() {
                self.gens.remove(&oldest);
            }
        }
    }
}

/// Whether the decision of `event` could wait for the prompt notification,
/// so the settings must be checked for the VCA `Notification` hook: a
/// `PermissionRequest` (not a question form, which always asks the person)
/// in a permission mode that settles calls on its own.
fn needs_notification_hook_check(event: &AgentHookEvent) -> bool {
    event.event_name == "PermissionRequest"
        && !event.decision.as_ref().is_some_and(Decision::is_question)
        && event
            .permission_mode
            .as_deref()
            .is_some_and(vca_adapters::hook::mode_settles_without_prompt)
}

/// Whether a hook decision waits for the agent's prompt notification
/// instead of a grace period: see [`needs_notification_hook_check`], from a
/// session whose settings have the VCA `Notification` hook
/// (`hook_installed`, checked off the async workers).
fn awaits_prompt_notification(event: &AgentHookEvent, hook_installed: bool) -> bool {
    needs_notification_hook_check(event) && hook_installed
}

/// Most bytes read from one settings file.
const MAX_SETTINGS_BYTES: u64 = 256 * 1024;
/// Most parent folders searched for project settings.
const MAX_SETTINGS_DEPTH: usize = 32;

/// May the folder reported by a hook be searched for settings? Only a
/// local absolute path: a network share (`\\host\share`) or a device path
/// would make vcad open files on another machine (and send it the user's
/// credentials) because any local process can send a hook event.
fn searchable_cwd(cwd: &Path) -> bool {
    use std::path::{Component, Prefix};
    if !cwd.is_absolute() {
        return false;
    }
    match cwd.components().next() {
        Some(Component::Prefix(prefix)) => {
            matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        }
        Some(Component::RootDir) => {
            // `//host/share` on Unix is a plain path; keep it local anyway.
            !cwd.to_string_lossy().starts_with("//")
        }
        _ => false,
    }
}

/// A settings file as JSON, if it is small enough.
fn read_settings(path: &Path) -> Option<serde_json::Value> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_SETTINGS_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_SETTINGS_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether Claude Code settings that apply in `cwd` run a VCA
/// `Notification` hook: the user settings in `config_dir`, and the project
/// (`.claude/settings.json`) and local (`.claude/settings.local.json`)
/// settings of `cwd` and its parent folders. Read-only and blocking: call
/// it off the async workers. A `cwd` that is not a local absolute path is
/// not searched.
pub(crate) fn notification_hook_installed(config_dir: &Path, cwd: Option<&Path>) -> bool {
    let mut files = vec![config_dir.join("settings.json")];
    let cwd = cwd.filter(|c| searchable_cwd(c));
    for dir in cwd
        .into_iter()
        .flat_map(Path::ancestors)
        .take(MAX_SETTINGS_DEPTH)
    {
        files.push(dir.join(".claude").join("settings.json"));
        files.push(dir.join(".claude").join("settings.local.json"));
    }
    files.iter().any(|path| {
        read_settings(path).is_some_and(|settings| runs_vca_notification_hook(&settings))
    })
}

fn runs_vca_notification_hook(settings: &serde_json::Value) -> bool {
    let groups = settings
        .get("hooks")
        .and_then(|h| h.get("Notification"))
        .and_then(serde_json::Value::as_array);
    groups
        .into_iter()
        .flatten()
        .filter_map(|g| g.get("hooks").and_then(serde_json::Value::as_array))
        .flatten()
        .filter_map(|h| h.get("command").and_then(serde_json::Value::as_str))
        .any(|c| c.contains(" hook claude notification"))
}

/// The broker's admission key of a VCA session's tool calls.
pub(crate) fn session_admission_key(id: &SessionId) -> String {
    format!("session:{}", id.0)
}

fn generation_id(base: &str, generation: u32) -> String {
    if generation == 0 {
        base.to_owned()
    } else {
        format!("{base}.{generation}")
    }
}

/// The generation of `base` that `id` is ([`generation_id`]), if any.
fn generation_of(base: &str, id: &str) -> Option<u32> {
    let rest = id.strip_prefix(base)?;
    if rest.is_empty() {
        return Some(0);
    }
    rest.strip_prefix('.')?.parse().ok().filter(|&g| g > 0)
}

fn busy(reason: &str) -> HookDecision {
    HookDecision::Deny {
        reason: format!(
            "VCA resource broker: the machine is busy ({reason}). \
             Wait for running builds or tests to finish, then retry."
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use vca_broker::contain::Scope;
    use vca_core::{ControlTier, SessionInfo, SessionOrigin};

    use super::*;
    use crate::events::LogLimits;

    #[test]
    fn decision_ids_forget_only_the_oldest() {
        let mut gens = DecisionGens::default();
        for i in 0..DecisionGens::MAX + 10 {
            gens.insert(format!("d{i}"), 0);
        }
        // The oldest ten are gone; every recent one is still known.
        assert_eq!(gens.get("d0"), None);
        assert_eq!(gens.get("d9"), None);
        assert_eq!(gens.get("d10"), Some(0));
        assert_eq!(gens.get(&format!("d{}", DecisionGens::MAX + 9)), Some(0));
        // A new generation of a known id does not count as another id.
        gens.insert("d10".into(), 1);
        assert_eq!(gens.get("d10"), Some(1));
        assert_eq!(gens.order.len(), DecisionGens::MAX);
    }
    use crate::sessions::{ManagerConfig, Observed, ObservedIdentity, Update};
    use crate::workspace::WorkspaceRoots;

    /// A manager, an observed Claude Code session (agent session `abc`)
    /// and a project folder whose settings run the VCA hooks, as a project
    /// scope install writes them.
    struct World {
        _dir: tempfile::TempDir,
        mgr: SessionManager,
        s: Arc<Session>,
        project: PathBuf,
    }

    impl World {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mgr = SessionManager::new(
                ManagerConfig {
                    limits: LogLimits::default(),
                    exited_retention: Duration::from_secs(60),
                    pause_scope: Scope::ChildrenOnly,
                    heuristics_interval: Duration::from_secs(1),
                    control_endpoint: PathBuf::from("unused"),
                    attach_dir: None,
                    workspace_roots: WorkspaceRoots::new(Vec::new()),
                    session_names: Some(dir.path().join("names.json")),
                    allow_generic_launch: false,
                },
                None,
            );
            let project = dir.path().join("shop");
            write_vca_hooks(&project.join(".claude").join("settings.json"));
            let info = SessionInfo {
                id: SessionId::new(),
                origin: SessionOrigin::Local {
                    agent: vca_core::AgentKind::ClaudeCode,
                    project_dir: project.clone(),
                    pid: Some(4_242_426),
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
                identity: ObservedIdentity::default(),
            };
            let s = mgr.insert_observed(info, observed, Some("abc".into()));
            Self {
                _dir: dir,
                mgr,
                s,
                project,
            }
        }

        /// Sends a Claude Code hook payload; `cwd` is a folder inside the
        /// project (Claude Code reports the current folder).
        async fn hook(&self, json: serde_json::Value) {
            let mut json = json;
            json["session_id"] = "abc".into();
            json["cwd"] = self
                .project
                .join("src")
                .to_string_lossy()
                .into_owned()
                .into();
            let event = vca_adapters::claude::ClaudeHookPayload::parse(&json.to_string())
                .unwrap()
                .to_agent_event();
            self.mgr.hook_for(&self.s.id, event).await;
        }

        fn requested(&self) -> Vec<Decision> {
            lock(self.s.log())
                .replay_after(0)
                .events
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::DecisionRequested { decision, .. } => Some(decision),
                    _ => None,
                })
                .collect()
        }

        /// Nothing about a decision reached the person, ever.
        fn silent(&self, updates: &mut tokio::sync::broadcast::Receiver<Update>) {
            assert!(self.requested().is_empty(), "{:?}", self.requested());
            let info = self.s.info();
            assert!(info.pending_decision.is_none());
            assert_ne!(info.state, SessionState::AwaitingDecision);
            while let Ok(update) = updates.try_recv() {
                if let Update::Changed(info) = update {
                    assert!(info.pending_decision.is_none());
                    assert_ne!(info.state, SessionState::AwaitingDecision);
                }
            }
        }
    }

    fn write_vca_hooks(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let hooks = serde_json::json!({ "hooks": {
            "PermissionRequest": [{ "hooks": [{ "type": "command",
                "command": "vca hook claude permission-request" }] }],
            "Notification": [{ "hooks": [{ "type": "command",
                "command": "vca hook claude notification" }] }],
        }});
        std::fs::write(path, hooks.to_string()).unwrap();
    }

    /// `PermissionRequest` as documented: no `tool_use_id`.
    fn request(mode: &str, command: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "permission_mode": mode,
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "permission_suggestions": [],
        })
    }

    fn tool(event: &str, mode: &str, id: &str, command: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": event,
            "permission_mode": mode,
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "tool_use_id": id,
        })
    }

    fn prompt_notice() -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "Notification",
            "message": "Claude needs your permission to use Bash",
            "title": "Permission needed",
            "notification_type": "permission_prompt",
        })
    }

    /// Longer than the grace period.
    async fn past_grace() {
        tokio::time::sleep(DECISION_GRACE + Duration::from_millis(700)).await;
    }

    const LONG: &str = "cargo test -p vca-server; cargo clippy -- -D warnings";

    #[tokio::test]
    async fn auto_mode_long_running_command_never_raises_a_decision() {
        let w = World::new();
        let mut updates = w.mgr.subscribe_updates();
        w.hook(tool("PreToolUse", "auto", "toolu_l", LONG)).await;
        w.hook(request("auto", LONG)).await;
        // The classifier allowed it; the command runs far beyond the grace
        // period. The request is held without a timer.
        past_grace().await;
        assert!(
            lock(&w.s.deferred)
                .as_ref()
                .is_some_and(|d| d.awaits_prompt)
        );
        past_grace().await;
        assert!(w.requested().is_empty());
        // It ends: PostToolUse drops the held request.
        w.hook(tool("PostToolUse", "auto", "toolu_l", LONG)).await;
        assert!(lock(&w.s.deferred).is_none());
        past_grace().await;
        w.silent(&mut updates);
        assert_eq!(w.s.info().unread, 0);
    }

    #[tokio::test]
    async fn a_failed_call_or_the_turn_end_drops_a_held_request() {
        for end in [
            tool("PostToolUseFailure", "auto", "toolu_f", "false"),
            serde_json::json!({ "hook_event_name": "Stop", "last_assistant_message": "Done." }),
        ] {
            let w = World::new();
            let mut updates = w.mgr.subscribe_updates();
            w.hook(request("bypassPermissions", "false")).await;
            assert!(lock(&w.s.deferred).is_some());
            w.hook(end).await;
            assert!(lock(&w.s.deferred).is_none());
            w.silent(&mut updates);
        }
    }

    #[tokio::test]
    async fn auto_mode_real_prompt_is_raised_by_its_notification_at_once() {
        let w = World::new();
        w.hook(tool("PreToolUse", "auto", "toolu_p", "git push --force"))
            .await;
        w.hook(request("auto", "git push --force")).await;
        assert!(w.requested().is_empty());
        w.hook(prompt_notice()).await;
        // No grace period: the prompt has waited on screen already.
        let requested = w.requested();
        assert_eq!(requested.len(), 1, "{requested:?}");
        assert_eq!(requested[0].prompt, "Allow Bash: git push --force?");
        let info = w.s.info();
        assert_eq!(info.state, SessionState::AwaitingDecision);
        assert_eq!(info.pending_decision.unwrap().decision.options.len(), 3);
        // A repeated notice is no second decision.
        w.hook(prompt_notice()).await;
        past_grace().await;
        assert_eq!(w.requested().len(), 1);
    }

    #[tokio::test]
    async fn a_prompt_notification_alone_is_raised_at_once() {
        let w = World::new();
        w.hook(prompt_notice()).await;
        assert_eq!(w.requested().len(), 1);
        assert_eq!(w.s.info().state, SessionState::AwaitingDecision);
    }

    #[tokio::test]
    async fn default_mode_keeps_the_grace_period() {
        for mode in ["default", "acceptEdits", "plan"] {
            let w = World::new();
            w.hook(request(mode, "rm -rf build")).await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            assert!(
                w.requested().is_empty(),
                "{mode}: not within the grace period"
            );
            assert!(
                lock(&w.s.deferred)
                    .as_ref()
                    .is_some_and(|d| !d.awaits_prompt)
            );
            past_grace().await;
            assert_eq!(w.requested().len(), 1, "{mode}");
            assert_eq!(w.s.info().state, SessionState::AwaitingDecision, "{mode}");
        }
        // Allowed and run within the grace period: nothing.
        let w = World::new();
        let mut updates = w.mgr.subscribe_updates();
        w.hook(request("default", "ls")).await;
        w.hook(tool("PostToolUse", "default", "toolu_d", "ls"))
            .await;
        past_grace().await;
        w.silent(&mut updates);
    }

    /// `PermissionRequest` names no call: it is tied to the recent call with
    /// the same input, so another call does not drop it and its own result
    /// (Esc at the prompt) clears it.
    #[tokio::test]
    async fn a_permission_request_is_tied_to_its_call() {
        let w = World::new();
        w.hook(tool("PreToolUse", "default", "toolu_a", "rm -rf build"))
            .await;
        w.hook(request("default", "rm -rf build")).await;
        assert_eq!(
            lock(&w.s.deferred)
                .as_ref()
                .and_then(|d| d.tool_use_id.clone())
                .as_deref(),
            Some("toolu_a")
        );
        // A parallel call starting and ending says nothing about it.
        w.hook(tool("PreToolUse", "default", "toolu_b", "ls")).await;
        w.hook(tool("PostToolUse", "default", "toolu_b", "ls"))
            .await;
        assert!(lock(&w.s.deferred).is_some(), "still held");
        past_grace().await;
        assert_eq!(w.requested().len(), 1);
        assert_eq!(
            lock(&w.s.pending)
                .as_ref()
                .and_then(|p| p.tool_use_id.clone())
                .as_deref(),
            Some("toolu_a")
        );
        // Esc at the prompt: only the call's result in the transcript.
        w.mgr.observe_deferred(
            &w.s,
            &HistoryItem::ToolResult {
                id: Some("toolu_a".into()),
                text: String::new(),
                is_error: true,
                at: None,
            },
        );
        assert!(w.s.info().pending_decision.is_none());
    }

    /// Two calls wait on prompts at once: neither replaces the other.
    #[tokio::test]
    async fn parallel_permission_requests_are_shown_one_after_another() {
        let w = World::new();
        w.hook(tool("PreToolUse", "default", "toolu_1", "git push --force"))
            .await;
        w.hook(tool("PreToolUse", "default", "toolu_2", "rm -rf dist"))
            .await;
        w.hook(request("default", "git push --force")).await;
        w.hook(request("default", "rm -rf dist")).await;
        assert_eq!(lock(&w.s.deferred).iter().count(), 2);
        past_grace().await;
        let first = lock(&w.s.pending).as_ref().unwrap().tool_use_id.clone();
        assert_eq!(first.as_deref(), Some("toolu_1"));
        assert_eq!(w.requested().len(), 1, "the second waits its turn");
        assert!(lock(&w.s.deferred).iter().all(|d| d.ready));
        // The first call runs: the second prompt is shown now.
        w.hook(tool(
            "PostToolUse",
            "default",
            "toolu_1",
            "git push --force",
        ))
        .await;
        let second = lock(&w.s.pending).as_ref().unwrap().tool_use_id.clone();
        assert_eq!(second.as_deref(), Some("toolu_2"));
        assert_eq!(w.requested().len(), 2);
        assert!(lock(&w.s.deferred).is_none());
    }

    /// In a mode that settles calls on its own, an allowed long command is
    /// still held when a later call's prompt is reported: the prompt shown
    /// is the later call's.
    #[tokio::test]
    async fn the_prompt_notification_shows_the_call_that_asked() {
        let w = World::new();
        w.hook(tool("PreToolUse", "auto", "toolu_l", LONG)).await;
        w.hook(request("auto", LONG)).await;
        w.hook(tool("PreToolUse", "auto", "toolu_p", "git push --force"))
            .await;
        w.hook(request("auto", "git push --force")).await;
        assert_eq!(lock(&w.s.deferred).iter().count(), 2);
        w.hook(prompt_notice()).await;
        let requested = w.requested();
        assert_eq!(requested.len(), 1, "{requested:?}");
        assert_eq!(requested[0].prompt, "Allow Bash: git push --force?");
        assert_eq!(
            lock(&w.s.pending)
                .as_ref()
                .and_then(|p| p.tool_use_id.clone())
                .as_deref(),
            Some("toolu_p")
        );
        // The running command stays held, silently.
        assert!(
            lock(&w.s.deferred)
                .iter()
                .any(|d| d.tool_use_id.as_deref() == Some("toolu_l"))
        );
    }

    /// A decision held for the session, as a hook reports it.
    async fn held_decision(w: &World) -> Decision {
        w.hook(request("auto", "rm -rf build")).await;
        let d = lock(&w.s.deferred)
            .as_ref()
            .unwrap()
            .pending
            .decision
            .clone();
        lock(&w.s.deferred).clear();
        d
    }

    fn pending_for(decision: Decision, call: &str) -> PendingDecision {
        PendingDecision {
            decision,
            submit: false,
            heuristic: false,
            at: now_millis(),
            tool_use_id: Some(call.to_owned()),
        }
    }

    fn result_of(call: &str) -> HistoryItem {
        HistoryItem::ToolResult {
            id: Some(call.to_owned()),
            text: String::new(),
            is_error: false,
            at: None,
        }
    }

    /// A call that ends while its held decision is being shown never
    /// leaves a prompt behind, whatever the timing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_ending_during_promotion_leaves_no_prompt() {
        let w = World::new();
        let base = held_decision(&w).await;
        for i in 0..300 {
            let call = format!("toolu_{i}");
            let decision = Decision {
                id: format!("{}-{i}", base.id),
                ..base.clone()
            };
            w.mgr.defer_decision(
                &w.s,
                pending_for(decision, &call),
                Some(call.clone()),
                None,
                true,
            );
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let (m1, s1, b1) = (w.mgr.clone(), w.s.clone(), barrier.clone());
            let promote = tokio::task::spawn_blocking(move || {
                b1.wait();
                m1.promote_now(&s1);
            });
            let (m2, s2, c2) = (w.mgr.clone(), w.s.clone(), call.clone());
            let end = tokio::task::spawn_blocking(move || {
                barrier.wait();
                m2.observe_deferred(&s2, &result_of(&c2));
            });
            promote.await.unwrap();
            end.await.unwrap();
            assert!(
                lock(&w.s.pending).is_none(),
                "round {i}: a prompt for an ended call"
            );
            assert!(lock(&w.s.deferred).is_none(), "round {i}");
            assert_ne!(
                w.s.info().state,
                SessionState::AwaitingDecision,
                "round {i}"
            );
        }
    }

    /// A decision that falls due while another prompt is being answered is
    /// shown after it, never left held.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_decision_due_while_the_shown_one_ends_is_never_stranded() {
        let w = World::new();
        let base = held_decision(&w).await;
        for i in 0..300 {
            let shown = Decision {
                id: format!("{}-a{i}", base.id),
                ..base.clone()
            };
            let next = Decision {
                id: format!("{}-b{i}", base.id),
                ..base.clone()
            };
            let (a, b) = (format!("toolu_a{i}"), format!("toolu_b{i}"));
            w.mgr.request_decision(&w.s, pending_for(shown, &a));
            w.mgr.defer_decision(
                &w.s,
                pending_for(next.clone(), &b),
                Some(b.clone()),
                None,
                true,
            );
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let (m1, s1, b1) = (w.mgr.clone(), w.s.clone(), barrier.clone());
            let promote = tokio::task::spawn_blocking(move || {
                b1.wait();
                m1.promote_now(&s1);
            });
            let (m2, s2) = (w.mgr.clone(), w.s.clone());
            let end = tokio::task::spawn_blocking(move || {
                barrier.wait();
                m2.observe_deferred(&s2, &result_of(&a));
            });
            promote.await.unwrap();
            end.await.unwrap();
            assert_eq!(
                lock(&w.s.pending).as_ref().map(|p| p.decision.id.clone()),
                Some(next.id.clone()),
                "round {i}: the next decision was left held"
            );
            w.mgr.observe_deferred(&w.s, &result_of(&b));
            assert!(lock(&w.s.pending).is_none());
        }
    }

    /// An id forgotten (past [`DecisionGens::MAX`]) while one of its
    /// generations is still pending is a repeat of it, and a new
    /// generation keeps an id from being forgotten first.
    #[tokio::test]
    async fn a_forgotten_id_that_is_still_pending_is_no_new_decision() {
        let mut gens = DecisionGens::default();
        gens.insert("d0".into(), 0);
        for i in 1..DecisionGens::MAX {
            gens.insert(format!("d{i}"), 0);
        }
        gens.insert("d0".into(), 1);
        gens.insert("new".into(), 0);
        assert_eq!(gens.get("d0"), Some(1));
        assert_eq!(gens.get("d1"), None);

        let w = World::new();
        let decision = held_decision(&w).await;
        *lock(&w.s.decision_gens) = DecisionGens::default();
        let pending = Decision {
            id: generation_id(&decision.id, 2),
            ..decision.clone()
        };
        *lock(&w.s.pending) = Some(pending_for(pending, "toolu_x"));
        assert!(admit_decision(&w.s, &decision).is_none());
        assert_eq!(generation_of("abc", "abc.2"), Some(2));
        assert_eq!(generation_of("abc", "abc"), Some(0));
        assert_eq!(generation_of("abc", "abcd"), None);
        assert_eq!(generation_of("abc", "abc.x"), None);
    }

    #[test]
    fn network_and_device_folders_are_never_searched() {
        assert!(!searchable_cwd(Path::new("relative/dir")));
        if cfg!(windows) {
            assert!(searchable_cwd(Path::new(r"C:\work\app")));
            assert!(!searchable_cwd(Path::new(r"\\203.0.113.9\share\a")));
            assert!(!searchable_cwd(Path::new(r"\\?\UNC\host\share\a")));
            assert!(!searchable_cwd(Path::new(r"\\.\pipe\x")));
        } else {
            assert!(searchable_cwd(Path::new("/work/app")));
            assert!(!searchable_cwd(Path::new("//host/share")));
        }
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("settings.json");
        std::fs::write(&big, " ".repeat(MAX_SETTINGS_BYTES as usize + 1)).unwrap();
        assert!(
            read_settings(&big).is_none(),
            "oversized settings are skipped"
        );
    }

    #[test]
    fn admission_keys_of_sessions_and_agents_never_meet() {
        let id = SessionId::from("abc");
        assert_eq!(session_admission_key(&id), "session:abc");
        assert_ne!(session_admission_key(&id), format!("agent:{}", id.0));
    }

    #[test]
    fn the_notification_hook_is_found_in_user_project_and_local_settings() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let project = dir.path().join("work").join("app");
        let cwd = project.join("src").join("deep");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        assert!(!notification_hook_installed(&config, Some(&cwd)));
        assert!(!notification_hook_installed(&config, None));
        // Other hooks, or a Notification hook of another tool, do not count.
        std::fs::write(
            config.join("settings.json"),
            r#"{"hooks":{"Notification":[{"hooks":[{"type":"command","command":"notify-send hi"}]}],
               "Stop":[{"hooks":[{"type":"command","command":"vca hook claude stop"}]}]}}"#,
        )
        .unwrap();
        assert!(!notification_hook_installed(&config, Some(&cwd)));
        let local = project.join(".claude").join("settings.local.json");
        write_vca_hooks(&local);
        assert!(notification_hook_installed(&config, Some(&cwd)));
        assert!(!notification_hook_installed(&config, None));
        std::fs::remove_file(&local).unwrap();
        write_vca_hooks(&config.join("settings.json"));
        assert!(notification_hook_installed(&config, None));
    }
}
