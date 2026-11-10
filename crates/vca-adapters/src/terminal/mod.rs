//! Delivery tier 3: typing into terminals through their scripting APIs,
//! without focusing the window (architecture §4).
//!
//! | Terminal        | Detection                                  | Injection                                   | Status |
//! |-----------------|--------------------------------------------|---------------------------------------------|--------|
//! | tmux            | `TMUX`/`TMUX_PANE`, `tmux list-panes -a`   | `tmux send-keys -t <pane> -l` + `Enter`      | tested |
//! | WezTerm         | `WEZTERM_PANE`, `wezterm cli list` (tty)   | `wezterm cli send-text --no-paste`           | unverified |
//! | kitty           | `KITTY_WINDOW_ID`, `kitty @ ls` (pids)     | `kitty @ send-text --match id:<id>`          | unverified |
//! | iTerm2 (macOS)  | `ITERM_SESSION_ID`                         | AppleScript `write text`                     | unverified |
//! | Windows console | process id                                 | `vca console-send` + `WriteConsoleInputW`    | unverified |
//!
//! Detection prefers the target process's own environment and falls back
//! to listing panes and matching the process or one of its ancestors.

pub mod console;
mod others;
pub mod tmux;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vca_core::ControlKey;

use crate::error::{AdapterError, Result};
use crate::process::{ProcessTable, process_environ};

pub use others::{
    KittyWindow, WezTermPane, iterm2_script, kitty_escape, parse_kitty_ls, parse_wezterm_list,
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
        /// The process's creation time (a `FILETIME`), checked before
        /// typing so that a reused PID never receives the text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created: Option<u64>,
    },
}

/// One external command to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Written to the command's standard input (which is empty otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
}

impl CommandSpec {
    pub(crate) fn new(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.to_owned(),
            args,
            stdin: None,
        }
    }

    /// Runs the command (without a console window on Windows) and returns
    /// its stdout.
    pub async fn run(&self) -> Result<String> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;

        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&self.args)
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        if let (Some(text), Some(mut stdin)) = (&self.stdin, child.stdin.take()) {
            stdin.write_all(text.as_bytes()).await?;
            // Dropping it closes the pipe: the helper reads to the end.
        }
        let out = child.wait_with_output().await?;
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
        TerminalTarget::WindowsConsole { pid, created } => {
            vec![console::console_send_command(*pid, *created, text, submit)]
        }
    }
}

/// The lock that lets one `vca console-send` helper at a time type into
/// the console of `pid`: two helpers writing at once interleave their text,
/// and their bursts join into reads that are too long.
pub fn console_lock(pid: u32) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<u32, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Forget consoles nobody is typing into.
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    Arc::clone(locks.entry(pid).or_default())
}

/// Holds the console lock of a Windows console target.
async fn serialize(target: &TerminalTarget) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    match target {
        TerminalTarget::WindowsConsole { pid, .. } => Some(console_lock(*pid).lock_owned().await),
        _ => None,
    }
}

/// May vcad start the console helper for `target`? A Windows console needs
/// the process's creation time (a bare PID may have been reused by another
/// program) and the helper installed next to vcad (never one on `PATH`).
fn check_console_target(target: &TerminalTarget) -> Result<()> {
    let TerminalTarget::WindowsConsole { pid, created } = target else {
        return Ok(());
    };
    if cfg!(windows) && created.is_none() {
        return Err(AdapterError::Unsupported(format!(
            "the start time of process {pid} is unknown, so it cannot be told from a \
             process that reused its id; nothing was typed"
        )));
    }
    console::console_helper()?;
    Ok(())
}

/// Types `text` into `target`. Returns what the helper reported (the Windows
/// console helper says how much it typed), trimmed; empty for others.
pub async fn inject(target: &TerminalTarget, text: &str, submit: bool) -> Result<String> {
    check_console_target(target)?;
    let _turn = serialize(target).await;
    let mut report = String::new();
    for cmd in injection_commands(target, text, submit) {
        let out = cmd.run().await?;
        if matches!(target, TerminalTarget::WindowsConsole { .. }) {
            report = out.trim().to_owned();
        }
    }
    Ok(report)
}

/// One step of a key sequence typed into a terminal (answering a form).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KeyStep {
    Key {
        key: ControlKey,
    },
    /// Literal text (a single space toggles a check box).
    Text {
        text: String,
    },
    Pause {
        ms: u64,
    },
}

/// One action of [`key_actions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    Run(CommandSpec),
    Sleep(Duration),
}

/// tmux key names of the control keys.
fn tmux_key(key: ControlKey) -> &'static str {
    match key {
        ControlKey::Enter => "Enter",
        ControlKey::Escape => "Escape",
        ControlKey::Interrupt => "C-c",
        ControlKey::Tab => "Tab",
        ControlKey::Up => "Up",
        ControlKey::Down => "Down",
        ControlKey::Left => "Left",
        ControlKey::Right => "Right",
        ControlKey::Backspace => "BSpace",
    }
}

/// The bytes a terminal sends for a control key (normal cursor mode).
fn key_bytes(key: ControlKey) -> &'static str {
    match key {
        ControlKey::Enter => "\r",
        ControlKey::Escape => "\x1b",
        ControlKey::Interrupt => "\x03",
        ControlKey::Tab => "\t",
        ControlKey::Up => "\x1b[A",
        ControlKey::Down => "\x1b[B",
        ControlKey::Right => "\x1b[C",
        ControlKey::Left => "\x1b[D",
        ControlKey::Backspace => "\x7f",
    }
}

/// The commands (and pauses) that type `steps` into `target`. A Windows
/// console takes the whole sequence in one helper run, which pauses
/// itself. kitty and iTerm2 are not supported (their key encodings are
/// unverified).
pub fn key_actions(target: &TerminalTarget, steps: &[KeyStep]) -> Result<Vec<KeyAction>> {
    let sleep = |ms: u64| KeyAction::Sleep(Duration::from_millis(ms));
    match target {
        TerminalTarget::WindowsConsole { pid, created } => Ok(vec![KeyAction::Run(
            console::console_keys_command(*pid, *created, steps),
        )]),
        TerminalTarget::Tmux { pane, socket } => Ok(steps
            .iter()
            .flat_map(|step| match step {
                KeyStep::Key { key } => {
                    tmux::send_key_command(socket.as_deref(), pane, tmux_key(*key))
                        .into_iter()
                        .map(KeyAction::Run)
                        .collect()
                }
                KeyStep::Text { text } => {
                    tmux::send_text_commands(socket.as_deref(), pane, text, false)
                        .into_iter()
                        .map(KeyAction::Run)
                        .collect()
                }
                KeyStep::Pause { ms } => vec![sleep(*ms)],
            })
            .collect()),
        TerminalTarget::WezTerm { pane_id } => Ok(steps
            .iter()
            .map(|step| match step {
                KeyStep::Key { key } => KeyAction::Run(wezterm_send(*pane_id, key_bytes(*key))),
                KeyStep::Text { text } => KeyAction::Run(wezterm_send(*pane_id, text)),
                KeyStep::Pause { ms } => sleep(*ms),
            })
            .collect()),
        TerminalTarget::Kitty { .. } | TerminalTarget::ITerm2 { .. } => Err(
            AdapterError::Unsupported("keys cannot be sent to this terminal".into()),
        ),
    }
}

fn wezterm_send(pane_id: u64, payload: &str) -> CommandSpec {
    CommandSpec::new(
        "wezterm",
        vec![
            "cli".into(),
            "send-text".into(),
            "--pane-id".into(),
            pane_id.to_string(),
            "--no-paste".into(),
            "--".into(),
            payload.to_owned(),
        ],
    )
}

/// Types `steps` into `target`.
pub async fn send_keys(target: &TerminalTarget, steps: &[KeyStep]) -> Result<()> {
    check_console_target(target)?;
    let _turn = serialize(target).await;
    for action in key_actions(target, steps)? {
        match action {
            KeyAction::Run(cmd) => {
                cmd.run().await?;
            }
            KeyAction::Sleep(d) => tokio::time::sleep(d).await,
        }
    }
    Ok(())
}

/// The visible screen of `target` as text, where the terminal offers it:
/// tmux, and a Windows console (read by the `vca console-read` helper).
/// `None` elsewhere, and when reading fails.
pub async fn screen_text(target: &TerminalTarget) -> Option<String> {
    match target {
        TerminalTarget::Tmux { pane, socket } => {
            tmux::capture_pane(socket.as_deref(), pane).await.ok()
        }
        TerminalTarget::WindowsConsole { pid, created } => {
            check_console_target(target).ok()?;
            match console::console_read_command(*pid, *created).run().await {
                Ok(screen) => Some(screen),
                Err(e) => {
                    tracing::debug!(pid, error = %e, "cannot read the console screen");
                    None
                }
            }
        }
        _ => None,
    }
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
/// On Windows it is the console of the process itself, identified by PID
/// and creation time; no environment block is read and no terminal tool
/// is started. Elsewhere it tries, in order: the environment of the
/// process and of its ancestors that the scanner already reads (shells,
/// terminals, agents and interpreters), tmux panes (default server), kitty
/// windows and WezTerm panes (matched by tty).
pub async fn detect(pid: u32, procs: &ProcessTable) -> Option<TerminalTarget> {
    if cfg!(windows) {
        return windows_console(pid, procs);
    }
    let chain: Vec<u32> = std::iter::once(pid).chain(procs.ancestors(pid)).collect();
    // The agent's own environment, then its parents' (a wrapper may have
    // been started before the terminal variables were set). Only candidate
    // processes, whose command line the scanner read: never the memory of
    // an unrelated process.
    for p in chain
        .iter()
        .filter(|p| procs.get(**p).is_some_and(|i| !i.cmd.is_empty()))
    {
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
    None
}

/// The console target of `pid` on Windows, with the process's creation
/// time. `None` when the process is gone, or when the PID already belongs
/// to a process other than the one in `procs` (its start time differs).
fn windows_console(pid: u32, procs: &ProcessTable) -> Option<TerminalTarget> {
    let created = vca_core::proclist::creation_time(pid)?;
    if let Some(info) = procs.get(pid)
        && info.start_time != 0
        && info.start_time != vca_core::proclist::filetime_to_unix_secs(created)
    {
        return None;
    }
    Some(TerminalTarget::WindowsConsole {
        pid,
        created: Some(created),
    })
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

        let win = injection_commands(
            &TerminalTarget::WindowsConsole {
                pid: 77,
                created: None,
            },
            "dir",
            true,
        );
        assert_eq!(win.len(), 1);
        let program = std::path::Path::new(&win[0].program);
        assert!(
            program
                .file_stem()
                .is_some_and(|s| s.eq_ignore_ascii_case("vca")),
            "{program:?}"
        );
        assert_eq!(win[0].args, ["console-send", "--pid", "77", "--submit"]);
        // The text travels on stdin, never on a command line.
        assert_eq!(win[0].stdin.as_deref(), Some("dir"));
        assert!(!win[0].args.iter().any(|a| a.contains("dir")));
    }

    fn steps() -> Vec<KeyStep> {
        vec![
            KeyStep::Key {
                key: ControlKey::Down,
            },
            KeyStep::Pause { ms: 5 },
            KeyStep::Text { text: " ".into() },
            KeyStep::Key {
                key: ControlKey::Enter,
            },
        ]
    }

    #[test]
    fn key_sequences_per_terminal() {
        let tmux = key_actions(
            &TerminalTarget::Tmux {
                pane: "%1".into(),
                socket: None,
            },
            &steps(),
        )
        .unwrap();
        let args: Vec<Vec<String>> = tmux
            .iter()
            .filter_map(|a| match a {
                KeyAction::Run(c) => Some(c.args.clone()),
                KeyAction::Sleep(_) => None,
            })
            .collect();
        assert_eq!(
            args,
            [
                vec!["send-keys", "-t", "%1", "Down"],
                vec!["send-keys", "-t", "%1", "-l", "--", " "],
                vec!["send-keys", "-t", "%1", "Enter"],
            ]
        );
        assert_eq!(tmux[1], KeyAction::Sleep(Duration::from_millis(5)));

        let wez = key_actions(&TerminalTarget::WezTerm { pane_id: 3 }, &steps()).unwrap();
        let KeyAction::Run(down) = &wez[0] else {
            panic!()
        };
        assert_eq!(down.args.last().map(String::as_str), Some("\x1b[B"));

        // One helper run with the steps on stdin.
        let win = key_actions(
            &TerminalTarget::WindowsConsole {
                pid: 9,
                created: Some(5),
            },
            &steps(),
        )
        .unwrap();
        let [KeyAction::Run(cmd)] = &win[..] else {
            panic!("{win:?}")
        };
        assert_eq!(
            cmd.args,
            ["console-send", "--pid", "9", "--created", "5", "--keys"]
        );
        let sent: Vec<KeyStep> = serde_json::from_str(cmd.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(sent, steps());

        for target in [
            TerminalTarget::Kitty {
                window_id: 1,
                listen_on: None,
            },
            TerminalTarget::ITerm2 {
                session_id: "x".into(),
            },
        ] {
            assert!(key_actions(&target, &steps()).is_err());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn console_helpers_take_turns() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let active = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for i in 0..8 {
            let (active, most, order) = (active.clone(), most.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let _turn = serialize(&TerminalTarget::WindowsConsole {
                    pid: 4242,
                    created: None,
                })
                .await;
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                order.lock().unwrap().push(i);
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        // Another console is not held up meanwhile.
        let other = console_lock(4243);
        assert!(other.try_lock().is_ok());
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(most.load(Ordering::SeqCst), 1, "two helpers typed at once");
        assert_eq!(order.lock().unwrap().len(), 8);
        // Other terminals need no turn.
        assert!(
            serialize(&TerminalTarget::WezTerm { pane_id: 1 })
                .await
                .is_none()
        );
        // The same console shares one lock while it is in use.
        let a = console_lock(4244);
        let b = console_lock(4244);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn a_console_without_a_start_time_is_never_typed_into() {
        let target = TerminalTarget::WindowsConsole {
            pid: 4,
            created: None,
        };
        // Refused before any helper runs (on Windows for the missing start
        // time; everywhere for the helper missing next to the test binary).
        assert!(inject(&target, "dir", true).await.is_err());
        assert!(
            send_keys(&target, &[KeyStep::Text { text: "x".into() }])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_program_is_an_error() {
        let cmd = CommandSpec::new("vca-definitely-missing-program", vec![]);
        assert!(cmd.run().await.is_err());
    }
}
