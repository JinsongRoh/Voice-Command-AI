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
use vca_core::{MessageRole, UnixMillis};

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

/// Parses one transcript record into history items.
pub fn parse_record(record: &Value) -> Vec<HistoryItem> {
    let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "user" | "assistant") {
        return Vec::new();
    }
    // Sidechains are subagent conversations; meta records are injected
    // context that the user never typed.
    if is_true(record, "isSidechain") || is_true(record, "isMeta") {
        return Vec::new();
    }
    let at = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_millis);
    let Some(message) = record.get("message") else {
        return Vec::new();
    };
    let role = if kind == "user" {
        MessageRole::User
    } else {
        MessageRole::Assistant
    };
    match message.get("content") {
        Some(Value::String(text)) => text_item(at, role, text).into_iter().collect(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| parse_block(at, role, b))
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_block(at: Option<UnixMillis>, role: MessageRole, block: &Value) -> Option<HistoryItem> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => text_item(at, role, block.get("text")?.as_str()?),
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
        // thinking, redacted_thinking, image, document, ...
        _ => None,
    }
}

fn text_item(at: Option<UnixMillis>, role: MessageRole, text: &str) -> Option<HistoryItem> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with("<system-reminder>") {
        return None;
    }
    // Slash commands, local command output and interruption markers are
    // written as user records but are not something the user typed.
    let role = if role == MessageRole::User
        && (trimmed.starts_with("<command-")
            || trimmed.starts_with("<local-command-")
            || trimmed.starts_with("[Request interrupted"))
    {
        MessageRole::System
    } else {
        role
    };
    Some(HistoryItem::Message {
        at,
        role,
        text: trimmed.to_owned(),
    })
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
        "Task" | "Agent" => field("description").map(str::to_owned),
        _ => None,
    };
    let summary = summary.unwrap_or_else(|| match input {
        Value::Null => String::new(),
        other => other.to_string(),
    });
    truncate(&summary.replace('\n', " "), MAX_SUMMARY)
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
                        } => Some(truncate(&text.replace('\n', " "), 80)),
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
}
