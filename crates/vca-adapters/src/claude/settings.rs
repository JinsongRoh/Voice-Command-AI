//! Installing VCA hooks into Claude Code `settings.json` files.
//!
//! Claude Code reads hooks from `~/.claude/settings.json` (user),
//! `.claude/settings.json` (project) and `.claude/settings.local.json`
//! (local project). The `hooks` object maps an event name to a list of
//! matcher groups, each with a list of handlers:
//!
//! ```json
//! { "hooks": { "PreToolUse": [ { "matcher": "Bash",
//!     "hooks": [ { "type": "command", "command": "vca hook claude pre-tool-use" } ] } ] } }
//! ```
//!
//! VCA adds its own matcher groups and recognizes its handlers by the
//! ` hook claude ` marker in the command, so user hooks are never touched.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::claude::hook_event::ClaudeHookEventName;
use crate::error::{AdapterError, Result};
use crate::fsutil;
use crate::types::{HookEntryStatus, HookOptions, HookScope, HookState, HookStatus};

/// One hook VCA installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeHookSpec {
    pub event: ClaudeHookEventName,
    /// Tool-name matcher, for tool events.
    pub matcher: Option<&'static str>,
}

/// The hooks VCA installs by default.
///
/// `PreToolUse` is limited to shell tools: it is where admission control
/// classifies commands, and running a hook process for every file read
/// would slow the agent down.
pub fn default_hooks() -> Vec<ClaudeHookSpec> {
    use ClaudeHookEventName as E;
    vec![
        ClaudeHookSpec {
            event: E::SessionStart,
            matcher: None,
        },
        ClaudeHookSpec {
            event: E::UserPromptSubmit,
            matcher: None,
        },
        ClaudeHookSpec {
            event: E::PreToolUse,
            matcher: Some("Bash|PowerShell"),
        },
        ClaudeHookSpec {
            event: E::PermissionRequest,
            matcher: None,
        },
        ClaudeHookSpec {
            event: E::Notification,
            matcher: None,
        },
        ClaudeHookSpec {
            event: E::Stop,
            matcher: None,
        },
        ClaudeHookSpec {
            event: E::SessionEnd,
            matcher: None,
        },
    ]
}

const MARKER: &str = " hook claude ";

/// The shell command for one event, for example `vca hook claude stop`.
pub fn hook_command(opts: &HookOptions, event: &ClaudeHookEventName) -> String {
    let mut parts: Vec<String> = opts
        .command
        .iter()
        .map(|a| fsutil::shell_quote(a))
        .collect();
    if parts.is_empty() {
        parts.push("vca".into());
    }
    parts.extend(["hook".into(), "claude".into(), event.cli_arg()]);
    parts.join(" ")
}

fn is_vca_handler(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(MARKER))
}

/// Path of the settings file for `scope`, given the Claude config directory.
pub fn settings_path(config_dir: &Path, scope: &HookScope) -> PathBuf {
    match scope {
        HookScope::User => config_dir.join("settings.json"),
        HookScope::Project(dir) => dir.join(".claude").join("settings.json"),
        HookScope::ProjectLocal(dir) => dir.join(".claude").join("settings.local.json"),
    }
}

/// Status of each hook in a parsed settings document.
pub fn status_of(
    settings: &Value,
    specs: &[ClaudeHookSpec],
    opts: &HookOptions,
) -> Vec<HookEntryStatus> {
    specs
        .iter()
        .map(|spec| {
            let groups = settings
                .get("hooks")
                .and_then(|h| h.get(spec.event.as_str()))
                .and_then(Value::as_array);
            let expected = hook_command(opts, &spec.event);
            let mut state = HookState::Missing;
            for group in groups.into_iter().flatten() {
                let matcher = group.get("matcher").and_then(Value::as_str);
                let handlers = group.get("hooks").and_then(Value::as_array);
                for handler in handlers.into_iter().flatten().filter(|h| is_vca_handler(h)) {
                    let exact = handler.get("command").and_then(Value::as_str) == Some(&expected)
                        && matcher.filter(|m| !m.is_empty()) == spec.matcher;
                    state = if exact {
                        HookState::Installed
                    } else {
                        HookState::Outdated
                    };
                }
            }
            HookEntryStatus {
                event: spec.event.as_str().to_owned(),
                state,
            }
        })
        .collect()
}

/// Removes VCA handlers from `settings` (for all events when `only` is
/// `None`). Empty groups and event lists left behind are dropped. Returns
/// true when something changed.
pub fn remove_vca_hooks(settings: &mut Value, only: Option<&str>) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let changed = remove_from_hooks(hooks, only);
    if hooks.is_empty()
        && let Some(obj) = settings.as_object_mut()
    {
        obj.remove("hooks");
    }
    changed
}

fn remove_from_hooks(hooks: &mut Map<String, Value>, only: Option<&str>) -> bool {
    let mut changed = false;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        if only.is_some_and(|o| o != event) {
            continue;
        }
        let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = handlers.len();
                handlers.retain(|h| !is_vca_handler(h));
                changed |= handlers.len() != before;
            }
        }
        let before = groups.len();
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|h| !h.is_empty())
        });
        changed |= groups.len() != before;
        if groups.is_empty() {
            hooks.remove(&event);
        }
    }
    changed
}

/// Adds or repairs VCA hooks in `settings`. Returns true when something
/// changed. Fails if `settings` or its `hooks` field has the wrong shape.
pub fn add_vca_hooks(
    settings: &mut Value,
    specs: &[ClaudeHookSpec],
    opts: &HookOptions,
) -> Result<bool> {
    let statuses = status_of(settings, specs, opts);
    let obj = settings
        .as_object_mut()
        .ok_or_else(|| AdapterError::InvalidConfig("settings root is not an object".into()))?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| AdapterError::InvalidConfig("`hooks` is not an object".into()))?;
    let mut changed = false;
    for (spec, status) in specs.iter().zip(statuses) {
        if status.state == HookState::Installed {
            continue;
        }
        let event = spec.event.as_str();
        if !hooks.get(event).is_none_or(Value::is_array) {
            return Err(AdapterError::InvalidConfig(format!(
                "`hooks.{event}` is not an array"
            )));
        }
        if status.state == HookState::Outdated {
            remove_from_hooks(hooks, Some(event));
        }
        let mut group = Map::new();
        if let Some(m) = spec.matcher {
            group.insert("matcher".into(), Value::String(m.into()));
        }
        group.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": hook_command(opts, &spec.event) }]),
        );
        hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("checked above")
            .push(Value::Object(group));
        changed = true;
    }
    Ok(changed)
}

fn load(path: &Path) -> Result<Value> {
    match fsutil::read_optional(path)? {
        None => Ok(Value::Object(Map::new())),
        Some(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Some(text) => serde_json::from_str(&text).map_err(|source| AdapterError::Json {
            path: path.to_owned(),
            source,
        }),
    }
}

fn save(path: &Path, value: &Value) -> Result<Option<PathBuf>> {
    let backup = fsutil::backup(path)?;
    let mut text = serde_json::to_string_pretty(value).map_err(|source| AdapterError::Json {
        path: path.to_owned(),
        source,
    })?;
    text.push('\n');
    fsutil::write_atomic(path, &text)?;
    Ok(backup)
}

/// Installs hooks into the settings file at `path`.
pub fn install(path: &Path, specs: &[ClaudeHookSpec], opts: &HookOptions) -> Result<HookStatus> {
    let mut settings = load(path)?;
    let backup_path = if add_vca_hooks(&mut settings, specs, opts)? {
        save(path, &settings)?
    } else {
        None
    };
    Ok(HookStatus {
        config_path: path.to_owned(),
        entries: status_of(&settings, specs, opts),
        backup_path,
    })
}

/// Removes all VCA hooks from the settings file at `path`.
pub fn uninstall(path: &Path, specs: &[ClaudeHookSpec], opts: &HookOptions) -> Result<HookStatus> {
    let mut settings = load(path)?;
    let backup_path = if remove_vca_hooks(&mut settings, None) {
        save(path, &settings)?
    } else {
        None
    };
    Ok(HookStatus {
        config_path: path.to_owned(),
        entries: status_of(&settings, specs, opts),
        backup_path,
    })
}

/// Reports hook status for the settings file at `path`.
pub fn status(path: &Path, specs: &[ClaudeHookSpec], opts: &HookOptions) -> Result<HookStatus> {
    let settings = load(path)?;
    Ok(HookStatus {
        config_path: path.to_owned(),
        entries: status_of(&settings, specs, opts),
        backup_path: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_SETTINGS: &str = r#"{
  "model": "opus",
  "permissions": { "allow": ["Bash(npm test)"] },
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "~/.claude/stop-hook-git-check.sh" } ] }
    ],
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [ { "type": "command", "command": "/usr/local/bin/block-rm.sh" } ] }
    ]
  }
}"#;

    fn opts() -> HookOptions {
        HookOptions::default()
    }

    #[test]
    fn install_merges_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, USER_SETTINGS).unwrap();

        let st = install(&path, &default_hooks(), &opts()).unwrap();
        assert!(st.all_installed(), "{st:?}");
        let backup = st.backup_path.expect("backup written");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), USER_SETTINGS);

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // User settings and hooks are preserved, in order.
        assert_eq!(v["model"], "opus");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "permissions", "hooks"]);
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(
            stop[0]["hooks"][0]["command"],
            "~/.claude/stop-hook-git-check.sh"
        );
        assert_eq!(stop[1]["hooks"][0]["command"], "vca hook claude stop");
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre[1]["matcher"], "Bash|PowerShell");
        assert_eq!(
            pre[1]["hooks"][0]["command"],
            "vca hook claude pre-tool-use"
        );

        // Second install changes nothing and writes no backup.
        let before = std::fs::read_to_string(&path).unwrap();
        let st2 = install(&path, &default_hooks(), &opts()).unwrap();
        assert!(st2.backup_path.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn outdated_command_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install(&path, &default_hooks(), &opts()).unwrap();
        let new_opts = HookOptions {
            command: vec!["/opt/VCA Tools/vca".into()],
        };
        let st = status(&path, &default_hooks(), &new_opts).unwrap();
        assert!(st.entries.iter().all(|e| e.state == HookState::Outdated));
        let st = install(&path, &default_hooks(), &new_opts).unwrap();
        assert!(st.all_installed());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(
            stop[0]["hooks"][0]["command"],
            "'/opt/VCA Tools/vca' hook claude stop"
        );
    }

    #[test]
    fn uninstall_leaves_user_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, USER_SETTINGS).unwrap();
        install(&path, &default_hooks(), &opts()).unwrap();
        let st = uninstall(&path, &default_hooks(), &opts()).unwrap();
        assert!(st.entries.iter().all(|e| e.state == HookState::Missing));
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let original: Value = serde_json::from_str(USER_SETTINGS).unwrap();
        assert_eq!(after, original);
        // Nothing left to remove: no write, no backup.
        assert!(
            uninstall(&path, &default_hooks(), &opts())
                .unwrap()
                .backup_path
                .is_none()
        );
    }

    #[test]
    fn creates_missing_file_and_rejects_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path(), &HookScope::Project(dir.path().join("proj")));
        let st = install(&path, &default_hooks(), &opts()).unwrap();
        assert!(st.all_installed());
        assert!(st.backup_path.is_none());

        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        assert!(matches!(
            install(&bad, &default_hooks(), &opts()),
            Err(AdapterError::Json { .. })
        ));
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
    }
}
