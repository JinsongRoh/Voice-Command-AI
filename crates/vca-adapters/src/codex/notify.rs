//! Codex `notify` integration.
//!
//! `config.toml` takes a top-level `notify` array: the program Codex runs
//! after each agent turn. Codex appends one JSON argument, for example
//! `{"type":"agent-turn-complete","thread-id":"...","turn-id":"...",
//! "cwd":"...","client":"codex-tui","input-messages":[...],
//! "last-assistant-message":"..."}` (checked against openai/codex
//! `hooks/src/legacy_notify.rs`, October 2026).
//!
//! `notify` holds a single program, so when the user already has one VCA
//! chains it: `notify = ["vca", "hook", "codex", "--", <previous argv...>]`.
//! `vca hook codex` then forwards the payload to the previous program (see
//! [`parse_notify_args`]), and uninstalling restores it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use toml_edit::{Array, DocumentMut, Item, value};
use vca_core::{AgentKind, SessionState};

use crate::error::{AdapterError, Result};
use crate::fsutil;
use crate::hook::{AgentHookEvent, HookNotification};
use crate::types::{HookEntryStatus, HookOptions, HookState, HookStatus, truncate};

const SEPARATOR: &str = "--";

/// The `notify` argument vector VCA installs, chaining `previous`.
pub fn notify_argv(opts: &HookOptions, previous: &[String]) -> Vec<String> {
    let mut argv = if opts.command.is_empty() {
        vec!["vca".to_owned()]
    } else {
        opts.command.clone()
    };
    argv.extend(["hook".into(), "codex".into()]);
    if !previous.is_empty() {
        argv.push(SEPARATOR.into());
        argv.extend(previous.iter().cloned());
    }
    argv
}

/// Splits a `notify` argv into VCA's part and the chained previous program.
/// Returns `None` when it is not a VCA notify command.
fn split_vca_argv(argv: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let pos = argv
        .windows(2)
        .position(|w| w[0] == "hook" && w[1] == "codex")?;
    let ours = argv[..pos + 2].to_vec();
    let rest = &argv[pos + 2..];
    let previous = match rest.first() {
        Some(s) if s == SEPARATOR => rest[1..].to_vec(),
        _ => Vec::new(),
    };
    Some((ours, previous))
}

/// Arguments received by `vca hook codex`, after the `hook codex` words:
/// an optional `-- <previous program...>` chain followed by the payload
/// that Codex appends last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyArgs {
    /// Previous notify program to forward the payload to (payload appended).
    pub chain: Vec<String>,
    pub payload: String,
}

pub fn parse_notify_args(args: &[String]) -> Option<NotifyArgs> {
    let (payload, rest) = args.split_last()?;
    let chain = match rest.first() {
        Some(s) if s == SEPARATOR => rest[1..].to_vec(),
        _ => Vec::new(),
    };
    Some(NotifyArgs {
        chain,
        payload: payload.clone(),
    })
}

fn read_notify(doc: &DocumentMut) -> Option<Vec<String>> {
    let arr = doc.get("notify")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
    )
}

fn set_notify(doc: &mut DocumentMut, argv: &[String]) {
    let mut arr = Array::new();
    for a in argv {
        arr.push(a.as_str());
    }
    match doc.get_mut("notify").and_then(Item::as_value_mut) {
        // Keep the key's position and comments.
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = toml_edit::Value::Array(arr);
            *existing.decor_mut() = decor;
        }
        None => {
            doc["notify"] = value(arr);
        }
    }
}

fn state_of(doc: &DocumentMut, opts: &HookOptions) -> HookState {
    match read_notify(doc).as_deref().and_then(split_vca_argv) {
        None => HookState::Missing,
        Some((ours, _)) if ours == notify_argv(opts, &[]) => HookState::Installed,
        Some(_) => HookState::Outdated,
    }
}

fn load(path: &Path) -> Result<DocumentMut> {
    let text = fsutil::read_optional(path)?.unwrap_or_default();
    text.parse::<DocumentMut>().map_err(|e| AdapterError::Toml {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

fn status_value(
    path: &Path,
    doc: &DocumentMut,
    opts: &HookOptions,
    backup: Option<PathBuf>,
) -> HookStatus {
    HookStatus {
        config_path: path.to_owned(),
        entries: vec![HookEntryStatus {
            event: "notify".into(),
            state: state_of(doc, opts),
        }],
        backup_path: backup,
    }
}

fn save(path: &Path, doc: &DocumentMut) -> Result<Option<PathBuf>> {
    let backup = fsutil::backup(path)?;
    fsutil::write_atomic(path, &doc.to_string())?;
    Ok(backup)
}

/// Installs VCA as the `notify` program in `config.toml` at `path`.
pub fn install(path: &Path, opts: &HookOptions) -> Result<HookStatus> {
    let mut doc = load(path)?;
    if doc.get("notify").is_some_and(|i| i.as_array().is_none()) {
        return Err(AdapterError::InvalidConfig(format!(
            "`notify` in {} is not an array",
            path.display()
        )));
    }
    let current = read_notify(&doc).unwrap_or_default();
    let previous = match split_vca_argv(&current) {
        Some((_, previous)) => previous,
        None => current,
    };
    let desired = notify_argv(opts, &previous);
    let backup = if read_notify(&doc).as_deref() != Some(desired.as_slice()) {
        set_notify(&mut doc, &desired);
        save(path, &doc)?
    } else {
        None
    };
    Ok(status_value(path, &doc, opts, backup))
}

/// Removes VCA from `notify`, restoring a chained program if there was one.
pub fn uninstall(path: &Path, opts: &HookOptions) -> Result<HookStatus> {
    let mut doc = load(path)?;
    let backup = match read_notify(&doc).as_deref().and_then(split_vca_argv) {
        None => None,
        Some((_, previous)) => {
            if previous.is_empty() {
                doc.remove("notify");
            } else {
                set_notify(&mut doc, &previous);
            }
            save(path, &doc)?
        }
    };
    Ok(status_value(path, &doc, opts, backup))
}

pub fn status(path: &Path, opts: &HookOptions) -> Result<HookStatus> {
    let doc = load(path)?;
    Ok(status_value(path, &doc, opts, None))
}

/// The JSON payload Codex passes to the notify program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexNotifyPayload {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "thread-id", default)]
    pub thread_id: Option<String>,
    #[serde(rename = "turn-id", default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub client: Option<String>,
    #[serde(rename = "input-messages", default)]
    pub input_messages: Vec<String>,
    #[serde(rename = "last-assistant-message", default)]
    pub last_assistant_message: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl CodexNotifyPayload {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Translates the payload into VCA terms. `agent-turn-complete` means
    /// the agent finished its turn and is idle. Other types are reported
    /// without a state change.
    pub fn to_agent_event(&self) -> AgentHookEvent {
        let (state, notification) = match self.kind.as_str() {
            "agent-turn-complete" => (
                Some(SessionState::Idle),
                Some(HookNotification {
                    title: "Task complete".into(),
                    body: self
                        .last_assistant_message
                        .as_deref()
                        .map(|m| truncate(m, 300))
                        .unwrap_or_default(),
                }),
            ),
            other => (
                None,
                Some(HookNotification {
                    title: "Codex".into(),
                    body: other.to_owned(),
                }),
            ),
        };
        AgentHookEvent {
            agent: AgentKind::Codex,
            event_name: self.kind.clone(),
            agent_session_id: self.thread_id.clone(),
            cwd: self.cwd.clone(),
            transcript_path: None,
            state,
            notification,
            decision: None,
            tool: None,
            blocking: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_CONFIG: &str = r#"# My Codex settings
model = "gpt-5-codex" # favourite

[projects."/home/me/app"]
trust_level = "trusted"
"#;

    #[test]
    fn install_preserves_formatting_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, USER_CONFIG).unwrap();
        let st = install(&path, &HookOptions::default()).unwrap();
        assert!(st.all_installed());
        assert!(st.backup_path.is_some());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# My Codex settings\nmodel = \"gpt-5-codex\" # favourite\n"));
        assert!(text.contains("[projects.\"/home/me/app\"]\ntrust_level = \"trusted\""));
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(read_notify(&doc).unwrap(), ["vca", "hook", "codex"]);
        // notify must stay a top-level key, before the first table.
        assert!(text.find("notify").unwrap() < text.find("[projects").unwrap());

        let st2 = install(&path, &HookOptions::default()).unwrap();
        assert!(st2.backup_path.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        let st3 = uninstall(&path, &HookOptions::default()).unwrap();
        assert_eq!(st3.entries[0].state, HookState::Missing);
        let restored = std::fs::read_to_string(&path).unwrap();
        assert!(!restored.contains("notify"));
        assert!(restored.contains("# favourite"));
    }

    #[test]
    fn chains_existing_notify_program() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "notify = [\"python3\", \"/home/me/notify.py\"]\n").unwrap();
        install(&path, &HookOptions::default()).unwrap();
        let doc = load(&path).unwrap();
        assert_eq!(
            read_notify(&doc).unwrap(),
            [
                "vca",
                "hook",
                "codex",
                "--",
                "python3",
                "/home/me/notify.py"
            ]
        );
        // Reinstall with a new path keeps the chain.
        let opts = HookOptions {
            command: vec!["/usr/local/bin/vca".into()],
        };
        assert_eq!(
            status(&path, &opts).unwrap().entries[0].state,
            HookState::Outdated
        );
        install(&path, &opts).unwrap();
        let doc = load(&path).unwrap();
        assert_eq!(
            read_notify(&doc).unwrap(),
            [
                "/usr/local/bin/vca",
                "hook",
                "codex",
                "--",
                "python3",
                "/home/me/notify.py"
            ]
        );
        uninstall(&path, &opts).unwrap();
        let doc = load(&path).unwrap();
        assert_eq!(
            read_notify(&doc).unwrap(),
            ["python3", "/home/me/notify.py"]
        );
    }

    #[test]
    fn creates_missing_config_and_rejects_bad_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home").join("config.toml");
        assert!(
            install(&path, &HookOptions::default())
                .unwrap()
                .all_installed()
        );
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "notify = [").unwrap();
        assert!(matches!(
            install(&bad, &HookOptions::default()),
            Err(AdapterError::Toml { .. })
        ));
    }

    #[test]
    fn parses_notify_args_and_payload() {
        let payload = r#"{"type":"agent-turn-complete","thread-id":"b5f6c1c2-1111-2222-3333-444455556666","turn-id":"12345","cwd":"/Users/example/project","client":"codex-tui","input-messages":["Rename `foo` to `bar`."],"last-assistant-message":"Rename complete."}"#;
        let args: Vec<String> = ["--", "python3", "n.py", payload]
            .map(String::from)
            .to_vec();
        let parsed = parse_notify_args(&args).unwrap();
        assert_eq!(parsed.chain, ["python3", "n.py"]);
        let p = CodexNotifyPayload::parse(&parsed.payload).unwrap();
        let ev = p.to_agent_event();
        assert_eq!(ev.state, Some(SessionState::Idle));
        assert_eq!(
            ev.agent_session_id.as_deref(),
            Some("b5f6c1c2-1111-2222-3333-444455556666")
        );
        assert_eq!(ev.notification.unwrap().body, "Rename complete.");
        assert_eq!(p.client.as_deref(), Some("codex-tui"));

        let plain = parse_notify_args(&[payload.to_owned()]).unwrap();
        assert!(plain.chain.is_empty());
        assert!(parse_notify_args(&[]).is_none());
    }
}
