//! Watches whether the VCA hooks are still in Claude Code's user settings.
//!
//! Another tool can rewrite `~/.claude/settings.json` and drop the VCA
//! hooks. Claude Code sessions then report nothing through hooks: their
//! progress shows late (only from the transcript) and approvals are
//! inferred. vcad checks the file at startup, every [`CHECK_EVERY`] and
//! whenever its size or modification time changes (looked at every
//! [`STAT_EVERY`]), and marks the Claude Code sessions that never reported
//! through a hook with the `hooks_missing` problem
//! ([`vca_core::SESSION_PROBLEM_HOOKS_MISSING`]). Clients show a banner
//! with a repair button.
//!
//! Read-only: vcad never writes the agent's settings. Repairing is the
//! desktop app's `vca_host_hooks_install` command (or `vca hooks
//! install`), run only when the person at the PC asks for it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use vca_adapters::claude::settings as claude_settings;
use vca_adapters::{HookOptions, HookScope, HookState};

use crate::sessions::SessionManager;

/// How often the settings are read regardless of changes.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often the file's size and modification time are compared.
pub(crate) const STAT_EVERY: Duration = Duration::from_secs(15);

/// What a check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeHooks {
    /// Every VCA hook is there and runs a `vca` that exists.
    Present,
    /// A VCA hook is missing, or the `vca` they run is gone.
    Missing,
    /// The settings could not be read (malformed JSON): no claim either way.
    Unknown,
}

/// Is a hook program a path to no file (a `vca` that moved or was
/// removed)? A bare name is looked up on `PATH` by the shell: it counts.
fn program_gone(program: &str) -> bool {
    let path = Path::new(program);
    let is_path = path.is_absolute() || program.contains(['/', '\\']);
    is_path && !path.is_file()
}

/// Checks the Claude Code user settings in `config_dir`. Blocking: call it
/// off the async workers.
pub(crate) fn check_claude_hooks(config_dir: &Path) -> ClaudeHooks {
    let path = claude_settings::settings_path(config_dir, &HookScope::User);
    let specs = vca_adapters::claude::default_hooks();
    // The programs the VCA hooks run, whichever `vca` that is: the check is
    // whether hooks report at all, not whether they run this build's CLI.
    let Ok(probe) = claude_settings::status(&path, &specs, &HookOptions::default()) else {
        return ClaudeHooks::Unknown;
    };
    let present = probe.programs.iter().any(|program| {
        if program_gone(program) {
            return false;
        }
        let opts = HookOptions {
            command: vec![program.clone()],
        };
        claude_settings::status(&path, &specs, &opts).is_ok_and(|st| {
            !st.entries.is_empty() && st.entries.iter().all(|e| e.state != HookState::Missing)
        })
    });
    if present {
        ClaudeHooks::Present
    } else {
        ClaudeHooks::Missing
    }
}

/// Size and modification time of a file (`None`: no file).
fn stamp(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// Runs the checks until the manager is dropped by the server.
pub(crate) fn spawn(mgr: SessionManager, config_dir: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let settings = claude_settings::settings_path(&config_dir, &HookScope::User);
        let mut tick = tokio::time::interval(STAT_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_stamp = None;
        let mut last_check: Option<Instant> = None;
        loop {
            tick.tick().await;
            let path = settings.clone();
            let now_stamp = tokio::task::spawn_blocking(move || stamp(&path))
                .await
                .ok()
                .flatten();
            let due = last_check.is_none_or(|at| at.elapsed() >= CHECK_EVERY);
            if !due && now_stamp == last_stamp {
                continue;
            }
            last_stamp = now_stamp;
            last_check = Some(Instant::now());
            let dir = config_dir.clone();
            let found = tokio::task::spawn_blocking(move || check_claude_hooks(&dir))
                .await
                .unwrap_or(ClaudeHooks::Unknown);
            match found {
                ClaudeHooks::Present => mgr.set_claude_hooks_missing(false),
                ClaudeHooks::Missing => {
                    if !mgr.claude_hooks_missing() {
                        tracing::warn!(
                            settings = %settings.display(),
                            "the VCA hooks are missing from Claude Code's settings; \
                             progress shows late until they are repaired"
                        );
                    }
                    mgr.set_claude_hooks_missing(true);
                }
                ClaudeHooks::Unknown => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `vca` that exists, in the temp dir.
    fn fake_cli(dir: &Path) -> String {
        let cli = dir.join(format!("vca{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cli, b"").unwrap();
        cli.to_string_lossy().into_owned()
    }

    #[test]
    fn missing_without_a_settings_file_or_hooks() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_claude_hooks(dir.path()), ClaudeHooks::Missing);
        std::fs::write(dir.path().join("settings.json"), r#"{"model":"opus"}"#).unwrap();
        assert_eq!(check_claude_hooks(dir.path()), ClaudeHooks::Missing);
    }

    #[test]
    fn present_once_installed_and_missing_when_a_hook_or_the_cli_goes() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake_cli(dir.path());
        let path = dir.path().join("settings.json");
        claude_settings::install(
            &path,
            &vca_adapters::claude::default_hooks(),
            &HookOptions {
                command: vec![cli.clone()],
            },
        )
        .unwrap();
        assert_eq!(check_claude_hooks(dir.path()), ClaudeHooks::Present);

        // Another tool rewrote the file without the prompt hook.
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("UserPromptSubmit");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(check_claude_hooks(dir.path()), ClaudeHooks::Missing);

        // Repaired, then the CLI the hooks run is removed.
        claude_settings::install(
            &path,
            &vca_adapters::claude::default_hooks(),
            &HookOptions {
                command: vec![cli.clone()],
            },
        )
        .unwrap();
        assert_eq!(check_claude_hooks(dir.path()), ClaudeHooks::Present);
        std::fs::remove_file(&cli).unwrap();
        assert_eq!(check_claude_hooks(dir.path()), ClaudeHooks::Missing);
    }

    #[test]
    fn unreadable_settings_claim_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), "{ not json").unwrap();
        assert_eq!(check_claude_hooks(dir.path()), ClaudeHooks::Unknown);
    }
}
