//! Which saved entry a live session belongs to, and which window it is in.
//!
//! The agent's session id decides first. Then the folder and the title. A
//! folder alone cannot name a window: every Windows Terminal tab shares one
//! process and two sessions can share a folder, so it only settles a match
//! when one entry and one session are left in that folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vca_core::{AgentKind, RestoreEntry, SessionId};

use super::backend::LiveWindow;

/// A running agent session, as the snapshot sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    pub id: SessionId,
    pub agent: AgentKind,
    pub folder: PathBuf,
    /// The agent's own session id.
    pub session_id: Option<String>,
    /// The VCA display name.
    pub title: String,
    /// The user renamed it: the name becomes the window title.
    pub custom_name: bool,
    /// vcad hosts it in its own pseudo console (`ControlTier::ManagedPty`):
    /// made in the app, or by `vca run` in a terminal window. Its ancestors
    /// are vcad's, so their consoles say nothing about its window.
    pub vca_started: bool,
    /// The agent process, then its ancestors, nearest first.
    pub pids: Vec<u32>,
    pub transcript: Option<PathBuf>,
    /// The agent's command line, without the program.
    pub args: Vec<String>,
}

/// Paths compare without regard to case, separators or a trailing slash
/// when they are Windows paths.
pub fn folder_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    let windows = text.as_bytes().get(1) == Some(&b':') || text.starts_with(r"\\");
    let text = text.trim_end_matches(['/', '\\']);
    if windows {
        text.replace('\\', "/").to_lowercase()
    } else {
        text.to_owned()
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    folder_key(a) == folder_key(b)
}

/// For each live session, the index of its entry.
pub fn match_entries(entries: &[RestoreEntry], live: &[LiveSession]) -> Vec<Option<usize>> {
    let mut out: Vec<Option<usize>> = vec![None; live.len()];
    let mut taken = vec![false; entries.len()];
    let claim = |out: &mut Vec<Option<usize>>, taken: &mut Vec<bool>, s: usize, e: usize| {
        out[s] = Some(e);
        taken[e] = true;
    };
    // 1. The exact conversation.
    for (s, session) in live.iter().enumerate() {
        let Some(id) = &session.session_id else {
            continue;
        };
        let hit = entries.iter().enumerate().find(|(e, entry)| {
            !taken[*e] && entry.agent == session.agent && entry.session_id.as_ref() == Some(id)
        });
        if let Some((e, _)) = hit {
            claim(&mut out, &mut taken, s, e);
        }
    }
    // 2. The same folder and title.
    for (s, session) in live.iter().enumerate() {
        if out[s].is_some() {
            continue;
        }
        let hit = entries.iter().enumerate().find(|(e, entry)| {
            !taken[*e]
                && entry.agent == session.agent
                && entry.title == session.title
                && same_folder(&entry.folder, &session.folder)
        });
        if let Some((e, _)) = hit {
            claim(&mut out, &mut taken, s, e);
        }
    }
    // 3. The only session and the only entry left in a folder.
    let mut sessions_in: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (s, session) in live.iter().enumerate() {
        if out[s].is_none() {
            let key = (folder_key(&session.folder), format!("{:?}", session.agent));
            sessions_in.entry(key).or_default().push(s);
        }
    }
    for (key, sessions) in sessions_in {
        let free: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(e, entry)| {
                !taken[*e]
                    && folder_key(&entry.folder) == key.0
                    && format!("{:?}", entry.agent) == key.1
            })
            .map(|(e, _)| e)
            .collect();
        if let ([s], [e]) = (sessions.as_slice(), free.as_slice()) {
            claim(&mut out, &mut taken, *s, *e);
        }
    }
    out
}

/// The window of a session: through its console when the backend can tell
/// (`owners`: first process on a console to its terminal window), else the
/// one window with exactly the entry's or the session's title.
///
/// A session vcad hosts is looked up by its own process only: its ancestors
/// lead to vcad and whatever started vcad, so a terminal that started the
/// app or vcad would otherwise seem to hold every such session. Without a
/// window of its own (a pseudo console, which is never a listed window) it
/// gets one only by title, as a `vca run` window titled after it does.
pub fn window_of<'a>(
    session: &LiveSession,
    entry_title: Option<&str>,
    windows: &'a [LiveWindow],
    owners: &HashMap<u32, u64>,
) -> Option<&'a LiveWindow> {
    let own = if session.vca_started {
        session.pids.get(..1).unwrap_or_default()
    } else {
        &session.pids
    };
    let by_console = own
        .iter()
        .find_map(|pid| owners.get(pid))
        .and_then(|id| windows.iter().find(|w| w.id == *id));
    if by_console.is_some() {
        return by_console;
    }
    [entry_title, Some(session.title.as_str())]
        .into_iter()
        .flatten()
        .find_map(|title| {
            let mut same = windows.iter().filter(|w| w.title == title);
            match (same.next(), same.next()) {
                (Some(window), None) => Some(window),
                _ => None,
            }
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use vca_core::{RestoreEntrySource, WindowRect};

    use super::*;

    pub(crate) fn entry(
        id: &str,
        title: &str,
        folder: &str,
        session: Option<&str>,
    ) -> RestoreEntry {
        RestoreEntry {
            id: id.into(),
            title: title.into(),
            agent: AgentKind::ClaudeCode,
            folder: PathBuf::from(folder),
            session_id: session.map(str::to_owned),
            rect: None,
            monitor: None,
            maximized: false,
            enabled: true,
            order: 0,
            args: Vec::new(),
            transcript: None,
            note: None,
            source: RestoreEntrySource::Manual,
            updated_at: 0,
        }
    }

    pub(crate) fn live(title: &str, folder: &str, session: Option<&str>) -> LiveSession {
        LiveSession {
            id: SessionId::from(format!("vca-{title}").as_str()),
            agent: AgentKind::ClaudeCode,
            folder: PathBuf::from(folder),
            session_id: session.map(str::to_owned),
            title: title.into(),
            custom_name: false,
            vca_started: false,
            pids: Vec::new(),
            transcript: None,
            args: Vec::new(),
        }
    }

    pub(crate) fn window(id: u64, title: &str) -> LiveWindow {
        LiveWindow {
            id,
            title: title.into(),
            rect: WindowRect {
                x: 1,
                y: 2,
                w: 800,
                h: 600,
            },
            maximized: false,
            minimized: false,
        }
    }

    #[test]
    fn the_session_id_wins_over_folder_and_title() {
        let entries = [
            entry("a", "first", "E:\\p", Some("s1")),
            entry("b", "second", "E:\\p", Some("s2")),
        ];
        // Two sessions in one folder, their titles swapped.
        let sessions = [
            live("second", "E:\\p", Some("s1")),
            live("first", "E:\\p", Some("s2")),
        ];
        assert_eq!(match_entries(&entries, &sessions), vec![Some(0), Some(1)]);
    }

    #[test]
    fn folder_and_title_match_without_an_id() {
        let entries = [
            entry("a", "API", "E:\\p", None),
            entry("b", "UI", "E:\\p", None),
        ];
        let sessions = [live("UI", "e:/P/", Some("s9"))];
        assert_eq!(match_entries(&entries, &sessions), vec![Some(1)]);
    }

    #[test]
    fn the_only_pair_in_a_folder_matches_by_folder() {
        let entries = [entry("a", "Old name", "E:\\p", Some("gone"))];
        let sessions = [live("New name", "E:\\p", Some("s2"))];
        assert_eq!(match_entries(&entries, &sessions), vec![Some(0)]);
    }

    #[test]
    fn a_folder_alone_does_not_pick_between_two() {
        let entries = [
            entry("a", "x", "E:\\p", None),
            entry("b", "y", "E:\\p", None),
        ];
        let sessions = [live("z", "E:\\p", None)];
        assert_eq!(match_entries(&entries, &sessions), vec![None]);
        let entries = [entry("a", "x", "E:\\p", None)];
        let sessions = [live("y", "E:\\p", None), live("z", "E:\\p", None)];
        assert_eq!(match_entries(&entries, &sessions), vec![None, None]);
    }

    #[test]
    fn another_agent_in_the_folder_does_not_match() {
        let mut e = entry("a", "x", "E:\\p", None);
        e.agent = AgentKind::Codex;
        assert_eq!(match_entries(&[e], &[live("x", "E:\\p", None)]), vec![None]);
    }

    #[test]
    fn folder_keys_ignore_case_and_separators_on_windows_paths_only() {
        assert_eq!(folder_key(Path::new("E:\\Code\\App\\")), "e:/code/app");
        assert_eq!(folder_key(Path::new("e:/code/app")), "e:/code/app");
        assert_eq!(folder_key(Path::new("/Home/x/")), "/Home/x");
    }

    #[test]
    fn the_console_finds_the_window_whatever_its_title() {
        let windows = [window(7, "something else"), window(8, "title")];
        let mut session = live("title", "E:\\p", None);
        session.pids = vec![100, 200];
        let owners = HashMap::from([(200, 7)]);
        assert_eq!(
            window_of(&session, None, &windows, &owners).map(|w| w.id),
            Some(7)
        );
    }

    #[test]
    fn without_a_console_only_a_unique_exact_title_counts() {
        let windows = [window(7, "title"), window(8, "title"), window(9, "other")];
        let session = live("title", "E:\\p", None);
        let none = HashMap::new();
        assert_eq!(window_of(&session, None, &windows, &none), None);
        assert_eq!(
            window_of(&session, Some("other"), &windows, &none).map(|w| w.id),
            Some(9)
        );
        assert_eq!(
            window_of(&live("nope", "x", None), None, &windows, &none),
            None
        );
    }
}
