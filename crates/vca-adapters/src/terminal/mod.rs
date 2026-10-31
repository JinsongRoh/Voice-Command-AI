//! Delivery tier 3: typing into terminals through their scripting APIs,
//! without focusing the window (architecture §4).
//!
//! | Terminal        | Detection                                  | Injection                                   | Status |
//! |-----------------|--------------------------------------------|---------------------------------------------|--------|
//! | tmux            | `TMUX`/`TMUX_PANE`, `tmux list-panes -a`   | `tmux send-keys -t <pane> -l` + `Enter`      | tested |
//! | WezTerm         | `WEZTERM_PANE`, `wezterm cli list` (tty)   | `wezterm cli send-text --no-paste`           | unverified |
//! | kitty           | `KITTY_WINDOW_ID`, `kitty @ ls` (pids)     | `kitty @ send-text --match id:<id>`          | unverified |
//! | iTerm2 (macOS)  | `ITERM_SESSION_ID`                         | AppleScript `write text`                     | unverified |
//! | Windows console | process id                                 | PowerShell + `WriteConsoleInputW`            | unverified |
//!
//! Detection prefers the target process's own environment and falls back
//! to listing panes and matching the process or one of its ancestors.

mod others;
pub mod tmux;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AdapterError, Result};
use crate::process::{ProcessTable, process_environ};

pub use others::{
    KittyWindow, WezTermPane, iterm2_script, kitty_escape, parse_kitty_ls, parse_wezterm_list,
    windows_console_script,
};

/// A terminal location that can receive text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalTarget {
    Tmux {
        /// Pane id such as `%3`.
        pane: String,
        /// Server socket (from `TMUX`); `None` uses the default server.
        socket: Option<PathBuf>,
    },
    WezTerm {
        pane_id: u64,
    },
    Kitty {
        window_id: u64,
        /// Remote control address (`KITTY_LISTEN_ON`), when known.
        listen_on: Option<String>,
    },
    /// iTerm2 session unique id (the part of `ITERM_SESSION_ID` after `:`).
    ITerm2 {
        session_id: String,
    },
    /// A Windows console process (conhost or a ConPTY-hosted console).
    WindowsConsole {
        pid: u32,
    },
}

/// One external command to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    fn new(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.to_owned(),
            args,
        }
    }

    /// Runs the command and returns its stdout.
    pub async fn run(&self) -> Result<String> {
        let out = tokio::process::Command::new(&self.program)
            .args(&self.args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await?;
        if !out.status.success() {
            return Err(AdapterError::CommandFailed {
                program: self.program.clone(),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Commands that type `text` into `target`, followed by Enter when
/// `submit` is set. Text is sent literally (no key-name interpretation and
/// no bracketed paste).
pub fn injection_commands(target: &TerminalTarget, text: &str, submit: bool) -> Vec<CommandSpec> {
    match target {
        TerminalTarget::Tmux { pane, socket } => {
            tmux::send_text_commands(socket.as_deref(), pane, text, submit)
        }
        TerminalTarget::WezTerm { pane_id } => {
            let mut payload = text.to_owned();
            if submit {
                payload.push('\r');
            }
            vec![CommandSpec::new(
                "wezterm",
                vec![
                    "cli".into(),
                    "send-text".into(),
                    "--pane-id".into(),
                    pane_id.to_string(),
                    "--no-paste".into(),
                    "--".into(),
                    payload,
                ],
            )]
        }
        TerminalTarget::Kitty {
            window_id,
            listen_on,
        } => {
            let mut args = vec!["@".to_owned()];
            if let Some(addr) = listen_on {
                args.extend(["--to".into(), addr.clone()]);
            }
            let mut payload = kitty_escape(text);
            if submit {
                payload.push_str("\\r");
            }
            args.extend([
                "send-text".into(),
                "--match".into(),
                format!("id:{window_id}"),
                "--".into(),
                payload,
            ]);
            vec![CommandSpec::new("kitty", args)]
        }
        TerminalTarget::ITerm2 { session_id } => vec![CommandSpec::new(
            "osascript",
            vec!["-e".into(), iterm2_script(session_id, text, submit)],
        )],
        TerminalTarget::WindowsConsole { pid } => vec![CommandSpec::new(
            "powershell.exe",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                windows_console_script(*pid, text, submit),
            ],
        )],
    }
}

/// Types `text` into `target`.
pub async fn inject(target: &TerminalTarget, text: &str, submit: bool) -> Result<()> {
    for cmd in injection_commands(target, text, submit) {
        cmd.run().await?;
    }
    Ok(())
}

/// Detects a terminal target from a process environment.
pub fn detect_from_env(env: &[(String, String)]) -> Option<TerminalTarget> {
    let get = |k: &str| {
        env.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    if let (Some(tmux), Some(pane)) = (get("TMUX"), get("TMUX_PANE")) {
        let socket = tmux
            .split(',')
            .next()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        return Some(TerminalTarget::Tmux {
            pane: pane.to_owned(),
            socket,
        });
    }
    if let Some(id) = get("WEZTERM_PANE").and_then(|v| v.parse().ok()) {
        return Some(TerminalTarget::WezTerm { pane_id: id });
    }
    if let Some(id) = get("KITTY_WINDOW_ID").and_then(|v| v.parse().ok()) {
        return Some(TerminalTarget::Kitty {
            window_id: id,
            listen_on: get("KITTY_LISTEN_ON").map(str::to_owned),
        });
    }
    if let Some(id) = get("ITERM_SESSION_ID") {
        let uuid = id.split_once(':').map_or(id, |(_, u)| u);
        return Some(TerminalTarget::ITerm2 {
            session_id: uuid.to_owned(),
        });
    }
    None
}

/// Finds the terminal that hosts process `pid`.
///
/// Tries, in order: the process environment, tmux panes (default server),
/// kitty windows and WezTerm panes (matched by tty), and on Windows the
/// console of the process itself.
pub async fn detect(pid: u32, procs: &ProcessTable) -> Option<TerminalTarget> {
    let chain: Vec<u32> = std::iter::once(pid).chain(procs.ancestors(pid)).collect();
    // The agent's own environment, then its parents' (a wrapper may have
    // been started before the terminal variables were set).
    for p in &chain {
        let env = tokio::task::spawn_blocking({
            let p = *p;
            move || process_environ(p)
        })
        .await
        .ok()
        .flatten();
        if let Some(target) = env.as_deref().and_then(detect_from_env) {
            return Some(target);
        }
    }
    if let Ok(panes) = tmux::list_panes(None).await
        && let Some(pane) = panes.iter().find(|p| chain.contains(&p.pane_pid))
    {
        return Some(TerminalTarget::Tmux {
            pane: pane.pane_id.clone(),
            socket: None,
        });
    }
    let kitty = CommandSpec::new("kitty", vec!["@".into(), "ls".into()]);
    if let Ok(json) = kitty.run().await
        && let Some(w) = parse_kitty_ls(&json)
            .into_iter()
            .find(|w| w.pids().any(|p| chain.contains(&p)))
    {
        return Some(TerminalTarget::Kitty {
            window_id: w.id,
            listen_on: None,
        });
    }
    if let Some(tty) = process_tty(pid) {
        let wez = CommandSpec::new(
            "wezterm",
            vec![
                "cli".into(),
                "list".into(),
                "--format".into(),
                "json".into(),
            ],
        );
        if let Ok(json) = wez.run().await
            && let Some(p) = parse_wezterm_list(&json)
                .into_iter()
                .find(|p| p.tty_name.as_deref() == Some(tty.as_str()))
        {
            return Some(TerminalTarget::WezTerm { pane_id: p.pane_id });
        }
    }
    if cfg!(windows) {
        return Some(TerminalTarget::WindowsConsole { pid });
    }
    None
}

/// Controlling terminal device of a process (Linux only).
pub fn process_tty(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let link = std::fs::read_link(format!("/proc/{pid}/fd/0")).ok()?;
        let s = link.to_string_lossy().into_owned();
        s.starts_with("/dev/pts/").then_some(s)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn detects_from_environment() {
        assert_eq!(
            detect_from_env(&env(&[
                ("TMUX", "/tmp/tmux-1000/default,1234,0"),
                ("TMUX_PANE", "%7")
            ])),
            Some(TerminalTarget::Tmux {
                pane: "%7".into(),
                socket: Some("/tmp/tmux-1000/default".into())
            })
        );
        assert_eq!(
            detect_from_env(&env(&[("WEZTERM_PANE", "12")])),
            Some(TerminalTarget::WezTerm { pane_id: 12 })
        );
        assert_eq!(
            detect_from_env(&env(&[
                ("KITTY_WINDOW_ID", "3"),
                ("KITTY_LISTEN_ON", "unix:/tmp/kitty")
            ])),
            Some(TerminalTarget::Kitty {
                window_id: 3,
                listen_on: Some("unix:/tmp/kitty".into())
            })
        );
        assert_eq!(
            detect_from_env(&env(&[(
                "ITERM_SESSION_ID",
                "w0t1p0:6A3C1A3E-1F0B-4B8C-9C55-1D0C3E7A9B21"
            )])),
            Some(TerminalTarget::ITerm2 {
                session_id: "6A3C1A3E-1F0B-4B8C-9C55-1D0C3E7A9B21".into()
            })
        );
        assert_eq!(detect_from_env(&env(&[("TERM", "xterm")])), None);
    }

    #[test]
    fn wezterm_and_kitty_commands() {
        let wez = injection_commands(&TerminalTarget::WezTerm { pane_id: 5 }, "ls -la", true);
        assert_eq!(wez.len(), 1);
        assert_eq!(wez[0].program, "wezterm");
        assert_eq!(
            wez[0].args,
            [
                "cli",
                "send-text",
                "--pane-id",
                "5",
                "--no-paste",
                "--",
                "ls -la\r"
            ]
        );

        let kitty = injection_commands(
            &TerminalTarget::Kitty {
                window_id: 9,
                listen_on: Some("unix:/tmp/k".into()),
            },
            r"echo C:\temp",
            true,
        );
        assert_eq!(
            kitty[0].args,
            [
                "@",
                "--to",
                "unix:/tmp/k",
                "send-text",
                "--match",
                "id:9",
                "--",
                r"echo C:\\temp\r"
            ]
        );
    }

    #[test]
    fn iterm_and_windows_commands() {
        let it = injection_commands(
            &TerminalTarget::ITerm2 {
                session_id: "ABC".into(),
            },
            "hi",
            false,
        );
        assert_eq!(it[0].program, "osascript");
        assert!(it[0].args[1].contains(r#"write text "hi" newline NO"#));

        let win = injection_commands(&TerminalTarget::WindowsConsole { pid: 77 }, "dir", true);
        assert_eq!(win[0].program, "powershell.exe");
        assert!(win[0].args[3].contains("[VcaConsoleInput]::Send(77,"));
    }

    #[tokio::test]
    async fn missing_program_is_an_error() {
        let cmd = CommandSpec::new("vca-definitely-missing-program", vec![]);
        assert!(cmd.run().await.is_err());
    }
}
