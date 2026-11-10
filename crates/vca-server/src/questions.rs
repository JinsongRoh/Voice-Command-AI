//! Question forms: Claude Code's `AskUserQuestion` tool as an answerable
//! decision.
//!
//! * Detection: the transcript tailer sees the tool call
//!   ([`HistoryItem::Question`], fed by [`observe_item`]); the form is open
//!   until the call's result arrives or the conversation moves on. A form
//!   still open after [`SETTLE`] becomes the session's pending decision, so
//!   forms answered long ago do not alert again while a transcript is read
//!   from the start. With hooks installed, the `PreToolUse` /
//!   `PermissionRequest` hook reports the same form under the same
//!   decision id (see `vca_adapters::claude::hook_event`), after the same
//!   grace period (see [`crate::hooks::DECISION_GRACE`]).
//! * Answering ([`answer`]): the answers become the keys a person would
//!   press in the form ([`vca_adapters::claude::answer_keys`]), typed
//!   through the session's own input path: the managed PTY, or the
//!   terminal of an observed session (`vca console-send --keys` for a
//!   Windows console). Where VCA can read the screen (managed PTY, tmux,
//!   `vca console-read` for a Windows console) the form must show freshly
//!   drawn before anything is typed, and typed text must show in its text
//!   field before Enter submits it. The answer counts once the agent took
//!   it (the call's result, the `PostToolUse` hook, or the form gone from
//!   the screen). A form is typed into once. Claude Code's files are never
//!   touched.
//!
//! Answers arrive as `SendInput` ([`vca_core::Input::Answer`]), which needs
//! command scope like answering a permission prompt, and remote requests
//! are written to the audit log (SEC-07).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vca_adapters::HistoryItem;
use vca_adapters::claude::{
    KeyStep, answer_keys, form_on_screen, form_still_open, question_prompt, text_on_screen,
};
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ControlKey, Decision, Error, Input, MessageRole, Question, QuestionAnswer,
    SessionState, now_millis,
};

use crate::hooks::admit_decision;
use crate::sessions::{Kind, PendingDecision, Session, SessionManager};
use crate::util::lock;

/// How long a form must stay open before it alerts: the grace period of
/// every hook or transcript decision.
pub(crate) const SETTLE: Duration = crate::hooks::DECISION_GRACE;
/// How long to wait for the agent to take typed answers (its result, or
/// a screen without the form) before reporting them as not taken.
const CONFIRM: Duration = Duration::from_secs(8);
/// The same where the screen cannot be read: only the agent's result
/// tells, and it may come late (a slow hook, a busy agent).
const CONFIRM_BLIND: Duration = Duration::from_secs(15);
const CONFIRM_POLL: Duration = Duration::from_millis(100);
/// How often the screen is looked at while waiting.
const SCREEN_POLL: Duration = Duration::from_millis(1000);

/// A form the transcript shows as unanswered.
#[derive(Debug, Clone)]
pub(crate) struct OpenQuestion {
    /// The tool call id.
    pub tool_id: String,
    pub questions: Vec<Question>,
    pub at: Option<u64>,
}

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

/// The decision id of the form of tool call `tool_id`: the same id the
/// Claude Code hooks give it (`claude:<agent session>:<tool_use_id>`).
pub(crate) fn decision_id(session: &Session, tool_id: &str) -> String {
    let scope = session
        .agent_session_id()
        .unwrap_or_else(|| session.id.0.clone());
    format!("claude:{scope}:{tool_id}")
}

/// Feeds one transcript item (called by the transcript tailer).
pub(crate) fn observe_item(mgr: &SessionManager, session: &Arc<Session>, item: &HistoryItem) {
    if session.agent() != Some(AgentKind::ClaudeCode) {
        return;
    }
    match item {
        HistoryItem::Question { id, questions, at } => {
            *lock(&session.open_question) = Some(OpenQuestion {
                tool_id: id.clone(),
                questions: questions.clone(),
                at: *at,
            });
            let weak = Arc::downgrade(session);
            let mgr = mgr.clone();
            let tool_id = id.clone();
            session.add_task(tokio::spawn(async move {
                tokio::time::sleep(SETTLE).await;
                if let Some(session) = weak.upgrade() {
                    raise(&mgr, &session, &tool_id);
                }
            }));
        }
        HistoryItem::ToolResult { id: Some(id), .. } => {
            let open = lock(&session.open_question)
                .as_ref()
                .is_some_and(|q| &q.tool_id == id);
            if open {
                close(mgr, session);
            } else {
                // A hook-reported form whose call the tailer saw earlier.
                clear_pending(mgr, session, &decision_id(session, id));
            }
        }
        // The turn moved on: the form is gone (answered or abandoned).
        HistoryItem::Message {
            role: MessageRole::User | MessageRole::Assistant,
            ..
        }
        | HistoryItem::Rewritten => close(mgr, session),
        _ => {}
    }
}

/// Turns the open form into the session's pending decision, unless it was
/// answered meanwhile or is already pending (from a hook).
fn raise(mgr: &SessionManager, session: &Session, tool_id: &str) {
    let Some(open) = lock(&session.open_question)
        .clone()
        .filter(|q| q.tool_id == tool_id)
    else {
        return;
    };
    if matches!(session.info().state, SessionState::Exited { .. }) {
        return;
    }
    let id = decision_id(session, tool_id);
    // Answered from VCA and gone from the screen, its result still to come.
    if lock(&session.typed_answer).as_deref() == Some(id.as_str()) {
        return;
    }
    let decision = Decision {
        id,
        prompt: question_prompt(&open.questions),
        options: Vec::new(),
        inferred: false,
        questions: open.questions,
        command: None,
    };
    let already = lock(&session.pending)
        .as_ref()
        .is_some_and(|p| same_form(&p.decision, &decision.id, &decision.questions));
    if already {
        return;
    }
    if let Some(decision) = admit_decision(session, &decision) {
        tracing::debug!(session = %session.id, "the agent asks a question form");
        mgr.request_decision(
            session,
            PendingDecision {
                decision,
                submit: false,
                heuristic: false,
                at: open.at.unwrap_or_else(now_millis),
                tool_use_id: None,
            },
        );
    }
}

/// Whether `pending` reports the form `id` with `questions`: the same id,
/// or a hook payload without the agent's tool call id (its id is derived
/// from the content) asking the same questions.
fn same_form(pending: &Decision, id: &str, questions: &[Question]) -> bool {
    pending.id == id || (pending.is_question() && pending.questions == questions)
}

/// The open form ended: forgets it and drops its pending decision.
fn close(mgr: &SessionManager, session: &Session) {
    let Some(open) = lock(&session.open_question).take() else {
        return;
    };
    let id = decision_id(session, &open.tool_id);
    clear_if(mgr, session, |p| same_form(p, &id, &open.questions));
}

/// Drops the pending decision `id`, if it is the pending one; the session
/// runs again.
fn clear_pending(mgr: &SessionManager, session: &Session, id: &str) {
    clear_if(mgr, session, |p| p.id == id);
}

/// Drops the pending decision if `matches` says so; the session runs again.
/// A matching decision still in its grace period is dropped silently.
fn clear_if(mgr: &SessionManager, session: &Session, matches: impl Fn(&Decision) -> bool) {
    mgr.drop_deferred(session, |d| matches(&d.pending.decision));
    let taken = {
        let mut pending = lock(&session.pending);
        if pending.as_ref().is_some_and(|p| matches(&p.decision)) {
            pending.take()
        } else {
            None
        }
    };
    if taken.is_none() {
        return;
    }
    if session.info().state == SessionState::AwaitingDecision {
        mgr.set_state(session, SessionState::Running);
    } else {
        mgr.notify(session);
    }
}

/// Whether the decision `id` is still the pending one.
fn still_pending(session: &Session, id: &str) -> bool {
    lock(&session.pending)
        .as_ref()
        .is_some_and(|p| p.decision.id == id)
}

/// Keys typed in one go, then (with `check`) the typed text that the
/// screen must show in the form before the next segment presses Enter.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    steps: Vec<KeyStep>,
    check: Option<String>,
}

/// Splits answer keys before every Enter that submits typed text: the
/// text is checked on the screen first, so a text that went anywhere but
/// the form's text field is never submitted.
fn segments(steps: Vec<KeyStep>) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut last_text: Option<String> = None;
    for step in steps {
        match &step {
            KeyStep::Text { text } if text != " " => last_text = Some(text.clone()),
            KeyStep::Key {
                key: ControlKey::Enter,
            } => {
                if let Some(text) = last_text.take() {
                    out.push(Segment {
                        steps: std::mem::take(&mut current),
                        check: Some(text),
                    });
                }
            }
            KeyStep::Pause { .. } => {}
            _ => last_text = None,
        }
        current.push(step);
    }
    if !current.is_empty() {
        out.push(Segment {
            steps: current,
            check: None,
        });
    }
    out
}

/// Where the answer keys go.
enum Channel<'a> {
    Pty,
    Console(&'a vca_adapters::terminal::TerminalTarget),
}

impl Channel<'_> {
    fn of(session: &Session) -> Result<Channel<'_>, Error> {
        match &session.kind {
            Kind::Managed(_) => Ok(Channel::Pty),
            Kind::Observed(o) => o
                .terminal
                .as_ref()
                .map(Channel::Console)
                .ok_or_else(|| api_error(ErrorCode::Unsupported, "no input channel")),
            Kind::Cloud(_) => Err(api_error(
                ErrorCode::Unsupported,
                "cloud sessions have no question forms",
            )),
        }
    }

    /// The visible screen, where it can be read.
    async fn screen(&self, session: &Session) -> Option<String> {
        match self {
            Channel::Pty => session.screen_text(),
            Channel::Console(target) => vca_adapters::terminal::screen_text(target).await,
        }
    }

    async fn type_steps(&self, session: &Session, steps: &[KeyStep]) -> Result<(), Error> {
        match self {
            Channel::Pty => type_into_pty(session, steps).await,
            Channel::Console(target) => vca_adapters::terminal::send_keys(target, steps)
                .await
                .map_err(|e| api_error(ErrorCode::Unavailable, e.to_string())),
        }
    }
}

/// Types the answers to the pending form `decision_id` into the session's
/// terminal and waits until the agent took them.
///
/// Where the screen can be read (a managed session, tmux, a Windows
/// console), the form must be on it freshly drawn before anything is
/// typed, typed text must show in the form's text field before Enter
/// submits it, and a form gone from the screen counts as taken even when
/// the agent's result is slow to arrive. A form is typed into once: a
/// repeated answer (a retry after a dropped connection, say) is refused.
pub(crate) async fn answer(
    mgr: &SessionManager,
    session: &Session,
    decision_id: &str,
    answers: &[QuestionAnswer],
) -> Result<(), Error> {
    let questions = lock(&session.pending)
        .as_ref()
        .filter(|p| p.decision.id == decision_id && p.decision.is_question())
        .map(|p| p.decision.questions.clone())
        .ok_or_else(|| api_error(ErrorCode::Conflict, "that question is no longer pending"))?;
    if lock(&session.typed_answer).as_deref() == Some(decision_id) {
        return Err(api_error(
            ErrorCode::Conflict,
            "answers to this question were already typed into the terminal; check it there",
        ));
    }
    let steps = answer_keys(&questions, answers)
        .map_err(|e| api_error(ErrorCode::InvalidRequest, e.to_string()))?;
    let channel = Channel::of(session)?;
    if matches!(channel, Channel::Pty) && session.pty().is_none() {
        return Err(api_error(ErrorCode::Unsupported, "no input channel"));
    }
    if let Some(screen) = channel.screen(session).await
        && !form_on_screen(&screen, &questions)
    {
        return Err(api_error(
            ErrorCode::Conflict,
            "the terminal does not show the question as it was asked; answer it in the terminal",
        ));
    }
    // From the first key on, the form is not as it was asked any more.
    *lock(&session.typed_answer) = Some(decision_id.to_owned());
    for segment in segments(steps) {
        channel.type_steps(session, &segment.steps).await?;
        if let Some(text) = &segment.check
            && let Some(screen) = channel.screen(session).await
            && !text_on_screen(&screen, text)
        {
            tracing::warn!(session = %session.id, "a typed answer did not show in the question form");
            return Err(api_error(
                ErrorCode::Conflict,
                "the typed answer did not show in the question's text field, so it was not \
                 submitted; answer it in the terminal",
            ));
        }
    }
    tracing::info!(session = %session.id, questions = questions.len(), "typed answers to a question form");
    confirm(mgr, session, &channel, decision_id, &questions).await
}

/// Waits until the agent took the typed answers: its result (or hook)
/// clears the pending form. A screen that no longer shows the form counts
/// too (the result may come late); the form is then cleared here.
async fn confirm(
    mgr: &SessionManager,
    session: &Session,
    channel: &Channel<'_>,
    decision_id: &str,
    questions: &[Question],
) -> Result<(), Error> {
    let start = Instant::now();
    let mut next_look = start + SCREEN_POLL;
    let mut readable = false;
    loop {
        if !still_pending(session, decision_id) {
            return Ok(());
        }
        let now = Instant::now();
        if now >= next_look {
            next_look = now + SCREEN_POLL;
            if let Some(screen) = channel.screen(session).await {
                readable = true;
                if !form_still_open(&screen, questions) {
                    tracing::info!(session = %session.id, "the question form left the screen");
                    clear_pending(mgr, session, decision_id);
                    return Ok(());
                }
            }
        }
        let limit = if readable { CONFIRM } else { CONFIRM_BLIND };
        if start.elapsed() >= limit {
            return Err(if readable {
                api_error(
                    ErrorCode::Unavailable,
                    "the answers were typed, but the question is still open in the terminal; \
                     answer it there",
                )
            } else {
                api_error(
                    ErrorCode::Unavailable,
                    "the answers were typed, but the agent has not taken them yet; check its \
                     terminal",
                )
            });
        }
        tokio::time::sleep(CONFIRM_POLL).await;
    }
}

/// Types the steps into a managed session's PTY, one write per key.
async fn type_into_pty(session: &Session, steps: &[KeyStep]) -> Result<(), Error> {
    let Some(pty) = session.pty() else {
        return Err(api_error(ErrorCode::Unsupported, "no input channel"));
    };
    for step in steps {
        let input = match step {
            KeyStep::Key { key } => Input::Key { key: *key },
            KeyStep::Text { text } => Input::Text {
                text: text.clone(),
                submit: false,
                at_prompt: true,
            },
            KeyStep::Pause { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                continue;
            }
        };
        pty.send_input(&input)
            .await
            .map_err(|e| api_error(ErrorCode::Unavailable, e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use vca_broker::contain::Scope;
    use vca_core::{ControlTier, SessionEvent, SessionId, SessionInfo, SessionOrigin};

    use super::*;
    use crate::events::LogLimits;
    use crate::sessions::{ManagerConfig, Observed, ObservedIdentity};
    use crate::workspace::WorkspaceRoots;

    const ASK: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_q","name":"AskUserQuestion","input":{"questions":[{"question":"Which price?","header":"Price","options":[{"label":"Ship price (Recommended)","description":"usual"},{"label":"Per field","description":""}],"multiSelect":false},{"question":"Costs?","header":"Costs","options":[{"label":"Hardware","description":""},{"label":"Licenses","description":""}],"multiSelect":true}]}}]},"timestamp":"2026-11-01T10:00:00.000Z"}"#;
    const RESULT: &str = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_q","type":"tool_result","content":"User has answered your questions"}]},"toolUseResult":{}}"#;
    const ID: &str = "claude:abc:toolu_q";

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

    /// An observed Claude Code session in a terminal VCA cannot type into.
    fn observe(mgr: &SessionManager) -> Arc<Session> {
        let info = SessionInfo {
            id: SessionId::new(),
            origin: SessionOrigin::Local {
                agent: AgentKind::ClaudeCode,
                project_dir: PathBuf::from("/code/shop"),
                pid: Some(4_242_425),
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
        mgr.insert_observed(info, observed, Some("abc".into()))
    }

    fn feed(mgr: &SessionManager, s: &Arc<Session>, line: &str) {
        for item in vca_adapters::claude::transcript::parse_line(line) {
            observe_item(mgr, s, &item);
        }
    }

    fn requested(s: &Session) -> Vec<Decision> {
        lock(s.log())
            .replay_after(0)
            .events
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::DecisionRequested { decision, .. } => Some(decision),
                _ => None,
            })
            .collect()
    }

    async fn settle() {
        tokio::time::sleep(SETTLE + Duration::from_millis(300)).await;
    }

    #[tokio::test]
    async fn an_open_form_becomes_a_question_decision_until_its_result() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        feed(&mgr, &s, ASK);
        assert!(requested(&s).is_empty(), "not before it settled");
        settle().await;
        let requested = requested(&s);
        assert_eq!(requested.len(), 1, "{requested:?}");
        let d = &requested[0];
        assert_eq!(d.id, ID);
        assert_eq!(d.prompt, "[Price] Which price? (+1)");
        assert!(d.options.is_empty() && !d.inferred);
        assert_eq!(d.questions.len(), 2);
        let info = s.info();
        assert_eq!(info.state, SessionState::AwaitingDecision);
        assert_eq!(info.unread, 1);
        assert!(info.pending_decision.unwrap().decision.is_question());

        feed(&mgr, &s, RESULT);
        let info = s.info();
        assert!(info.pending_decision.is_none());
        assert_eq!(info.state, SessionState::Running);
    }

    #[tokio::test]
    async fn answered_forms_do_not_alert_when_a_transcript_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        feed(&mgr, &s, ASK);
        feed(&mgr, &s, RESULT);
        // A form the conversation moved past is gone too.
        let other = ASK.replace("toolu_q", "toolu_r");
        feed(&mgr, &s, &other);
        feed(
            &mgr,
            &s,
            r#"{"type":"user","message":{"role":"user","content":"never mind"},"origin":{"kind":"human"}}"#,
        );
        settle().await;
        assert!(requested(&s).is_empty());
        assert!(s.info().pending_decision.is_none());
    }

    #[tokio::test]
    async fn a_form_answered_within_the_grace_period_never_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        feed(&mgr, &s, ASK);
        tokio::time::sleep(Duration::from_secs(1)).await;
        feed(&mgr, &s, RESULT);
        settle().await;
        assert!(requested(&s).is_empty());
        let info = s.info();
        assert!(info.pending_decision.is_none());
        assert_eq!(info.state, SessionState::Running);
        assert_eq!(info.unread, 0);
    }

    #[tokio::test]
    async fn hook_and_transcript_report_one_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        let input = r#"{"questions":[{"question":"Which price?","header":"Price","options":[{"label":"Ship price (Recommended)","description":"usual"},{"label":"Per field","description":""}],"multiSelect":false}]}"#;
        let pre = format!(
            r#"{{"session_id":"abc","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"toolu_q","tool_input":{input}}}"#
        );
        let event = vca_adapters::claude::ClaudeHookPayload::parse(&pre)
            .unwrap()
            .to_agent_event();
        mgr.hook_for(&s.id, event).await;
        // Nothing shows before the grace period ends.
        assert_eq!(s.info().state, SessionState::Running);
        assert!(s.info().pending_decision.is_none());
        feed(&mgr, &s, ASK);
        settle().await;
        assert_eq!(s.info().state, SessionState::AwaitingDecision);
        let ids: Vec<String> = requested(&s).into_iter().map(|d| d.id).collect();
        assert_eq!(ids, [ID]);
        // The PostToolUse hook ends it.
        let post = pre.replace("PreToolUse", "PostToolUse");
        let event = vca_adapters::claude::ClaudeHookPayload::parse(&post)
            .unwrap()
            .to_agent_event();
        mgr.hook_for(&s.id, event).await;
        assert!(s.info().pending_decision.is_none());
    }

    #[tokio::test]
    async fn a_hook_without_the_tool_id_is_the_same_form() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        // The transcript's form, as a permission hook without `tool_use_id`.
        let input = ASK
            .split_once(r#""input":"#)
            .and_then(|(_, rest)| {
                rest.strip_suffix(r#"}]},"timestamp":"2026-11-01T10:00:00.000Z"}"#)
            })
            .unwrap();
        let hook = format!(
            r#"{{"session_id":"abc","hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{input}}}"#
        );
        let event = vca_adapters::claude::ClaudeHookPayload::parse(&hook)
            .unwrap()
            .to_agent_event();
        mgr.hook_for(&s.id, event).await;
        // The transcript shows the call a moment later.
        tokio::time::sleep(Duration::from_millis(100)).await;
        feed(&mgr, &s, ASK);
        settle().await;
        assert_eq!(requested(&s).len(), 1);
        let hook_id = s.info().pending_decision.unwrap().decision.id;
        assert_ne!(hook_id, ID);
        // Its result ends the hook's decision too.
        feed(&mgr, &s, RESULT);
        assert!(s.info().pending_decision.is_none());
        assert_eq!(s.info().state, SessionState::Running);
    }

    fn code(err: Error) -> ErrorCode {
        match err {
            Error::Api { code, .. } => code,
            other => panic!("{other:?}"),
        }
    }

    async fn send(mgr: &SessionManager, s: &Session, input: Input) -> ErrorCode {
        let done = mgr.send_input(&s.id, input).unwrap();
        code(done.await.unwrap().unwrap_err())
    }

    fn answer(id: &str, answers: Vec<QuestionAnswer>) -> Input {
        Input::Answer {
            decision_id: id.into(),
            answers,
        }
    }

    #[tokio::test]
    async fn answers_are_checked_before_anything_is_typed() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        let pick = |o: u32| QuestionAnswer {
            options: vec![o],
            text: None,
        };
        let both = || vec![pick(0), pick(1)];
        // Nothing pending.
        assert_eq!(
            send(&mgr, &s, answer(ID, both())).await,
            ErrorCode::Conflict
        );
        feed(&mgr, &s, ASK);
        settle().await;
        let other = answer("claude:abc:other", both());
        assert_eq!(send(&mgr, &s, other).await, ErrorCode::Conflict);
        // One answer for two questions; an option that does not exist.
        let short = answer(ID, vec![pick(0)]);
        assert_eq!(send(&mgr, &s, short).await, ErrorCode::InvalidRequest);
        let missing = answer(ID, vec![pick(5), pick(0)]);
        assert_eq!(send(&mgr, &s, missing).await, ErrorCode::InvalidRequest);
        // Valid, but this terminal takes no keys from VCA.
        assert_eq!(
            send(&mgr, &s, answer(ID, both())).await,
            ErrorCode::Unsupported
        );
        // Still pending: the person can answer at the terminal.
        assert!(s.info().pending_decision.is_some());
        // A permission-style answer does not fit a form.
        let choose = Input::Choose {
            decision_id: ID.into(),
            option_id: "1".into(),
        };
        assert_eq!(send(&mgr, &s, choose).await, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn a_form_is_typed_into_once() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        feed(&mgr, &s, ASK);
        settle().await;
        let pick = |o: u32| QuestionAnswer {
            options: vec![o],
            text: None,
        };
        // An earlier answer reached the terminal (its result still to come):
        // a second one (a retry after a dropped connection) types nothing.
        *lock(&s.typed_answer) = Some(ID.into());
        assert_eq!(
            send(&mgr, &s, answer(ID, vec![pick(0), pick(1)])).await,
            ErrorCode::Conflict
        );
        assert!(s.info().pending_decision.is_some());
    }

    #[tokio::test]
    async fn a_typed_form_does_not_alert_again() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(&dir.path().join("names.json"));
        let s = observe(&mgr);
        *lock(&s.typed_answer) = Some(ID.into());
        feed(&mgr, &s, ASK);
        settle().await;
        assert!(requested(&s).is_empty());
    }

    fn names(segment: &Segment) -> Vec<String> {
        segment
            .steps
            .iter()
            .filter_map(|s| match s {
                KeyStep::Key { key } => Some(format!("{key:?}")),
                KeyStep::Text { text } => Some(format!("'{text}'")),
                KeyStep::Pause { .. } => None,
            })
            .collect()
    }

    #[test]
    fn typed_text_is_checked_before_its_enter() {
        let q = |multi: bool| Question {
            header: "H".into(),
            question: "Q?".into(),
            options: ["a", "b", "c"]
                .iter()
                .map(|l| vca_core::QuestionOption {
                    label: (*l).into(),
                    description: String::new(),
                })
                .collect(),
            multi_select: multi,
            free_text: true,
        };
        let typed = |t: &str| QuestionAnswer {
            options: Vec::new(),
            text: Some(t.into()),
        };
        // The field report: one question, typed text.
        let steps = answer_keys(&[q(false)], &[typed("PC B,C 모두")]).unwrap();
        let parts = segments(steps);
        assert_eq!(parts.len(), 2);
        assert_eq!(names(&parts[0]), ["Down", "Down", "Down", "'PC B,C 모두'"]);
        assert_eq!(parts[0].check.as_deref(), Some("PC B,C 모두"));
        assert_eq!(names(&parts[1]), ["Enter"]);
        assert_eq!(parts[1].check, None);
        // An option pick needs no check; a check box is not text.
        let pick = QuestionAnswer {
            options: vec![1],
            text: None,
        };
        let parts = segments(answer_keys(&[q(false)], std::slice::from_ref(&pick)).unwrap());
        assert_eq!(parts.len(), 1);
        assert_eq!(names(&parts[0]), ["Down", "Enter"]);
        let parts = segments(answer_keys(&[q(true)], &[pick]).unwrap());
        assert_eq!(parts.len(), 1);
        // Multiple choice text is followed by Down (to the button), not Enter.
        let parts = segments(answer_keys(&[q(true)], &[typed("z")]).unwrap());
        assert_eq!(parts.len(), 1);
        // Two questions with text each: a check before each text's Enter.
        let parts =
            segments(answer_keys(&[q(false), q(false)], &[typed("x"), typed("y")]).unwrap());
        let checks: Vec<_> = parts.iter().map(|p| p.check.as_deref()).collect();
        assert_eq!(checks, [Some("x"), Some("y"), None]);
        assert_eq!(names(&parts[2]), ["Enter", "Enter"]);
    }
}
