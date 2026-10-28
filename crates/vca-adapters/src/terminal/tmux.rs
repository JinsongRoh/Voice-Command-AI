//! tmux integration.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::CommandSpec;
use crate::error::Result;

/// One pane from `tmux list-panes -a`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxPane {
    pub pane_id: String,
    /// Process started in the pane (usually the shell).
    pub pane_pid: u32,
    pub session_name: String,
}

// tmux replaces control characters such as tabs in `-F` output, so the
// fields are separated by spaces; the session name comes last because it
// may contain spaces itself.
const LIST_FORMAT: &str = "#{pane_id} #{pane_pid} #{session_name}";

fn base_args(socket: Option<&Path>) -> Vec<String> {
    match socket {
        Some(s) => vec!["-S".into(), s.to_string_lossy().into_owned()],
        None => Vec::new(),
    }
}

/// `tmux [-S socket] list-panes -a -F ...`.
pub fn list_panes_command(socket: Option<&Path>) -> CommandSpec {
    let mut args = base_args(socket);
    args.extend([
        "list-panes".into(),
        "-a".into(),
        "-F".into(),
        LIST_FORMAT.into(),
    ]);
    CommandSpec {
        program: "tmux".into(),
        args,
    }
}

pub fn parse_list_panes(output: &str) -> Vec<TmuxPane> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            Some(TmuxPane {
                pane_id: parts.next()?.to_owned(),
                pane_pid: parts.next()?.trim().parse().ok()?,
                session_name: parts.next().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

/// Lists all panes of a tmux server. Fails when tmux is not installed or
/// no server is running.
pub async fn list_panes(socket: Option<&Path>) -> Result<Vec<TmuxPane>> {
    Ok(parse_list_panes(&list_panes_command(socket).run().await?))
}

/// tmux treats a command-line argument ending in `;` as a command
/// separator; `\;` keeps a literal semicolon.
fn escape_trailing_semicolon(text: &str) -> String {
    match text.strip_suffix(';') {
        Some(rest) => format!("{rest}\\;"),
        None => text.to_owned(),
    }
}

/// Commands that type `text` literally into `pane`, then press Enter.
pub fn send_text_commands(
    socket: Option<&Path>,
    pane: &str,
    text: &str,
    submit: bool,
) -> Vec<CommandSpec> {
    let mut out = Vec::new();
    if !text.is_empty() {
        let mut args = base_args(socket);
        args.extend([
            "send-keys".into(),
            "-t".into(),
            pane.into(),
            "-l".into(),
            "--".into(),
            escape_trailing_semicolon(text),
        ]);
        out.push(CommandSpec {
            program: "tmux".into(),
            args,
        });
    }
    if submit {
        let mut args = base_args(socket);
        args.extend(["send-keys".into(), "-t".into(), pane.into(), "Enter".into()]);
        out.push(CommandSpec {
            program: "tmux".into(),
            args,
        });
    }
    out
}

/// `tmux capture-pane -p` (visible screen as text), useful for heuristics.
pub async fn capture_pane(socket: Option<&Path>, pane: &str) -> Result<String> {
    let mut args = base_args(socket);
    args.extend(["capture-pane".into(), "-p".into(), "-t".into(), pane.into()]);
    CommandSpec {
        program: "tmux".into(),
        args,
    }
    .run()
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_panes_output() {
        let panes = parse_list_panes("%0 1234 main\n%1 5678 work space\nbroken\n");
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[1].pane_id, "%1");
        assert_eq!(panes[1].pane_pid, 5678);
        assert_eq!(panes[1].session_name, "work space");
    }

    #[test]
    fn builds_send_keys_commands() {
        let cmds = send_text_commands(Some(Path::new("/tmp/s")), "%2", "make;", true);
        assert_eq!(cmds.len(), 2);
        assert_eq!(
            cmds[0].args,
            [
                "-S",
                "/tmp/s",
                "send-keys",
                "-t",
                "%2",
                "-l",
                "--",
                "make\\;"
            ]
        );
        assert_eq!(
            cmds[1].args,
            ["-S", "/tmp/s", "send-keys", "-t", "%2", "Enter"]
        );
        assert_eq!(send_text_commands(None, "%2", "", true).len(), 1);
    }
}
