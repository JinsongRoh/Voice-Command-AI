//! WezTerm, kitty and iTerm2 helpers (the Windows console is in `console`).
//!
//! These are unverified on real installations: the formats follow the
//! terminals' documentation and are covered by unit tests only.

use serde::Deserialize;

/// One pane from `wezterm cli list --format json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WezTermPane {
    pub pane_id: u64,
    #[serde(default)]
    pub tab_id: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub tty_name: Option<String>,
}

pub fn parse_wezterm_list(json: &str) -> Vec<WezTermPane> {
    serde_json::from_str(json).unwrap_or_default()
}

/// One window from `kitty @ ls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyWindow {
    pub id: u64,
    pub pid: Option<u32>,
    pub foreground_pids: Vec<u32>,
}

impl KittyWindow {
    pub fn pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.pid
            .into_iter()
            .chain(self.foreground_pids.iter().copied())
    }
}

/// Parses `kitty @ ls`: OS windows contain tabs, tabs contain windows.
pub fn parse_kitty_ls(json: &str) -> Vec<KittyWindow> {
    #[derive(Deserialize)]
    struct OsWindow {
        #[serde(default)]
        tabs: Vec<Tab>,
    }
    #[derive(Deserialize)]
    struct Tab {
        #[serde(default)]
        windows: Vec<Window>,
    }
    #[derive(Deserialize)]
    struct Window {
        id: u64,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        foreground_processes: Vec<Fg>,
    }
    #[derive(Deserialize)]
    struct Fg {
        pid: u32,
    }
    let os_windows: Vec<OsWindow> = serde_json::from_str(json).unwrap_or_default();
    os_windows
        .into_iter()
        .flat_map(|o| o.tabs)
        .flat_map(|t| t.windows)
        .map(|w| KittyWindow {
            id: w.id,
            pid: w.pid,
            foreground_pids: w.foreground_processes.into_iter().map(|f| f.pid).collect(),
        })
        .collect()
}

/// `kitty @ send-text` interprets Python-style escapes, so backslashes in
/// the text are doubled.
pub fn kitty_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
}

fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// AppleScript that writes `text` to the iTerm2 session with `session_id`.
/// Unverified on macOS.
pub fn iterm2_script(session_id: &str, text: &str, submit: bool) -> String {
    format!(
        r#"tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if unique id of s is {id} then
          tell s to write text {text} newline {newline}
          return
        end if
      end repeat
    end repeat
  end repeat
  error "iTerm2 session not found"
end tell"#,
        id = applescript_string(session_id),
        text = applescript_string(text),
        newline = if submit { "YES" } else { "NO" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wezterm_list() {
        let json = r#"[{"window_id":0,"tab_id":0,"pane_id":3,"workspace":"default","size":{"rows":24,"cols":80},"title":"zsh","cwd":"file:///home/u/app","tty_name":"/dev/pts/4","is_active":true}]"#;
        let panes = parse_wezterm_list(json);
        assert_eq!(panes[0].pane_id, 3);
        assert_eq!(panes[0].tty_name.as_deref(), Some("/dev/pts/4"));
        assert!(parse_wezterm_list("nope").is_empty());
    }

    #[test]
    fn parses_kitty_ls() {
        let json = r#"[{"id":1,"tabs":[{"id":1,"windows":[{"id":7,"pid":100,"cwd":"/w","foreground_processes":[{"pid":120,"cmdline":["claude"]}]}]}]}]"#;
        let windows = parse_kitty_ls(json);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].pids().collect::<Vec<_>>(), [100, 120]);
    }

    #[test]
    fn escapes_applescript() {
        let s = iterm2_script("ID", r#"say "hi" \ bye"#, true);
        assert!(s.contains(r#"write text "say \"hi\" \\ bye" newline YES"#));
    }
}
