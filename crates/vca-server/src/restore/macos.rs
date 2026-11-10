//! The macOS backend: Terminal.app and iTerm2 through AppleScript.
//!
//! Coordinates are points of the global desktop space with the origin at the
//! top left of the main display, the convention of AppleScript `bounds`;
//! other displays have negative or large values. `NSScreen` counts from the
//! bottom left and is converted here. Points do not depend on a display's
//! scale, so every monitor reports 96 DPI and a window keeps its numbers when
//! it changes display.
//!
//! Text only reaches a script as an AppleScript string ([`applescript_text`])
//! and, for the command line, through POSIX quoting first
//! ([`shell_quote`]). Numbers are integers from Rust; the scripts have no
//! other input.
//!
//! The list scripts ask `is running` first, because a `tell` would launch the
//! application: reading must not start anything. No script activates an
//! application, so restore does not take the focus. Nothing here closes a
//! window. iTerm's scripts are only sent when it is installed, since they do
//! not compile without its dictionary.

#![cfg_attr(test, allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use vca_core::{Error, RestoreMonitor, WindowRect};

use super::backend::{LiveWindow, OpenRequest, WindowBackend};
use super::tmux::{always_unset, check_request, env_unset_prefix, find_agent, run, unavailable};
use crate::util::lock;

const OSASCRIPT: &str = "/usr/bin/osascript";
/// Set on the ids of iTerm windows: both applications number from one.
const ITERM_FLAG: u64 = 1 << 40;
/// The session variable holding the title. iTerm lets a program rename a
/// session, so the name alone does not stay what restore set.
const ITERM_TITLE_VARIABLE: &str = "user.vca_title";
/// "Not authorized to send Apple events": the user has not allowed it yet.
const NOT_AUTHORIZED: &str = "-1743";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Terminal,
    Iterm,
}

/// Writes the fields of one window, separated by tabs, the title last. Used
/// inside `tell application`, where `tab` would mean a Terminal tab.
const WINDOW_LINE: &str = r#"set found to found & (id of w) & sep & (item 1 of b) & sep & (item 2 of b) & sep & (item 3 of b) & sep & (item 4 of b) & sep & (zoomed of w) & sep & (miniaturized of w) & sep & ttys & sep & caption & newline"#;

/// Lists the windows of Terminal: one line per window.
pub fn terminal_list_script() -> String {
    format!(
        r#"set sep to character id 9
set newline to character id 10
set found to ""
if application "Terminal" is running then
	tell application "Terminal"
		repeat with w in windows
			try
				if visible of w then
					set b to bounds of w
					set ttys to ""
					repeat with t in tabs of w
						set ttys to ttys & (tty of t) & ","
					end repeat
					set s to selected tab of w
					if title displays custom title of s then
						set caption to custom title of s
					else
						set caption to name of w
					end if
					{WINDOW_LINE}
				end if
			end try
		end repeat
	end tell
end if
return found"#
    )
}

/// Lists the windows of iTerm: one line per window.
pub fn iterm_list_script() -> String {
    format!(
        r#"set sep to character id 9
set newline to character id 10
set found to ""
if application "iTerm" is running then
	tell application "iTerm"
		repeat with w in windows
			try
				set b to bounds of w
				set ttys to ""
				repeat with t in tabs of w
					repeat with p in sessions of t
						set ttys to ttys & (tty of p) & ","
					end repeat
				end repeat
				set caption to ""
				try
					tell current session of w
						set caption to (variable named {variable})
					end tell
				end try
				if caption is missing value or caption is "" then set caption to name of w
				{WINDOW_LINE}
			end try
		end repeat
	end tell
end if
return found"#,
        variable = applescript_text(ITERM_TITLE_VARIABLE),
    )
}

/// One POSIX shell word. Nothing inside single quotes is special, and a
/// single quote itself is closed, escaped and reopened.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// An AppleScript string. Only `\` and `"` are special; line ends and tabs
/// are written as escapes so the source stays one line.
pub fn applescript_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// What the shell of the new window runs. Without `exec`: the shell stays
/// when the agent ends, as on the other platforms. The window is a login
/// shell of Terminal.app or iTerm2, so it already has the person's default
/// environment (launchd's and the shell profile's), not vcad's; `unset`
/// (see [`super::tmux::always_unset`]) runs the agent through
/// `/usr/bin/env -u ...` as a floor, in case the terminal application
/// itself was started from a Claude Code session.
pub fn shell_line(folder: &Path, program: &str, args: &[String], unset: &[String]) -> String {
    let mut out = format!("cd -- {} && ", shell_quote(&folder.to_string_lossy()));
    for word in env_unset_prefix(unset) {
        out.push_str(&word);
        out.push(' ');
    }
    out.push_str(&shell_quote(program));
    for arg in args {
        out.push(' ');
        out.push_str(&shell_quote(arg));
    }
    out
}

/// Opens a Terminal window, sets the title and returns the id of the window.
pub fn terminal_open_script(title: &str, command: &str) -> String {
    let title = applescript_text(title);
    let command = applescript_text(command);
    format!(
        r#"tell application "Terminal"
	set t to do script {command}
	set custom title of t to {title}
	set title displays custom title of t to true
	set title displays device name of t to false
	set title displays shell path of t to false
	set title displays window size of t to false
	set title displays file name of t to false
	try
		return id of first window whose tabs contains t
	on error
		return id of front window
	end try
end tell"#
    )
}

/// Opens an iTerm window, sets the title and returns the id of the window.
pub fn iterm_open_script(title: &str, command: &str) -> String {
    let title = applescript_text(title);
    let command = applescript_text(command);
    let variable = applescript_text(ITERM_TITLE_VARIABLE);
    format!(
        r#"tell application "iTerm"
	set w to (create window with default profile)
	tell current session of w
		set name to {title}
		try
			set variable named {variable} to {title}
		end try
		write text {command}
	end tell
	return id of w
end tell"#
    )
}

/// Moves one window, by id.
pub fn place_script(app: App, id: u64, rect: WindowRect, maximized: bool) -> String {
    let name = match app {
        App::Terminal => "Terminal",
        App::Iterm => "iTerm",
    };
    let (left, top, right, bottom) = bounds_of(rect);
    let zoom = if maximized {
        "\n\tset zoomed of w to true"
    } else {
        ""
    };
    format!(
        r#"tell application "{name}"
	set w to window id {id}
	if miniaturized of w then set miniaturized of w to false
	if zoomed of w then set zoomed of w to false
	set bounds of w to {{{left}, {top}, {right}, {bottom}}}{zoom}
end tell"#
    )
}

/// AppleScript bounds: left, top, right, bottom.
pub fn bounds_of(rect: WindowRect) -> (i32, i32, i32, i32) {
    (
        rect.x,
        rect.y,
        rect.x.saturating_add(rect.w),
        rect.y.saturating_add(rect.h),
    )
}

/// The opposite of [`bounds_of`].
pub fn rect_of(left: i32, top: i32, right: i32, bottom: i32) -> WindowRect {
    WindowRect {
        x: left,
        y: top,
        w: right.saturating_sub(left),
        h: bottom.saturating_sub(top),
    }
}

/// A window of Terminal or iTerm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacWindow {
    pub window: LiveWindow,
    /// Short names of the terminals in its tabs, without `/dev/`.
    pub ttys: Vec<String>,
}

fn encode_id(app: App, raw: u64) -> u64 {
    match app {
        App::Terminal => raw,
        App::Iterm => raw | ITERM_FLAG,
    }
}

/// Which application a window id of [`encode_id`] belongs to, and its own id.
pub fn decode_id(id: u64) -> (App, u64) {
    if id & ITERM_FLAG != 0 {
        (App::Iterm, id & !ITERM_FLAG)
    } else {
        (App::Terminal, id)
    }
}

/// Reads the output of a list script. A window title holding a line end
/// makes a line fail to parse, and that line is skipped.
pub fn parse_list(app: App, stdout: &str) -> Vec<MacWindow> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(9, '\t');
            let id: u64 = fields.next()?.parse().ok()?;
            let left = fields.next()?.parse().ok()?;
            let top = fields.next()?.parse().ok()?;
            let right = fields.next()?.parse().ok()?;
            let bottom = fields.next()?.parse().ok()?;
            let zoomed = fields.next()? == "true";
            let miniaturized = fields.next()? == "true";
            let ttys = fields
                .next()?
                .split(',')
                .filter(|t| !t.is_empty())
                .map(|t| t.trim_start_matches("/dev/").to_owned())
                .collect();
            let title = fields.next()?.to_owned();
            Some(MacWindow {
                window: LiveWindow {
                    id: encode_id(app, id),
                    title,
                    rect: rect_of(left, top, right, bottom),
                    maximized: zoomed,
                    minimized: miniaturized,
                },
                ttys,
            })
        })
        .collect()
}

/// The screens as `number, frame, visible frame`, tabs between the fields,
/// bottom-left coordinates. Reads only, sends no Apple event.
pub const SCREENS_SCRIPT: &str = r#"ObjC.import("AppKit");
var s = $.NSScreen.screens, o = [];
for (var i = 0; i < s.count; i++) {
	var m = s.objectAtIndex(i), f = m.frame, v = m.visibleFrame;
	o.push([m.deviceDescription.objectForKey("NSScreenNumber").unsignedIntValue,
		f.origin.x, f.origin.y, f.size.width, f.size.height,
		v.origin.x, v.origin.y, v.size.width, v.size.height].join("\t"));
}
o.join("\n");"#;

/// Reads [`SCREENS_SCRIPT`]. The first line is the main display, whose top
/// edge is y = 0 of the coordinates AppleScript uses.
pub fn parse_screens(stdout: &str) -> Vec<RestoreMonitor> {
    let rows: Vec<(&str, [f64; 8])> = stdout
        .lines()
        .filter_map(|line| {
            let (id, numbers) = line.split_once('\t')?;
            let mut out = [0f64; 8];
            let mut parts = numbers.split('\t');
            for slot in &mut out {
                *slot = parts.next()?.parse().ok()?;
            }
            Some((id, out))
        })
        .collect();
    let Some(main_height) = rows.first().map(|(_, n)| n[3]) else {
        return Vec::new();
    };
    let flip = |x: f64, y: f64, w: f64, h: f64| WindowRect {
        x: x.round() as i32,
        y: (main_height - (y + h)).round() as i32,
        w: w.round() as i32,
        h: h.round() as i32,
    };
    rows.iter()
        .map(|(id, n)| RestoreMonitor {
            id: (*id).to_owned(),
            rect: flip(n[0], n[1], n[2], n[3]),
            work: flip(n[4], n[5], n[6], n[7]),
            dpi: 96,
        })
        .collect()
}

/// Reads `ps -axo pid=,tty=`, keeping processes with a terminal.
pub fn parse_ps(stdout: &str) -> Vec<(u32, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let tty = words.next()?;
            (tty != "??" && tty != "?").then(|| (pid, tty.to_owned()))
        })
        .collect()
}

/// Every process on a terminal to the window holding that terminal.
pub fn owners_by_tty(windows: &[MacWindow], processes: &[(u32, String)]) -> HashMap<u32, u64> {
    let window_of: HashMap<&str, u64> = windows
        .iter()
        .flat_map(|w| w.ttys.iter().map(|t| (t.as_str(), w.window.id)))
        .collect();
    processes
        .iter()
        .filter_map(|(pid, tty)| Some((*pid, *window_of.get(tty.as_str())?)))
        .collect()
}

/// A program may retitle its terminal: a window this run opened keeps the
/// title it was given, or the runner could not find it again.
pub fn apply_titles(windows: &mut [MacWindow], opened: &HashMap<u64, String>) {
    for window in windows {
        if let Some(title) = opened.get(&window.window.id) {
            window.window.title.clone_from(title);
        }
    }
}

/// The user has not allowed VCA to control this application yet.
pub fn not_authorized(stderr: &str) -> bool {
    stderr.contains(NOT_AUTHORIZED)
}

/// iTerm when it is installed: having installed it is the preference.
pub fn pick_app(iterm_installed: bool) -> App {
    if iterm_installed {
        App::Iterm
    } else {
        App::Terminal
    }
}

fn iterm_installed() -> bool {
    let system = Path::new("/Applications/iTerm.app");
    let user =
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications/iTerm.app"));
    system.is_dir() || user.is_some_and(|p| p.is_dir())
}

fn osascript(script: &str) -> Result<super::tmux::Finished, Error> {
    run(Command::new(OSASCRIPT).args(["-e", script]))
}

fn script_failed(stderr: &str) -> Error {
    if not_authorized(stderr) {
        return unavailable(
            "allow Voice Command AI to control the terminal in System Settings, Privacy & \
             Security, Automation",
        );
    }
    unavailable(format!("AppleScript failed: {}", stderr.trim()))
}

/// Terminal.app and iTerm2.
#[derive(Default)]
pub struct MacBackend {
    /// The windows this process opened, with the titles it gave them.
    opened: Mutex<HashMap<u64, String>>,
}

impl MacBackend {
    fn list_one(&self, app: App) -> Result<Vec<MacWindow>, Error> {
        let script = match app {
            App::Terminal => terminal_list_script(),
            App::Iterm => iterm_list_script(),
        };
        let done = osascript(&script)?;
        if done.ok {
            Ok(parse_list(app, &done.stdout))
        } else if not_authorized(&done.stderr) {
            // Not an error of the list: `open` runs into it and reports it,
            // so a duplicate window cannot come of this.
            tracing::warn!("no Automation permission for the terminal yet");
            Ok(Vec::new())
        } else {
            Err(script_failed(&done.stderr))
        }
    }

    fn list(&self) -> Result<Vec<MacWindow>, Error> {
        let mut out = self.list_one(App::Terminal)?;
        if iterm_installed() {
            out.extend(self.list_one(App::Iterm)?);
        }
        apply_titles(&mut out, &lock(&self.opened));
        Ok(out)
    }
}

impl WindowBackend for MacBackend {
    fn platform(&self) -> &'static str {
        "macos"
    }

    fn unsupported(&self) -> Option<String> {
        (!Path::new(OSASCRIPT).exists()).then(|| "osascript is missing".to_owned())
    }

    fn monitors(&self) -> Result<Vec<RestoreMonitor>, Error> {
        let done = run(Command::new(OSASCRIPT).args(["-l", "JavaScript", "-e", SCREENS_SCRIPT]))?;
        if done.ok {
            Ok(parse_screens(&done.stdout))
        } else {
            Err(script_failed(&done.stderr))
        }
    }

    fn windows(&self) -> Result<Vec<LiveWindow>, Error> {
        Ok(self.list()?.into_iter().map(|w| w.window).collect())
    }

    fn console_owners(&self) -> Result<HashMap<u32, u64>, Error> {
        let windows = self.list()?;
        let done = run(Command::new("/bin/ps").args(["-axo", "pid=,tty="]))?;
        Ok(owners_by_tty(&windows, &parse_ps(&done.stdout)))
    }

    fn open(&self, request: &OpenRequest) -> Result<(), Error> {
        check_request(request)?;
        // The shell of the window has the user's `PATH` and may know an
        // agent this process cannot find.
        let program = find_agent(&request.program).map_or_else(
            || request.program.clone(),
            |p| p.to_string_lossy().into_owned(),
        );
        let command = shell_line(&request.folder, &program, &request.args, &always_unset());
        let app = pick_app(iterm_installed());
        let script = match app {
            App::Terminal => terminal_open_script(&request.title, &command),
            App::Iterm => iterm_open_script(&request.title, &command),
        };
        let done = osascript(&script)?;
        if !done.ok {
            return Err(script_failed(&done.stderr));
        }
        if let Ok(id) = done.stdout.trim().parse::<u64>() {
            lock(&self.opened).insert(encode_id(app, id), request.title.clone());
        }
        Ok(())
    }

    fn place(&self, id: u64, rect: WindowRect, maximized: bool) -> Result<(), Error> {
        if !self.list()?.iter().any(|w| w.window.id == id) {
            return Err(unavailable("the window is gone"));
        }
        let (app, raw) = decode_id(id);
        let done = osascript(&place_script(app, raw, rect, maximized))?;
        if done.ok {
            Ok(())
        } else {
            Err(script_failed(&done.stderr))
        }
    }
}

/// The backend of this computer.
pub fn backend() -> Arc<dyn WindowBackend> {
    Arc::new(MacBackend::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> WindowRect {
        WindowRect { x, y, w, h }
    }

    /// Walks a generated script the way the AppleScript scanner does and
    /// returns what is outside string literals. Fails on an open string.
    fn outside_strings(script: &str) -> String {
        let mut out = String::new();
        let mut chars = script.chars();
        let mut inside = false;
        while let Some(c) = chars.next() {
            match (inside, c) {
                (true, '\\') => {
                    chars.next();
                }
                (true, '"') => inside = false,
                (true, _) => {}
                (false, '"') => {
                    inside = true;
                    out.push('"');
                }
                (false, c) => out.push(c),
            }
        }
        assert!(!inside, "unterminated string in {script}");
        out
    }

    const NASTY: &[&str] = &[
        "plain",
        "with spaces",
        "결제 서버",
        "say \"hi\"",
        "back\\slash",
        "trailing\\",
        "\" & (do shell script \"rm -rf ~\") & \"",
        "\\\" & (do shell script \"x\") & \\\"",
        "it's $(rm -rf ~); `x` | y",
        "line\nbreak\ttab",
    ];

    #[test]
    fn applescript_strings_escape_what_is_special() {
        assert_eq!(applescript_text("plain"), "\"plain\"");
        assert_eq!(applescript_text("a\"b"), "\"a\\\"b\"");
        assert_eq!(applescript_text("a\\b"), "\"a\\\\b\"");
        assert_eq!(applescript_text("a\nb"), "\"a\\nb\"");
        assert_eq!(applescript_text("결제"), "\"결제\"");
    }

    #[test]
    fn no_title_or_command_can_leave_its_string() {
        let clean_terminal = outside_strings(&terminal_open_script("t", "c"));
        let clean_iterm = outside_strings(&iterm_open_script("t", "c"));
        for title in NASTY {
            for command in NASTY {
                assert_eq!(
                    outside_strings(&terminal_open_script(title, command)),
                    clean_terminal,
                    "{title} / {command}"
                );
                assert_eq!(
                    outside_strings(&iterm_open_script(title, command)),
                    clean_iterm,
                    "{title} / {command}"
                );
            }
        }
    }

    #[test]
    fn the_scripts_never_take_the_focus_or_close_anything() {
        for script in [
            terminal_list_script(),
            iterm_list_script(),
            terminal_open_script("t", "c"),
            iterm_open_script("t", "c"),
            place_script(App::Terminal, 1, rect(0, 0, 1, 1), true),
            place_script(App::Iterm, 1, rect(0, 0, 1, 1), false),
        ] {
            for word in ["activate", "close", "quit", "reopen", "set frontmost"] {
                assert!(!script.contains(word), "{word} in {script}");
            }
        }
    }

    #[test]
    fn reading_never_launches_an_application() {
        assert!(terminal_list_script().contains("if application \"Terminal\" is running then"));
        assert!(iterm_list_script().contains("if application \"iTerm\" is running then"));
    }

    #[test]
    fn the_separators_are_not_terminal_tabs() {
        // Inside `tell application "Terminal"`, `tab` is a class.
        for script in [terminal_list_script(), iterm_list_script()] {
            assert!(script.starts_with("set sep to character id 9"));
            assert!(!script.contains("& tab &"));
        }
    }

    #[test]
    fn shell_quoting_leaves_no_word_open() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("a b; rm -rf ~"), "'a b; rm -rf ~'");
        assert_eq!(shell_quote("$(x) `y` \"z\" 한글"), "'$(x) `y` \"z\" 한글'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn the_shell_line_quotes_every_word() {
        let line = shell_line(
            Path::new("/Users/me/결제 'x' $(y)"),
            "/opt/homebrew/bin/claude",
            &["--resume".to_owned(), "ab-1".to_owned()],
            &[],
        );
        assert_eq!(
            line,
            r"cd -- '/Users/me/결제 '\''x'\'' $(y)' && '/opt/homebrew/bin/claude' '--resume' 'ab-1'"
        );
    }

    #[test]
    fn the_agent_runs_without_the_session_variables() {
        let unset = vec!["CLAUDECODE".to_owned(), "VCA_SESSION_ID".to_owned()];
        let line = shell_line(Path::new("/w"), "/opt/homebrew/bin/claude", &[], &unset);
        assert_eq!(
            line,
            "cd -- '/w' && /usr/bin/env -u CLAUDECODE -u VCA_SESSION_ID '/opt/homebrew/bin/claude'"
        );
        // The real list only has plain names: nothing for the shell to expand.
        for name in always_unset() {
            assert!(
                name.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "{name}"
            );
        }
    }

    #[test]
    fn a_folder_goes_through_both_quotings_and_stays_one_word() {
        let line = shell_line(Path::new("/a\"b\\c"), "claude", &[], &[]);
        let script = terminal_open_script("t", &line);
        assert!(
            script.contains(r#"do script "cd -- '/a\"b\\c' && 'claude'""#),
            "{script}"
        );
    }

    #[test]
    fn terminal_opens_with_a_fixed_title() {
        let script = terminal_open_script("결제 \"API\"", "cd -- '/w' && 'claude'");
        assert!(script.contains("set t to do script \"cd -- '/w' && 'claude'\""));
        assert!(script.contains("set custom title of t to \"결제 \\\"API\\\"\""));
        assert!(script.contains("set title displays custom title of t to true"));
    }

    #[test]
    fn iterm_writes_the_command_into_the_login_shell() {
        let script = iterm_open_script("API", "cd -- '/w' && 'claude'");
        assert!(script.contains("create window with default profile"));
        assert!(script.contains("set name to \"API\""));
        assert!(script.contains("set variable named \"user.vca_title\" to \"API\""));
        assert!(script.contains("write text \"cd -- '/w' && 'claude'\""));
    }

    #[test]
    fn placing_works_on_negative_coordinates_of_other_displays() {
        let script = place_script(App::Terminal, 42, rect(-1920, -200, 800, 600), false);
        assert!(script.contains("tell application \"Terminal\""));
        assert!(script.contains("set w to window id 42"));
        assert!(script.contains("set bounds of w to {-1920, -200, -1120, 400}"));
        assert!(!script.contains("set zoomed of w to true"));
        let script = place_script(App::Iterm, 42, rect(0, 25, 800, 600), true);
        assert!(script.contains("tell application \"iTerm\""));
        assert!(script.contains("set zoomed of w to true"));
    }

    #[test]
    fn bounds_and_rectangles_convert_both_ways() {
        let r = rect(-1920, 25, 800, 600);
        let (left, top, right, bottom) = bounds_of(r);
        assert_eq!((left, top, right, bottom), (-1920, 25, -1120, 625));
        assert_eq!(rect_of(left, top, right, bottom), r);
    }

    #[test]
    fn the_list_output_parses_with_any_title() {
        let out = "\
123\t0\t25\t800\t625\tfalse\tfalse\t/dev/ttys003,/dev/ttys004,\t결제 \"q\" 's' $(x)
7\t-1920\t-200\t-1120\t400\ttrue\ttrue\t\tname\twith\ttabs
not a window
8\t1\t2\t3
";
        let windows = parse_list(App::Terminal, out);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].window.id, 123);
        assert_eq!(windows[0].window.title, "결제 \"q\" 's' $(x)");
        assert_eq!(windows[0].window.rect, rect(0, 25, 800, 600));
        assert_eq!(windows[0].ttys, ["ttys003", "ttys004"]);
        assert!(!windows[0].window.maximized);
        assert_eq!(windows[1].window.rect, rect(-1920, -200, 800, 600));
        assert!(windows[1].window.maximized && windows[1].window.minimized);
        assert_eq!(windows[1].window.title, "name\twith\ttabs");
    }

    #[test]
    fn terminal_and_iterm_ids_stay_apart() {
        let windows = parse_list(App::Iterm, "5\t0\t0\t1\t1\tfalse\tfalse\t\tt\n");
        assert_ne!(windows[0].window.id, 5);
        assert_eq!(decode_id(windows[0].window.id), (App::Iterm, 5));
        assert_eq!(decode_id(5), (App::Terminal, 5));
    }

    #[test]
    fn screens_flip_to_the_top_left_origin() {
        // The first line is real output of the test Mac.
        let out = "1\t0\t0\t1728\t1117\t0\t70\t1728\t1014\n\
                   2\t-1920\t200\t1920\t1080\t-1920\t200\t1920\t1055\n\
                   3\t1728\t1117\t2560\t1440\t1728\t1117\t2560\t1415\n";
        let monitors = parse_screens(out);
        assert_eq!(monitors.len(), 3);
        assert_eq!(monitors[0].id, "1");
        assert_eq!(monitors[0].rect, rect(0, 0, 1728, 1117));
        assert_eq!(monitors[0].work, rect(0, 33, 1728, 1014));
        // Left of the main display, its bottom 200 above the main bottom.
        assert_eq!(monitors[1].rect, rect(-1920, -163, 1920, 1080));
        assert_eq!(monitors[1].work, rect(-1920, -138, 1920, 1055));
        // Above the main display.
        assert_eq!(monitors[2].rect, rect(1728, -1440, 2560, 1440));
        assert!(monitors.iter().all(|m| m.dpi == 96));
        assert!(parse_screens("").is_empty());
    }

    #[test]
    fn processes_find_their_window_through_the_terminal() {
        let windows = parse_list(
            App::Terminal,
            "9\t0\t0\t1\t1\tfalse\tfalse\t/dev/ttys003,\tt\n",
        );
        let ps = parse_ps("  101 ttys003\n  102 ttys009\n  103 ??\n");
        assert_eq!(ps.len(), 2);
        assert_eq!(owners_by_tty(&windows, &ps), HashMap::from([(101, 9)]));
    }

    #[test]
    fn a_retitled_window_keeps_the_title_restore_gave_it() {
        let mut windows = parse_list(
            App::Terminal,
            "9\t0\t0\t1\t1\tfalse\tfalse\t\tClaude Code\n8\t0\t0\t1\t1\tfalse\tfalse\t\tother\n",
        );
        apply_titles(&mut windows, &HashMap::from([(9, "API".to_owned())]));
        assert_eq!(windows[0].window.title, "API");
        assert_eq!(windows[1].window.title, "other");
    }

    #[test]
    fn a_missing_automation_permission_is_recognized() {
        assert!(not_authorized(
            "execution error: Not authorized to send Apple events to Terminal. (-1743)"
        ));
        assert!(!not_authorized("syntax error"));
        assert!(script_failed("(-1743)").to_string().contains("Automation"));
    }

    #[test]
    fn iterm_is_used_when_installed() {
        assert_eq!(pick_app(true), App::Iterm);
        assert_eq!(pick_app(false), App::Terminal);
    }
}
