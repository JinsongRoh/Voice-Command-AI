//! WezTerm, kitty, iTerm2 and Windows console helpers.
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

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// PowerShell script that attaches to the console of `pid` and writes key
/// events with `WriteConsoleInputW`. The text travels as base64 so no
/// quoting is needed. Unverified on Windows.
pub fn windows_console_script(pid: u32, text: &str, submit: bool) -> String {
    let mut payload = text.to_owned();
    if submit {
        payload.push('\r');
    }
    let b64 = base64(payload.as_bytes());
    format!(
        r#"$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class VcaConsoleInput {{
  [StructLayout(LayoutKind.Explicit, CharSet = CharSet.Unicode)]
  public struct KEY_EVENT_RECORD {{
    [FieldOffset(0)] public int bKeyDown;
    [FieldOffset(4)] public ushort wRepeatCount;
    [FieldOffset(6)] public ushort wVirtualKeyCode;
    [FieldOffset(8)] public ushort wVirtualScanCode;
    [FieldOffset(10)] public char UnicodeChar;
    [FieldOffset(12)] public uint dwControlKeyState;
  }}
  [StructLayout(LayoutKind.Explicit)]
  public struct INPUT_RECORD {{
    [FieldOffset(0)] public ushort EventType;
    [FieldOffset(4)] public KEY_EVENT_RECORD KeyEvent;
  }}
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool FreeConsole();
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool AttachConsole(uint pid);
  [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sec, uint disp, uint flags, IntPtr tmpl);
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern bool WriteConsoleInputW(IntPtr h, INPUT_RECORD[] recs, uint len, out uint written);
  public static void Send(uint pid, string text) {{
    FreeConsole();
    if (!AttachConsole(pid)) throw new Exception("AttachConsole failed: " + Marshal.GetLastWin32Error());
    try {{
      IntPtr h = CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
      var recs = new INPUT_RECORD[text.Length * 2];
      for (int i = 0; i < text.Length; i++) {{
        for (int d = 0; d < 2; d++) {{
          var r = new INPUT_RECORD();
          r.EventType = 1;
          r.KeyEvent.bKeyDown = d == 0 ? 1 : 0;
          r.KeyEvent.wRepeatCount = 1;
          r.KeyEvent.wVirtualKeyCode = (ushort)(text[i] == '\r' ? 0x0D : 0);
          r.KeyEvent.UnicodeChar = text[i];
          recs[i * 2 + d] = r;
        }}
      }}
      uint written;
      if (!WriteConsoleInputW(h, recs, (uint)recs.Length, out written))
        throw new Exception("WriteConsoleInput failed: " + Marshal.GetLastWin32Error());
    }} finally {{
      FreeConsole();
    }}
  }}
}}
'@
[VcaConsoleInput]::Send({pid}, [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{b64}')))"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("héllo\r".as_bytes()), "aMOpbGxvDQ==");
    }

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
