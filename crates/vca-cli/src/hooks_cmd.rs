//! `vca hooks install|uninstall|status`.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::ValueEnum;
use vca_adapters::claude::ClaudeAdapter;
use vca_adapters::codex::CodexAdapter;
use vca_adapters::{AgentAdapter, HookOptions, HookScope, HookState, HookStatus};

use crate::error::{CliError, CliResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HooksAction {
    Install,
    Uninstall,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum HookAgent {
    Claude,
    Codex,
    #[default]
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ScopeArg {
    #[default]
    User,
    Project,
    ProjectLocal,
}

/// Builds the adapter scope; project scopes use `dir` or the current
/// directory.
pub fn hook_scope(scope: ScopeArg, dir: Option<PathBuf>) -> CliResult<HookScope> {
    let project_dir = || match dir.clone() {
        Some(dir) => Ok(dir),
        None => std::env::current_dir()
            .map_err(|err| CliError::failure(format!("cannot read the current directory: {err}"))),
    };
    Ok(match scope {
        ScopeArg::User => HookScope::User,
        ScopeArg::Project => HookScope::Project(project_dir()?),
        ScopeArg::ProjectLocal => HookScope::ProjectLocal(project_dir()?),
    })
}

/// The command hooks call: this executable.
fn hook_options() -> HookOptions {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| "vca".into());
    HookOptions { command: vec![exe] }
}

fn state_label(state: HookState) -> &'static str {
    match state {
        HookState::Installed => "installed",
        HookState::Missing => "missing",
        HookState::Outdated => "outdated",
    }
}

pub fn format_status(agent: &str, status: &HookStatus) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{agent}: {}", status.config_path.display());
    let width = status
        .entries
        .iter()
        .map(|e| e.event.len())
        .max()
        .unwrap_or(0);
    for entry in &status.entries {
        let _ = writeln!(
            out,
            "  {:<width$}  {}",
            entry.event,
            state_label(entry.state)
        );
    }
    for program in &status.programs {
        let _ = writeln!(out, "  runs: {program}");
    }
    if let Some(backup) = &status.backup_path {
        let _ = writeln!(out, "  backup: {}", backup.display());
    }
    out
}

pub async fn hooks(
    action: HooksAction,
    agent: HookAgent,
    scope: ScopeArg,
    dir: Option<PathBuf>,
) -> CliResult<i32> {
    let scope = hook_scope(scope, dir)?;
    let opts = hook_options();
    let mut adapters: Vec<(&str, Box<dyn AgentAdapter>)> = Vec::new();
    if matches!(agent, HookAgent::Claude | HookAgent::All) {
        adapters.push(("claude", Box::new(ClaudeAdapter::new())));
    }
    if matches!(agent, HookAgent::Codex | HookAgent::All) {
        adapters.push(("codex", Box::new(CodexAdapter::new())));
    }

    let mut failed = false;
    for (name, adapter) in adapters {
        let result = match action {
            HooksAction::Install => adapter.install_hooks(&scope, &opts).await,
            HooksAction::Uninstall => adapter.uninstall_hooks(&scope).await,
            HooksAction::Status => adapter.hooks_status(&scope, &opts).await,
        };
        match result {
            Ok(status) => print!("{}", format_status(name, &status)),
            Err(err) => {
                eprintln!("error: {name}: {err}");
                failed = true;
            }
        }
    }
    Ok(if failed { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use vca_adapters::HookEntryStatus;

    use super::*;

    #[test]
    fn scopes() {
        assert_eq!(hook_scope(ScopeArg::User, None).unwrap(), HookScope::User);
        assert_eq!(
            hook_scope(ScopeArg::ProjectLocal, Some("/p".into())).unwrap(),
            HookScope::ProjectLocal("/p".into())
        );
        assert!(matches!(
            hook_scope(ScopeArg::Project, None).unwrap(),
            HookScope::Project(_)
        ));
    }

    #[test]
    fn status_format() {
        let status = HookStatus {
            config_path: "/home/u/.claude/settings.json".into(),
            entries: vec![
                HookEntryStatus {
                    event: "Stop".into(),
                    state: HookState::Installed,
                },
                HookEntryStatus {
                    event: "PreToolUse".into(),
                    state: HookState::Missing,
                },
            ],
            backup_path: None,
            programs: Vec::new(),
        };
        assert_eq!(
            format_status("claude", &status),
            "claude: /home/u/.claude/settings.json\n  Stop        installed\n  PreToolUse  missing\n"
        );
    }

    #[tokio::test]
    async fn hooks_of_an_old_vca_show_missing_and_install_replaces_them() {
        // A temporary Claude config dir; never the real `~/.claude`.
        let dir = tempfile::tempdir().unwrap();
        let adapter = ClaudeAdapter::with_config_dir(dir.path());
        let old = HookOptions {
            command: vec![
                dir.path()
                    .join("deleted")
                    .join("vca")
                    .to_string_lossy()
                    .into_owned(),
            ],
        };
        adapter.install_hooks(&HookScope::User, &old).await.unwrap();
        let current = hook_options();
        let st = adapter
            .hooks_status(&HookScope::User, &current)
            .await
            .unwrap();
        assert!(st.entries.iter().all(|e| e.state == HookState::Missing));
        let text = format_status("claude", &st);
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with("Stop ") && l.ends_with(" missing")),
            "{text}"
        );

        let st = adapter
            .install_hooks(&HookScope::User, &current)
            .await
            .unwrap();
        assert!(st.all_installed(), "{st:?}");
        assert_eq!(st.programs, current.command);
        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert_eq!(text.matches(" hook claude stop").count(), 1, "{text}");
    }

    #[test]
    fn status_format_names_the_program() {
        let status = HookStatus {
            config_path: "/home/u/.codex/config.toml".into(),
            entries: vec![HookEntryStatus {
                event: "notify".into(),
                state: HookState::Outdated,
            }],
            backup_path: Some("/home/u/.codex/config.toml.vca-backup-1".into()),
            programs: vec!["/opt/old/vca".into()],
        };
        assert_eq!(
            format_status("codex", &status),
            "codex: /home/u/.codex/config.toml\n  notify  outdated\n  runs: /opt/old/vca\n  backup: /home/u/.codex/config.toml.vca-backup-1\n"
        );
    }
}
