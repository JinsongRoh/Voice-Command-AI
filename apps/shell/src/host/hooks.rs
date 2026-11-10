//! Agent hooks for exact approval alerts (desktop only).
//!
//! Claude Code and Codex report approval prompts and turn ends to VCA
//! through hooks: Claude Code runs `<vca> hook claude <event>` from its
//! user `settings.json`, and Codex runs `<vca> hook codex` as its `notify`
//! program (chaining the user's own `notify`). The commands here install,
//! repair and remove those hooks in the user scope so that they run the
//! `vca` CLI bundled next to this app (`vca.exe` / `vca`, a Tauri sidecar
//! like vcad; on macOS inside `Contents/MacOS`). The merge, the backups and
//! the Codex chain are `vca_adapters`' (the same code as `vca hooks
//! install`), so user hooks are kept and every change writes a backup first.
//!
//! These are Tauri commands of the desktop app, so only the person at this
//! PC can run them: the webview of a phone or a paired device has no such
//! command (the mobile app answers `forbidden`, see `lib.rs`), and vcad's
//! remote API has no request that touches agent settings. Nothing is
//! installed until the user asks.
//!
//! Commands: `vca_host_hooks_status`, `vca_host_hooks_install` and
//! `vca_host_hooks_uninstall`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vca_adapters::claude::settings as claude_settings;
use vca_adapters::codex::notify as codex_notify;
use vca_adapters::{HookOptions, HookScope, HookState, HookStatus};

use crate::{CommandError, CommandResult};

/// One install or uninstall at a time (a double click must not interleave
/// two read-modify-write cycles of the same file).
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// An agent whose hooks the app manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAgent {
    Claude,
    Codex,
}

impl HookAgent {
    pub const ALL: [HookAgent; 2] = [HookAgent::Claude, HookAgent::Codex];
}

/// What the app shows for one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHookState {
    /// Every hook is present and runs the bundled `vca`.
    Installed,
    /// No VCA hook.
    Missing,
    /// VCA hooks run the bundled `vca` but some are missing or changed.
    Outdated,
    /// VCA hooks run another `vca` (for example a manual or older install).
    OtherPath,
    /// The agent's settings could not be read (see `error`).
    Error,
}

/// `vca_host_hooks_*`: one agent's hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentHooks {
    pub agent: HookAgent,
    pub state: AgentHookState,
    /// The settings file the hooks live in.
    pub config_path: String,
    /// Programs the VCA hooks found there run.
    pub programs: Vec<String>,
    /// Backup written by this install or uninstall, if it changed the file.
    pub backup_path: Option<String>,
    pub error: Option<String>,
}

/// `vca_host_hooks_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentHooksStatus {
    /// The bundled `vca` the hooks run; `None` when this build has none
    /// (installing is then impossible).
    pub cli_path: Option<String>,
    pub agents: Vec<AgentHooks>,
}

/// The `vca` CLI bundled next to the app's executable, when present.
pub fn bundled_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe
        .parent()?
        .join(format!("vca{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

/// Where the hooks go and which program they run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookTargets {
    /// Claude Code's config directory (`CLAUDE_CONFIG_DIR` or `~/.claude`).
    pub claude_dir: PathBuf,
    /// `CODEX_HOME` or `~/.codex`.
    pub codex_home: PathBuf,
    /// The bundled `vca`.
    pub cli: Option<PathBuf>,
}

impl HookTargets {
    /// The user's real agent settings and the bundled CLI.
    pub fn detect() -> Self {
        Self {
            claude_dir: vca_adapters::claude::default_config_dir(),
            codex_home: vca_adapters::codex::default_home(),
            cli: bundled_cli(),
        }
    }

    fn config_path(&self, agent: HookAgent) -> PathBuf {
        match agent {
            HookAgent::Claude => claude_settings::settings_path(&self.claude_dir, &HookScope::User),
            HookAgent::Codex => self.codex_home.join("config.toml"),
        }
    }

    fn cli_text(&self) -> Option<String> {
        self.cli.as_ref().map(|p| p.to_string_lossy().into_owned())
    }

    fn options(&self) -> HookOptions {
        match self.cli_text() {
            Some(cli) => HookOptions { command: vec![cli] },
            None => HookOptions::default(),
        }
    }

    /// Status of every agent.
    pub fn status(&self) -> AgentHooksStatus {
        AgentHooksStatus {
            cli_path: self.cli_text(),
            agents: HookAgent::ALL
                .iter()
                .map(|&agent| self.agent_status(agent))
                .collect(),
        }
    }

    pub fn agent_status(&self, agent: HookAgent) -> AgentHooks {
        let path = self.config_path(agent);
        let opts = self.options();
        let result = match agent {
            HookAgent::Claude => {
                claude_settings::status(&path, &vca_adapters::claude::default_hooks(), &opts)
            }
            HookAgent::Codex => codex_notify::status(&path, &opts),
        };
        self.report(agent, &path, result)
    }

    /// Installs (or repairs) the agent's user-scope hooks so they run the
    /// bundled `vca`.
    pub fn install(&self, agent: HookAgent) -> CommandResult<AgentHooks> {
        if self.cli.is_none() {
            return Err(CommandError::new(
                "not_bundled",
                "this build has no bundled vca CLI",
            ));
        }
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.config_path(agent);
        let opts = self.options();
        let result = match agent {
            HookAgent::Claude => {
                claude_settings::install(&path, &vca_adapters::claude::default_hooks(), &opts)
            }
            HookAgent::Codex => codex_notify::install(&path, &opts),
        };
        let result = result.map_err(|e| CommandError::new("hooks", e.to_string()))?;
        Ok(self.report(agent, &path, Ok(result)))
    }

    /// Removes every VCA hook of the agent (whichever `vca` it runs),
    /// keeping the user's own hooks and restoring a chained Codex `notify`.
    pub fn uninstall(&self, agent: HookAgent) -> CommandResult<AgentHooks> {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.config_path(agent);
        let opts = self.options();
        let result = match agent {
            HookAgent::Claude => {
                claude_settings::uninstall(&path, &vca_adapters::claude::default_hooks(), &opts)
            }
            HookAgent::Codex => codex_notify::uninstall(&path, &opts),
        };
        let result = result.map_err(|e| CommandError::new("hooks", e.to_string()))?;
        Ok(self.report(agent, &path, Ok(result)))
    }

    fn report(
        &self,
        agent: HookAgent,
        path: &Path,
        result: vca_adapters::Result<HookStatus>,
    ) -> AgentHooks {
        match result {
            Ok(status) => AgentHooks {
                agent,
                state: classify(&status, self.cli_text().as_deref()),
                config_path: status.config_path.to_string_lossy().into_owned(),
                programs: status.programs.clone(),
                backup_path: status
                    .backup_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                error: None,
            },
            Err(e) => AgentHooks {
                agent,
                state: AgentHookState::Error,
                config_path: path.to_string_lossy().into_owned(),
                programs: Vec::new(),
                backup_path: None,
                error: Some(e.to_string()),
            },
        }
    }
}

/// Compares program paths the way the file system does (case-insensitive,
/// either slash, on Windows).
fn same_program(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        let norm = |s: &str| s.replace('/', "\\").to_lowercase();
        norm(a) == norm(b)
    } else {
        a == b
    }
}

/// Folds the per-event states into one state for the UI. `cli` is the
/// bundled program (`None`: not bundled, then `vca` on `PATH`).
pub fn classify(status: &HookStatus, cli: Option<&str>) -> AgentHookState {
    let expected = cli.unwrap_or("vca");
    if status.all_installed() && status.programs.iter().all(|p| same_program(p, expected)) {
        return AgentHookState::Installed;
    }
    if status.programs.iter().any(|p| !same_program(p, expected)) {
        return AgentHookState::OtherPath;
    }
    if status.programs.is_empty() && status.entries.iter().all(|e| e.state == HookState::Missing) {
        return AgentHookState::Missing;
    }
    AgentHookState::Outdated
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> CommandResult<T> + Send + 'static,
) -> CommandResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("hooks", e.to_string()))?
}

#[tauri::command]
pub async fn vca_host_hooks_status() -> CommandResult<AgentHooksStatus> {
    blocking(|| Ok(HookTargets::detect().status())).await
}

#[tauri::command]
pub async fn vca_host_hooks_install(agent: HookAgent) -> CommandResult<AgentHooks> {
    blocking(move || HookTargets::detect().install(agent)).await
}

#[tauri::command]
pub async fn vca_host_hooks_uninstall(agent: HookAgent) -> CommandResult<AgentHooks> {
    blocking(move || HookTargets::detect().uninstall(agent)).await
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    const CLAUDE_USER: &str = r#"{
  "model": "opus",
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "~/.claude/my-stop-hook.sh" } ] }
    ]
  }
}"#;

    const CODEX_USER: &str =
        "model = \"gpt-5-codex\"\nnotify = [\"python3\", \"/home/me/notify.py\"]\n";

    /// Temporary agent config dirs; never the user's real ones.
    fn targets(dir: &Path) -> HookTargets {
        HookTargets {
            claude_dir: dir.join("claude"),
            codex_home: dir.join("codex"),
            cli: Some(dir.join("Voice Command AI").join("vca")),
        }
    }

    fn agent(status: &AgentHooksStatus, which: HookAgent) -> &AgentHooks {
        status.agents.iter().find(|a| a.agent == which).unwrap()
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn missing_by_default_and_nothing_written() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        let st = t.status();
        assert_eq!(st.agents.len(), 2);
        for a in &st.agents {
            assert_eq!(a.state, AgentHookState::Missing, "{a:?}");
            assert!(a.programs.is_empty());
        }
        assert!(!dir.path().join("claude").exists());
        assert!(!dir.path().join("codex").exists());
    }

    #[test]
    fn install_keeps_user_hooks_chains_notify_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        let cli = t.cli_text().unwrap();
        std::fs::create_dir_all(&t.claude_dir).unwrap();
        std::fs::create_dir_all(&t.codex_home).unwrap();
        let claude_path = t.config_path(HookAgent::Claude);
        let codex_path = t.config_path(HookAgent::Codex);
        std::fs::write(&claude_path, CLAUDE_USER).unwrap();
        std::fs::write(&codex_path, CODEX_USER).unwrap();

        let claude = t.install(HookAgent::Claude).unwrap();
        assert_eq!(claude.state, AgentHookState::Installed);
        assert_eq!(claude.programs, std::slice::from_ref(&cli));
        let backup = claude.backup_path.expect("backup");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), CLAUDE_USER);
        let v = read_json(&claude_path);
        assert_eq!(v["model"], "opus");
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop[0]["hooks"][0]["command"], "~/.claude/my-stop-hook.sh");
        assert_eq!(
            stop[1]["hooks"][0]["command"],
            format!("'{cli}' hook claude stop")
        );

        let codex = t.install(HookAgent::Codex).unwrap();
        assert_eq!(codex.state, AgentHookState::Installed);
        assert_eq!(
            std::fs::read_to_string(codex.backup_path.unwrap()).unwrap(),
            CODEX_USER
        );
        let text = std::fs::read_to_string(&codex_path).unwrap();
        assert!(text.starts_with("model = \"gpt-5-codex\"\n"), "{text}");
        assert!(
            text.contains(r#", "hook", "codex", "--", "python3", "/home/me/notify.py"]"#),
            "{text}"
        );
        assert!(text.contains(&cli), "{text}");

        // Installing again changes nothing.
        let again = t.install(HookAgent::Claude).unwrap();
        assert_eq!(again.state, AgentHookState::Installed);
        assert!(again.backup_path.is_none());
        let st = t.status();
        assert!(
            st.agents
                .iter()
                .all(|a| a.state == AgentHookState::Installed)
        );
    }

    #[test]
    fn other_path_is_detected_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        // A stopgap install that runs another vca.
        let old = HookTargets {
            cli: Some(dir.path().join("old").join("vca")),
            ..t.clone()
        };
        std::fs::create_dir_all(&t.codex_home).unwrap();
        std::fs::write(t.config_path(HookAgent::Codex), CODEX_USER).unwrap();
        old.install(HookAgent::Claude).unwrap();
        old.install(HookAgent::Codex).unwrap();

        let st = t.status();
        for which in HookAgent::ALL {
            let a = agent(&st, which);
            assert_eq!(a.state, AgentHookState::OtherPath, "{a:?}");
            assert_eq!(a.programs, [old.cli_text().unwrap()]);
        }

        // Repair = install with the bundled path.
        for which in HookAgent::ALL {
            let a = t.install(which).unwrap();
            assert_eq!(a.state, AgentHookState::Installed, "{a:?}");
            assert_eq!(a.programs, [t.cli_text().unwrap()]);
            assert!(a.backup_path.is_some());
        }
        let text = std::fs::read_to_string(t.config_path(HookAgent::Codex)).unwrap();
        assert!(text.contains("/home/me/notify.py"), "{text}");
        assert!(!text.contains(&old.cli_text().unwrap()), "{text}");
    }

    #[test]
    fn a_missing_hook_is_outdated() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        t.install(HookAgent::Claude).unwrap();
        let path = t.config_path(HookAgent::Claude);
        let mut v = read_json(&path);
        v["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("PermissionRequest");
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        assert_eq!(
            t.agent_status(HookAgent::Claude).state,
            AgentHookState::Outdated
        );
        assert_eq!(
            t.install(HookAgent::Claude).unwrap().state,
            AgentHookState::Installed
        );
    }

    #[test]
    fn uninstall_restores_user_settings() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        std::fs::create_dir_all(&t.claude_dir).unwrap();
        std::fs::create_dir_all(&t.codex_home).unwrap();
        std::fs::write(t.config_path(HookAgent::Claude), CLAUDE_USER).unwrap();
        std::fs::write(t.config_path(HookAgent::Codex), CODEX_USER).unwrap();
        for which in HookAgent::ALL {
            t.install(which).unwrap();
        }
        for which in HookAgent::ALL {
            let a = t.uninstall(which).unwrap();
            assert_eq!(a.state, AgentHookState::Missing, "{a:?}");
            assert!(a.backup_path.is_some());
        }
        let original: Value = serde_json::from_str(CLAUDE_USER).unwrap();
        assert_eq!(read_json(&t.config_path(HookAgent::Claude)), original);
        let codex = std::fs::read_to_string(t.config_path(HookAgent::Codex)).unwrap();
        assert!(
            codex.contains("notify = [\"python3\", \"/home/me/notify.py\"]"),
            "{codex}"
        );
        // Nothing left: no change, no backup.
        assert!(t.uninstall(HookAgent::Codex).unwrap().backup_path.is_none());
    }

    #[test]
    fn install_needs_the_bundled_cli() {
        let dir = tempfile::tempdir().unwrap();
        let t = HookTargets {
            cli: None,
            ..targets(dir.path())
        };
        let err = t.install(HookAgent::Claude).unwrap_err();
        assert_eq!(err.code, "not_bundled");
        assert!(!dir.path().join("claude").exists());
        assert_eq!(t.status().cli_path, None);
    }

    #[test]
    fn unreadable_settings_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        std::fs::create_dir_all(&t.claude_dir).unwrap();
        std::fs::write(t.config_path(HookAgent::Claude), "{ not json").unwrap();
        let a = t.agent_status(HookAgent::Claude);
        assert_eq!(a.state, AgentHookState::Error);
        assert!(a.error.is_some());
        assert!(t.install(HookAgent::Claude).is_err());
        assert_eq!(
            std::fs::read_to_string(t.config_path(HookAgent::Claude)).unwrap(),
            "{ not json"
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
}
