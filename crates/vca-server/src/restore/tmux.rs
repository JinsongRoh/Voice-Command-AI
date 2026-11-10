//! The headless backend (a headless GPU server, SSH-only machines, no display): each
//! restored window is one detached tmux session running the agent.
//!
//! A session is named after the list entry ([`entry_session_name`]), so a
//! rename does not lose it; the title the person sees is kept in the
//! `@vca_title` user option and listed in place of the name.
//!
//! There is no window to place, so `place` answers `unsupported` (the run
//! reports "opened, not placed") and `monitors` is empty. A session lives as
//! long as its agent: `tmux has-session` is what "already open" means.
//!
//! tmux is a command language of its own, and these are the traps a title or
//! a folder could otherwise fall into (each one checked on tmux 3.4):
//!
//! * `-c`, `-s` and `-n` are format-expanded, and `#(...)` in a format runs a
//!   shell command. The folder is written with `##`; the name is sanitized;
//!   `-n` is not used. The title is kept in a user option, which is stored
//!   literally and is not expanded when read back.
//! * An argument ending in `;` ends the tmux command, so that `;` is escaped
//!   ([`tmux_arg`]). Two commands are never chained on one line.
//! * A bare `-t name` also matches by prefix, so targets are always `=name`.
//! * Without `-u`, a client outside a UTF-8 locale (a daemon) reads Korean
//!   titles back as `_`.
//!
//! The helpers shared by the Unix backends (finding programs, running a
//! command with a time limit, the last check of a request) are here too,
//! because tmux is the one backend every Unix build has.

#![cfg_attr(any(test, target_os = "macos"), allow(dead_code))]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vca_adapters::generic::AgentSpec;
use vca_core::error::ErrorCode;
use vca_core::{AgentKind, Error, RestoreMonitor, WindowRect};

use super::backend::{LiveWindow, OpenRequest, WindowBackend};
use super::launch::stripped_env;
use super::resume::filter_args;

/// Every helper command gets this long; none of them should take a second.
pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Variables a restored window on macOS, Linux or tmux unsets by name even
/// when vcad itself does not have them: the identity of a Claude Code
/// session and VCA's own per-session variables. A tmux server that was
/// already running keeps the environment it was started with, and a
/// terminal application (Terminal.app, a terminal server) has its own.
/// Prefixed session variables (`CLAUDE_CODE_SESSION_*`) are only removed
/// by name when vcad has them.
pub(super) fn always_unset() -> Vec<String> {
    let mut names: Vec<String> = vca_core::child_env::SESSION_VARS
        .iter()
        .chain(&[crate::control::SESSION_ENV, crate::control::CONTROL_ENV])
        .map(|n| (*n).to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `/usr/bin/env -u NAME ...`: runs what follows without the `unset`
/// variables (nothing when `unset` is empty).
pub(super) fn env_unset_prefix(unset: &[String]) -> Vec<String> {
    if unset.is_empty() {
        return Vec::new();
    }
    let mut out = vec!["/usr/bin/env".to_owned()];
    for name in unset {
        out.push("-u".to_owned());
        out.push(name.clone());
    }
    out
}

/// Holds the title of a session. The value starts with [`TITLE_PREFIX`].
const TITLE_OPTION: &str = "@vca_title";
/// tmux does not allow `:` in a session name, so a listed name starting with
/// this cannot be mistaken for one of our titles. It also keeps the value
/// true for `#{?...}`, which a title of `0` would not be.
const TITLE_PREFIX: &str = "t:";
const LIST_FORMAT: &str = "#{session_id} #{?@vca_title,#{@vca_title},#{session_name}}";
const PANES_FORMAT: &str = "#{pane_pid} #{session_id}";
/// Longest readable part of a session name.
const MAX_SLUG: usize = 32;

pub(super) fn unavailable(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::Unavailable, message)
}

/// The last check before anything starts, whatever the caller did: known
/// agents only, never a permission-bypass flag, nothing a title or a path
/// should not hold. The planner already guarantees the first two.
pub(super) fn check_request(request: &OpenRequest) -> Result<(), Error> {
    let known = matches!(request.program.as_str(), "claude" | "codex")
        || AgentSpec::builtin()
            .iter()
            .any(|s| s.program == request.program);
    if !known {
        return Err(Error::api(
            ErrorCode::Forbidden,
            format!("{} is not a known agent", request.program),
        ));
    }
    if filter_args(&AgentKind::ClaudeCode, &request.args).1 {
        return Err(Error::api(
            ErrorCode::Forbidden,
            "a permission-bypass flag is never restored",
        ));
    }
    let folder = request.folder.to_string_lossy();
    let bad_text = request.title.chars().any(char::is_control)
        || folder.chars().any(char::is_control)
        || request.args.iter().any(|a| a.contains('\0'));
    if bad_text {
        return Err(Error::api(
            ErrorCode::InvalidRequest,
            "control characters in the title, folder or arguments",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Looks `name` up in the absolute directories of `path`, then in `extra`.
/// The working directory never counts: a project must not shadow the agent.
pub(super) fn find_in(name: &str, path: Option<&OsStr>, extra: &[PathBuf]) -> Option<PathBuf> {
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|d| d.is_absolute())
        .chain(extra.iter().cloned())
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

pub(super) fn find_program(name: &str) -> Option<PathBuf> {
    find_in(name, std::env::var_os("PATH").as_deref(), &[])
}

/// The agent's executable: `PATH`, then the per-user install locations a
/// server started by a service manager may not have on its `PATH`.
pub(super) fn find_agent(program: &str) -> Option<PathBuf> {
    let mut extra: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for dir in [".local/bin", ".npm-global/bin", ".bun/bin", ".cargo/bin"] {
            extra.push(home.join(dir));
        }
    }
    extra.push(PathBuf::from("/usr/local/bin"));
    extra.push(PathBuf::from("/opt/homebrew/bin"));
    find_in(program, std::env::var_os("PATH").as_deref(), &extra)
}

/// Takes vcad's own session markers out of a child's environment.
pub(super) fn strip_env(command: &mut Command) {
    for name in stripped_env(std::env::vars_os().map(|(k, _)| k)) {
        command.env_remove(name);
    }
}

/// The variables to unset inside a session, from the names in `env`.
pub(super) fn stripped_names(env: impl Iterator<Item = String>) -> Vec<String> {
    let env: Vec<String> = env.collect();
    let mut names: Vec<String> = stripped_env(&env)
        .into_iter()
        .map(|n| n.to_string_lossy().into_owned())
        .chain(always_unset())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// What a finished helper command said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Finished {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Runs a short helper to its end. One that hangs (a permission dialog
/// nobody answers) is given up on: only this child, which we started, is
/// ended.
pub(super) fn run(command: &mut Command) -> Result<Finished, Error> {
    let name = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| unavailable(format!("cannot start {name}: {e}")))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!("{name} did not answer in time")));
            }
            Err(e) => return Err(unavailable(format!("{name} failed: {e}"))),
        }
    };
    Ok(Finished {
        ok: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Starts a program that keeps running and collects it when it exits, so it
/// does not stay behind as a zombie.
pub(super) fn spawn_and_forget(command: &mut Command) -> Result<(), Error> {
    let name = command.get_program().to_string_lossy().into_owned();
    let mut child: Child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| unavailable(format!("cannot start {name}: {e}")))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// FNV-1a: unlike the std hasher, stable from one build to the next.
pub(super) fn stable_hash(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5, |hash: u32, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// A tmux session name for `key`: only ASCII letters, digits, `-` and `_`,
/// so it means nothing to tmux or a shell. The hash keeps names apart that
/// differ only in what was dropped (Korean titles lose all their letters).
pub fn session_name(key: &str) -> String {
    let mut slug = String::new();
    for c in key.chars() {
        if slug.len() >= MAX_SLUG {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '_' {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let hash = stable_hash(key);
    if slug.is_empty() {
        format!("vca-{hash:08x}")
    } else {
        format!("vca-{slug}-{hash:08x}")
    }
}

/// The tmux session of a list entry. The entry id is stable across
/// renames; a request without one (none is made so) falls back to the title.
pub fn entry_session_name(request: &OpenRequest) -> String {
    if request.entry_id.is_empty() {
        session_name(&request.title)
    } else {
        session_name(&request.entry_id)
    }
}

/// Why a tmux restore never places anything.
pub(super) const TMUX_NOTE: &str =
    "opened in tmux, not placed: a tmux session has no window to move";

/// An argument for tmux: a final `;` would end the command.
pub fn tmux_arg(value: &str) -> String {
    match value.strip_suffix(';') {
        Some(rest) => format!("{rest}\\;"),
        None => value.to_owned(),
    }
}

/// Text that tmux expands as a format, to be taken literally. `#` is the
/// only character that starts anything in a format.
pub fn format_literal(value: &str) -> String {
    value.replace('#', "##")
}

/// Starts the agent. It is run from an argument vector, without a shell, and
/// `env` keeps that vector longer than one word: tmux hands a single word to
/// a shell.
pub fn new_session_args(
    name: &str,
    folder: &Path,
    program: &Path,
    args: &[String],
    unset: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = ["-u", "new-session", "-d", "-s", name, "-c"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    out.push(format_literal(&folder.to_string_lossy()));
    out.push("--".into());
    out.push("/usr/bin/env".into());
    for variable in unset {
        out.push("-u".into());
        out.push(variable.clone());
    }
    out.push(tmux_arg(&program.to_string_lossy()));
    out.extend(args.iter().map(|a| tmux_arg(a)));
    out
}

/// Stores the title. The trailing `:` makes the target a session.
pub fn set_title_args(name: &str, title: &str) -> Vec<String> {
    vec![
        "-u".into(),
        "set-option".into(),
        "-t".into(),
        format!("={name}:"),
        TITLE_OPTION.into(),
        tmux_arg(&format!("{TITLE_PREFIX}{title}")),
    ]
}

/// Exit status 0 when the session exists. `=` turns off prefix matching.
pub fn has_session_args(name: &str) -> Vec<String> {
    vec!["has-session".into(), "-t".into(), format!("={name}")]
}

pub fn list_sessions_args() -> Vec<String> {
    vec![
        "-u".into(),
        "list-sessions".into(),
        "-F".into(),
        LIST_FORMAT.into(),
    ]
}

pub fn list_panes_args() -> Vec<String> {
    vec![
        "-u".into(),
        "list-panes".into(),
        "-a".into(),
        "-F".into(),
        PANES_FORMAT.into(),
    ]
}

fn session_number(id: &str) -> Option<u64> {
    id.strip_prefix('$')?.parse().ok()
}

/// Reads [`LIST_FORMAT`]. Sessions have no rectangle: they are marked
/// minimized so the snapshot keeps no measurement of them.
pub fn parse_sessions(stdout: &str) -> Vec<LiveWindow> {
    stdout
        .lines()
        .filter_map(|line| {
            let (id, title) = line.split_once(' ')?;
            Some(LiveWindow {
                id: session_number(id)?,
                title: title.strip_prefix(TITLE_PREFIX).unwrap_or(title).to_owned(),
                rect: WindowRect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                },
                maximized: false,
                minimized: true,
            })
        })
        .collect()
}

/// Reads [`PANES_FORMAT`]: the first process of each pane to its session.
pub fn parse_panes(stdout: &str) -> HashMap<u32, u64> {
    stdout
        .lines()
        .filter_map(|line| {
            let (pid, id) = line.split_once(' ')?;
            Some((pid.parse().ok()?, session_number(id)?))
        })
        .collect()
}

/// tmux lists nothing because no server runs: no sessions, not an error.
pub fn no_server(stderr: &str) -> bool {
    stderr.contains("no server running")
        || stderr.contains("error connecting to")
        || stderr.contains("no sessions")
}

/// tmux sessions in place of windows.
#[derive(Default)]
pub struct TmuxBackend;

impl TmuxBackend {
    fn tmux(&self) -> Option<PathBuf> {
        find_program("tmux")
    }

    fn query(&self, args: &[String]) -> Result<String, Error> {
        let Some(tmux) = self.tmux() else {
            return Ok(String::new());
        };
        let done = run(Command::new(tmux).args(args))?;
        if done.ok {
            Ok(done.stdout)
        } else if no_server(&done.stderr) {
            Ok(String::new())
        } else {
            Err(unavailable(format!("tmux: {}", done.stderr.trim())))
        }
    }
}

impl WindowBackend for TmuxBackend {
    fn platform(&self) -> &'static str {
        std::env::consts::OS
    }

    fn unsupported(&self) -> Option<String> {
        self.tmux().is_none().then(|| {
            "this computer has no display, so sessions restore into tmux, and tmux is not \
             installed"
                .to_owned()
        })
    }

    fn monitors(&self) -> Result<Vec<RestoreMonitor>, Error> {
        Ok(Vec::new())
    }

    fn windows(&self) -> Result<Vec<LiveWindow>, Error> {
        Ok(parse_sessions(&self.query(&list_sessions_args())?))
    }

    fn console_owners(&self) -> Result<HashMap<u32, u64>, Error> {
        Ok(parse_panes(&self.query(&list_panes_args())?))
    }

    fn open(&self, request: &OpenRequest) -> Result<(), Error> {
        check_request(request)?;
        let tmux = self
            .tmux()
            .ok_or_else(|| Error::api(ErrorCode::Unsupported, "tmux is not installed"))?;
        let name = entry_session_name(request);
        if run(Command::new(&tmux).args(has_session_args(&name)))?.ok {
            return Ok(());
        }
        let program = find_agent(&request.program).ok_or_else(|| {
            unavailable(format!(
                "{} is not installed on this computer",
                request.program
            ))
        })?;
        let names = std::env::vars_os().map(|(k, _)| k.to_string_lossy().into_owned());
        let args = new_session_args(
            &name,
            &request.folder,
            &program,
            &request.args,
            &stripped_names(names),
        );
        let mut start = Command::new(&tmux);
        start.args(args).current_dir(&request.folder);
        strip_env(&mut start);
        let done = run(&mut start)?;
        if !done.ok {
            return Err(unavailable(format!(
                "tmux could not start the session: {}",
                done.stderr.trim()
            )));
        }
        let done = run(Command::new(&tmux).args(set_title_args(&name, &request.title)))?;
        if done.ok {
            Ok(())
        } else {
            // The session is gone already: the agent ended at once.
            Err(unavailable(format!(
                "{} exited right after it started: {}",
                request.program,
                done.stderr.trim()
            )))
        }
    }

    /// A session has no place on a screen.
    fn place(&self, _: u64, _: WindowRect, _: bool) -> Result<(), Error> {
        Err(Error::api(ErrorCode::Unsupported, TMUX_NOTE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(title: &str, program: &str, args: &[&str]) -> OpenRequest {
        OpenRequest {
            entry_id: "e1".into(),
            title: title.into(),
            folder: PathBuf::from("/home/me/app"),
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            managed: None,
        }
    }

    #[test]
    fn session_names_hold_only_harmless_characters() {
        for title in [
            "Payments API",
            "결제 서버",
            "a:b.c",
            "$(rm -rf ~); `x` 'q' \"d\"",
            "#(touch /tmp/x)",
            "-t =evil",
            "한글 and English",
            "",
        ] {
            let name = session_name(title);
            assert!(name.starts_with("vca-"), "{name}");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{name}"
            );
        }
        assert_eq!(session_name("Payments API"), session_name("Payments API"));
        assert!(session_name("Payments API").starts_with("vca-Payments-API-"));
        assert_ne!(session_name("결제"), session_name("정산"));
        assert_ne!(session_name("a b"), session_name("a-b"));
        assert!(session_name(&"x".repeat(500)).len() < 60);
    }

    #[test]
    fn a_session_is_named_after_its_entry_and_keeps_its_title_apart() {
        let mut first = request("Payments API", "claude", &[]);
        first.entry_id = "e0123abcd".into();
        let mut renamed = first.clone();
        renamed.title = "결제 서버 $(x)".into();
        // A rename keeps the session: the name comes from the entry.
        assert_eq!(entry_session_name(&first), entry_session_name(&renamed));
        assert_eq!(entry_session_name(&first), session_name("e0123abcd"));
        assert!(entry_session_name(&first).starts_with("vca-e0123abcd-"));
        // Two entries with one title are two sessions.
        let mut other = first.clone();
        other.entry_id = "e9999".into();
        assert_ne!(entry_session_name(&first), entry_session_name(&other));
        // The title still goes into the user option, literally.
        let name = entry_session_name(&renamed);
        assert_eq!(
            set_title_args(&name, &renamed.title).last().unwrap(),
            "t:결제 서버 $(x)"
        );
        let mut bare = first.clone();
        bare.entry_id.clear();
        assert_eq!(entry_session_name(&bare), session_name("Payments API"));
    }

    #[test]
    fn tmux_opens_but_never_places() {
        let rect = WindowRect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let err = TmuxBackend.place(1, rect, false).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unsupported);
        assert!(err.to_string().contains("not placed"), "{err}");
    }

    #[test]
    fn the_hash_of_a_name_never_changes() {
        // Sessions of an earlier version must still be found.
        assert_eq!(stable_hash("a"), 0xe40c_292c);
        assert_eq!(stable_hash(""), 0x811c_9dc5);
    }

    #[test]
    fn a_final_semicolon_cannot_end_the_tmux_command() {
        assert_eq!(tmux_arg("go on;"), "go on\\;");
        assert_eq!(tmux_arg("a;b"), "a;b");
        assert_eq!(tmux_arg("plain"), "plain");
    }

    #[test]
    fn a_folder_cannot_run_a_format_command() {
        assert_eq!(format_literal("/a/#(touch x)/b"), "/a/##(touch x)/b");
        assert_eq!(format_literal("/plain"), "/plain");
    }

    #[test]
    fn a_session_runs_the_agent_from_an_argument_vector() {
        let args = new_session_args(
            "vca-api-1",
            Path::new("/work/결제 #(x)/it's"),
            Path::new("/home/me/.local/bin/claude"),
            &["--resume".to_owned(), "ab-1".to_owned()],
            &["CLAUDECODE".to_owned()],
        );
        assert_eq!(
            args,
            [
                "-u",
                "new-session",
                "-d",
                "-s",
                "vca-api-1",
                "-c",
                "/work/결제 ##(x)/it's",
                "--",
                "/usr/bin/env",
                "-u",
                "CLAUDECODE",
                "/home/me/.local/bin/claude",
                "--resume",
                "ab-1",
            ]
        );
    }

    #[test]
    fn a_prompt_stays_one_argument() {
        let args = new_session_args(
            "n",
            Path::new("/w"),
            Path::new("/bin/claude"),
            &["$(rm -rf ~); next;".to_owned()],
            &[],
        );
        assert_eq!(args.last().unwrap(), "$(rm -rf ~); next\\;");
    }

    #[test]
    fn the_title_is_stored_as_a_literal_user_option() {
        let args = set_title_args("n", "결제 \"q\" 's' #(x);");
        assert_eq!(
            args,
            [
                "-u",
                "set-option",
                "-t",
                "=n:",
                "@vca_title",
                "t:결제 \"q\" 's' #(x)\\;",
            ]
        );
    }

    #[test]
    fn already_open_is_an_exact_has_session() {
        assert_eq!(has_session_args("a"), ["has-session", "-t", "=a"]);
    }

    #[test]
    fn parses_sessions_with_and_without_a_title() {
        // The second line is exactly what tmux 3.4 printed for this title.
        let out = "$0 work\n$12 t:한글 \"q\" 's' a,b} #(touch x) y;\n$3 my session\nnonsense\n";
        let windows = parse_sessions(out);
        let got: Vec<(u64, &str)> = windows.iter().map(|w| (w.id, w.title.as_str())).collect();
        assert_eq!(
            got,
            [
                (0, "work"),
                (12, "한글 \"q\" 's' a,b} #(touch x) y;"),
                (3, "my session"),
            ]
        );
        assert!(windows.iter().all(|w| w.minimized && !w.maximized));
    }

    #[test]
    fn a_title_of_zero_survives_the_conditional_format() {
        assert_eq!(parse_sessions("$1 t:0\n")[0].title, "0");
    }

    #[test]
    fn panes_map_the_first_process_to_its_session() {
        let owners = parse_panes("2726856 $0\n99 $4\nbad line\n");
        assert_eq!(owners, HashMap::from([(2_726_856, 0), (99, 4)]));
    }

    #[test]
    fn no_server_is_an_empty_list_and_not_a_failure() {
        assert!(no_server(
            "error connecting to /tmp/tmux-1000/default (No such file or directory)"
        ));
        assert!(no_server("no server running on /tmp/tmux-1000/default"));
        assert!(!no_server("unknown option -- x"));
    }

    #[test]
    fn only_known_agents_and_no_bypass_flags_get_through() {
        assert!(check_request(&request("t", "claude", &["--resume", "a"])).is_ok());
        assert!(check_request(&request("t", "codex", &["resume", "--last"])).is_ok());
        for program in ["evil", "/bin/sh", "claude; rm -rf ~", "../claude"] {
            let err = check_request(&request("t", program, &[])).unwrap_err();
            assert_eq!(err.code(), ErrorCode::Forbidden, "{program}");
        }
        for flag in ["--dangerously-skip-permissions", "--yolo", "--full-auto"] {
            let err = check_request(&request("t", "claude", &[flag])).unwrap_err();
            assert_eq!(err.code(), ErrorCode::Forbidden, "{flag}");
        }
        let err = check_request(&request(
            "t",
            "claude",
            &["--permission-mode", "bypassPermissions"],
        ))
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Forbidden);
    }

    #[test]
    fn control_characters_stop_at_the_door() {
        let err = check_request(&request("a\nb", "claude", &[])).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        let mut req = request("t", "claude", &[]);
        req.folder = PathBuf::from("/a\u{1b}]0;x");
        assert!(check_request(&req).is_err());
        assert!(check_request(&request("t", "claude", &["a\0b"])).is_err());
        assert!(check_request(&request("결제 \"q\" 's' $(x);", "claude", &[])).is_ok());
    }

    #[test]
    fn the_working_directory_cannot_shadow_a_program() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let program = bin.join("agent");
        std::fs::write(&program, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([PathBuf::from("."), bin.clone()]).unwrap();
        assert_eq!(
            find_in("agent", Some(path.as_os_str()), &[]),
            Some(program.clone())
        );
        let relative = std::env::join_paths([PathBuf::from(".")]).unwrap();
        assert_eq!(find_in("agent", Some(relative.as_os_str()), &[]), None);
        assert_eq!(find_in("agent", None, &[bin]), Some(program));
    }

    #[test]
    fn markers_of_the_parent_session_are_unset() {
        let names = stripped_names(
            [
                "PATH",
                "CLAUDE_CODE_SSE_PORT",
                "VCA_CONTROL",
                "CLAUDE_CONFIG_DIR",
            ]
            .iter()
            .map(|s| (*s).to_owned()),
        );
        assert_eq!(
            names,
            [
                "CLAUDECODE",
                "CLAUDE_CODE_BRIDGE_SESSION_ID",
                "CLAUDE_CODE_CHILD_SESSION",
                "CLAUDE_CODE_ENTRYPOINT",
                "CLAUDE_CODE_SSE_PORT",
                "CLAUDE_PID",
                "VCA_CONDUCTOR_TOKEN",
                "VCA_CONTROL",
                "VCA_HOOK_TOKEN",
                "VCA_SESSION_ID",
            ]
        );
        // Prefixed identity variables go by name; configuration stays; and
        // NO_COLOR goes when it came from a Claude Code session.
        let names = stripped_names(
            [
                "CLAUDE_CODE_SESSION_ID",
                "CLAUDE_CODE_MESSAGING_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK",
                "NO_COLOR",
            ]
            .iter()
            .map(|s| (*s).to_owned()),
        );
        assert!(names.contains(&"CLAUDE_CODE_SESSION_ID".to_owned()));
        assert!(names.contains(&"CLAUDE_CODE_MESSAGING_TOKEN".to_owned()));
        assert!(names.contains(&"NO_COLOR".to_owned()));
        assert!(!names.contains(&"CLAUDE_CODE_USE_BEDROCK".to_owned()));
    }

    #[test]
    fn a_missing_helper_is_an_error_and_not_a_panic() {
        let err = run(&mut Command::new("vca-no-such-program")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
    }
}
