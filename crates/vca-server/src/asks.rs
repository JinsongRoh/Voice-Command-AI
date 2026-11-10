//! Turn-level facts about an agent session: its permission mode, and the
//! plain question its final message of a turn asks the person.
//!
//! # Permission mode
//!
//! Claude Code reports its permission mode in hook payloads
//! (`permission_mode`) and in the transcript (`permission-mode` records).
//! In `auto`, `dontAsk` and `bypassPermissions` the agent settles tool calls
//! without a prompt, so prompts are not inferred from a quiet process tree
//! there (see [`crate::observed_watch`]).
//!
//! # Questions ("Question" alerts)
//!
//! An agent often ends its turn by asking the person something in plain
//! text ("어떻게 할까요?" (how should I go on?), "Should I push?") instead
//! of a form or a permission prompt. When a turn ends (the `Stop` hook with its
//! `last_assistant_message`, or the transcript's `turn_duration` record
//! after the final assistant message) and that message asks, the session
//! gets [`SessionInfo::question`](vca_core::SessionInfo::question): the
//! last question sentence, shortened. Clients list it in the Alerts menu,
//! count it in the badge and announce it.
//!
//! The rule list ([`question_excerpt`]), code blocks removed:
//!
//! 1. In the message's last paragraph (a trailing option list counts
//!    together with the paragraph before it), a sentence ends with `?` (or
//!    the full-width `？`), contains a request phrase of
//!    [`REQUEST_PHRASES`] (Korean, English, Japanese, Spanish; matched
//!    case-insensitively), or starts with "Which ".
//! 2. Anywhere in the message, a line names decisions left to the person
//!    ([`DECISION_CUES`], e.g. "대표님이 정하실 것 3가지" (3 things for you
//!    to decide) or "Decisions for you") and does not deny it
//!    ("승인은 필요 없습니다", no approval needed). Long reports often put
//!    that list in the middle and end with a status line.
//!
//! Limits: at most one question per turn and session; none for the
//! conductor's own session, remote shells, or a session that already waits
//! on a decision; none for turns that ended before VCA read them (a
//! transcript read from the start), see [`FRESH_TURN`]. The question stays
//! until the person writes to the session (a typed prompt) or a later turn
//! asks a new one: the agent working on, a message from another session or
//! a background task finishing does not answer it.

use vca_adapters::{AgentHookEvent, HistoryItem};
use vca_core::{AgentQuestion, MessageRole, SessionState, UnixMillis, now_millis};

use crate::sessions::{Session, SessionManager};
use crate::util::lock;

/// A transcript turn end older than this was not seen live (the transcript
/// is being read from the start) and raises nothing.
pub(crate) const FRESH_TURN: u64 = 120_000;

/// Longest question excerpt, in characters.
const MAX_EXCERPT: usize = 160;

/// Most bytes of a final message kept and scanned for a question: its end,
/// where the question is. A multi-megabyte report is not kept twice.
const MAX_SCANNED_BYTES: usize = 64 * 1024;

/// A transcript turn end this far in the future is skewed or malformed and
/// does not count as fresh.
const FUTURE_SLACK: u64 = 5_000;

/// Request phrases that make a sentence a question to the person, lower
/// case. Matched within one sentence of the final paragraph.
pub(crate) const REQUEST_PHRASES: &[&str] = &[
    // Korean
    "정해 주세요",
    "정해주세요",
    "알려 주세요",
    "알려주세요",
    "어떻게 할까요",
    "어떻게 하면 될까요",
    "선택해 주세요",
    "선택해주세요",
    "골라 주세요",
    "골라주세요",
    "결정해 주세요",
    "결정해주세요",
    "확인 부탁",
    "확인해 주세요",
    "확인해주세요",
    "말씀해 주세요",
    "말씀해주세요",
    "승인해 주세요",
    "승인해주세요",
    "의견 주세요",
    "할까요",
    "될까요",
    "괜찮을까요",
    "하시겠어요",
    "하시겠습니까",
    "원하시나요",
    "원하세요",
    "해 주세요",
    "해주세요",
    "해 주십시오",
    "부탁드립니다",
    "부탁합니다",
    "주시겠어요",
    "주시겠습니까",
    // English
    "should i ",
    "shall i ",
    "do you want",
    "would you like",
    "let me know",
    "please confirm",
    "please choose",
    "please decide",
    "please pick",
    "want me to",
    "which option",
    "which one",
    "your call",
    // Japanese
    "どうしますか",
    "どちらにしますか",
    "しましょうか",
    "教えてください",
    "選んでください",
    "決めてください",
    "確認してください",
    "よろしいですか",
    "いかがですか",
    // Spanish
    "¿quieres",
    "¿quiere",
    "¿prefieres",
    "¿prefiere",
    "¿debo",
    "¿cuál",
    "¿te parece",
    "avísame",
    "indícame",
    "confírmame",
    "elige ",
];

/// Phrases that name decisions or approvals left to the person, lower case.
/// Matched on any line of the message (see the module docs).
pub(crate) const DECISION_CUES: &[&str] = &[
    // Korean
    "정하실 것",
    "정하실 일",
    "정하실 사항",
    "정해 주실",
    "결정하실",
    "결정해 주실",
    "결정이 필요",
    "결정 필요",
    "승인이 필요",
    "승인 필요",
    "승인 요청",
    "승인을 요청",
    "승인해 주실",
    "판단이 필요",
    "판단해 주실",
    "확인이 필요한 것",
    "답해 주실",
    "대표님 결정",
    "대표님 승인",
    "대표님 확인",
    "대표님이 정",
    "대표님이 결정",
    "대표님이 승인",
    "대표님이 확인",
    // English
    "decisions for you",
    "decision for you",
    "decisions needed",
    "decision needed",
    "needs your decision",
    "need your decision",
    "needs your approval",
    "need your approval",
    "needs your sign-off",
    "awaiting your",
    "waiting on you",
    "waiting for your",
    "for you to decide",
    // Japanese
    "ご判断ください",
    "ご判断をお願い",
    "承認が必要",
    "決めていただき",
    // Spanish
    "decisiones para ti",
    "necesito tu aprobación",
    "necesito tu decisión",
];

/// Words that turn a decision cue into its denial ("승인은 필요 없습니다",
/// no approval needed).
const CUE_DENIALS: &[&str] = &[
    "필요 없",
    "필요없",
    "필요하지 않",
    "것은 없",
    "건 없",
    "no decision",
    "nothing to decide",
    "not need",
    "no approval",
    "don't need",
    "不要",
    "no necesit",
];

/// Per-session turn bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct TurnState {
    /// The agent's permission mode, as last reported.
    pub(crate) permission_mode: Option<String>,
    /// The latest assistant text of the current turn (`None` once a tool
    /// call followed it).
    last_text: Option<String>,
    /// A question was raised for the current turn.
    alerted: bool,
    /// The open question, shown on the session card.
    pub(crate) question: Option<OpenAsk>,
    /// Questions raised so far (part of the question id).
    count: u64,
}

/// The open question and the message it came from.
#[derive(Debug, Clone)]
pub(crate) struct OpenAsk {
    pub(crate) info: AgentQuestion,
    /// SHA-256 of the scanned message ([`scanned`]), to tell its own
    /// transcript copy from new text without keeping the message.
    source: [u8; 32],
}

/// The end of `text` that is scanned for a question (at most
/// [`MAX_SCANNED_BYTES`]). The cut starts a whole line where there is
/// one, and never inside a code block: a cut that lands in one starts
/// after its closing fence instead (or yields nothing when the block runs
/// to the end), so [`strip_code`] does not read the rest of the block as
/// prose and the prose after it as code. Within the limit `text` is
/// returned whole, so this is idempotent.
fn scanned(text: &str) -> &str {
    if text.len() <= MAX_SCANNED_BYTES {
        return text;
    }
    let mut start = text.len() - MAX_SCANNED_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    // A cut line is a fragment, and a fragment of a fence line misleads.
    if text.as_bytes()[start - 1] != b'\n'
        && let Some(nl) = text[start..].find('\n')
    {
        start += nl + 1;
    }
    let fenced = text[..start].lines().filter(|l| is_fence(l)).count() % 2 == 1;
    let tail = &text[start..];
    if !fenced {
        return tail;
    }
    let mut offset = 0;
    for line in tail.split_inclusive('\n') {
        offset += line.len();
        if is_fence(line) {
            return &tail[offset..];
        }
    }
    ""
}

/// A line opening or closing a fenced code block (see [`strip_code`]).
fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn digest(text: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(scanned(text).as_bytes()).into()
}

/// Was a turn that ended at `at` seen live? Not when it is older than
/// [`FRESH_TURN`], nor when it lies in the future (a skewed clock).
fn fresh_turn(at: UnixMillis, now: UnixMillis) -> bool {
    at <= now.saturating_add(FUTURE_SLACK) && now.saturating_sub(at) <= FRESH_TURN
}

impl TurnState {
    /// True when the last reported permission mode settles tool calls
    /// without a prompt.
    pub(crate) fn mode_settles_without_prompt(&self) -> bool {
        self.permission_mode
            .as_deref()
            .is_some_and(vca_adapters::hook::mode_settles_without_prompt)
    }
}

/// Feeds one transcript item (called by the transcript tailer).
pub(crate) fn observe_item(mgr: &SessionManager, session: &Session, item: &HistoryItem) {
    let mut cleared = false;
    let mut ended = None;
    {
        let mut turn = lock(&session.turn);
        match item {
            HistoryItem::PermissionMode { mode, .. } => {
                turn.permission_mode = Some(mode.clone());
            }
            HistoryItem::Message {
                role: MessageRole::Assistant,
                text,
                ..
            } => {
                // The agent going on does not answer an open question.
                turn.last_text = Some(scanned(text).to_owned());
            }
            HistoryItem::Message {
                role: MessageRole::User,
                ..
            } => {
                // The person wrote to the session: the question is answered.
                cleared |= turn.question.take().is_some();
                turn.last_text = None;
                turn.alerted = false;
            }
            HistoryItem::Message {
                role: MessageRole::System,
                ..
            } => {
                // A turn the person did not start (a peer message, a
                // background task's notice): it may ask again, but the open
                // question stays.
                turn.last_text = None;
                turn.alerted = false;
            }
            HistoryItem::ToolCall { .. } => {
                turn.last_text = None;
            }
            HistoryItem::Rewritten => {
                turn.last_text = None;
            }
            HistoryItem::TurnEnd { at } => {
                // The turn is over: its text is not kept either way.
                let text = turn.last_text.take();
                if at.is_some_and(|at| fresh_turn(at, now_millis())) {
                    ended = text;
                }
            }
            _ => {}
        }
    }
    if cleared {
        mgr.notify(session);
    }
    if let Some(text) = ended {
        raise(mgr, session, &text);
    }
}

/// Applies a hook event: the permission mode, a new prompt, the agent at
/// work again, or the final message of a turn.
pub(crate) fn on_hook(mgr: &SessionManager, session: &Session, event: &AgentHookEvent) {
    let mut cleared = false;
    {
        let mut turn = lock(&session.turn);
        if let Some(mode) = &event.permission_mode {
            turn.permission_mode = Some(mode.clone());
        }
        match event.event_name.as_str() {
            "UserPromptSubmit" => {
                cleared |= turn.question.take().is_some();
                turn.last_text = None;
                turn.alerted = false;
            }
            // The agent at work again (also after a peer message, which
            // fires no prompt hook): a new turn may ask again. The open
            // question stays until the person answers.
            _ if event.state == Some(SessionState::Running) => {
                turn.alerted = false;
            }
            _ => {}
        }
    }
    if cleared {
        mgr.notify(session);
    }
    if event.state == Some(SessionState::Idle) {
        if let Some(text) = &event.last_message {
            raise(mgr, session, text);
        }
    }
}

/// Raises the question of `text`, the final message of a turn, if it asks
/// one and the session may alert.
fn raise(mgr: &SessionManager, session: &Session, text: &str) {
    if session.is_conductor() || session.is_shell() {
        return;
    }
    // A decision already asks the person.
    if lock(&session.pending).is_some() || lock(&session.deferred).is_some() {
        return;
    }
    let text = scanned(text);
    let Some(excerpt) = question_excerpt(text) else {
        return;
    };
    let source = digest(text);
    {
        let mut turn = lock(&session.turn);
        // The transcript copy of a message the `Stop` hook already raised.
        if turn.alerted || turn.question.as_ref().is_some_and(|q| q.source == source) {
            return;
        }
        // A decision may have been requested while the excerpt was found.
        if lock(&session.pending).is_some() || lock(&session.deferred).is_some() {
            return;
        }
        turn.alerted = true;
        turn.count += 1;
        let at: UnixMillis = now_millis();
        turn.question = Some(OpenAsk {
            info: AgentQuestion {
                id: format!("{at}-{}", turn.count),
                text: excerpt,
                at,
            },
            source,
        });
    }
    tracing::info!(session = %session.id, "the agent's turn ended with a question");
    mgr.add_unread(session);
    mgr.notify(session);
}

/// The question `message` asks the person, if any: the last matching
/// sentence of its last paragraph, shortened (see the module docs). Only
/// the end of a very long message is read.
pub fn question_excerpt(message: &str) -> Option<String> {
    let text = strip_code(scanned(message));
    let paragraphs = paragraphs(&text);
    let last = paragraphs.last()?;
    let mut scope = vec![last.as_str()];
    // "어떻게 할까요?\n\n1. A\n2. B" (how should I go on? 1. A 2. B): the
    // options follow the question.
    if is_option_list(last) && paragraphs.len() > 1 {
        scope.insert(0, paragraphs[paragraphs.len() - 2].as_str());
    }
    let sentences: Vec<String> = scope.iter().flat_map(|p| sentences(p)).collect();
    if let Some(found) = sentences.iter().rev().find(|s| asks(s)) {
        return Some(shorten(found));
    }
    decision_list(&text)
}

/// A line naming decisions left to the person, anywhere in `text`, with the
/// first item of the list under it: "대표님이 정하실 것 3가지: PC C 오늘 밤 측정 시작"
/// (3 things for you to decide: start the PC C measurement tonight).
fn decision_list(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        if !DECISION_CUES.iter().any(|c| lower.contains(c))
            || CUE_DENIALS.iter().any(|d| lower.contains(d))
        {
            continue;
        }
        let head = clean_line(line);
        let head = head.trim_end_matches([':', '：']).trim();
        if head.is_empty() {
            continue;
        }
        let first = lines[i + 1..]
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .filter(|l| is_option_list(l))
            .map(|l| clean_line(&strip_marker(l)));
        return Some(shorten(&match first {
            Some(item) if !item.is_empty() => format!("{head}: {item}"),
            _ => head.to_owned(),
        }));
    }
    None
}

/// The line without Markdown emphasis or heading marks.
fn clean_line(line: &str) -> String {
    line.trim()
        .trim_start_matches(['#', '>', ' '])
        .replace("**", "")
        .replace("__", "")
        .trim()
        .to_owned()
}

/// A list item without its bullet or number.
fn strip_marker(item: &str) -> String {
    let t = item.trim_start();
    if let Some(rest) = t.strip_prefix(['-', '*', '•']) {
        return rest.trim().to_owned();
    }
    match t.split_once(['.', ')']) {
        Some((n, rest))
            if !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            rest.trim().to_owned()
        }
        _ => t.to_owned(),
    }
}

fn asks(sentence: &str) -> bool {
    let s = sentence
        .trim()
        .trim_end_matches(['*', '_', '`', ')', '"', '\'', '」', '』', ' '])
        .to_lowercase();
    if s.ends_with('?') || s.ends_with('？') {
        return true;
    }
    let plain = s.trim_start_matches(['*', '_', '-', ' ', '>']);
    plain.starts_with("which ") || REQUEST_PHRASES.iter().any(|p| s.contains(p))
}

/// The text without fenced code blocks.
fn strip_code(text: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if is_fence(line) {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.trim().is_empty() {
                out.push(std::mem::take(&mut current));
            }
            current.clear();
        } else {
            current.push_str(line.trim_end());
            current.push('\n');
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

fn is_option_list(paragraph: &str) -> bool {
    paragraph.lines().filter(|l| !l.trim().is_empty()).all(|l| {
        let l = l.trim_start();
        l.starts_with(['-', '*', '•'])
            || l.split_once(['.', ')']).is_some_and(|(n, _)| {
                !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()) && n.len() <= 2
            })
    })
}

/// Splits a paragraph into sentences: at line breaks, after `。`, `？`,
/// `！`, and after `.`, `?`, `!` followed by a space or the end.
fn sentences(paragraph: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in paragraph.lines() {
        let mut current = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            current.push(c);
            let ends = matches!(c, '。' | '？' | '！')
                || (matches!(c, '.' | '?' | '!') && chars.peek().is_none_or(|n| n.is_whitespace()));
            if ends {
                if !current.trim().is_empty() {
                    out.push(current.trim().to_owned());
                }
                current.clear();
            }
        }
        if !current.trim().is_empty() {
            out.push(current.trim().to_owned());
        }
    }
    out
}

fn shorten(sentence: &str) -> String {
    let s = sentence
        .trim()
        .trim_start_matches(['*', '_', '-', '>', ' '])
        .replace("**", "");
    match s.char_indices().nth(MAX_EXCERPT) {
        None => s,
        Some((idx, _)) => format!("{}…", &s[..idx]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_questions_at_the_end_are_found() {
        let cases = [
            ("다 고쳤습니다.\n\n푸시할까요?", "푸시할까요?"),
            (
                "Tests pass.\n\nShould I open the PR?",
                "Should I open the PR?",
            ),
            (
                "Done. Which branch should I use",
                "Which branch should I use",
            ),
            (
                "빌드는 끝났습니다. 배포 시점을 정해 주세요.",
                "배포 시점을 정해 주세요.",
            ),
            (
                "두 가지 방법이 있습니다. 확인 부탁드립니다.",
                "확인 부탁드립니다.",
            ),
            (
                "Two options are open. Let me know which you prefer.",
                "Let me know which you prefer.",
            ),
            ("修正しました。どうしますか", "どうしますか"),
            (
                "Listo. ¿Quieres que lo publique?",
                "¿Quieres que lo publique?",
            ),
            ("**어느 쪽으로 진행할까요?**", "어느 쪽으로 진행할까요?"),
        ];
        for (message, expected) in cases {
            assert_eq!(
                question_excerpt(message).as_deref(),
                Some(expected),
                "{message}"
            );
        }
    }

    #[test]
    fn options_after_the_question_count_with_it() {
        let message = "Two ways to fix it.\n\n어떻게 할까요?\n\n1. 지금 고치기\n2. 다음 릴리스에서";
        assert_eq!(question_excerpt(message).as_deref(), Some("어떻게 할까요?"));
        let numbered = "Plan:\n1. A\n2. B\n\n어느 쪽을 선택해 주세요";
        assert_eq!(
            question_excerpt(numbered).as_deref(),
            Some("어느 쪽을 선택해 주세요")
        );
    }

    #[test]
    fn reports_and_earlier_questions_are_not_asks() {
        for message in [
            "All tests pass. The PR is open.",
            "왜 실패했을까요? 경로가 틀렸습니다.\n\n고쳤고 테스트도 통과했습니다.",
            "```\nwhat?\n```",
            "Fixed the parser.\n\n```rust\nfn main() { let x = a?; }\n```",
            "",
        ] {
            assert_eq!(question_excerpt(message), None, "{message}");
        }
    }

    #[test]
    fn decision_lists_in_the_middle_are_found() {
        let report = "선박 세션에서 결과를 받았습니다.\n\n**처리한 것**\n- 커밋했습니다.\n\n\
            **대표님이 정하실 것 3가지**\n1. **PC C 오늘 밤 측정 시작**: 18:30 전에 지시가 필요합니다.\n\
            2. **손상 파일 복구 승인**: 복구를 권합니다.\n\n\
            노멀맵은 T 뒤로 미뤘습니다. 3D 세션도 동의했습니다.";
        assert_eq!(
            question_excerpt(report).as_deref(),
            Some("대표님이 정하실 것 3가지: PC C 오늘 밤 측정 시작: 18:30 전에 지시가 필요합니다.")
        );
        let english = "Build is green.\n\n## Decisions for you\n- Ship tonight or tomorrow\n\nAll tests pass.";
        assert_eq!(
            question_excerpt(english).as_deref(),
            Some("Decisions for you: Ship tonight or tomorrow")
        );
        for denied in [
            "모두 끝났습니다.\n\n대표님 결정이 필요한 것은 없습니다.\n\n테스트도 통과했습니다.",
            "Done.\n\nNo decision needed from you.\n\nAll green.",
        ] {
            assert_eq!(question_excerpt(denied), None, "{denied}");
        }
    }

    #[test]
    fn long_questions_are_shortened() {
        let long = format!("{}?", "가".repeat(400));
        let excerpt = question_excerpt(&long).unwrap();
        assert_eq!(excerpt.chars().count(), MAX_EXCERPT + 1);
        assert!(excerpt.ends_with('…'));
    }

    /// A message longer than the scanned end whose cut lands inside a code
    /// block: the block's rest is still code, and the prose after it is
    /// still prose (the fences were counted the wrong way round).
    #[test]
    fn a_long_code_block_cut_in_the_middle_stays_code() {
        let diff: String = (0..4000)
            .map(|i| format!("+ line {i}: does this work?\n"))
            .collect();
        assert!(diff.len() > MAX_SCANNED_BYTES);
        let message = format!("Here is the patch.\n\n```diff\n{diff}```\n\nShould I commit this?");
        assert_eq!(
            question_excerpt(&message).as_deref(),
            Some("Should I commit this?")
        );
        // A message that ends inside the block asks nothing.
        let open = format!("Here is the patch.\n\n```diff\n{diff}");
        assert_eq!(question_excerpt(&open), None);
        // Scanning is idempotent: the stored end and the full message agree.
        let once = scanned(&message);
        assert_eq!(scanned(once), once);
        assert!(once.len() <= MAX_SCANNED_BYTES);
    }

    mod sessions {
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::time::Duration;

        use vca_broker::contain::Scope;
        use vca_core::{AgentKind, ControlTier, SessionId, SessionInfo, SessionOrigin};

        use super::super::*;
        use crate::events::LogLimits;
        use crate::sessions::{ManagerConfig, Observed, ObservedIdentity, Update};
        use crate::workspace::WorkspaceRoots;

        const ASKS: &str = "Fixed the parser.\n\n배포는 언제 할까요?";

        fn manager(dir: &std::path::Path) -> SessionManager {
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

        fn observe(mgr: &SessionManager, conductor: bool) -> Arc<Session> {
            let info = SessionInfo {
                id: SessionId::new(),
                origin: SessionOrigin::Local {
                    agent: AgentKind::ClaudeCode,
                    project_dir: PathBuf::from("/code/shop"),
                    pid: Some(4_242_427),
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
                conductor,
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

        fn say(text: &str) -> HistoryItem {
            HistoryItem::Message {
                at: None,
                role: MessageRole::Assistant,
                text: text.into(),
            }
        }

        fn user(text: &str) -> HistoryItem {
            HistoryItem::Message {
                at: None,
                role: MessageRole::User,
                text: text.into(),
            }
        }

        fn end(ago_ms: u64) -> HistoryItem {
            HistoryItem::TurnEnd {
                at: Some(now_millis() - ago_ms),
            }
        }

        fn feed(mgr: &SessionManager, s: &Session, items: &[HistoryItem]) {
            for item in items {
                observe_item(mgr, s, item);
            }
        }

        async fn hook(mgr: &SessionManager, s: &Session, json: serde_json::Value) {
            let event = vca_adapters::claude::ClaudeHookPayload::parse(&json.to_string())
                .unwrap()
                .to_agent_event();
            mgr.hook_for(&s.id, event).await;
        }

        #[tokio::test]
        async fn a_turn_end_from_the_future_is_not_fresh() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            let future = HistoryItem::TurnEnd {
                at: Some(now_millis() + 60_000),
            };
            feed(&mgr, &s, &[say(ASKS), future]);
            assert!(s.info().question.is_none());
            // The turn's text is not kept after its end.
            assert!(lock(&s.turn).last_text.is_none());
            feed(&mgr, &s, &[say(ASKS), end(0)]);
            assert!(s.info().question.is_some());
            assert!(lock(&s.turn).last_text.is_none());
        }

        #[tokio::test]
        async fn a_decision_replaces_an_open_question() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            feed(&mgr, &s, &[say(ASKS), end(0)]);
            assert!(s.info().question.is_some());
            mgr.request_decision(
                &s,
                crate::sessions::PendingDecision {
                    decision: vca_core::Decision {
                        id: "d1".into(),
                        prompt: "Allow Bash: ls?".into(),
                        options: Vec::new(),
                        inferred: false,
                        questions: Vec::new(),
                        command: None,
                    },
                    submit: false,
                    heuristic: false,
                    at: now_millis(),
                    tool_use_id: None,
                },
            );
            let info = s.info();
            assert!(info.question.is_none());
            assert!(info.pending_decision.is_some());
        }

        #[tokio::test]
        async fn a_huge_final_message_is_kept_only_in_part() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            let huge = format!("{}\n\n배포는 언제 할까요?", "로그 ".repeat(200_000));
            feed(&mgr, &s, &[say(&huge)]);
            let kept = lock(&s.turn).last_text.as_ref().map_or(0, String::len);
            assert!(kept <= MAX_SCANNED_BYTES, "{kept}");
            feed(&mgr, &s, &[end(0)]);
            assert_eq!(
                s.info().question.map(|q| q.text).as_deref(),
                Some("배포는 언제 할까요?")
            );
            // The hook's full copy of the same message is no second question.
            let before = s.info().unread;
            raise(&mgr, &s, &huge);
            assert_eq!(s.info().unread, before);
        }

        /// Nobody can answer an ended process: its open question must not
        /// stay on the card (and in the alerts) for the whole retention.
        #[tokio::test]
        async fn an_exited_session_drops_its_open_question() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            feed(&mgr, &s, &[say(ASKS), end(0)]);
            assert!(s.info().question.is_some());
            mgr.set_state(&s, SessionState::Exited { code: None });
            assert_eq!(s.info().question, None);
            // The log ends with the state the session is in.
            let (events, _) = lock(s.log()).page_before(None, 1);
            assert!(matches!(
                &events[0],
                vca_core::SessionEvent::StateChanged {
                    state: SessionState::Exited { .. },
                    ..
                }
            ));
        }

        #[tokio::test]
        async fn a_turn_ending_in_a_question_raises_one_question() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            let mut updates = mgr.subscribe_updates();
            feed(
                &mgr,
                &s,
                &[user("fix it"), say("Looking."), say(ASKS), end(0)],
            );
            let info = s.info();
            let question = info.question.expect("a question");
            assert_eq!(question.text, "배포는 언제 할까요?");
            assert_eq!(info.unread, 1);
            // Every client hears it on the session card.
            let mut heard = false;
            while let Ok(update) = updates.try_recv() {
                if let Update::Changed(info) = update {
                    heard |= info.question.is_some_and(|q| q.id == question.id);
                }
            }
            assert!(heard);
            // One per turn: a second turn end (a re-read) changes nothing.
            feed(&mgr, &s, &[end(0)]);
            assert_eq!(s.info().unread, 1);
            // The person answers: gone. The next turn may ask again.
            feed(&mgr, &s, &[user("내일")]);
            assert!(s.info().question.is_none());
            feed(&mgr, &s, &[say("PR도 열까요?"), end(0)]);
            let next = s.info().question.expect("the next turn's question");
            assert_ne!(next.id, question.id);
            assert_eq!(s.info().unread, 2);
            // The agent works on, or a background task reports back: the
            // question is still open.
            feed(
                &mgr,
                &s,
                &[
                    HistoryItem::ToolCall {
                        at: None,
                        id: Some("toolu_1".into()),
                        name: "Bash".into(),
                        summary: "ls".into(),
                    },
                    say("Checking."),
                    HistoryItem::Message {
                        at: None,
                        role: MessageRole::System,
                        text: "<task-notification>done</task-notification>".into(),
                    },
                    say("Background job finished."),
                    end(0),
                ],
            );
            assert_eq!(s.info().question.map(|q| q.id), Some(next.id));
        }

        #[tokio::test]
        async fn reports_old_turns_and_text_before_tool_calls_raise_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            // A report.
            feed(&mgr, &s, &[say("All tests pass."), end(0)]);
            // A turn read from the start of the transcript.
            feed(&mgr, &s, &[user("x"), say(ASKS), end(FRESH_TURN + 60_000)]);
            // A question the agent then went on from.
            feed(
                &mgr,
                &s,
                &[
                    user("y"),
                    say("Should I check the logs?"),
                    HistoryItem::ToolCall {
                        at: None,
                        id: Some("toolu_2".into()),
                        name: "Read".into(),
                        summary: "log".into(),
                    },
                    end(0),
                ],
            );
            let info = s.info();
            assert!(info.question.is_none());
            assert_eq!(info.unread, 0);
        }

        #[tokio::test]
        async fn a_turn_started_by_another_session_may_ask_again() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            feed(&mgr, &s, &[user("go"), say(ASKS), end(0)]);
            let first = s.info().question.expect("a question").id;
            // A peer message starts a turn that asks something new.
            feed(
                &mgr,
                &s,
                &[
                    HistoryItem::Message {
                        at: None,
                        role: MessageRole::System,
                        text: "Another Claude session sent a message: hi".into(),
                    },
                    say("Done.\n\n**대표님이 정하실 것**\n1. 오늘 밤 측정 시작\n\nAll green."),
                    end(0),
                ],
            );
            let second = s.info().question.expect("the new question");
            assert_ne!(second.id, first);
            assert_eq!(second.text, "대표님이 정하실 것: 오늘 밤 측정 시작");
        }

        #[tokio::test]
        async fn the_conductor_never_raises_a_question() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, true);
            feed(&mgr, &s, &[say(ASKS), end(0)]);
            hook(
                &mgr,
                &s,
                serde_json::json!({"session_id": "abc", "hook_event_name": "Stop",
                    "last_assistant_message": ASKS}),
            )
            .await;
            assert!(s.info().question.is_none());
        }

        #[tokio::test]
        async fn the_stop_hook_raises_it_and_the_transcript_copy_keeps_it() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            hook(
                &mgr,
                &s,
                serde_json::json!({"session_id": "abc", "hook_event_name": "Stop",
                    "permission_mode": "auto", "last_assistant_message": ASKS}),
            )
            .await;
            let question = s.info().question.expect("a question");
            assert_eq!(question.text, "배포는 언제 할까요?");
            assert!(lock(&s.turn).mode_settles_without_prompt());
            // The transcript catches up with the same message: still one.
            let unread = s.info().unread;
            feed(&mgr, &s, &[say(ASKS), end(0)]);
            assert_eq!(s.info().question.map(|q| q.id), Some(question.id));
            assert_eq!(s.info().unread, unread);
            // A new prompt clears it.
            hook(
                &mgr,
                &s,
                serde_json::json!({"session_id": "abc", "hook_event_name": "UserPromptSubmit",
                    "prompt": "내일"}),
            )
            .await;
            assert!(s.info().question.is_none());
        }

        #[tokio::test]
        async fn the_transcript_reports_the_permission_mode() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = manager(dir.path());
            let s = observe(&mgr, false);
            assert!(!lock(&s.turn).mode_settles_without_prompt());
            for line in [r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"abc"}"#]
            {
                for item in vca_adapters::claude::transcript::parse_line(line) {
                    observe_item(&mgr, &s, &item);
                }
            }
            assert!(lock(&s.turn).mode_settles_without_prompt());
            feed(
                &mgr,
                &s,
                &[HistoryItem::PermissionMode {
                    at: None,
                    mode: "default".into(),
                }],
            );
            assert!(!lock(&s.turn).mode_settles_without_prompt());
        }
    }
}
