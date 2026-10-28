//! Human-readable output: session tables and resource summaries.

use std::fmt::Write as _;

use vca_core::{
    AgentKind, CloudProvider, ControlTier, JobClass, ResourceSnapshot, SessionId, SessionInfo,
    SessionOrigin, SessionState,
};

/// Length of the session id prefix shown in tables.
pub const SHORT_ID_LEN: usize = 8;

pub fn short_id(id: &SessionId) -> &str {
    match id.0.char_indices().nth(SHORT_ID_LEN) {
        Some((end, _)) => &id.0[..end],
        None => &id.0,
    }
}

pub fn agent_label(agent: &AgentKind) -> &str {
    match agent {
        AgentKind::ClaudeCode => "claude",
        AgentKind::Codex => "codex",
        AgentKind::GeminiCli => "gemini",
        AgentKind::Antigravity => "antigravity",
        AgentKind::Grok => "grok",
        AgentKind::CursorCli => "cursor",
        AgentKind::CopilotCli => "copilot",
        AgentKind::Aider => "aider",
        AgentKind::Opencode => "opencode",
        AgentKind::QwenCode => "qwen",
        AgentKind::Generic(name) => name,
    }
}

fn provider_label(provider: CloudProvider) -> &'static str {
    match provider {
        CloudProvider::Jules => "jules",
        CloudProvider::CursorCloud => "cursor-cloud",
        CloudProvider::Devin => "devin",
        CloudProvider::CopilotCloud => "copilot-cloud",
        CloudProvider::ClaudeManagedAgents => "claude-managed",
        CloudProvider::ClaudeCodeRoutines => "claude-routines",
        CloudProvider::CodexCloud => "codex-cloud",
    }
}

pub fn state_label(state: &SessionState) -> String {
    match state {
        SessionState::Starting => "starting".into(),
        SessionState::Running => "running".into(),
        SessionState::Idle => "idle".into(),
        SessionState::AwaitingInput => "awaiting-input".into(),
        SessionState::AwaitingDecision => "awaiting-decision".into(),
        SessionState::Paused => "paused".into(),
        SessionState::Exited { code: Some(code) } => format!("exited({code})"),
        SessionState::Exited { code: None } => "exited".into(),
        SessionState::Error { .. } => "error".into(),
    }
}

pub fn tier_label(tier: ControlTier) -> &'static str {
    match tier {
        ControlTier::ManagedPty => "managed",
        ControlTier::NativeChannel => "native",
        ControlTier::TerminalApi => "terminal",
        ControlTier::ObserveOnly => "observe",
        ControlTier::Cloud => "cloud",
    }
}

fn origin_columns(origin: &SessionOrigin) -> (String, String) {
    match origin {
        SessionOrigin::Local {
            agent, project_dir, ..
        } => (
            agent_label(agent).to_owned(),
            project_dir.display().to_string(),
        ),
        SessionOrigin::Cloud {
            provider,
            remote_id,
            ..
        } => (provider_label(*provider).to_owned(), remote_id.clone()),
    }
}

/// Renders rows as left-aligned columns separated by two spaces. The last
/// column is not padded.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let mut out = String::new();
    let header: Vec<String> = header.iter().map(|h| (*h).to_owned()).collect();
    for row in std::iter::once(&header).chain(rows) {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                let pad = widths[i].saturating_sub(cell.chars().count());
                line.push_str(cell);
                line.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

pub fn sessions_table(sessions: &[SessionInfo]) -> String {
    if sessions.is_empty() {
        return "no sessions\n".into();
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            let (agent, project) = origin_columns(&s.origin);
            vec![
                short_id(&s.id).to_owned(),
                agent,
                state_label(&s.state),
                tier_label(s.control).to_owned(),
                project,
                s.display_name.clone(),
                s.unread.to_string(),
            ]
        })
        .collect();
    table(
        &[
            "ID", "AGENT", "STATE", "CONTROL", "PROJECT", "NAME", "UNREAD",
        ],
        &rows,
    )
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn resources(snapshot: &ResourceSnapshot) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "cpu      {:.1}%", snapshot.cpu_percent);
    let _ = writeln!(
        out,
        "memory   {} / {}",
        bytes(snapshot.memory_used_bytes),
        bytes(snapshot.memory_total_bytes)
    );
    let _ = writeln!(
        out,
        "disk     read {}/s, write {}/s",
        bytes(snapshot.disk_read_bytes_per_sec),
        bytes(snapshot.disk_write_bytes_per_sec)
    );
    let _ = writeln!(
        out,
        "network  rx {}/s, tx {}/s",
        bytes(snapshot.net_rx_bytes_per_sec),
        bytes(snapshot.net_tx_bytes_per_sec)
    );
    if !snapshot.sessions.is_empty() {
        out.push('\n');
        let rows: Vec<Vec<String>> = snapshot
            .sessions
            .iter()
            .map(|u| {
                vec![
                    short_id(&u.session_id).to_owned(),
                    format!("{:.1}%", u.cpu_percent),
                    bytes(u.memory_bytes),
                    u.process_count.to_string(),
                ]
            })
            .collect();
        out.push_str(&table(&["SESSION", "CPU", "MEMORY", "PROCS"], &rows));
    }
    if !snapshot.queue.is_empty() {
        out.push('\n');
        let mut queue = snapshot.queue.clone();
        queue.sort_by_key(|q| q.position);
        let rows: Vec<Vec<String>> = queue
            .iter()
            .map(|q| {
                vec![
                    q.position.to_string(),
                    q.job_id.clone(),
                    q.session_id
                        .as_ref()
                        .map(|s| short_id(s).to_owned())
                        .unwrap_or_else(|| "-".into()),
                    match q.class {
                        JobClass::Light => "light".into(),
                        JobClass::Heavy => "heavy".into(),
                    },
                    q.command.clone(),
                ]
            })
            .collect();
        out.push_str(&table(
            &["POS", "JOB", "SESSION", "CLASS", "COMMAND"],
            &rows,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::session;

    #[test]
    fn short_ids() {
        assert_eq!(short_id(&"0123456789abcdef".into()), "01234567");
        assert_eq!(short_id(&"abc".into()), "abc");
    }

    #[test]
    fn table_aligns_columns() {
        let out = table(
            &["A", "BB", "C"],
            &[
                vec!["xxx".into(), "y".into(), "last".into()],
                vec!["z".into(), "".into(), "".into()],
            ],
        );
        assert_eq!(out, "A    BB  C\nxxx  y   last\nz\n");
    }

    #[test]
    fn sessions_table_shape() {
        let mut s = session("0123456789abcdef");
        s.unread = 3;
        s.state = SessionState::Exited { code: Some(2) };
        let out = sessions_table(&[s]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("ID        AGENT   STATE"));
        assert!(lines[1].starts_with("01234567  claude  exited(2)"));
        assert!(lines[1].contains("managed"));
        assert!(lines[1].contains("/work/app"));
        assert!(lines[1].ends_with("app   3"));
        assert_eq!(sessions_table(&[]), "no sessions\n");
    }

    #[test]
    fn byte_units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
