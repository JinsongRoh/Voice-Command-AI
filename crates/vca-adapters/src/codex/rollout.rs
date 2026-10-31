//! OpenAI Codex session files ("rollouts").
//!
//! Codex writes `<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`.
//! Current versions write one envelope per line:
//! `{"timestamp": "...", "type": "session_meta" | "response_item" |
//! "event_msg" | "turn_context" | "compacted", "payload": {...}}`.
//! Older versions wrote a bare metadata object first and bare response
//! items afterwards. Both are accepted.
//!
//! The history is built from `response_item` records (the model-visible
//! conversation): `message`, `function_call`, `custom_tool_call`,
//! `local_shell_call` and their outputs. `event_msg` records repeat the
//! same content for UIs and are ignored to avoid duplicates.
//!
//! Format checked against the openai/codex sources (protocol crate,
//! October 2026); field names are not a public contract, so parsing is
//! tolerant.

use std::path::{Path, PathBuf};

use serde_json::Value;
use vca_core::{MessageRole, UnixMillis};

use crate::types::{HistoryItem, parse_rfc3339_millis, truncate};

const MAX_TOOL_TEXT: usize = 2_000;
const MAX_SUMMARY: usize = 200;

/// Parses one rollout line into history items.
pub fn parse_line(line: &str) -> Vec<HistoryItem> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let at = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_millis);
    match (
        record.get("type").and_then(Value::as_str),
        record.get("payload"),
    ) {
        (Some("response_item"), Some(payload)) => parse_response_item(at, payload),
        (Some(_), Some(_)) => Vec::new(),
        // Legacy format: bare response items.
        (Some(_), None) => parse_response_item(at, &record),
        (None, _) => Vec::new(),
    }
}

fn parse_response_item(at: Option<UnixMillis>, item: &Value) -> Vec<HistoryItem> {
    let s = |k: &str| item.get(k).and_then(Value::as_str);
    let call_id = s("call_id").or_else(|| s("id")).map(str::to_owned);
    match s("type").unwrap_or("") {
        "message" => {
            let role = match s("role") {
                Some("user") => MessageRole::User,
                Some("assistant") => MessageRole::Assistant,
                // developer / system messages are instructions, not chat.
                _ => return Vec::new(),
            };
            let text = message_text(item.get("content"));
            if text.trim().is_empty() || (role == MessageRole::User && is_injected_context(&text)) {
                return Vec::new();
            }
            vec![HistoryItem::Message {
                at,
                role,
                text: text.trim().to_owned(),
            }]
        }
        "function_call" => {
            let name = s("name").unwrap_or("function").to_owned();
            let args = s("arguments")
                .and_then(|a| serde_json::from_str::<Value>(a).ok())
                .unwrap_or(Value::Null);
            vec![HistoryItem::ToolCall {
                at,
                id: call_id,
                summary: call_summary(&args, s("arguments")),
                name,
            }]
        }
        "custom_tool_call" => vec![HistoryItem::ToolCall {
            at,
            id: call_id,
            name: s("name").unwrap_or("tool").to_owned(),
            summary: truncate(&s("input").unwrap_or("").replace('\n', " "), MAX_SUMMARY),
        }],
        "local_shell_call" => {
            let command = item
                .get("action")
                .and_then(|a| a.get("command"))
                .map(command_string)
                .unwrap_or_default();
            vec![HistoryItem::ToolCall {
                at,
                id: call_id,
                name: "shell".into(),
                summary: truncate(&command, MAX_SUMMARY),
            }]
        }
        "function_call_output" | "custom_tool_call_output" => {
            let (text, is_error) = output_text(item.get("output"));
            vec![HistoryItem::ToolResult {
                at,
                id: call_id,
                text: truncate(&text, MAX_TOOL_TEXT),
                is_error,
            }]
        }
        // reasoning, web_search_call, compaction, ...
        _ => Vec::new(),
    }
}

/// Messages Codex injects as user turns (environment, AGENTS.md, ...).
fn is_injected_context(text: &str) -> bool {
    let t = text.trim_start();
    [
        "<environment_context>",
        "<user_instructions>",
        "<permissions instructions>",
        "<user_shell_command>",
        "# AGENTS.md instructions",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

fn message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text" | "text") => {
                    p.get("text").and_then(Value::as_str).map(str::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Function call arguments: shell tools carry `command` as a list or string.
fn call_summary(args: &Value, raw: Option<&str>) -> String {
    let text = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .map(command_string)
        .unwrap_or_else(|| raw.unwrap_or("").to_owned());
    truncate(&text.replace('\n', " "), MAX_SUMMARY)
}

fn command_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            // ["bash", "-lc", "<script>"] -> "<script>"
            if parts.len() == 3 && matches!(parts[1], "-lc" | "-c") {
                parts[2].to_owned()
            } else {
                parts.join(" ")
            }
        }
        other => other.to_string(),
    }
}

/// Tool output: a string, a JSON-encoded `{output, metadata}` string, or an
/// object with `content` / `success`.
fn output_text(output: Option<&Value>) -> (String, bool) {
    let exit_error = |v: &Value| {
        v.get("metadata")
            .and_then(|m| m.get("exit_code"))
            .and_then(Value::as_i64)
            .is_some_and(|c| c != 0)
    };
    match output {
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(v) if v.get("output").is_some() => (
                v["output"].as_str().unwrap_or_default().to_owned(),
                exit_error(&v),
            ),
            _ => (s.clone(), false),
        },
        Some(v @ Value::Object(_)) => {
            let text = v
                .get("content")
                .or_else(|| v.get("output"))
                .map(|c| {
                    c.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| c.to_string())
                })
                .unwrap_or_default();
            let failed = v.get("success").and_then(Value::as_bool) == Some(false) || exit_error(v);
            (text, failed)
        }
        Some(other) => (other.to_string(), false),
        None => (String::new(), false),
    }
}

/// Metadata of a rollout file for discovery.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RolloutMeta {
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub started_at: Option<UnixMillis>,
    pub first_prompt: Option<String>,
}

/// Reads metadata from the first lines of a rollout.
pub fn read_meta(path: &Path) -> std::io::Result<RolloutMeta> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(path)?;
    let mut meta = RolloutMeta::default();
    for line in BufReader::new(file).lines().take(200) {
        let line = line?;
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let payload = match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => record.get("payload"),
            // Legacy: first line is the bare metadata object.
            None if meta.session_id.is_none() => Some(&record),
            _ => None,
        };
        if let Some(p) = payload {
            let s = |k: &str| p.get(k).and_then(Value::as_str);
            meta.session_id = s("id").or_else(|| s("session_id")).map(str::to_owned);
            meta.cwd = s("cwd").map(PathBuf::from).or(meta.cwd.take());
            meta.started_at = s("timestamp").and_then(parse_rfc3339_millis);
            continue;
        }
        if meta.first_prompt.is_none() {
            meta.first_prompt = parse_line(&line).into_iter().find_map(|i| match i {
                HistoryItem::Message {
                    role: MessageRole::User,
                    text,
                    ..
                } => Some(truncate(&text.replace('\n', " "), 80)),
                _ => None,
            });
        }
        if meta.session_id.is_some() && meta.first_prompt.is_some() {
            break;
        }
    }
    Ok(meta)
}

/// Session id from a file name `rollout-2025-05-07T17-24-21-<uuid>.jsonl`.
pub fn id_from_file_name(name: &str) -> Option<String> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    // A UUID is the last 36 characters.
    (stem.len() > 36).then(|| stem[stem.len() - 36..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_from_rollout_name() {
        assert_eq!(
            id_from_file_name(
                "rollout-2025-05-07T17-24-21-5973b6c0-94b8-487b-a530-2aeb6098ae0e.jsonl"
            )
            .as_deref(),
            Some("5973b6c0-94b8-487b-a530-2aeb6098ae0e")
        );
        assert_eq!(id_from_file_name("other.jsonl"), None);
    }

    #[test]
    fn shell_call_summaries() {
        let line = r#"{"timestamp":"2025-09-01T10:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"cargo test\"],\"workdir\":\"/w\"}","call_id":"call_1"}}"#;
        match &parse_line(line)[..] {
            [
                HistoryItem::ToolCall {
                    name, summary, id, ..
                },
            ] => {
                assert_eq!((name.as_str(), summary.as_str()), ("shell", "cargo test"));
                assert_eq!(id.as_deref(), Some("call_1"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn tool_output_exit_code_marks_error() {
        let line = r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"{\"output\":\"boom\",\"metadata\":{\"exit_code\":1,\"duration_seconds\":0.1}}"}}"#;
        match &parse_line(line)[..] {
            [HistoryItem::ToolResult { text, is_error, .. }] => {
                assert_eq!(text, "boom");
                assert!(*is_error);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn skips_injected_and_developer_messages() {
        let env = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"}]}}"#;
        let dev = r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"rules"}]}}"#;
        let event = r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#;
        assert!(parse_line(env).is_empty());
        assert!(parse_line(dev).is_empty());
        assert!(parse_line(event).is_empty());
        assert!(parse_line("garbage").is_empty());
    }
}
