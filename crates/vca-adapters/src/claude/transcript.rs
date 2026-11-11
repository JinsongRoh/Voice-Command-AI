//! Claude Code session transcripts.
//!
//! Claude Code writes one JSONL file per session to
//! `<config dir>/projects/<encoded project path>/<session id>.jsonl`. Each
//! line is a record with a `type`. Conversation records are `user` and
//! `assistant`, whose `message` follows the Anthropic Messages API shape
//! (`content` is a string or a list of `text`, `thinking`, `tool_use` and
//! `tool_result` blocks). Other record types (`summary`, `ai-title`,
//! `custom-title`, `system`, `attachment`, `queue-operation`, ...) carry
//! metadata or are internal and are ignored by the history parser.
//!
//! The format is not a public contract. Parsing is tolerant: unknown record
//! types, unknown blocks and malformed lines are skipped.

use std::path::{Path, PathBuf};

use serde_json::Value;
use vca_core::{MessageRole, Question, QuestionOption, UnixMillis};

use crate::types::{HistoryItem, parse_rfc3339_millis, truncate};

/// Maximum length of tool results and tool input summaries in history.
const MAX_TOOL_TEXT: usize = 2_000;
const MAX_SUMMARY: usize = 200;

/// Parses one transcript line into history items.
pub fn parse_line(line: &str) -> Vec<HistoryItem> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    parse_record(&record)
}

/// Who wrote a `user` record.
///
/// Claude Code writes many `user` records that the person never typed: tool
/// results, background task notifications, sub-agent hand-back reports,
/// slash command wrappers, compaction summaries and injected context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Author {
    /// Typed by the person (`origin.kind == "human"`).
    Human,
    /// Written by the harness, a tool, another session or the model.
    Machine,
    /// Older transcripts without an `origin`: decided by the text.
    Unknown,
    /// Injected context that is not shown at all.
    Hidden,
}

fn user_author(record: &Value) -> Author {
    let origin = record
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str);
    if is_true(record, "isMeta") {
        // Meta records are injected context (skill bodies, image notes,
        // hook feedback). Messages from other sessions and sub-agent
        // hand-back reports are kept as agent activity.
        let peer = origin == Some("peer") || message_texts(record).any(is_agent_message);
        return if peer {
            Author::Machine
        } else {
            Author::Hidden
        };
    }
    if is_true(record, "isCompactSummary") || record.get("toolUseResult").is_some() {
        return Author::Machine;
    }
    match origin {
        Some("human") => Author::Human,
        Some(_) => Author::Machine,
        None if record.get("promptSource").and_then(Value::as_str) == Some("system") => {
            Author::Machine
        }
        None => Author::Unknown,
    }
}

/// Plain text blocks of a record's message.
fn message_texts(record: &Value) -> impl Iterator<Item = &str> {
    let content = record.get("message").and_then(|m| m.get("content"));
    let single = content.and_then(Value::as_str);
    let blocks = content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str));
    single.into_iter().chain(blocks)
}

/// Parses one transcript record into history items.
pub fn parse_record(record: &Value) -> Vec<HistoryItem> {
    let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
    // Sidechains are sub-agent conversations; their outcome reaches the
    // main transcript as a tool result or a task notification.
    if is_true(record, "isSidechain") {
        return Vec::new();
    }
    let at = || {
        record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_millis)
    };
    match kind {
        "user" | "assistant" => {}
        // `{"type":"permission-mode","permissionMode":"auto",...}`, written
        // when the session starts and whenever the mode changes.
        "permission-mode" => {
            return record
                .get("permissionMode")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(|mode| HistoryItem::PermissionMode {
                    at: at(),
                    mode: mode.to_owned(),
                })
                .into_iter()
                .collect();
        }
        // `{"type":"system","subtype":"turn_duration",...}` closes a turn.
        "system" if record.get("subtype").and_then(Value::as_str) == Some("turn_duration") => {
            return vec![HistoryItem::TurnEnd { at: at() }];
        }
        _ => return Vec::new(),
    }
    let (role, author) = if kind == "user" {
        (MessageRole::User, user_author(record))
    } else {
        (MessageRole::Assistant, Author::Machine)
    };
    if author == Author::Hidden {
        return Vec::new();
    }
    let at = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_millis);
    let Some(message) = record.get("message") else {
        return Vec::new();
    };
    let ctx = Ctx { at, role, author };
    match message.get("content") {
        Some(Value::String(text)) => ctx.text_item(text).into_iter().collect(),
        Some(Value::Array(blocks)) => blocks.iter().flat_map(|b| ctx.items(b)).collect(),
        _ => Vec::new(),
    }
}

/// Per-record context for turning content blocks into history items.
struct Ctx {
    at: Option<UnixMillis>,
    role: MessageRole,
    author: Author,
}

impl Ctx {
    /// The items of one content block: usually one, plus the question form
    /// of an `AskUserQuestion` call.
    fn items(&self, block: &Value) -> Vec<HistoryItem> {
        let Some(item) = self.block(block) else {
            return Vec::new();
        };
        let question = match &item {
            HistoryItem::ToolCall {
                id: Some(id), name, ..
            } if name == ASK_USER_QUESTION => {
                block
                    .get("input")
                    .and_then(parse_questions)
                    .map(|questions| HistoryItem::Question {
                        at: self.at,
                        id: id.clone(),
                        questions,
                    })
            }
            _ => None,
        };
        std::iter::once(item).chain(question).collect()
    }

    fn block(&self, block: &Value) -> Option<HistoryItem> {
        let at = self.at;
        match block.get("type").and_then(Value::as_str)? {
            "text" => self.text_item(block.get("text")?.as_str()?),
            "tool_use" | "server_tool_use" => {
                let name = block.get("name")?.as_str()?.to_owned();
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                Some(HistoryItem::ToolCall {
                    at,
                    id: block.get("id").and_then(Value::as_str).map(str::to_owned),
                    summary: tool_summary(&name, &input),
                    name,
                })
            }
            "tool_result" => Some(HistoryItem::ToolResult {
                at,
                id: block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                text: truncate(&content_text(block.get("content")), MAX_TOOL_TEXT),
                is_error: is_true(block, "is_error"),
            }),
            // Only that the agent thinks: the text is redacted or private
            // and is never carried.
            "thinking" | "redacted_thinking" if self.role == MessageRole::Assistant => {
                Some(HistoryItem::Thinking { at })
            }
            // image, document, ...
            _ => None,
        }
    }

    fn text_item(&self, text: &str) -> Option<HistoryItem> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.starts_with("<system-reminder>") {
            return None;
        }
        // Only text the person typed is their message; everything else
        // written as a user record is shown as system or agent activity.
        let machine = match self.author {
            Author::Human => is_known_machine_text(trimmed),
            Author::Unknown => is_machine_text(trimmed),
            Author::Machine | Author::Hidden => true,
        };
        let role = if self.role == MessageRole::User && machine {
            MessageRole::System
        } else {
            self.role
        };
        Some(HistoryItem::Message {
            at: self.at,
            role,
            text: trimmed.to_owned(),
        })
    }
}

/// Text of a `tool_result` content field (a string or a list of blocks).
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).map(str::to_owned),
                Some(other) => Some(format!("[{other}]")),
                None => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One-line description of a tool call, for example the shell command.
pub fn tool_summary(name: &str, input: &Value) -> String {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let summary = match name {
        "Bash" | "PowerShell" => field("command").map(str::to_owned),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => field("file_path")
            .or_else(|| field("notebook_path"))
            .map(str::to_owned),
        "Grep" | "Glob" => field("pattern").map(str::to_owned),
        "WebFetch" => field("url").map(str::to_owned),
        "WebSearch" => field("query").map(str::to_owned),
        n if is_subagent_tool(n) => field("description").map(str::to_owned),
        ASK_USER_QUESTION => input
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|qs| qs.first())
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    };
    let summary = summary.unwrap_or_else(|| match input {
        Value::Null => String::new(),
        other => other.to_string(),
    });
    truncate(&summary.replace('\n', " "), MAX_SUMMARY)
}

/// Claude Code's tool that asks the person a question form.
pub const ASK_USER_QUESTION: &str = "AskUserQuestion";

/// Most questions and options taken from one form; Claude Code asks 1-4
/// questions with 2-4 options each.
const MAX_QUESTIONS: usize = 8;
const MAX_OPTIONS: usize = 12;
const MAX_QUESTION_TEXT: usize = 2_000;
const MAX_OPTION_TEXT: usize = 500;

/// The questions of an `AskUserQuestion` call, whose input is
/// `{questions: [{question, header, options: [{label, description,
/// preview?}], multiSelect}]}`. `None` when no question is usable (a form
/// VCA cannot describe is left to the terminal).
///
/// A single-choice question whose options carry a `preview` is drawn by
/// Claude Code as a side-by-side preview list without the "Type
/// something." row, so it takes no free text ([`Question::free_text`]).
pub fn parse_questions(input: &Value) -> Option<Vec<Question>> {
    let raw = input.get("questions")?.as_array()?;
    if raw.is_empty() || raw.len() > MAX_QUESTIONS {
        return None;
    }
    raw.iter().map(parse_question).collect()
}

/// A trimmed string field.
fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).map(str::trim)
}

fn parse_question(q: &Value) -> Option<Question> {
    let text = str_field;
    let question = text(q, "question").filter(|t| !t.is_empty())?;
    let raw = q.get("options")?.as_array()?;
    if raw.is_empty() || raw.len() > MAX_OPTIONS {
        return None;
    }
    let mut previews = false;
    let options = raw
        .iter()
        .map(|o| {
            previews |= text(o, "preview").is_some_and(|p| !p.is_empty());
            let label = text(o, "label").filter(|l| !l.is_empty())?;
            Some(QuestionOption {
                label: truncate(label, MAX_OPTION_TEXT),
                description: truncate(text(o, "description").unwrap_or(""), MAX_OPTION_TEXT),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let multi_select = q
        .get("multiSelect")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(Question {
        header: truncate(text(q, "header").unwrap_or(""), MAX_OPTION_TEXT),
        question: truncate(question, MAX_QUESTION_TEXT),
        options,
        multi_select,
        free_text: multi_select || !previews,
    })
}

/// How a tool call relates to permission prompts. Used to infer a pending
/// approval from the transcript when no agent hooks are installed: the
/// call is written before the agent asks, its result only after the answer.
/// Shared by the Claude Code and Codex adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermission {
    /// Never asks: reads, searches, planning, sub-agents.
    Never,
    /// May ask; once allowed it runs a child process (shell commands).
    Command,
    /// May ask; once allowed it runs inside the agent (file edits).
    Local,
    /// May ask; once allowed it may wait on the network for a while with
    /// no child process (web fetches, MCP tools).
    Slow,
}

impl ToolPermission {
    pub fn may_ask(self) -> bool {
        self != Self::Never
    }
}

/// Whether a Claude Code tool usually asks for permission.
pub fn tool_permission(name: &str) -> ToolPermission {
    match name {
        "Bash" | "PowerShell" => ToolPermission::Command,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ToolPermission::Local,
        "WebFetch" => ToolPermission::Slow,
        n if n.starts_with("mcp__") => ToolPermission::Slow,
        // Read, Glob, Grep, LS, TodoWrite, Task, Agent, WebSearch, ...
        _ => ToolPermission::Never,
    }
}

/// Whether a Claude Code tool call starts a sub-agent (`Task`, renamed
/// `Agent` in Claude Code 2.1).
pub fn is_subagent_tool(name: &str) -> bool {
    matches!(name, "Task" | "Agent")
}

/// Tags Claude Code wraps around user records the person did not type.
const MACHINE_TAGS: &[&str] = &[
    "<task-notification",
    "<command-name",
    "<command-message",
    "<command-args",
    "<local-command-",
    "<bash-input",
    "<bash-stdout",
    "<bash-stderr",
    "<system-reminder",
    "<cross-session-message",
    "<agent-message",
    "<user-prompt-submit-hook",
    "<artifact-content",
    "<tool_use_error",
];

/// Plain-text openings of harness-written user records.
const MACHINE_PREFIXES: &[&str] = &[
    "Caveat: The messages below were generated",
    "Another Claude session sent a message",
    "[Subagent hand-back]",
    "This session is being continued from a previous conversation",
    "[Request interrupted",
    "[Cross-session idle notice]",
    "Stop hook feedback:",
    "Base directory for this skill:",
    "[Image: original ",
    "[Image: source:",
];

/// Tags a prompt the person typed can start with (pasted text blocks).
const HUMAN_TAGS: &[&str] = &["<pasted_content", "<pasted-content"];

/// Text that is certainly not typed by the person, even in a record with a
/// human origin (slash command wrappers, interruption markers, ...).
fn is_known_machine_text(text: &str) -> bool {
    let t = text.trim_start();
    MACHINE_TAGS.iter().any(|p| t.starts_with(p))
        || MACHINE_PREFIXES.iter().any(|p| t.starts_with(p))
}

/// Whether the text carries a sub-agent hand-back or a message from
/// another session.
fn is_agent_message(text: &str) -> bool {
    text.trim_start()
        .starts_with("Another Claude session sent a message")
        || text.contains("<agent-message")
        || text.contains("<cross-session-message")
        || text.contains("[Subagent hand-back]")
}

/// User records the person did not type: background task notices, slash
/// command wrappers, sub-agent reports and similar tagged text. Used for
/// records without an `origin`, so any leading harness-style tag counts.
fn is_machine_text(text: &str) -> bool {
    let t = text.trim_start();
    if is_known_machine_text(t) || is_agent_message(t) {
        return true;
    }
    starts_with_tag(t) && !HUMAN_TAGS.iter().any(|p| t.starts_with(p))
}

/// `<name>` or `<name attr…>` at the start, like the harness wrappers.
fn starts_with_tag(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('<') else {
        return false;
    };
    if !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return false;
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    matches!(rest[end..].chars().next(), Some('>' | ' ' | '\n' | '\r'))
}

fn is_true(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Metadata extracted from part of a transcript for session discovery.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptMeta {
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub custom_title: Option<String>,
    pub ai_title: Option<String>,
    pub summary: Option<String>,
    pub first_prompt: Option<String>,
    pub last_timestamp: Option<UnixMillis>,
}

impl TranscriptMeta {
    /// Updates the metadata with one transcript line. Later titles win.
    pub fn observe_line(&mut self, line: &str) {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let str_field = |k: &str| record.get(k).and_then(Value::as_str).map(str::to_owned);
        if self.session_id.is_none() {
            self.session_id = str_field("sessionId");
        }
        if let Some(cwd) = str_field("cwd") {
            self.cwd = Some(PathBuf::from(cwd));
        }
        if let Some(ts) = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_millis)
        {
            self.last_timestamp = Some(self.last_timestamp.map_or(ts, |t| t.max(ts)));
        }
        match record.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                self.custom_title = str_field("customTitle").or_else(|| str_field("title"))
            }
            Some("ai-title") => self.ai_title = str_field("aiTitle").or_else(|| str_field("title")),
            Some("summary") => self.summary = str_field("summary"),
            Some("user") if self.first_prompt.is_none() => {
                self.first_prompt = parse_record(&record)
                    .into_iter()
                    .find_map(|item| match item {
                        HistoryItem::Message {
                            role: MessageRole::User,
                            text,
                            ..
                        } if !is_machine_text(&text) => {
                            Some(truncate(&text.replace('\n', " "), 80))
                        }
                        _ => None,
                    });
            }
            _ => {}
        }
    }

    /// Best available title.
    pub fn title(&self) -> Option<String> {
        self.custom_title
            .clone()
            .or_else(|| self.ai_title.clone())
            .or_else(|| self.summary.clone())
            .or_else(|| self.first_prompt.clone())
    }
}

/// Reads metadata from the head and tail of a transcript without parsing
/// the whole file, which can be many megabytes.
pub fn read_meta(path: &Path) -> std::io::Result<TranscriptMeta> {
    use std::io::{Read, Seek, SeekFrom};

    const HEAD: u64 = 64 * 1024;
    const TAIL: u64 = 256 * 1024;

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut meta = TranscriptMeta::default();

    let mut head = Vec::new();
    (&mut file).take(HEAD.min(len)).read_to_end(&mut head)?;
    let head_complete = len <= HEAD;
    for_each_line(&head, !head_complete, false, |l| meta.observe_line(l));

    if !head_complete {
        let start = len.saturating_sub(TAIL).max(HEAD);
        file.seek(SeekFrom::Start(start))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        for_each_line(&tail, false, start > HEAD, |l| meta.observe_line(l));
    }
    Ok(meta)
}

/// Calls `f` for each line of `buf`, optionally dropping a partial first
/// or last line.
fn for_each_line(buf: &[u8], drop_last: bool, drop_first: bool, mut f: impl FnMut(&str)) {
    let text = String::from_utf8_lossy(buf);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if drop_last {
        lines.pop();
    }
    if drop_first && !lines.is_empty() {
        lines.remove(0);
    }
    for line in lines {
        let line = line.trim();
        if !line.is_empty() {
            f(line);
        }
    }
}

/// Encodes a project directory the way Claude Code names its folder under
/// `projects/`: every character other than an ASCII letter or digit becomes
/// `-`. For example `/home/user/my.app` becomes `-home-user-my-app`.
///
/// Claude Code shortens very long names with a hash suffix; such folders
/// still decode through the `cwd` recorded in the transcript.
pub fn encode_project_dir(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Decodes a project folder name back into a path.
///
/// The encoding is lossy (`/`, `.`, `_`, `-` and spaces all become `-`), so
/// the decoder walks the file system and picks existing directories whose
/// encoded names match. If nothing matches, it falls back to treating every
/// `-` as a path separator. Prefer the `cwd` recorded in the transcript
/// when available.
pub fn decode_project_dir(encoded: &str) -> PathBuf {
    let (root, rest) = split_root(encoded);
    if let Some(found) = search(&root, rest, 0) {
        return found;
    }
    let mut naive = root;
    for part in rest.split('-').filter(|p| !p.is_empty()) {
        naive.push(part);
    }
    naive
}

fn split_root(encoded: &str) -> (PathBuf, &str) {
    let bytes = encoded.as_bytes();
    // Windows drive: `C:\Users\me` encodes as `C--Users-me`.
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b'-' && bytes[2] == b'-' {
        let drive = format!("{}:\\", bytes[0] as char);
        return (PathBuf::from(drive), &encoded[3..]);
    }
    match encoded.strip_prefix('-') {
        Some(rest) => (PathBuf::from("/"), rest),
        None => (PathBuf::new(), encoded),
    }
}

fn search(dir: &Path, rest: &str, depth: usize) -> Option<PathBuf> {
    if rest.is_empty() {
        return Some(dir.to_path_buf());
    }
    if depth > 64 {
        return None;
    }
    let entries = std::fs::read_dir(if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    })
    .ok()?;
    let mut candidates: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok())
        // `Path::is_dir` follows symlinks (macOS `/var` -> `/private/var`).
        .filter(|e| e.path().is_dir())
        .map(|e| (encode_project_dir(Path::new(&e.file_name())), e.path()))
        .filter(|(enc, _)| {
            rest == enc || (rest.starts_with(enc.as_str()) && rest[enc.len()..].starts_with('-'))
        })
        .collect();
    // Longer names first, so `my-app` wins over `my` when both exist.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0.len()));
    for (enc, path) in candidates {
        let remaining = rest[enc.len()..].strip_prefix('-').unwrap_or("");
        if let Some(found) = search(&path, remaining, depth + 1) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_claude_code() {
        assert_eq!(
            encode_project_dir(Path::new("/home/user/Voice-Command-AI")),
            "-home-user-Voice-Command-AI"
        );
        assert_eq!(
            encode_project_dir(Path::new("/Users/me/.config/my_app.v2")),
            "-Users-me--config-my-app-v2"
        );
    }

    #[cfg(unix)]
    #[test]
    fn decodes_against_the_file_system() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("my-app.v2").join(".hidden").join("x_y");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(tmp.path().join("my")).unwrap();
        let encoded = encode_project_dir(&target);
        assert_eq!(decode_project_dir(&encoded), target);
    }

    #[test]
    fn decode_falls_back_to_naive_split() {
        let decoded = decode_project_dir("-nonexistent-vca-path-abc");
        assert_eq!(decoded, PathBuf::from("/nonexistent/vca/path/abc"));
        let (root, rest) = split_root("C--Users-me");
        assert_eq!(root, PathBuf::from("C:\\"));
        assert_eq!(rest, "Users-me");
    }

    #[test]
    fn ignores_garbage_and_unknown_records() {
        assert!(parse_line("not json").is_empty());
        assert!(parse_line(r#"{"type":"queue-operation","operation":"enqueue"}"#).is_empty());
        assert!(parse_line(r#"{"type":"user"}"#).is_empty());
        assert!(
            parse_line(r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"x"}}"#)
                .is_empty()
        );
    }

    #[test]
    fn tool_summary_picks_relevant_field() {
        let input = serde_json::json!({"command": "cargo test\n--all", "description": "x"});
        assert_eq!(tool_summary("Bash", &input), "cargo test --all");
        let input = serde_json::json!({"foo": 1});
        assert_eq!(tool_summary("mcp__x__y", &input), r#"{"foo":1}"#);
    }

    #[test]
    fn permission_tools() {
        for name in ["Bash", "PowerShell"] {
            assert_eq!(tool_permission(name), ToolPermission::Command, "{name}");
        }
        for name in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            assert_eq!(tool_permission(name), ToolPermission::Local, "{name}");
        }
        for name in ["WebFetch", "mcp__github__create_issue"] {
            assert_eq!(tool_permission(name), ToolPermission::Slow, "{name}");
        }
        for name in [
            "Read",
            "Glob",
            "Grep",
            "TodoWrite",
            "Task",
            "Agent",
            "WebSearch",
        ] {
            assert!(!tool_permission(name).may_ask(), "{name}");
        }
    }

    /// A pending tool call: the `tool_use` line is written before the
    /// permission prompt, its `tool_result` only after the answer.
    #[test]
    fn tool_use_and_result_pair_by_id() {
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"rm -rf build"}}]},"timestamp":"2026-11-01T10:00:00.000Z"}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_9","type":"tool_result","content":"done"}]},"toolUseResult":{}}"#;
        assert!(matches!(
            &parse_line(call)[..],
            [HistoryItem::ToolCall { id: Some(id), name, summary, .. }]
                if id == "toolu_9" && name == "Bash" && summary == "rm -rf build"
        ));
        assert!(matches!(
            &parse_line(result)[..],
            [HistoryItem::ToolResult { id: Some(id), .. }] if id == "toolu_9"
        ));
    }

    // An `AskUserQuestion` call in the shape Claude Code 2.1 writes it.
    const ASK_MULTI: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Some questions first."},{"type":"tool_use","id":"toolu_q1","name":"AskUserQuestion","input":{"questions":[{"question":"어떤 기준으로 가격을 정할까요?","header":"가격 기준","options":[{"label":"선박 납품가 기준 (Recommended)","description":"가장 익숙한 기준"},{"label":"분야별 따로","description":"분야마다 다르게"},{"label":"고객 미팅 뒤에 정함","description":"미팅 후 결정"}],"multiSelect":false},{"question":"Which costs go in?","header":"비용 항목","options":[{"label":"Hardware","description":"Boxes"},{"label":"Licenses","description":"Seats"}],"multiSelect":true}]}}]},"timestamp":"2026-11-01T10:00:00.000Z"}"#;

    fn question_items(line: &str) -> Vec<(String, Vec<Question>)> {
        parse_line(line)
            .into_iter()
            .filter_map(|item| match item {
                HistoryItem::Question { id, questions, .. } => Some((id, questions)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ask_user_question_is_a_tool_call_and_a_question_form() {
        let items = parse_line(ASK_MULTI);
        assert_eq!(items.len(), 3, "{items:?}");
        assert!(matches!(
            &items[1],
            HistoryItem::ToolCall { id: Some(id), name, summary, .. }
                if id == "toolu_q1" && name == "AskUserQuestion"
                    && summary == "어떤 기준으로 가격을 정할까요?"
        ));
        let [(id, questions)] = &question_items(ASK_MULTI)[..] else {
            panic!("one form expected");
        };
        assert_eq!(id, "toolu_q1");
        assert_eq!(questions.len(), 2);
        let q = &questions[0];
        assert_eq!(q.header, "가격 기준");
        assert_eq!(q.question, "어떤 기준으로 가격을 정할까요?");
        assert_eq!(q.options.len(), 3);
        assert_eq!(q.options[0].label, "선박 납품가 기준 (Recommended)");
        assert_eq!(q.options[1].description, "분야마다 다르게");
        assert!(!q.multi_select);
        assert!(q.free_text);
        assert!(questions[1].multi_select);
        assert!(questions[1].free_text);
    }

    #[test]
    fn single_question_and_previews() {
        let single = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_q2","name":"AskUserQuestion","input":{"questions":[{"question":"Ship it?","header":"Ship","options":[{"label":"Yes","description":""},{"label":"No","description":""}],"multiSelect":false}]}}]}}"#;
        let forms = question_items(single);
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].1.len(), 1);
        // A preview list has no "Type something." row.
        let preview = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_q3","name":"AskUserQuestion","input":{"questions":[{"question":"Layout?","header":"UI","options":[{"label":"A","description":"","preview":"<a>"},{"label":"B","description":""}],"multiSelect":false}]}}]}}"#;
        assert!(!question_items(preview)[0].1[0].free_text);
    }

    #[test]
    fn answered_forms_pair_with_their_result() {
        let result = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_q1","type":"tool_result","content":"User has answered your questions: ..."}]},"toolUseResult":{}}"#;
        assert!(matches!(
            &parse_line(result)[..],
            [HistoryItem::ToolResult { id: Some(id), .. }] if id == "toolu_q1"
        ));
    }

    #[test]
    fn unusable_forms_are_only_tool_calls() {
        for input in [
            r#"{}"#,
            r#"{"questions":[]}"#,
            r#"{"questions":[{"question":"","options":[{"label":"a"}]}]}"#,
            r#"{"questions":[{"question":"q","options":[]}]}"#,
            r#"{"questions":[{"question":"q","options":[{"label":""}]}]}"#,
        ] {
            let line = format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t","name":"AskUserQuestion","input":{input}}}]}}}}"#
            );
            let items = parse_line(&line);
            assert_eq!(items.len(), 1, "{input}");
            assert!(matches!(items[0], HistoryItem::ToolCall { .. }));
        }
        // Another tool with the same input is no form.
        let other = ASK_MULTI.replace("AskUserQuestion", "Other");
        assert!(question_items(&other).is_empty());
    }

    #[test]
    fn first_prompt_skips_machine_text() {
        let mut meta = TranscriptMeta::default();
        meta.observe_line(
            r#"{"type":"user","message":{"role":"user","content":"<task-notification> <task-id>b1</task-id>"}}"#,
        );
        assert_eq!(meta.first_prompt, None);
        meta.observe_line(
            r#"{"type":"user","message":{"role":"user","content":"Fix the login page"}}"#,
        );
        assert_eq!(meta.title().as_deref(), Some("Fix the login page"));
    }

    /// Roles of the message items a line produces (tool items are skipped).
    fn roles(line: &str) -> Vec<MessageRole> {
        parse_line(line)
            .into_iter()
            .filter_map(|item| match item {
                HistoryItem::Message { role, .. } => Some(role),
                _ => None,
            })
            .collect()
    }

    // Shapes below are trimmed copies of real Claude Code 2.1 records.

    #[test]
    fn human_prompts_are_user_messages() {
        let typed = r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":"Fix the login page"},"origin":{"kind":"human"},"promptSource":"user","timestamp":"2026-10-28T19:26:26.618Z"}"#;
        assert_eq!(roles(typed), [MessageRole::User]);
        // Pasted text keeps its wrapper but is still the person's prompt.
        let pasted = r#"{"type":"user","message":{"role":"user","content":"<pasted_content id=\"46f7\">\nYes\n</pasted_content>"},"origin":{"kind":"human"}}"#;
        assert_eq!(roles(pasted), [MessageRole::User]);
        let pasted_legacy = r#"{"type":"user","message":{"role":"user","content":"<pasted_content id=\"1\">x</pasted_content>"}}"#;
        assert_eq!(roles(pasted_legacy), [MessageRole::User]);
        // A prompt with an image: the text block is the person's.
        let with_image = r#"{"type":"user","message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":""}},{"type":"text","text":"What is wrong here?"}]},"origin":{"kind":"human"}}"#;
        assert_eq!(roles(with_image), [MessageRole::User]);
        // Older transcripts have no origin.
        let legacy = r#"{"type":"user","message":{"role":"user","content":"Run the tests"}}"#;
        assert_eq!(roles(legacy), [MessageRole::User]);
    }

    #[test]
    fn task_notifications_are_system_activity() {
        let with_origin = r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":"<task-notification>\n<task-id>bkcx5bhcq</task-id>\n<tool-use-id>toolu_01</tool-use-id>\n<status>completed</status>\n<summary>Background command \"Wait for CI\" completed (exit code 0)</summary>\n</task-notification>"},"origin":{"kind":"task-notification","producer":"session-task"},"promptSource":"system"}"#;
        assert_eq!(roles(with_origin), [MessageRole::System]);
        let agent_done = r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>a54a</task-id>\n<status>completed</status>\n<summary>Agent \"Build env\" finished</summary>\n<result>All 7 tests pass.</result>\n</task-notification>"}}"#;
        assert_eq!(roles(agent_done), [MessageRole::System]);
        match &parse_line(agent_done)[0] {
            HistoryItem::Message { text, .. } => assert!(text.contains("<result>All 7 tests")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn hand_back_and_cross_session_messages_are_system_activity() {
        let peer = r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Another Claude session sent a message:\n<agent-message from=\"a0e3\">\n[Subagent hand-back] The text below is the final report of a subagent. The report follows:\n  Done.\n</agent-message>"},"origin":{"kind":"peer","from":"a0e3"}}"#;
        assert_eq!(roles(peer), [MessageRole::System]);
        // Same content without the flags (other harness versions).
        let bare = r#"{"type":"user","message":{"role":"user","content":"Another Claude session sent a message:\n<cross-session-message from=\"x\">hi</cross-session-message>"}}"#;
        assert_eq!(roles(bare), [MessageRole::System]);
        let tagged = r#"{"type":"user","message":{"role":"user","content":"<cross-session-message from=\"x\">hi</cross-session-message>"}}"#;
        assert_eq!(roles(tagged), [MessageRole::System]);
        let handback =
            r#"{"type":"user","message":{"role":"user","content":"[Subagent hand-back] report"}}"#;
        assert_eq!(roles(handback), [MessageRole::System]);
    }

    #[test]
    fn command_wrappers_and_markers_are_system_activity() {
        for content in [
            "<command-name>/login</command-name>\n<command-message>login</command-message>\n<command-args></command-args>",
            "<local-command-stdout>Login successful</local-command-stdout>",
            "<bash-input>git status</bash-input>",
            "<bash-stdout>(Bash completed with no output)</bash-stdout><bash-stderr></bash-stderr>",
            "Caveat: The messages below were generated by the user while running local commands.",
            "[Request interrupted by user]",
            "<user-prompt-submit-hook>blocked</user-prompt-submit-hook>",
            "<some-new-wrapper>x</some-new-wrapper>",
        ] {
            let line = serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": content},
            })
            .to_string();
            assert_eq!(roles(&line), [MessageRole::System], "{content}");
        }
        // Even with a human origin, a slash command wrapper is not a prompt.
        let slash = r#"{"type":"user","message":{"role":"user","content":"<command-name>/goal</command-name>"},"origin":{"kind":"human"}}"#;
        assert_eq!(roles(slash), [MessageRole::System]);
        let interrupted = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
        assert_eq!(roles(interrupted), [MessageRole::System]);
    }

    #[test]
    fn compaction_summaries_are_system_activity() {
        let line = r#"{"type":"user","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"role":"user","content":"This session is being continued from a previous conversation that ran out of context."}}"#;
        assert_eq!(roles(line), [MessageRole::System]);
        let bare = r#"{"type":"user","message":{"role":"user","content":"This session is being continued from a previous conversation."}}"#;
        assert_eq!(roles(bare), [MessageRole::System]);
    }

    #[test]
    fn tool_results_are_tool_items_not_user_messages() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_013","type":"tool_result","content":[{"type":"text","text":"Async agent launched successfully."}]}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"aa3d","description":"Implement vca CLI crate"}}"#;
        let items = parse_line(line);
        assert!(roles(line).is_empty());
        assert!(matches!(
            &items[..],
            [HistoryItem::ToolResult { id: Some(id), text, .. }]
                if id == "toolu_013" && text.starts_with("Async agent launched")
        ));
        // Text blocks next to a tool result were added by the harness.
        let mixed = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t","type":"tool_result","content":"ok"},{"type":"text","text":"Base directory for this skill: C:\\skills\\x"}]},"toolUseResult":{}}"#;
        assert_eq!(roles(mixed), [MessageRole::System]);
    }

    #[test]
    fn injected_meta_and_sidechains_are_hidden() {
        for line in [
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"[Image: original 1320x2868, displayed at 920x2000]"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"Base directory for this skill: C:\\x"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>Caveat: ...</local-command-caveat>"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Stop hook feedback: keep going"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"You are a sub-agent. Do X."},"origin":{"kind":"human"}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"sub-agent reply"}]}}"#,
        ] {
            assert!(parse_line(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn assistant_text_stays_assistant() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"<b>Done</b>"},{"type":"tool_use","id":"t1","name":"Agent","input":{"description":"Wire cloud","prompt":"..."}}]}}"#;
        let items = parse_line(line);
        assert_eq!(roles(line), [MessageRole::Assistant]);
        assert!(matches!(
            &items[1],
            HistoryItem::ToolCall { name, summary, .. } if name == "Agent" && summary == "Wire cloud"
        ));
    }

    #[test]
    fn machine_text_heuristic() {
        assert!(is_machine_text("<task-notification>"));
        assert!(is_machine_text("  <agent-message from=\"a\">"));
        assert!(is_machine_text("<foo_bar attr=1>"));
        assert!(!is_machine_text("<pasted_content id=\"1\">"));
        assert!(!is_machine_text("< 3 items please"));
        assert!(!is_machine_text("<3 you"));
        assert!(!is_machine_text("Explain the <div> tag"));
        assert!(!is_machine_text("Fix the login page"));
        assert!(!is_known_machine_text("<div>hello</div>"));
    }

    #[test]
    fn first_prompt_skips_hand_backs_and_notifications() {
        let mut meta = TranscriptMeta::default();
        meta.observe_line(
            r#"{"type":"user","message":{"role":"user","content":"This session is being continued from a previous conversation."},"isCompactSummary":true}"#,
        );
        meta.observe_line(
            r#"{"type":"user","message":{"role":"user","content":"<task-notification>x</task-notification>"},"origin":{"kind":"task-notification"}}"#,
        );
        assert_eq!(meta.first_prompt, None);
        meta.observe_line(
            r#"{"type":"user","message":{"role":"user","content":"Ship it"},"origin":{"kind":"human"}}"#,
        );
        assert_eq!(meta.first_prompt.as_deref(), Some("Ship it"));
    }

    #[test]
    fn permission_mode_and_turn_end_records() {
        // Shapes as Claude Code 2.1.316 writes them.
        assert_eq!(
            parse_line(r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"c11d"}"#),
            [HistoryItem::PermissionMode {
                at: None,
                mode: "auto".into()
            }]
        );
        let end = r#"{"parentUuid":"04ac","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":52918,"messageCount":2109,"timestamp":"2026-11-01T15:18:41.750Z","isMeta":false}"#;
        assert_eq!(
            parse_line(end),
            [HistoryItem::TurnEnd {
                at: parse_rfc3339_millis("2026-11-01T15:18:41.750Z")
            }]
        );
        // Other system records and sub-agent records are not turn ends.
        assert!(
            parse_line(r#"{"type":"system","subtype":"stop_hook_summary","hookCount":2}"#)
                .is_empty()
        );
        assert!(
            parse_line(r#"{"type":"system","subtype":"turn_duration","isSidechain":true}"#)
                .is_empty()
        );
        assert!(parse_line(r#"{"type":"permission-mode"}"#).is_empty());
    }
}
