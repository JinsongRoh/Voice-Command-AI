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

/// The program part of the commands VCA writes for `opts` (the text before
/// ` hook claude `), as it appears in the settings file.
fn expected_program(opts: &HookOptions) -> String {
    let command = hook_command(opts, &ClaudeHookEventName::Stop);
    match command.split_once(MARKER) {
        Some((program, _)) => program.trim().to_owned(),
        None => command,
    }
}

/// The program part of a VCA handler's command, as written.
fn handler_program(handler: &Value) -> Option<&str> {
    let command = handler.get("command").and_then(Value::as_str)?;
    command
        .split_once(MARKER)
        .map(|(program, _)| program.trim())
}

/// Compares program paths the way the file system does: without case and
/// with either slash on Windows, exactly elsewhere.
pub fn same_program(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        let norm = |s: &str| s.replace('/', "\\").to_lowercase();
        norm(a) == norm(b)
    } else {
        a == b
    }
}

fn is_vca_handler(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(MARKER))
}

/// Reverses [`fsutil::shell_quote`].
fn shell_unquote(text: &str) -> String {
    match text
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        Some(inner) => inner.replace(r"'\''", "'"),
        None => text.to_owned(),
    }
}

/// Programs run by the VCA handlers in `settings` (any event): the command
/// text before ` hook claude `, unquoted, without duplicates.
pub fn vca_programs(settings: &Value) -> Vec<String> {
    let mut programs: Vec<String> = Vec::new();
    let events = settings.get("hooks").and_then(Value::as_object);
    for groups in events.into_iter().flat_map(|e| e.values()) {
        for group in groups.as_array().into_iter().flatten() {
            let handlers = group.get("hooks").and_then(Value::as_array);
            for command in handlers
                .into_iter()
                .flatten()
                .filter_map(|h| h.get("command").and_then(Value::as_str))
            {
                if let Some((program, _)) = command.split_once(MARKER) {
                    let program = shell_unquote(program.trim());
                    if !programs.contains(&program) {
                        programs.push(program);
                    }
                }
            }
        }
    }
    programs
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
///
/// A VCA handler counts only when it runs the program of `opts` (the `vca`
/// installed now): one left behind by an older install, or by a `vca` in a
/// folder that has since moved, cannot report anything, so its event is
/// [`HookState::Missing`]. An event whose handler runs the right program
/// but with another command line or matcher, or next to such a stale
/// handler, is [`HookState::Outdated`]; installing replaces both.
pub fn status_of(
    settings: &Value,
    specs: &[ClaudeHookSpec],
    opts: &HookOptions,
) -> Vec<HookEntryStatus> {
    let program = expected_program(opts);
    specs
        .iter()
        .map(|spec| {
            let groups = settings
                .get("hooks")
                .and_then(|h| h.get(spec.event.as_str()))
                .and_then(Value::as_array);
            let expected = hook_command(opts, &spec.event);
            let (mut exact, mut ours, mut stale) = (0usize, 0usize, 0usize);
            for group in groups.into_iter().flatten() {
                let matcher = group.get("matcher").and_then(Value::as_str);
                let handlers = group.get("hooks").and_then(Value::as_array);
                for handler in handlers.into_iter().flatten().filter(|h| is_vca_handler(h)) {
                    if !handler_program(handler).is_some_and(|p| same_program(p, &program)) {
                        stale += 1;
                        continue;
                    }
                    ours += 1;
                    if handler.get("command").and_then(Value::as_str) == Some(&expected)
                        && matcher.filter(|m| !m.is_empty()) == spec.matcher
                    {
                        exact += 1;
                    }
                }
            }
            let state = if ours == 0 {
                HookState::Missing
            } else if exact == 1 && ours == 1 && stale == 0 {
                HookState::Installed
            } else {
                HookState::Outdated
            };
            HookEntryStatus {
                event: spec.event.as_str().to_owned(),
                state,
            }
        })
        .collect()
}

/// Whether the program of `opts` is a path (not a bare name looked up on
/// `PATH`) to no file.
fn program_is_gone(opts: &HookOptions) -> bool {
    opts.command.first().is_some_and(|program| {
        let path = Path::new(program);
        let is_path = path.is_absolute() || program.contains(['/', '\\']);
        is_path && !path.is_file()
    })
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
        // Stale handlers (another program, or the right one with an old
        // command line) are replaced, never kept next to the new one.
        remove_from_hooks(hooks, Some(event));
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
        programs: vca_programs(&settings),
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
        programs: vca_programs(&settings),
    })
}

/// Reports hook status for the settings file at `path`.
pub fn status(path: &Path, specs: &[ClaudeHookSpec], opts: &HookOptions) -> Result<HookStatus> {
    let settings = load(path)?;
    Ok(HookStatus {
        config_path: path.to_owned(),
        entries: status_of(&settings, specs, opts),
        backup_path: None,
        programs: vca_programs(&settings),
    })
}

/// [`status`], and when the program of `opts` is a path to no file (the
/// `vca` the hooks would run is gone), every hook counts as missing. Only
/// reads the file system; nothing is rewritten.
pub fn status_on_disk(
    path: &Path,
    specs: &[ClaudeHookSpec],
    opts: &HookOptions,
) -> Result<HookStatus> {
    let mut st = status(path, specs, opts)?;
    if program_is_gone(opts) {
        for entry in &mut st.entries {
            entry.state = HookState::Missing;
        }
    }
    Ok(st)
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
    fn hooks_of_another_vca_are_missing_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install(&path, &default_hooks(), &opts()).unwrap();
        let new_opts = HookOptions {
            command: vec!["/opt/VCA Tools/vca".into()],
        };
        let st = status(&path, &default_hooks(), &new_opts).unwrap();
        assert!(st.entries.iter().all(|e| e.state == HookState::Missing));
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
    fn reports_the_programs_hooks_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, USER_SETTINGS).unwrap();
        assert!(
            status(&path, &default_hooks(), &opts())
                .unwrap()
                .programs
                .is_empty()
        );

        let old = HookOptions {
            command: vec![r"C:\Users\me\AppData\Local\Programs\vca\vca.exe".into()],
        };
        let st = install(&path, &default_hooks(), &old).unwrap();
        assert_eq!(
            st.programs,
            [r"C:\Users\me\AppData\Local\Programs\vca\vca.exe"]
        );

        // Pointing to the bundled CLI: missing until repaired, then only
        // the new program remains; user hooks stay.
        let bundled = HookOptions {
            command: vec!["/Applications/It's VCA.app/Contents/MacOS/vca".into()],
        };
        let st = status(&path, &default_hooks(), &bundled).unwrap();
        assert!(st.entries.iter().all(|e| e.state == HookState::Missing));
        let st = install(&path, &default_hooks(), &bundled).unwrap();
        assert!(st.all_installed());
        assert_eq!(
            st.programs,
            ["/Applications/It's VCA.app/Contents/MacOS/vca"]
        );
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "~/.claude/stop-hook-git-check.sh"
        );

        let st = uninstall(&path, &default_hooks(), &bundled).unwrap();
        assert!(st.programs.is_empty());
    }

    fn vca_commands(v: &Value, event: &str) -> Vec<String> {
        v["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_owned))
            .filter(|c| c.contains(MARKER))
            .collect()
    }

    #[test]
    fn stale_entries_are_replaced_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, USER_SETTINGS).unwrap();
        // An old install in a folder that no longer exists, for some events
        // only, and with an old matcher on PreToolUse.
        let old = HookOptions {
            command: vec!["/old/Voice Command AI/vca".into()],
        };
        install(&path, &default_hooks(), &old).unwrap();
        let current = HookOptions {
            command: vec!["/new/Voice Command AI/vca".into()],
        };
        let st = status(&path, &default_hooks(), &current).unwrap();
        assert!(st.entries.iter().all(|e| e.state == HookState::Missing));
        assert!(!st.all_installed());

        let st = install(&path, &default_hooks(), &current).unwrap();
        assert!(st.all_installed(), "{st:?}");
        assert_eq!(st.programs, ["/new/Voice Command AI/vca"]);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for spec in default_hooks() {
            let commands = vca_commands(&v, spec.event.as_str());
            assert_eq!(
                commands,
                [hook_command(&current, &spec.event)],
                "{}",
                spec.event.as_str()
            );
        }
        // User hooks stay.
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "~/.claude/stop-hook-git-check.sh"
        );
    }

    #[test]
    fn a_stale_handler_next_to_the_current_one_is_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install(&path, &default_hooks(), &opts()).unwrap();
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["hooks"]["Stop"].as_array_mut().unwrap().push(json!({
            "hooks": [{ "type": "command", "command": "/gone/vca hook claude stop" }]
        }));
        // Same program, old matcher.
        v["hooks"]["PreToolUse"][0]["matcher"] = json!("Bash");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        let st = status(&path, &default_hooks(), &opts()).unwrap();
        let state = |event: &str| st.entries.iter().find(|e| e.event == event).unwrap().state;
        assert_eq!(state("Stop"), HookState::Outdated);
        assert_eq!(state("PreToolUse"), HookState::Outdated);
        assert_eq!(state("SessionStart"), HookState::Installed);

        let st = install(&path, &default_hooks(), &opts()).unwrap();
        assert!(st.all_installed(), "{st:?}");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(vca_commands(&v, "Stop"), ["vca hook claude stop"]);
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash|PowerShell");
        assert_eq!(st.programs, ["vca"]);
    }

    #[test]
    fn hooks_running_a_vca_that_is_gone_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let vca = dir.path().join("Voice Command AI").join("vca");
        let opts = HookOptions {
            command: vec![vca.to_string_lossy().into_owned()],
        };
        install(&path, &default_hooks(), &opts).unwrap();
        // The program is not there: every hook is missing on disk.
        let st = status_on_disk(&path, &default_hooks(), &opts).unwrap();
        assert!(st.entries.iter().all(|e| e.state == HookState::Missing));
        // Once it exists the same hooks are installed.
        std::fs::create_dir_all(vca.parent().unwrap()).unwrap();
        std::fs::write(&vca, "").unwrap();
        assert!(
            status_on_disk(&path, &default_hooks(), &opts)
                .unwrap()
                .all_installed()
        );
        // A bare name is looked up on PATH and never checked here.
        let bare = HookOptions::default();
        install(&path, &default_hooks(), &bare).unwrap();
        assert!(
            status_on_disk(&path, &default_hooks(), &bare)
                .unwrap()
                .all_installed()
        );
    }

    #[test]
    fn program_comparison() {
        assert!(same_program("/a/vca", "/a/vca"));
        assert!(!same_program("/a/vca", "/b/vca"));
        if cfg!(windows) {
            assert!(same_program(r"C:\Apps\VCA\vca.exe", "c:/apps/vca/VCA.EXE"));
        }
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
