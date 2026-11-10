//! Spoken status briefings and result summaries.
//!
//! - A briefing ([`briefing`]) is built from the session list with fixed
//!   templates in English, Korean, Japanese and Spanish: counts by state, the
//!   session waiting longest and the sessions with errors, in at most three
//!   short sentences. No LLM is involved.
//! - A summary ([`Summarizer::summarize`]) shortens a session's last
//!   assistant message to one or two sentences in the listener's language
//!   with the server's LLM profile (`[[speech.llms]]`). A device must opt in
//!   first (`SetVoiceSummary`). Likely secrets are redacted and the input is
//!   capped before the model sees it, commands, paths and code are left out
//!   unless the device allowed details, each turn is summarized at most once
//!   (cached by session and message), and devices and the server are
//!   rate-limited. Without an LLM (or when a limit applies) a fixed
//!   "session N finished" line is returned instead.
//!
//! Neither the conversation text nor a summary is ever logged.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use vca_core::error::ErrorCode;
use vca_core::{
    Error, MessageRole, SUMMARY_MAX_CHARS, SessionEvent, SessionId, SessionInfo, SessionState,
    UnixMillis,
};
use vca_speech::llm::ChatBackend;

use crate::config::SpeechConfig;
use crate::speech_profiles::{LlmSlot, Secrets};
use crate::util::lock;

/// Characters of the assistant message the LLM sees at most.
pub const MAX_INPUT_CHARS: usize = 6000;
/// Longest session name read out.
const MAX_NAME_CHARS: usize = 40;
/// Sessions with an error named in a briefing; the rest are counted.
const MAX_ERROR_NUMBERS: usize = 3;
/// Summary requests per device per minute (cache hits included).
const DEVICE_PER_MINUTE: usize = 6;
/// LLM summaries per minute for the whole server.
const GLOBAL_PER_MINUTE: usize = 20;
/// LLM summaries running at the same time.
const MAX_CONCURRENT: usize = 2;
/// Summaries kept in memory.
const CACHE_ENTRIES: usize = 128;
/// Sessions whose last summarized turn is remembered.
const MAX_TURN_KEYS: usize = 1024;
/// Devices tracked by the opt-in list and the limiter at once.
const MAX_DEVICE_KEYS: usize = 10_000;
/// Rate-limit window.
const WINDOW: Duration = Duration::from_secs(60);
/// Shortest overall wait for one summary, including the wait for a free
/// slot (the request itself is bounded by the profile's `timeout_ms`).
const MIN_TIMEOUT: Duration = Duration::from_secs(8);
/// Error text of a device over its limit.
pub const RATE_LIMITED: &str = "too many summaries; try again in a minute";
/// Error text of a device that did not opt in.
pub const NOT_OPTED_IN: &str =
    "reading work results aloud is off on this device; turn it on in Settings first";

/// Languages with templates; anything else gets English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    Ko,
    Ja,
    Es,
}

impl Lang {
    pub fn parse(tag: Option<&str>) -> Self {
        let base = tag
            .unwrap_or_default()
            .trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match base.as_str() {
            "ko" => Self::Ko,
            "ja" => Self::Ja,
            "es" => Self::Es,
            _ => Self::En,
        }
    }
}

/// Sino-Korean reading of 0..=9999 ("영", "삼", "십이", "이천백"): numbered
/// items are read this way ("삼번 세션"), not with native numbers ("세 번").
pub fn sino_korean(n: u32) -> String {
    const DIGITS: [&str; 10] = ["", "일", "이", "삼", "사", "오", "육", "칠", "팔", "구"];
    const UNITS: [&str; 4] = ["", "십", "백", "천"];
    if n == 0 {
        return "영".into();
    }
    if n > 9999 {
        return n.to_string();
    }
    let digits: Vec<usize> = n
        .to_string()
        .bytes()
        .map(|b| usize::from(b - b'0'))
        .collect();
    let len = digits.len();
    let mut out = String::new();
    for (i, d) in digits.into_iter().enumerate() {
        if d == 0 {
            continue;
        }
        let unit = UNITS[len - 1 - i];
        // 십, 백, 천 are read without a leading 일.
        if d != 1 || unit.is_empty() {
            out.push_str(DIGITS[d]);
        }
        out.push_str(unit);
    }
    out
}

/// "session 3" in a sentence (lower case where the language has case).
fn session_ref(lang: Lang, n: u32) -> String {
    match lang {
        Lang::En => format!("session {n}"),
        Lang::Ko => format!("{}번 세션", sino_korean(n)),
        Lang::Ja => format!("{n}番のセッション"),
        Lang::Es => format!("la sesión {n}"),
    }
}

/// The number alone, as listed with other numbers.
fn number_word(lang: Lang, n: u32) -> String {
    match lang {
        Lang::Ko => format!("{}번", sino_korean(n)),
        Lang::Ja => format!("{n}番"),
        Lang::En | Lang::Es => n.to_string(),
    }
}

fn cap_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Single line, no control characters, at most [`MAX_NAME_CHARS`].
fn clean_name(name: &str) -> String {
    let flat: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::util::truncate_chars(&flat, MAX_NAME_CHARS, false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Approval,
    Input,
    Error,
    Running,
    Paused,
    Idle,
    Ended,
}

const CATEGORIES: [Category; 7] = [
    Category::Approval,
    Category::Input,
    Category::Error,
    Category::Running,
    Category::Paused,
    Category::Idle,
    Category::Ended,
];

fn category(state: &SessionState) -> Category {
    match state {
        SessionState::AwaitingDecision => Category::Approval,
        SessionState::AwaitingInput => Category::Input,
        SessionState::Error { .. } => Category::Error,
        SessionState::Starting | SessionState::Running => Category::Running,
        SessionState::Paused => Category::Paused,
        SessionState::Idle => Category::Idle,
        SessionState::Exited { .. } => Category::Ended,
    }
}

fn count_phrase(lang: Lang, cat: Category, n: usize) -> String {
    let one = n == 1;
    match lang {
        Lang::En => {
            let what = match cat {
                Category::Approval => "waiting for approval",
                Category::Input => "waiting for input",
                Category::Error if one => "with an error",
                Category::Error => "with errors",
                Category::Running => "running",
                Category::Paused => "paused",
                Category::Idle => "idle",
                Category::Ended => "ended",
            };
            format!("{n} {what}")
        }
        Lang::Ko => {
            let what = match cat {
                Category::Approval => "승인 대기",
                Category::Input => "입력 대기",
                Category::Error => "오류",
                Category::Running => "실행 중",
                Category::Paused => "일시 정지",
                Category::Idle => "유휴",
                Category::Ended => "종료",
            };
            format!("{what} {n}개")
        }
        Lang::Ja => {
            let what = match cat {
                Category::Approval => "承認待ち",
                Category::Input => "入力待ち",
                Category::Error => "エラー",
                Category::Running => "実行中",
                Category::Paused => "一時停止",
                Category::Idle => "待機中",
                Category::Ended => "終了",
            };
            format!("{what}{n}件")
        }
        Lang::Es => {
            let what = match cat {
                Category::Approval => "esperando aprobación",
                Category::Input => "esperando respuesta",
                Category::Error if one => "con error",
                Category::Error => "con errores",
                Category::Running => "en ejecución",
                Category::Paused => "en pausa",
                Category::Idle if one => "inactiva",
                Category::Idle => "inactivas",
                Category::Ended if one => "finalizada",
                Category::Ended => "finalizadas",
            };
            format!("{n} {what}")
        }
    }
}

fn counts_sentence(lang: Lang, total: usize, parts: &[String]) -> String {
    match lang {
        Lang::En => {
            let noun = if total == 1 { "session" } else { "sessions" };
            format!("{total} {noun}: {}.", parts.join(", "))
        }
        Lang::Ko => format!("세션 {total}개: {}.", parts.join(", ")),
        Lang::Ja => format!("セッション{total}件：{}。", parts.join("、")),
        Lang::Es => {
            let noun = if total == 1 { "sesión" } else { "sesiones" };
            format!("{total} {noun}: {}.", parts.join(", "))
        }
    }
}

fn no_sessions(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "There are no sessions on this computer.",
        Lang::Ko => "이 컴퓨터에는 세션이 없습니다.",
        Lang::Ja => "このコンピューターにセッションはありません。",
        Lang::Es => "No hay sesiones en este equipo.",
    }
}

fn nothing_waiting(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Nothing is waiting for you.",
        Lang::Ko => "기다리는 세션은 없습니다.",
        Lang::Ja => "対応待ちのセッションはありません。",
        Lang::Es => "Nada espera tu respuesta.",
    }
}

fn duration_phrase(lang: Lang, millis: u64) -> String {
    let minutes = millis / 60_000;
    let hours = minutes / 60;
    match lang {
        Lang::En => match (minutes, hours) {
            (0, _) => "under a minute".into(),
            (1, _) => "1 minute".into(),
            (m, 0) => format!("{m} minutes"),
            (_, 1) => "1 hour".into(),
            (_, h) => format!("{h} hours"),
        },
        Lang::Ko => match (minutes, hours) {
            (0, _) => "1분 미만".into(),
            (m, 0) => format!("{m}분"),
            (_, h) => format!("{h}시간"),
        },
        Lang::Ja => match (minutes, hours) {
            (0, _) => "1分未満".into(),
            (m, 0) => format!("{m}分"),
            (_, h) => format!("{h}時間"),
        },
        Lang::Es => match (minutes, hours) {
            (0, _) => "menos de un minuto".into(),
            (1, _) => "1 minuto".into(),
            (m, 0) => format!("{m} minutos"),
            (_, 1) => "1 hora".into(),
            (_, h) => format!("{h} horas"),
        },
    }
}

fn longest_sentence(lang: Lang, n: u32, name: &str, waited: u64) -> String {
    let session = session_ref(lang, n);
    let dur = duration_phrase(lang, waited);
    match lang {
        Lang::En => format!("Waiting longest: {session}, {name}, for {dur}."),
        Lang::Ko => format!("가장 오래 기다린 세션: {session} {name}, {dur}."),
        Lang::Ja => format!("最も長く待っているのは{session}「{name}」で、{dur}です。"),
        Lang::Es => format!("La que más espera es {session}, {name}, desde hace {dur}."),
    }
}

/// `a, b and c` in the language.
fn list_words(lang: Lang, words: &[String]) -> String {
    let (sep, last) = match lang {
        Lang::En => (", ", " and "),
        Lang::Es => (", ", " y "),
        Lang::Ko => (", ", ", "),
        Lang::Ja => ("、", "、"),
    };
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., tail] => format!("{}{last}{tail}", init.join(sep)),
    }
}

fn errors_sentence(lang: Lang, errors: &[(u32, String)]) -> String {
    if let [(n, name)] = errors {
        let session = session_ref(lang, *n);
        return match lang {
            Lang::En => format!(
                "{} has an error.",
                cap_first(&format!("{session}, {name},"))
            ),
            Lang::Ko => format!("{session} {name}에 오류가 있습니다."),
            Lang::Ja => format!("{session}「{name}」でエラーが起きています。"),
            Lang::Es => format!(
                "{} tiene un error.",
                cap_first(&format!("{session}, {name},"))
            ),
        };
    }
    let shown: Vec<String> = errors
        .iter()
        .take(MAX_ERROR_NUMBERS)
        .map(|(n, _)| number_word(lang, *n))
        .collect();
    let more = errors.len().saturating_sub(MAX_ERROR_NUMBERS);
    match lang {
        Lang::En => {
            let mut words = shown;
            if more > 0 {
                words.push(format!("{more} more"));
            }
            format!("Sessions {} have errors.", list_words(lang, &words))
        }
        Lang::Es => {
            let mut words = shown;
            if more > 0 {
                words.push(format!("{more} más"));
            }
            format!("Las sesiones {} tienen errores.", list_words(lang, &words))
        }
        Lang::Ko => {
            let list = list_words(lang, &shown);
            if more > 0 {
                format!("{list} 세션 외 {more}개에 오류가 있습니다.")
            } else {
                format!("{list} 세션에 오류가 있습니다.")
            }
        }
        Lang::Ja => {
            let list = list_words(lang, &shown);
            if more > 0 {
                format!("{list}ほか{more}件のセッションでエラーが起きています。")
            } else {
                format!("{list}のセッションでエラーが起きています。")
            }
        }
    }
}

/// Session numbers as the client shows them: the 1-based position in
/// `order`, and after those, the remaining sessions in list order.
fn numbered<'a>(sessions: &'a [SessionInfo], order: &[SessionId]) -> Vec<(u32, &'a SessionInfo)> {
    let mut next = u32::try_from(order.len()).unwrap_or(u32::MAX);
    sessions
        .iter()
        .map(|s| {
            let n = match order.iter().position(|id| id == &s.id) {
                Some(i) => u32::try_from(i + 1).unwrap_or(u32::MAX),
                None => {
                    next = next.saturating_add(1);
                    next
                }
            };
            (n, s)
        })
        .collect()
}

/// When a waiting session started waiting.
fn waiting_since(s: &SessionInfo) -> UnixMillis {
    match (&s.state, &s.pending_decision) {
        (SessionState::AwaitingDecision, Some(p)) => p.at,
        _ => s.last_activity,
    }
}

/// The spoken status of `sessions` in `language` (see the module docs).
pub fn briefing(
    sessions: &[SessionInfo],
    order: &[SessionId],
    language: Option<&str>,
    now: UnixMillis,
) -> String {
    let lang = Lang::parse(language);
    if sessions.is_empty() {
        return no_sessions(lang).to_owned();
    }
    let list = numbered(sessions, order);
    let parts: Vec<String> = CATEGORIES
        .iter()
        .filter_map(|cat| {
            let n = list
                .iter()
                .filter(|(_, s)| category(&s.state) == *cat)
                .count();
            (n > 0).then(|| count_phrase(lang, *cat, n))
        })
        .collect();
    let mut sentences = vec![counts_sentence(lang, list.len(), &parts)];

    let longest = list
        .iter()
        .filter(|(_, s)| matches!(category(&s.state), Category::Approval | Category::Input))
        .min_by_key(|(n, s)| (waiting_since(s), *n));
    sentences.push(match longest {
        Some((n, s)) => longest_sentence(
            lang,
            *n,
            &clean_name(&s.display_name),
            now.saturating_sub(waiting_since(s)),
        ),
        None => nothing_waiting(lang).to_owned(),
    });

    let mut errors: Vec<(u32, String)> = list
        .iter()
        .filter(|(_, s)| category(&s.state) == Category::Error)
        .map(|(n, s)| (*n, clean_name(&s.display_name)))
        .collect();
    errors.sort_by_key(|(n, _)| *n);
    if !errors.is_empty() {
        sentences.push(errors_sentence(lang, &errors));
    }
    let sep = if lang == Lang::Ja { "" } else { " " };
    sentences.join(sep)
}

// ------------------------------------------------------------------ summaries

/// Masks likely secrets: the audit log's patterns (bearer tokens,
/// `key=value` secrets, well-known key formats, long opaque strings), plus
/// credentials inside URLs and long hex strings.
pub fn redact(text: &str) -> String {
    let text = redact_url_credentials(text);
    let text = vca_net::audit::redact_secrets(&text);
    redact_long_hex(&text)
}

/// `scheme://user:pass@host` → `scheme://[REDACTED]@host`.
fn redact_url_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | '>'))
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(at) if at > 0 => {
                out.push_str("[REDACTED]");
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Runs of 32 or more hex digits (API keys, hashes used as tokens).
fn redact_long_hex(text: &str) -> String {
    const MIN: usize = 32;
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= MIN {
            out.push_str("[REDACTED]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Fenced code blocks become `[code]` (an unclosed fence drops the rest).
fn strip_code_blocks(text: &str) -> String {
    text.split("```")
        .enumerate()
        .map(|(i, part)| if i % 2 == 0 { part } else { " [code] " })
        .collect()
}

fn head_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// What the LLM sees of an assistant message: code dropped (unless
/// `details`), secrets redacted, at most [`MAX_INPUT_CHARS`].
pub fn prepare_input(text: &str, details: bool) -> String {
    // Bound the redaction work first; the final cap is far below this.
    let text = head_chars(text, MAX_INPUT_CHARS * 4);
    let text = if details {
        text.to_owned()
    } else {
        strip_code_blocks(text)
    };
    head_chars(&redact(&text), MAX_INPUT_CHARS).to_owned()
}

/// A BCP-47-looking tag for the prompt (nothing else reaches it).
fn prompt_language(language: Option<&str>) -> String {
    let tag: String = language
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(16)
        .collect();
    if tag.is_empty() { "en".into() } else { tag }
}

fn language_name(tag: &str) -> Option<&'static str> {
    match Lang::parse(Some(tag)) {
        Lang::Ko => Some("Korean"),
        Lang::Ja => Some("Japanese"),
        Lang::Es => Some("Spanish"),
        Lang::En if tag.to_ascii_lowercase().starts_with("en") => Some("English"),
        Lang::En => None,
    }
}

/// The system prompt of a summary request.
pub fn system_prompt(language: &str, details: bool) -> String {
    let target = match language_name(language) {
        Some(name) => format!("{name} ({language})"),
        None => format!("the language with the BCP-47 tag {language}"),
    };
    let details_rule = if details {
        "You may name a command or file when it matters, briefly."
    } else {
        "Do not include commands, file paths, URLs, code or identifiers; describe them in plain \
         words instead (for example \"the build\" or \"the settings file\")."
    };
    format!(
        "You turn the final message of an AI coding agent into a spoken update for someone \
         listening on headphones. Reply with one or two short, plain sentences, at most \
         {SUMMARY_MAX_CHARS} characters in total, in {target}, even when the message is written \
         in another language. Say what was done and whether the listener needs to do anything. \
         {details_rule} Never repeat secrets, tokens, keys or anything marked [REDACTED]. No \
         markdown, lists, quotes or emoji. Output only the update."
    )
}

/// The user message: the (prepared) assistant message, delimited.
fn user_message(input: &str) -> String {
    format!("Final message of the agent:\n<<<\n{input}\n>>>")
}

/// The model's reply as one spoken line of at most [`SUMMARY_MAX_CHARS`],
/// redacted again in case the model echoed something.
pub fn clean_summary(reply: &str) -> String {
    let flat: String = reply
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !matches!(c, '*' | '`' | '#' | '<' | '>'))
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = redact(flat.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace()));
    if flat.chars().count() <= SUMMARY_MAX_CHARS {
        return flat;
    }
    let head = head_chars(&flat, SUMMARY_MAX_CHARS);
    // End at the last full sentence when there is one, else at a word.
    let sentence_end = head
        .char_indices()
        .filter(|(_, c)| matches!(c, '.' | '!' | '?' | '。' | '！' | '？'))
        .map(|(i, c)| i + c.len_utf8())
        .next_back()
        .filter(|end| *end > head.len() / 3);
    match sentence_end {
        Some(end) => head[..end].to_owned(),
        None => {
            let cut = head
                .rfind(' ')
                .filter(|i| *i > head.len() / 3)
                .unwrap_or(head.len());
            let room = head_chars(&head[..cut], SUMMARY_MAX_CHARS - 1);
            format!("{}…", room.trim_end())
        }
    }
}

/// The spoken label of a session: its card number, else its name.
fn label(lang: Lang, number: Option<u32>, name: &str) -> String {
    match number {
        Some(n) => cap_first(&match lang {
            Lang::Es => format!("sesión {n}"),
            _ => session_ref(lang, n),
        }),
        None => name.to_owned(),
    }
}

fn with_prefix(lang: Lang, number: Option<u32>, name: &str, summary: &str) -> String {
    let label = label(lang, number, name);
    match lang {
        Lang::Ja => format!("{label}：{summary}"),
        _ => format!("{label}: {summary}"),
    }
}

/// The fixed line when no LLM summary is available.
pub fn finished_line(lang: Lang, number: Option<u32>, name: &str) -> String {
    match (lang, number) {
        (Lang::En, Some(n)) => format!("Session {n} finished its work."),
        (Lang::En, None) => format!("{name} finished its work."),
        (Lang::Ko, Some(n)) => format!("{} 작업이 끝났습니다.", session_ref(lang, n)),
        (Lang::Ko, None) => format!("{name} 세션 작업이 끝났습니다."),
        (Lang::Ja, Some(n)) => format!("{}の作業が終わりました。", session_ref(lang, n)),
        (Lang::Ja, None) => format!("「{name}」の作業が終わりました。"),
        (Lang::Es, Some(n)) => format!("La sesión {n} terminó su trabajo."),
        (Lang::Es, None) => format!("{name} terminó su trabajo."),
    }
}

/// The last non-empty assistant message: its seq and text.
pub fn last_assistant(events: &[SessionEvent]) -> Option<(u64, &str)> {
    events.iter().rev().find_map(|e| match e {
        SessionEvent::Message {
            seq,
            role: MessageRole::Assistant,
            text,
            ..
        } if !text.trim().is_empty() => Some((*seq, text.as_str())),
        _ => None,
    })
}

/// Sliding-window request counter per key.
struct Window {
    per_window: usize,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Window {
    fn new(per_window: usize) -> Self {
        Self {
            per_window: per_window.max(1),
            hits: Mutex::new(HashMap::new()),
        }
    }

    fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = lock(&self.hits);
        if hits.len() >= MAX_DEVICE_KEYS && !hits.contains_key(key) {
            hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < WINDOW));
            if hits.len() >= MAX_DEVICE_KEYS {
                return false;
            }
        }
        let q = hits.entry(key.to_owned()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            q.pop_front();
        }
        if q.len() >= self.per_window {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// One summary: the session, its message, the language and the details
/// choice (another choice is another summary).
type CacheKey = (SessionId, u64, String, bool);

/// The answer to a summary request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryReply {
    pub text: String,
    /// The fixed line, not an LLM summary.
    pub fallback: bool,
}

/// Summaries of finished turns. Shared by all clients.
pub struct Summarizer {
    llm: Option<LlmSlot>,
    secrets: Secrets,
    /// Devices that opted in, with their "read details" choice.
    opt_in: Mutex<HashMap<String, bool>>,
    cache: Mutex<VecDeque<(CacheKey, String)>>,
    /// The last assistant message seq summarized per session.
    turns: Mutex<HashMap<SessionId, u64>>,
    devices: Window,
    global: Window,
    permits: Semaphore,
}

impl Summarizer {
    /// Uses the default LLM profile (`[speech] default_llm`), else the first
    /// `[[speech.llms]]` profile. `default_llm = "none"` turns LLM summaries
    /// off; the fixed line is still available.
    pub fn from_config(cfg: &SpeechConfig, data_dir: &Path) -> Self {
        let timeout = Duration::from_millis(cfg.llm.timeout_ms.max(1));
        let profiles = cfg.llm_profiles();
        let off = cfg.default_llm.as_deref() == Some(vca_core::SPEECH_LLM_NONE);
        let chosen = if off {
            None
        } else {
            cfg.default_llm_id()
                .and_then(|id| profiles.iter().find(|p| p.id == id).cloned())
                .or_else(|| profiles.first().cloned())
        };
        Self {
            llm: chosen.map(|p| LlmSlot::new(p, timeout)),
            secrets: Secrets::in_data_dir(data_dir),
            opt_in: Mutex::new(HashMap::new()),
            cache: Mutex::new(VecDeque::new()),
            turns: Mutex::new(HashMap::new()),
            devices: Window::new(DEVICE_PER_MINUTE),
            global: Window::new(GLOBAL_PER_MINUTE),
            permits: Semaphore::new(MAX_CONCURRENT),
        }
    }

    /// Records a device's opt-in (`SetVoiceSummary`).
    pub fn set_opt_in(&self, device: &str, enabled: bool, details: bool) {
        let mut map = lock(&self.opt_in);
        if !enabled {
            map.remove(device);
            return;
        }
        if map.len() >= MAX_DEVICE_KEYS && !map.contains_key(device) {
            map.clear();
        }
        map.insert(device.to_owned(), details);
    }

    /// The device's "read details" choice, or `None` without an opt-in.
    fn opted_in(&self, device: &str) -> Option<bool> {
        lock(&self.opt_in).get(device).copied()
    }

    fn cached(&self, key: &CacheKey) -> Option<String> {
        lock(&self.cache)
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    fn remember(&self, key: CacheKey, summary: String) {
        let mut cache = lock(&self.cache);
        cache.retain(|(k, _)| k != &key);
        cache.push_back((key, summary));
        while cache.len() > CACHE_ENTRIES {
            cache.pop_front();
        }
    }

    /// Claims the one LLM call of this turn; `false` when it was spent or
    /// the server is over its limit.
    fn claim_turn(&self, session: &SessionId, seq: u64) -> bool {
        let mut turns = lock(&self.turns);
        if turns.get(session) == Some(&seq) {
            return false;
        }
        if !self.global.allow("all") {
            return false;
        }
        if turns.len() >= MAX_TURN_KEYS && !turns.contains_key(session) {
            turns.clear();
        }
        turns.insert(session.clone(), seq);
        true
    }

    fn chat(&self) -> Option<(Arc<dyn ChatBackend>, Duration)> {
        let slot = self.llm.as_ref()?;
        let corrector = slot.corrector(&self.secrets)?;
        Some((corrector.chat(), slot.timeout.max(MIN_TIMEOUT)))
    }

    /// Summarizes the last assistant message in `events` of `session` for
    /// `device` (see the module docs).
    pub async fn summarize(
        &self,
        device: &str,
        session: &SessionInfo,
        events: &[SessionEvent],
        language: Option<&str>,
        number: Option<u32>,
    ) -> Result<SummaryReply, Error> {
        let Some(details) = self.opted_in(device) else {
            return Err(Error::api(ErrorCode::Forbidden, NOT_OPTED_IN));
        };
        if !self.devices.allow(device) {
            return Err(Error::api(ErrorCode::Unavailable, RATE_LIMITED));
        }
        let lang = Lang::parse(language);
        let name = clean_name(&session.display_name);
        let fallback = || SummaryReply {
            text: finished_line(lang, number, &name),
            fallback: true,
        };
        let Some((seq, text)) = last_assistant(events) else {
            return Ok(fallback());
        };
        let tag = prompt_language(language);
        let key: CacheKey = (session.id.clone(), seq, tag.clone(), details);
        if let Some(hit) = self.cached(&key) {
            return Ok(SummaryReply {
                text: with_prefix(lang, number, &name, &hit),
                fallback: false,
            });
        }
        let Some((chat, timeout)) = self.chat() else {
            return Ok(fallback());
        };
        if !self.claim_turn(&session.id, seq) {
            return Ok(fallback());
        }
        let system = system_prompt(&tag, details);
        let user = user_message(&prepare_input(text, details));
        let attempt = async {
            let _permit = self.permits.acquire().await.ok()?;
            match chat.complete(&system, &user).await {
                Ok(reply) => Some(reply),
                Err(err) => {
                    // The error kind only: a provider may quote the input.
                    let kind = match err {
                        vca_speech::SpeechError::Api { status, .. } => format!("http {status}"),
                        vca_speech::SpeechError::Http(_) => "transport".into(),
                        _ => "decode".into(),
                    };
                    tracing::debug!(session = %session.id, %kind, "result summary failed");
                    None
                }
            }
        };
        let reply = match tokio::time::timeout(timeout, attempt).await {
            Ok(Some(reply)) => reply,
            Ok(None) => return Ok(fallback()),
            Err(_) => {
                tracing::debug!(session = %session.id, "result summary timed out");
                return Ok(fallback());
            }
        };
        let summary = clean_summary(&reply);
        if summary.is_empty() {
            return Ok(fallback());
        }
        self.remember(key, summary.clone());
        Ok(SummaryReply {
            text: with_prefix(lang, number, &name, &summary),
            fallback: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_core::{ControlTier, PendingDecisionInfo, SessionOrigin};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const MIN: u64 = 60_000;
    const NOW: UnixMillis = 1_800_000_000_000;

    fn session(id: &str, name: &str, state: SessionState, last: UnixMillis) -> SessionInfo {
        SessionInfo {
            id: SessionId(id.into()),
            origin: SessionOrigin::Local {
                agent: vca_core::AgentKind::ClaudeCode,
                project_dir: "/work/x".into(),
                pid: None,
            },
            display_name: name.into(),
            state,
            control: ControlTier::ManagedPty,
            profile: None,
            created_at: 0,
            last_activity: last,
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
        }
    }

    /// Five sessions: running, waiting for approval (asked 12 minutes ago),
    /// waiting for input (3 minutes), error, idle.
    fn fixture() -> (Vec<SessionInfo>, Vec<SessionId>) {
        let mut approval = session("b", "api", SessionState::AwaitingDecision, NOW - MIN);
        approval.pending_decision = Some(PendingDecisionInfo {
            decision: vca_core::Decision {
                id: "d".into(),
                prompt: "run pnpm build?".into(),
                options: Vec::new(),
                inferred: false,
                questions: Vec::new(),
                command: None,
            },
            at: NOW - 12 * MIN,
        });
        let sessions = vec![
            session("a", "web", SessionState::Running, NOW),
            approval,
            session("c", "docs", SessionState::AwaitingInput, NOW - 3 * MIN),
            session(
                "d",
                "build",
                SessionState::Error {
                    message: "C:\\secret\\path failed".into(),
                },
                NOW,
            ),
            session("e", "notes", SessionState::Idle, NOW),
        ];
        // The client shows them in another order: d=1, a=2, b=3; c and e
        // are not on its list yet.
        let order = vec!["d".into(), "a".into(), "b".into()];
        (sessions, order)
    }

    #[test]
    fn briefings_in_all_four_languages() {
        let (sessions, order) = fixture();
        let brief = |lang| briefing(&sessions, &order, Some(lang), NOW);
        assert_eq!(
            brief("en-US"),
            "5 sessions: 1 waiting for approval, 1 waiting for input, 1 with an error, \
             1 running, 1 idle. Waiting longest: session 3, api, for 12 minutes. \
             Session 1, build, has an error."
        );
        assert_eq!(
            brief("ko-KR"),
            "세션 5개: 승인 대기 1개, 입력 대기 1개, 오류 1개, 실행 중 1개, 유휴 1개. \
             가장 오래 기다린 세션: 삼번 세션 api, 12분. 일번 세션 build에 오류가 있습니다."
        );
        assert_eq!(
            brief("ja"),
            "セッション5件：承認待ち1件、入力待ち1件、エラー1件、実行中1件、待機中1件。\
             最も長く待っているのは3番のセッション「api」で、12分です。\
             1番のセッション「build」でエラーが起きています。"
        );
        assert_eq!(
            brief("es"),
            "5 sesiones: 1 esperando aprobación, 1 esperando respuesta, 1 con error, \
             1 en ejecución, 1 inactiva. La que más espera es la sesión 3, api, desde hace \
             12 minutos. La sesión 1, build, tiene un error."
        );
        // Unknown languages get English; nothing from the error text leaks.
        assert_eq!(brief("fr"), brief("en"));
        assert_eq!(briefing(&sessions, &order, None, NOW), brief("en"));
        for lang in ["en", "ko", "ja", "es"] {
            let text = brief(lang);
            assert!(!text.contains("secret"), "{text}");
            let ends = text.matches(['.', '。']).count();
            assert!(ends <= 3, "{lang}: {text}");
        }
    }

    #[test]
    fn briefings_without_sessions_waits_or_with_many_errors() {
        for (lang, want) in [
            ("en", "There are no sessions on this computer."),
            ("ko", "이 컴퓨터에는 세션이 없습니다."),
            ("ja", "このコンピューターにセッションはありません。"),
            ("es", "No hay sesiones en este equipo."),
        ] {
            assert_eq!(briefing(&[], &[], Some(lang), NOW), want);
        }
        let one = vec![session("a", "web", SessionState::Running, NOW)];
        assert_eq!(
            briefing(&one, &[], Some("en"), NOW),
            "1 session: 1 running. Nothing is waiting for you."
        );
        assert_eq!(
            briefing(&one, &[], Some("ko"), NOW),
            "세션 1개: 실행 중 1개. 기다리는 세션은 없습니다."
        );
        let err = |id: &str| {
            session(
                id,
                id,
                SessionState::Error {
                    message: "x".into(),
                },
                NOW,
            )
        };
        let five: Vec<SessionInfo> = ["a", "b", "c", "d", "e"].into_iter().map(err).collect();
        assert_eq!(
            briefing(&five, &[], Some("en"), NOW),
            "5 sessions: 5 with errors. Nothing is waiting for you. \
             Sessions 1, 2, 3 and 2 more have errors."
        );
        assert_eq!(
            briefing(&five[..2], &[], Some("ko"), NOW),
            "세션 2개: 오류 2개. 기다리는 세션은 없습니다. 일번, 이번 세션에 오류가 있습니다."
        );
        assert_eq!(
            briefing(&five, &[], Some("ja"), NOW),
            "セッション5件：エラー5件。対応待ちのセッションはありません。\
             1番、2番、3番ほか2件のセッションでエラーが起きています。"
        );
        assert_eq!(
            briefing(&five[..3], &[], Some("es"), NOW),
            "3 sesiones: 3 con errores. Nada espera tu respuesta. \
             Las sesiones 1, 2 y 3 tienen errores."
        );
        // Long waits are told in hours; names are single-line and capped.
        let mut long = session("z", "a\nb", SessionState::AwaitingInput, NOW - 125 * MIN);
        long.display_name.push_str(&"x".repeat(80));
        let text = briefing(&[long], &[], Some("en"), NOW);
        assert!(text.contains("for 2 hours"), "{text}");
        assert!(text.contains("a bxxx"), "{text}");
        assert!(text.contains('…'), "{text}");
    }

    #[test]
    fn korean_numbers_use_sino_readings() {
        for (n, want) in [
            (0, "영"),
            (1, "일"),
            (3, "삼"),
            (10, "십"),
            (12, "십이"),
            (21, "이십일"),
            (105, "백오"),
            (2100, "이천백"),
            (10_000, "10000"),
        ] {
            assert_eq!(sino_korean(n), want, "{n}");
        }
        assert_eq!(session_ref(Lang::Ko, 3), "삼번 세션");
        assert_eq!(session_ref(Lang::Ko, 12), "십이번 세션");
        assert_eq!(
            finished_line(Lang::Ko, Some(5), "x"),
            "오번 세션 작업이 끝났습니다."
        );
        let sessions = vec![session("a", "api", SessionState::AwaitingInput, NOW)];
        let order: Vec<SessionId> = ["p", "q", "r", "a"].into_iter().map(Into::into).collect();
        let text = briefing(&sessions, &order, Some("ko"), NOW);
        assert!(text.contains("사번 세션 api"), "{text}");
        assert!(!text.contains("4번"), "{text}");
    }

    #[test]
    fn redaction_masks_tokens_keys_and_url_credentials() {
        let text = "Deployed with sk-abcdefghijklmnopqrstuvwx and token=hunter2 to \
                    https://admin:pa55word@db.example.com/x, Bearer abc.def.ghi, \
                    hash 0123456789abcdef0123456789abcdef, AKIAABCDEFGHIJKLMNOP, \
                    ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaa and a key of \
                    QWxhZGRpbjpvcGVuIHNlc2FtZQQWxhZGRpbjpvcGVuIHNlc2FtZQ==";
        let out = redact(text);
        for secret in [
            "sk-abcdefghij",
            "hunter2",
            "pa55word",
            "admin:",
            "abc.def.ghi",
            "0123456789abcdef0123456789abcdef",
            "AKIAABCDEFGHIJKLMNOP",
            "ghp_aaaa",
            "QWxhZGRpbjpvcGVu",
        ] {
            assert!(!out.contains(secret), "{secret} left in {out}");
        }
        assert!(out.contains("Deployed with"), "{out}");
        assert!(out.contains("@db.example.com"), "{out}");
        // Plain URLs and short numbers stay.
        assert_eq!(
            redact("see https://example.com/a?b=1"),
            "see https://example.com/a?b=1"
        );
        assert_eq!(redact("fixed 3 tests in 12s"), "fixed 3 tests in 12s");
    }

    #[test]
    fn input_is_capped_and_code_dropped_unless_details() {
        let text = format!("Done.\n```sh\nrm -rf /tmp/x\n```\n{}", "가".repeat(10_000));
        let plain = prepare_input(&text, false);
        assert!(!plain.contains("rm -rf"), "{}", &plain[..40]);
        assert!(plain.contains("[code]"));
        assert_eq!(plain.chars().count(), MAX_INPUT_CHARS);
        let detailed = prepare_input(&text, true);
        assert!(detailed.contains("rm -rf"));
        // An unclosed fence hides the rest.
        assert_eq!(prepare_input("a ```b c", false), "a  [code] ");
    }

    #[test]
    fn summaries_are_one_capped_line() {
        assert_eq!(
            clean_summary("  \"**Built** the app.\"\n"),
            "Built the app."
        );
        let long = format!(
            "{} Second sentence that runs on and on.",
            "word ".repeat(30)
        );
        let out = clean_summary(&format!("First part. {long}"));
        assert!(out.chars().count() <= SUMMARY_MAX_CHARS, "{out}");
        let no_stop = "word ".repeat(80);
        let out = clean_summary(&no_stop);
        assert!(out.chars().count() <= SUMMARY_MAX_CHARS, "{out}");
        assert!(out.ends_with('…'), "{out}");
        assert!(!clean_summary("key sk-abcdefghijklmnopqrstuvwx").contains("sk-abc"));
    }

    #[test]
    fn prompts_name_the_language_and_the_details_rule() {
        let p = system_prompt("ko", false);
        assert!(p.contains("Korean (ko)"));
        assert!(p.contains("Do not include commands"));
        assert!(system_prompt("ko", true).contains("You may name a command"));
        assert!(system_prompt("fr-CA", false).contains("BCP-47 tag fr-CA"));
        assert_eq!(prompt_language(Some("ko-KR\n; drop")), "ko-KRdrop");
        assert_eq!(prompt_language(None), "en");
    }

    const SECRET: &str = "sk-livesecretlivesecret1234";

    fn events(seq: u64, text: &str) -> Vec<SessionEvent> {
        vec![
            SessionEvent::Message {
                seq: seq - 1,
                at: 0,
                role: MessageRole::User,
                text: "please deploy".into(),
            },
            SessionEvent::Message {
                seq,
                at: 0,
                role: MessageRole::Assistant,
                text: text.into(),
            },
            SessionEvent::StateChanged {
                seq: seq + 1,
                at: 0,
                state: SessionState::Idle,
            },
        ]
    }

    fn llm_config(base_url: &str) -> SpeechConfig {
        let toml = format!(
            r#"
            default_llm = "local"
            [[llms]]
            id = "local"
            base_url = "{base_url}/v1"
            model = "qwen"
            "#
        );
        toml::from_str(&toml).unwrap()
    }

    fn reply(content: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": content}}]
        }))
    }

    async fn mock_llm(content: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(reply(content))
            .mount(&server)
            .await;
        server
    }

    async fn request_bodies(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect()
    }

    #[tokio::test]
    async fn summaries_redact_before_the_llm_and_cache_per_turn() {
        let llm = mock_llm("빌드를 고치고 테스트를 모두 통과시켰습니다.").await;
        let dir = tempfile::tempdir().unwrap();
        let s = Summarizer::from_config(&llm_config(&llm.uri()), dir.path());
        let info = session("s1", "api", SessionState::Idle, NOW);
        let message = format!(
            "I fixed the build. Used {SECRET} and https://bob:hunter22@git.example.com/r.git\n\
             ```\ncat ~/.ssh/id_rsa\n```\n{}",
            "x ".repeat(5000)
        );
        let ev = events(10, &message);

        // No opt-in: refused, and nothing is spent.
        let err = s
            .summarize("dev", &info, &ev, Some("ko"), Some(3))
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Forbidden);
        assert!(request_bodies(&llm).await.is_empty());

        s.set_opt_in("dev", true, false);
        let out = s
            .summarize("dev", &info, &ev, Some("ko-KR"), Some(3))
            .await
            .unwrap();
        assert_eq!(
            out,
            SummaryReply {
                text: "삼번 세션: 빌드를 고치고 테스트를 모두 통과시켰습니다.".into(),
                fallback: false,
            }
        );
        let bodies = request_bodies(&llm).await;
        assert_eq!(bodies.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        let sent = body.to_string();
        for leaked in [SECRET, "hunter22", "id_rsa", "bob:"] {
            assert!(!sent.contains(leaked), "{leaked} reached the LLM");
        }
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(
            user.chars().count() <= MAX_INPUT_CHARS + 60,
            "{}",
            user.len()
        );
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("Korean (ko-KR)")
        );

        // The same turn again: cached, no new request (another device too).
        s.set_opt_in("dev2", true, false);
        let again = s
            .summarize("dev2", &info, &ev, Some("ko-KR"), Some(3))
            .await
            .unwrap();
        assert_eq!(again, out);
        // Another language for the same turn: the turn was spent already.
        let other = s
            .summarize("dev2", &info, &ev, Some("en"), Some(3))
            .await
            .unwrap();
        assert_eq!(
            other,
            SummaryReply {
                text: "Session 3 finished its work.".into(),
                fallback: true,
            }
        );
        assert_eq!(request_bodies(&llm).await.len(), 1);

        // A new turn gets a new summary.
        let next = s
            .summarize(
                "dev",
                &info,
                &events(20, "Second turn done."),
                Some("en"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            next.text,
            "api: 빌드를 고치고 테스트를 모두 통과시켰습니다."
        );
        assert_eq!(request_bodies(&llm).await.len(), 2);

        // Opting out stops it again.
        s.set_opt_in("dev", false, false);
        assert!(s.summarize("dev", &info, &ev, None, None).await.is_err());
    }

    #[tokio::test]
    async fn details_keep_code_and_the_reply_is_capped() {
        let long = format!("{} end", "Done and dusted ".repeat(40));
        let llm = mock_llm(&long).await;
        let dir = tempfile::tempdir().unwrap();
        let s = Summarizer::from_config(&llm_config(&llm.uri()), dir.path());
        s.set_opt_in("dev", true, true);
        let info = session("s1", "api", SessionState::Idle, NOW);
        let out = s
            .summarize(
                "dev",
                &info,
                &events(5, "Ran:\n```\npnpm build\n```"),
                None,
                Some(1),
            )
            .await
            .unwrap();
        assert!(!out.fallback);
        let summary = out.text.strip_prefix("Session 1: ").unwrap();
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS, "{summary}");
        let bodies = request_bodies(&llm).await;
        assert!(bodies[0].contains("pnpm build"));
        assert!(bodies[0].contains("You may name a command"));
    }

    #[tokio::test]
    async fn without_an_llm_or_a_message_the_fixed_line_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let s = Summarizer::from_config(&SpeechConfig::default(), dir.path());
        s.set_opt_in("dev", true, false);
        let info = session("s1", "api", SessionState::Idle, NOW);
        for (lang, want) in [
            ("en", "Session 2 finished its work."),
            ("ko", "이번 세션 작업이 끝났습니다."),
            ("ja", "2番のセッションの作業が終わりました。"),
            ("es", "La sesión 2 terminó su trabajo."),
        ] {
            let out = s
                .summarize("dev", &info, &events(3, "Done."), Some(lang), Some(2))
                .await
                .unwrap();
            assert_eq!(out.text, want);
            assert!(out.fallback);
        }
        // An explicit "none" default also means no LLM.
        let llm = mock_llm("never").await;
        let mut cfg = llm_config(&llm.uri());
        cfg.default_llm = Some("none".into());
        let s = Summarizer::from_config(&cfg, dir.path());
        s.set_opt_in("dev", true, false);
        let out = s
            .summarize("dev", &info, &events(3, "Done."), None, None)
            .await
            .unwrap();
        assert_eq!(out.text, "api finished its work.");
        assert!(request_bodies(&llm).await.is_empty());
        // No assistant message: the fixed line, no request.
        let s = Summarizer::from_config(&llm_config(&llm.uri()), dir.path());
        s.set_opt_in("dev", true, false);
        let out = s.summarize("dev", &info, &[], None, Some(1)).await.unwrap();
        assert!(out.fallback);
        assert!(request_bodies(&llm).await.is_empty());
    }

    #[tokio::test]
    async fn llm_failures_fall_back_and_devices_are_rate_limited() {
        let llm = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string(SECRET))
            .mount(&llm)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let s = Summarizer::from_config(&llm_config(&llm.uri()), dir.path());
        s.set_opt_in("dev", true, false);
        let info = session("s1", "api", SessionState::Idle, NOW);
        let out = s
            .summarize("dev", &info, &events(3, "Done."), Some("es"), Some(4))
            .await
            .unwrap();
        assert_eq!(out.text, "La sesión 4 terminó su trabajo.");
        assert!(out.fallback);
        // The failed turn is not retried.
        s.summarize("dev", &info, &events(3, "Done."), None, None)
            .await
            .unwrap();
        assert_eq!(request_bodies(&llm).await.len(), 1);
        // DEVICE_PER_MINUTE requests in total, then refused.
        for _ in 2..DEVICE_PER_MINUTE {
            s.summarize("dev", &info, &[], None, None).await.unwrap();
        }
        let err = s
            .summarize("dev", &info, &[], None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(err.to_string().contains(RATE_LIMITED));
        s.set_opt_in("other", true, false);
        assert!(s.summarize("other", &info, &[], None, None).await.is_ok());
    }

    #[tokio::test]
    async fn the_server_wide_limit_caps_llm_calls() {
        let llm = mock_llm("ok.").await;
        let dir = tempfile::tempdir().unwrap();
        let s = Summarizer::from_config(&llm_config(&llm.uri()), dir.path());
        let mut fallbacks = 0;
        for i in 0..(GLOBAL_PER_MINUTE + 3) {
            // A device per request, so only the global limit applies.
            let device = format!("dev{i}");
            s.set_opt_in(&device, true, false);
            let info = session(&format!("s{i}"), "x", SessionState::Idle, NOW);
            let out = s
                .summarize(&device, &info, &events(3, "Done."), None, None)
                .await
                .unwrap();
            fallbacks += usize::from(out.fallback);
        }
        assert_eq!(fallbacks, 3);
        assert_eq!(request_bodies(&llm).await.len(), GLOBAL_PER_MINUTE);
    }
}
