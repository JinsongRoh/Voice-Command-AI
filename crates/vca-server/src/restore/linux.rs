//! The Linux desktop backend: gnome-terminal, konsole or kitty.
//!
//! On X11 windows are read and moved with wmctrl, or xdotool when wmctrl is
//! missing. Wayland gives an application no way to see or move other
//! windows, so there (and on X11 with neither tool) restore opens windows and
//! says plainly that it did not place them. A machine with no display, or
//! with a display but no supported terminal, restores into tmux instead.
//!
//! Without a way to list windows, the runner could not tell that a window it
//! opened exists. `windows` then answers with what this backend opened in the
//! last minutes, marked minimized so the snapshot measures nothing from them.
//! Those entries are also what stops an immediate retry from opening the
//! window twice.
//!
//! Only `open` and `place` change anything, `place` only moves a terminal
//! window, and nothing here takes the focus, closes a window or ends a
//! process.

#![cfg_attr(test, allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vca_core::error::ErrorCode;
use vca_core::{Error, RestoreMonitor, WindowRect};

use super::backend::{LiveWindow, OpenRequest, WindowBackend};
use super::tmux::{
    TmuxBackend, always_unset, check_request, env_unset_prefix, find_agent, find_program, run,
    spawn_and_forget, stable_hash, strip_env, unavailable,
};
use crate::util::lock;

/// How long a window this backend cannot see still counts as open. Longer
/// than the runner's wait for a window.
const REMEMBER_OPENED: Duration = Duration::from_secs(120);

/// Keeps the window when the agent exits, like the other platforms: the
/// agent gets its arguments as `"$@"`, then the user's shell takes over.
const HOLD_OPEN: &str = r#""$@"; exec "${SHELL:-/bin/sh}""#;

/// Keeps a fixed title in terminals with no such option. Only Claude Code
/// knows this variable; the others ignore it.
const FIXED_TITLE_ENV: &str = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE";

const WAYLAND_NOTE: &str = "opened, but not placed: Wayland does not let an application move \
                            windows. Move it by hand.";
const NO_TOOL_NOTE: &str =
    "opened, but not placed: install wmctrl or xdotool to restore window positions";

/// What kind of screen this process can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Headless,
    X11,
    Wayland,
}

/// Reads the display from the environment. A display forwarded over SSH is
/// not this computer's screen: windows must not open on the visitor's.
pub fn detect_display(env: &dyn Fn(&str) -> Option<String>) -> Display {
    let set = |name: &str| env(name).filter(|v| !v.is_empty());
    if set("WAYLAND_DISPLAY").is_some()
        || set("XDG_SESSION_TYPE").is_some_and(|t| t.eq_ignore_ascii_case("wayland"))
    {
        return Display::Wayland;
    }
    match set("DISPLAY") {
        Some(d) if d.starts_with("localhost:") && set("SSH_CONNECTION").is_some() => {
            Display::Headless
        }
        Some(_) => Display::X11,
        None => Display::Headless,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Gnome,
    Konsole,
    Kitty,
}

impl Terminal {
    const ALL: [Self; 3] = [Self::Gnome, Self::Konsole, Self::Kitty];

    fn program(self) -> &'static str {
        match self {
            Self::Gnome => "gnome-terminal",
            Self::Konsole => "konsole",
            Self::Kitty => "kitty",
        }
    }
}

/// The terminal of the desktop if it is installed, else the first one found.
pub fn pick_terminal(installed: &[Terminal], desktop: &str) -> Option<Terminal> {
    let desktop = desktop.to_ascii_lowercase();
    let native = if desktop.contains("kde") {
        Some(Terminal::Konsole)
    } else if desktop.contains("gnome") {
        Some(Terminal::Gnome)
    } else {
        None
    };
    native
        .filter(|t| installed.contains(t))
        .or_else(|| Terminal::ALL.into_iter().find(|t| installed.contains(t)))
}

/// The argument vector of the window: no shell ever parses `program` or
/// `args`, they arrive as `"$@"`. With `unset` (see
/// [`super::tmux::always_unset`]) it starts with `/usr/bin/env -u ...`, so
/// neither the agent nor the shell that stays after it has those
/// variables, whatever environment the terminal application hands over.
pub fn held_command(program: &Path, args: &[String], unset: &[String]) -> Vec<String> {
    let mut out = env_unset_prefix(unset);
    out.extend([
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        HOLD_OPEN.to_owned(),
        "vca-restore".to_owned(),
        program.to_string_lossy().into_owned(),
    ]);
    out.extend_from_slice(args);
    out
}

/// The title konsole will show for `title`. Its titles are format strings
/// with no escape for `%`, and its `-p` values cannot hold `"`.
pub fn konsole_title(title: &str) -> String {
    title.replace('%', "\u{ff05}").replace('"', "'")
}

/// The title `terminal` will show for `title`.
pub fn shown_title(terminal: Terminal, title: &str) -> String {
    match terminal {
        Terminal::Konsole => konsole_title(title),
        Terminal::Gnome | Terminal::Kitty => title.to_owned(),
    }
}

/// Arguments of the terminal. Values are glued to their option where the
/// terminal allows it, so one starting with `-` stays a value.
pub fn terminal_args(
    terminal: Terminal,
    title: &str,
    folder: &Path,
    command: &[String],
) -> Vec<String> {
    let folder = folder.to_string_lossy();
    let mut out: Vec<String> = match terminal {
        Terminal::Gnome => vec![
            "--window".into(),
            format!("--title={title}"),
            format!("--working-directory={folder}"),
            "--".into(),
        ],
        // A process of its own per window, so a pid names one window.
        // Quoted because `;` separates the properties of `-p`.
        Terminal::Konsole => vec![
            "--separate".into(),
            format!("--workdir={folder}"),
            "-p".into(),
            format!("tabtitle=\"{}\"", konsole_title(title)),
            "-e".into(),
        ],
        Terminal::Kitty => vec![format!("--title={title}"), format!("--directory={folder}")],
    };
    out.extend_from_slice(command);
    out
}

/// A window of an X server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11Window {
    pub id: u64,
    pub pid: u32,
    pub rect: WindowRect,
    pub title: String,
}

/// Splits off `count` words and returns them with the rest of the line, its
/// inner spaces intact.
fn words_then_rest(line: &str, count: usize) -> Option<(Vec<&str>, &str)> {
    let mut rest = line.trim_start();
    let mut words = Vec::with_capacity(count);
    for _ in 0..count {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        words.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    Some((words, rest))
}

fn is_terminal_class(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    Terminal::ALL.iter().any(|t| class.contains(t.program()))
}

/// Konsole appends its name to the title of the tab.
fn strip_app_name<'a>(class: &str, title: &'a str) -> &'a str {
    if !class.to_ascii_lowercase().contains("konsole") {
        return title;
    }
    title
        .strip_suffix(" — Konsole")
        .or_else(|| title.strip_suffix(" - Konsole"))
        .unwrap_or(title)
}

/// Reads `wmctrl -l -G -p -x`, keeping terminal windows only:
/// `id desktop pid x y w h class host title...`.
pub fn parse_wmctrl(stdout: &str) -> Vec<X11Window> {
    stdout
        .lines()
        .filter_map(|line| {
            let (w, title) = words_then_rest(line, 9)?;
            if !is_terminal_class(w[7]) {
                return None;
            }
            Some(X11Window {
                id: u64::from_str_radix(w[0].strip_prefix("0x")?, 16).ok()?,
                pid: w[2].parse().ok()?,
                rect: WindowRect {
                    x: w[3].parse().ok()?,
                    y: w[4].parse().ok()?,
                    w: w[5].parse().ok()?,
                    h: w[6].parse().ok()?,
                },
                title: strip_app_name(w[7], title).to_owned(),
            })
        })
        .collect()
}

/// Reads `xdotool getwindowgeometry --shell`.
pub fn parse_xdotool_geometry(stdout: &str) -> Option<WindowRect> {
    let value = |key: &str| {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .and_then(|v| v.trim().parse::<i32>().ok())
    };
    Some(WindowRect {
        x: value("X")?,
        y: value("Y")?,
        w: value("WIDTH")?,
        h: value("HEIGHT")?,
    })
}

/// Reads `xprop -id ID _NET_WM_STATE`: maximized, minimized.
pub fn parse_xprop_state(stdout: &str) -> (bool, bool) {
    (
        stdout.contains("_NET_WM_STATE_MAXIMIZED_HORZ")
            && stdout.contains("_NET_WM_STATE_MAXIMIZED_VERT"),
        stdout.contains("_NET_WM_STATE_HIDDEN"),
    )
}

/// Reads `xrandr --query`: `HDMI-1 connected primary 1920x1080+0+0 ...`.
/// X reports no scaling a window needs to follow, so the DPI stays at 96.
pub fn parse_xrandr(stdout: &str) -> Vec<RestoreMonitor> {
    let geometry = |word: &str| -> Option<WindowRect> {
        let (w, rest) = word.split_once('x')?;
        let mut parts = rest.split('+');
        Some(WindowRect {
            w: w.parse().ok()?,
            h: parts.next()?.parse().ok()?,
            x: parts.next()?.parse().ok()?,
            y: parts.next()?.parse().ok()?,
        })
    };
    stdout
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let id = words.next()?;
            if words.next()? != "connected" {
                return None;
            }
            let rect = words.find_map(geometry)?;
            Some(RestoreMonitor {
                id: id.to_owned(),
                rect,
                work: rect,
                dpi: 96,
            })
        })
        .collect()
}

/// A pid to its window. gnome-terminal serves all its windows from one
/// process, and such a pid names no window, so it is left out.
pub fn unique_owners(windows: &[X11Window]) -> HashMap<u32, u64> {
    let mut count: HashMap<u32, u32> = HashMap::new();
    for window in windows {
        *count.entry(window.pid).or_default() += 1;
    }
    windows
        .iter()
        .filter(|w| w.pid != 0 && count.get(&w.pid) == Some(&1))
        .map(|w| (w.pid, w.id))
        .collect()
}

const MAXIMIZE: &str = "maximized_vert,maximized_horz";

/// The wmctrl calls of a move. `-i -r` names the window by id; without `-a`
/// nothing is raised or focused.
pub fn wmctrl_place(id: u64, rect: WindowRect, maximized: bool) -> Vec<Vec<String>> {
    let target = |rest: &[String]| {
        let mut call = vec!["-i".to_owned(), "-r".to_owned(), format!("{id:#x}")];
        call.extend_from_slice(rest);
        call
    };
    let state = |action: &str| target(&["-b".to_owned(), format!("{action},{MAXIMIZE}")]);
    let mut calls = vec![
        state("remove"),
        target(&[
            "-e".to_owned(),
            format!("0,{},{},{},{}", rect.x, rect.y, rect.w, rect.h),
        ]),
    ];
    if maximized {
        calls.push(state("add"));
    }
    calls
}

/// The xdotool calls of a move.
pub fn xdotool_place(id: u64, rect: WindowRect, maximized: bool) -> Vec<Vec<String>> {
    let call = |parts: &[&str]| parts.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let id = id.to_string();
    let state = |action: &str, which: &str| call(&["windowstate", action, which, &id]);
    let mut calls = vec![
        state("--remove", "MAXIMIZED_VERT"),
        state("--remove", "MAXIMIZED_HORZ"),
        call(&["windowsize", &id, &rect.w.to_string(), &rect.h.to_string()]),
        call(&["windowmove", &id, &rect.x.to_string(), &rect.y.to_string()]),
    ];
    if maximized {
        calls.push(state("--add", "MAXIMIZED_VERT"));
        calls.push(state("--add", "MAXIMIZED_HORZ"));
    }
    calls
}

/// A window this backend opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub title: String,
    /// The title the terminal shows, when it could not show `title`.
    pub shown: String,
    pub at: Instant,
}

/// The windows that cannot be seen but were opened a moment ago.
pub fn remembered_windows(opened: &[Opened], now: Instant) -> Vec<LiveWindow> {
    opened
        .iter()
        .filter(|o| now.saturating_duration_since(o.at) < REMEMBER_OPENED)
        .map(|o| LiveWindow {
            // Not a window handle: `place` never gets as far as using it.
            id: u64::from(stable_hash(&o.title)) | (1 << 40),
            title: o.title.clone(),
            rect: WindowRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            maximized: false,
            minimized: true,
        })
        .collect()
}

/// Gives windows their requested title back where the terminal changed it.
pub fn restore_titles(windows: &mut [LiveWindow], opened: &[Opened]) {
    for window in windows {
        if let Some(o) = opened.iter().find(|o| o.shown == window.title) {
            window.title.clone_from(&o.title);
        }
    }
}

/// How windows are read and moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placer {
    Wmctrl(PathBuf),
    Xdotool(PathBuf),
    /// Cannot be done here; the reason, for the progress view.
    None(&'static str),
}

pub fn pick_placer(display: Display, wmctrl: Option<PathBuf>, xdotool: Option<PathBuf>) -> Placer {
    if display == Display::Wayland {
        return Placer::None(WAYLAND_NOTE);
    }
    match (wmctrl, xdotool) {
        (Some(path), _) => Placer::Wmctrl(path),
        (None, Some(path)) => Placer::Xdotool(path),
        (None, None) => Placer::None(NO_TOOL_NOTE),
    }
}

/// Runs a read-only tool for its output.
fn read(program: &Path, args: &[&str]) -> Result<String, Error> {
    let done = run(Command::new(program).args(args))?;
    if done.ok {
        Ok(done.stdout)
    } else {
        Err(unavailable(format!(
            "{}: {}",
            program.display(),
            done.stderr.trim()
        )))
    }
}

/// A desktop terminal, with or without placement.
pub struct LinuxBackend {
    terminal: Option<Terminal>,
    placer: Placer,
    opened: Mutex<Vec<Opened>>,
}

impl LinuxBackend {
    pub fn new(terminal: Option<Terminal>, placer: Placer) -> Self {
        Self {
            terminal,
            placer,
            opened: Mutex::new(Vec::new()),
        }
    }

    fn x11_windows(&self) -> Result<Vec<X11Window>, Error> {
        match &self.placer {
            Placer::Wmctrl(wmctrl) => Ok(parse_wmctrl(&read(
                wmctrl,
                &["-l", "-G", "-p", "-x", "-u"],
            )?)),
            Placer::Xdotool(xdotool) => xdotool_windows(xdotool),
            Placer::None(_) => Ok(Vec::new()),
        }
    }
}

/// xdotool finds windows by class but reports one fact per call.
fn xdotool_windows(xdotool: &Path) -> Result<Vec<X11Window>, Error> {
    let pattern = "gnome-terminal|konsole|kitty";
    // Exit status 1 means no match.
    let done = run(Command::new(xdotool).args(["search", "--onlyvisible", "--class", pattern]))?;
    let mut out = Vec::new();
    for id in done.stdout.split_whitespace() {
        let Ok(number) = id.parse::<u64>() else {
            continue;
        };
        let Ok(geometry) = read(xdotool, &["getwindowgeometry", "--shell", id]) else {
            continue;
        };
        let Some(rect) = parse_xdotool_geometry(&geometry) else {
            continue;
        };
        let title = read(xdotool, &["getwindowname", id]).unwrap_or_default();
        let pid = read(xdotool, &["getwindowpid", id]).unwrap_or_default();
        out.push(X11Window {
            id: number,
            pid: pid.trim().parse().unwrap_or(0),
            rect,
            title: strip_app_name("konsole", title.trim_end()).to_owned(),
        });
    }
    Ok(out)
}

impl WindowBackend for LinuxBackend {
    fn platform(&self) -> &'static str {
        std::env::consts::OS
    }

    fn unsupported(&self) -> Option<String> {
        self.terminal.is_none().then(|| {
            "no supported terminal found: install gnome-terminal, konsole, kitty or tmux".to_owned()
        })
    }

    fn monitors(&self) -> Result<Vec<RestoreMonitor>, Error> {
        if matches!(self.placer, Placer::None(_)) {
            return Ok(Vec::new());
        }
        match find_program("xrandr") {
            Some(xrandr) => Ok(parse_xrandr(&read(&xrandr, &["--query"])?)),
            None => Ok(Vec::new()),
        }
    }

    fn windows(&self) -> Result<Vec<LiveWindow>, Error> {
        let opened = lock(&self.opened).clone();
        if matches!(self.placer, Placer::None(_)) {
            return Ok(remembered_windows(&opened, Instant::now()));
        }
        let xprop = find_program("xprop");
        let mut out = Vec::new();
        for window in self.x11_windows()? {
            let state = xprop.as_ref().and_then(|xprop| {
                let id = window.id.to_string();
                read(xprop, &["-id", &id, "_NET_WM_STATE"]).ok()
            });
            let (maximized, minimized) = state.map_or((false, false), |s| parse_xprop_state(&s));
            out.push(LiveWindow {
                id: window.id,
                title: window.title,
                rect: window.rect,
                maximized,
                minimized,
            });
        }
        restore_titles(&mut out, &opened);
        Ok(out)
    }

    fn console_owners(&self) -> Result<HashMap<u32, u64>, Error> {
        Ok(unique_owners(&self.x11_windows()?))
    }

    fn open(&self, request: &OpenRequest) -> Result<(), Error> {
        check_request(request)?;
        let terminal = self.terminal.ok_or_else(|| {
            Error::api(ErrorCode::Unsupported, "no supported terminal is installed")
        })?;
        let binary = find_program(terminal.program())
            .ok_or_else(|| unavailable(format!("{} is gone", terminal.program())))?;
        let agent = find_agent(&request.program).ok_or_else(|| {
            unavailable(format!(
                "{} is not installed on this computer",
                request.program
            ))
        })?;
        // The terminal gets vcad's environment without the session
        // variables (`strip_env`); the window's command unsets the known
        // ones again, since a terminal server may give the window its own.
        let held = held_command(&agent, &request.args, &always_unset());
        let mut command = Command::new(binary);
        command
            .args(terminal_args(
                terminal,
                &request.title,
                &request.folder,
                &held,
            ))
            .current_dir(&request.folder);
        strip_env(&mut command);
        command.env(FIXED_TITLE_ENV, "1");
        spawn_and_forget(&mut command)?;
        lock(&self.opened).push(Opened {
            title: request.title.clone(),
            shown: shown_title(terminal, &request.title),
            at: Instant::now(),
        });
        Ok(())
    }

    fn place(&self, id: u64, rect: WindowRect, maximized: bool) -> Result<(), Error> {
        let (tool, calls) = match &self.placer {
            Placer::Wmctrl(tool) => (tool, wmctrl_place(id, rect, maximized)),
            Placer::Xdotool(tool) => (tool, xdotool_place(id, rect, maximized)),
            Placer::None(reason) => return Err(Error::api(ErrorCode::Unsupported, *reason)),
        };
        if !self.x11_windows()?.iter().any(|w| w.id == id) {
            return Err(unavailable("that is no longer a terminal window"));
        }
        for call in calls {
            let done = run(Command::new(tool).args(&call))?;
            if !done.ok {
                return Err(unavailable(format!(
                    "the window could not be moved: {}",
                    done.stderr.trim()
                )));
            }
        }
        Ok(())
    }
}

/// The backend of this computer.
pub fn backend() -> Arc<dyn WindowBackend> {
    let display = detect_display(&|name| std::env::var(name).ok());
    if display == Display::Headless {
        return Arc::new(TmuxBackend);
    }
    let installed: Vec<Terminal> = Terminal::ALL
        .into_iter()
        .filter(|t| find_program(t.program()).is_some())
        .collect();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let terminal = pick_terminal(&installed, &desktop);
    if terminal.is_none() && find_program("tmux").is_some() {
        return Arc::new(TmuxBackend);
    }
    let placer = pick_placer(display, find_program("wmctrl"), find_program("xdotool"));
    Arc::new(LinuxBackend::new(terminal, placer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> WindowRect {
        WindowRect { x, y, w, h }
    }

    fn nasty_titles() -> Vec<&'static str> {
        vec![
            "plain",
            "with spaces",
            "결제 서버",
            "it's \"quoted\"",
            "$(rm -rf ~); `x` | y & z > w",
            "--new-tab",
            "100% done",
        ]
    }

    #[test]
    fn the_display_decides_how_a_restore_works() {
        assert_eq!(detect_display(&env(&[])), Display::Headless);
        assert_eq!(detect_display(&env(&[("DISPLAY", "")])), Display::Headless);
        assert_eq!(detect_display(&env(&[("DISPLAY", ":0")])), Display::X11);
        assert_eq!(
            detect_display(&env(&[("WAYLAND_DISPLAY", "wayland-0")])),
            Display::Wayland
        );
        // XWayland: the session is still Wayland.
        assert_eq!(
            detect_display(&env(&[("DISPLAY", ":0"), ("WAYLAND_DISPLAY", "wayland-0")])),
            Display::Wayland
        );
        assert_eq!(
            detect_display(&env(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "wayland")])),
            Display::Wayland
        );
    }

    #[test]
    fn a_forwarded_display_is_not_this_computers_screen() {
        assert_eq!(
            detect_display(&env(&[
                ("DISPLAY", "localhost:10.0"),
                ("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")
            ])),
            Display::Headless
        );
        assert_eq!(
            detect_display(&env(&[("DISPLAY", ":0"), ("SSH_CONNECTION", "x")])),
            Display::X11
        );
    }

    #[test]
    fn the_desktops_own_terminal_comes_first() {
        let all = Terminal::ALL;
        assert_eq!(pick_terminal(&all, "KDE"), Some(Terminal::Konsole));
        assert_eq!(pick_terminal(&all, "ubuntu:GNOME"), Some(Terminal::Gnome));
        assert_eq!(pick_terminal(&all, "XFCE"), Some(Terminal::Gnome));
        assert_eq!(
            pick_terminal(&[Terminal::Kitty], "KDE"),
            Some(Terminal::Kitty)
        );
        assert_eq!(pick_terminal(&[], "GNOME"), None);
    }

    #[test]
    fn gnome_terminal_gets_a_new_window_and_an_argument_vector() {
        let held = held_command(
            Path::new("/usr/bin/claude"),
            &["--resume".to_owned(), "ab".to_owned()],
            &[],
        );
        let args = terminal_args(Terminal::Gnome, "API", Path::new("/w/app"), &held);
        assert_eq!(
            args,
            [
                "--window",
                "--title=API",
                "--working-directory=/w/app",
                "--",
                "/bin/sh",
                "-c",
                HOLD_OPEN,
                "vca-restore",
                "/usr/bin/claude",
                "--resume",
                "ab",
            ]
        );
    }

    #[test]
    fn konsole_and_kitty_commands() {
        let command = vec!["/bin/sh".to_owned()];
        assert_eq!(
            terminal_args(Terminal::Konsole, "API", Path::new("/w"), &command),
            [
                "--separate",
                "--workdir=/w",
                "-p",
                "tabtitle=\"API\"",
                "-e",
                "/bin/sh",
            ]
        );
        assert_eq!(
            terminal_args(Terminal::Kitty, "API", Path::new("/w"), &command),
            ["--title=API", "--directory=/w", "/bin/sh"]
        );
    }

    #[test]
    fn no_title_or_folder_can_become_an_option_or_a_command() {
        let held = held_command(Path::new("/usr/bin/claude"), &[], &always_unset());
        for terminal in Terminal::ALL {
            for title in nasty_titles() {
                let folder = format!("/w/{title}");
                let args = terminal_args(terminal, title, Path::new(&folder), &held);
                // The command is not touched, and every option is one word.
                assert!(args.ends_with(&held), "{args:?}");
                let options = &args[..args.len() - held.len()];
                let title_word = options.iter().find(|a| a.contains("title")).unwrap();
                assert!(title_word.starts_with("--title=") || title_word.starts_with("tabtitle="));
            }
        }
    }

    #[test]
    fn the_shell_of_the_window_never_sees_the_arguments_as_text() {
        let nasty = "$(rm -rf ~); `x`".to_owned();
        let held = held_command(
            Path::new("/opt/my claude/claude"),
            std::slice::from_ref(&nasty),
            &[],
        );
        assert_eq!(held[2], r#""$@"; exec "${SHELL:-/bin/sh}""#);
        assert_eq!(held[4], "/opt/my claude/claude");
        assert_eq!(held[5], nasty);
    }

    #[test]
    fn the_window_unsets_the_session_variables_for_agent_and_shell() {
        let unset = always_unset();
        let held = held_command(
            Path::new("/usr/bin/claude"),
            &["--resume".to_owned()],
            &unset,
        );
        assert_eq!(held[0], "/usr/bin/env");
        let sh = held.iter().position(|a| a == "/bin/sh").unwrap();
        assert_eq!(sh, 1 + 2 * unset.len());
        for pair in held[1..sh].chunks(2) {
            assert_eq!(pair[0], "-u");
            assert!(unset.contains(&pair[1]), "{pair:?}");
        }
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "VCA_SESSION_ID",
            "VCA_HOOK_TOKEN",
        ] {
            assert!(unset.contains(&name.to_owned()), "{name}");
        }
        assert_eq!(
            &held[sh..],
            [
                "/bin/sh",
                "-c",
                HOLD_OPEN,
                "vca-restore",
                "/usr/bin/claude",
                "--resume"
            ]
        );
    }

    #[test]
    fn konsole_titles_lose_only_what_it_cannot_show() {
        assert_eq!(konsole_title("100% \"done\""), "100\u{ff05} 'done'");
        assert_eq!(konsole_title("a;b 결제"), "a;b 결제");
        assert_eq!(shown_title(Terminal::Kitty, "100%"), "100%");
        assert_eq!(shown_title(Terminal::Konsole, "100%"), "100\u{ff05}");
    }

    #[test]
    fn wmctrl_lists_terminals_with_their_full_titles() {
        let out = "\
0x03a00003  0 4242   100  200  800  600  gnome-terminal-server.Gnome-terminal host  결제  서버 \"q\" $(x)
0x04000007 -1 777    0    0    1920 1080 Navigator.firefox    host  Mozilla Firefox
0x05000001  1 888    1920 0    1000 900  konsole.konsole      N/A   API — Konsole
0x06000001  1 999    10   20   300  200  kitty.kitty          host  it's
broken line
";
        let windows = parse_wmctrl(out);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].id, 0x03a0_0003);
        assert_eq!(windows[0].pid, 4242);
        assert_eq!(windows[0].rect, rect(100, 200, 800, 600));
        assert_eq!(windows[0].title, "결제  서버 \"q\" $(x)");
        assert_eq!(windows[1].title, "API");
        assert_eq!(windows[2].title, "it's");
    }

    #[test]
    fn xdotool_geometry_and_xprop_state() {
        let out = "WINDOW=1234\nX=-1920\nY=40\nWIDTH=800\nHEIGHT=600\nSCREEN=0\n";
        assert_eq!(parse_xdotool_geometry(out), Some(rect(-1920, 40, 800, 600)));
        assert_eq!(parse_xdotool_geometry("nothing"), None);
        assert_eq!(
            parse_xprop_state(
                "_NET_WM_STATE(ATOM) = _NET_WM_STATE_MAXIMIZED_HORZ, _NET_WM_STATE_MAXIMIZED_VERT"
            ),
            (true, false)
        );
        assert_eq!(
            parse_xprop_state("_NET_WM_STATE(ATOM) = _NET_WM_STATE_HIDDEN"),
            (false, true)
        );
        assert_eq!(
            parse_xprop_state("_NET_WM_STATE:  not found."),
            (false, false)
        );
    }

    #[test]
    fn xrandr_gives_each_connected_monitor() {
        let out = "\
Screen 0: minimum 320 x 200, current 3840 x 1080, maximum 16384 x 16384
HDMI-1 connected primary 1920x1080+0+0 (normal left inverted right x axis y axis) 527mm x 296mm
   1920x1080     60.00*+
DP-1 connected 1920x1080+1920+0 (normal left) 527mm x 296mm
DP-2 disconnected (normal left inverted right x axis y axis)
DP-3 connected (normal left inverted right x axis y axis)
";
        let monitors = parse_xrandr(out);
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].id, "HDMI-1");
        assert_eq!(monitors[0].rect, rect(0, 0, 1920, 1080));
        assert_eq!(monitors[1].id, "DP-1");
        assert_eq!(monitors[1].rect, rect(1920, 0, 1920, 1080));
    }

    #[test]
    fn a_shared_terminal_process_names_no_window() {
        let window = |id, pid| X11Window {
            id,
            pid,
            rect: rect(0, 0, 1, 1),
            title: String::new(),
        };
        let windows = [window(1, 100), window(2, 100), window(3, 300), window(4, 0)];
        assert_eq!(unique_owners(&windows), HashMap::from([(300, 3)]));
    }

    #[test]
    fn wmctrl_moves_by_id_without_raising() {
        let calls = wmctrl_place(0x3a0_0003, rect(-1920, 40, 800, 600), true);
        assert_eq!(
            calls,
            [
                vec![
                    "-i",
                    "-r",
                    "0x3a00003",
                    "-b",
                    "remove,maximized_vert,maximized_horz"
                ],
                vec!["-i", "-r", "0x3a00003", "-e", "0,-1920,40,800,600"],
                vec![
                    "-i",
                    "-r",
                    "0x3a00003",
                    "-b",
                    "add,maximized_vert,maximized_horz"
                ],
            ]
        );
        assert!(calls.iter().flatten().all(|a| a != "-a" && a != "-R"));
        assert_eq!(wmctrl_place(1, rect(0, 0, 1, 1), false).len(), 2);
    }

    #[test]
    fn xdotool_moves_by_id_without_activating() {
        let calls = xdotool_place(77, rect(10, 20, 800, 600), false);
        assert_eq!(calls[2], ["windowsize", "77", "800", "600"]);
        assert_eq!(calls[3], ["windowmove", "77", "10", "20"]);
        assert_eq!(calls.len(), 4);
        assert!(calls.iter().all(|c| c[0] != "windowactivate"));
        assert_eq!(xdotool_place(77, rect(0, 0, 1, 1), true).len(), 6);
    }

    #[test]
    fn wayland_never_places_and_says_so() {
        let wmctrl = PathBuf::from("/usr/bin/wmctrl");
        let placer = pick_placer(Display::Wayland, Some(wmctrl.clone()), None);
        assert_eq!(placer, Placer::None(WAYLAND_NOTE));
        let backend = LinuxBackend::new(Some(Terminal::Gnome), placer);
        let err = backend.place(1, rect(0, 0, 800, 600), false).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unsupported);
        assert!(err.to_string().contains("Wayland"), "{err}");
        assert!(err.to_string().contains("not placed"), "{err}");
        assert!(backend.monitors().unwrap().is_empty());
        assert!(backend.windows().unwrap().is_empty());
        assert!(backend.console_owners().unwrap().is_empty());
        assert_eq!(
            pick_placer(Display::X11, Some(wmctrl.clone()), None),
            Placer::Wmctrl(wmctrl)
        );
    }

    #[test]
    fn x11_without_tools_opens_only_and_names_what_is_missing() {
        let placer = pick_placer(Display::X11, None, None);
        let backend = LinuxBackend::new(Some(Terminal::Kitty), placer);
        let err = backend.place(1, rect(0, 0, 1, 1), false).unwrap_err();
        assert!(err.to_string().contains("wmctrl"), "{err}");
        assert_eq!(
            pick_placer(Display::X11, None, Some(PathBuf::from("/x"))),
            Placer::Xdotool(PathBuf::from("/x"))
        );
    }

    #[test]
    fn no_terminal_is_unsupported_with_a_clear_message() {
        let backend = LinuxBackend::new(None, Placer::None(NO_TOOL_NOTE));
        let reason = backend.unsupported().unwrap();
        assert!(reason.contains("gnome-terminal"), "{reason}");
        let request = OpenRequest {
            entry_id: "e1".into(),
            title: "t".into(),
            folder: PathBuf::from("/tmp"),
            program: "claude".into(),
            args: Vec::new(),
            managed: None,
        };
        assert_eq!(
            backend.open(&request).unwrap_err().code(),
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn an_unseen_window_counts_as_open_for_a_while_and_is_never_measured() {
        let now = Instant::now();
        let opened = [Opened {
            title: "API".into(),
            shown: "API".into(),
            at: now,
        }];
        let windows = remembered_windows(&opened, now + Duration::from_secs(30));
        assert_eq!(windows[0].title, "API");
        assert!(windows[0].minimized);
        assert!(remembered_windows(&opened, now + REMEMBER_OPENED).is_empty());
    }

    #[test]
    fn a_title_the_terminal_had_to_change_is_found_again() {
        let opened = [Opened {
            title: "100%".into(),
            shown: "100\u{ff05}".into(),
            at: Instant::now(),
        }];
        let mut windows = [LiveWindow {
            id: 1,
            title: "100\u{ff05}".into(),
            rect: rect(0, 0, 1, 1),
            maximized: false,
            minimized: false,
        }];
        restore_titles(&mut windows, &opened);
        assert_eq!(windows[0].title, "100%");
    }
}
