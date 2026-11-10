//! Pure snapshot: folds the live sessions and windows into a profile.
//!
//! Sessions with a terminal window are registered on their own; plain
//! shells, managed sessions without a window and agents started by other
//! apps have none and stay off the list. An entry without a running session
//! keeps its last measurement, unless a window with its exact title exists.
//! Nothing here deletes an entry.

use std::collections::HashMap;

use vca_core::{
    MAX_RESTORE_TITLE_CHARS, RestoreEntry, RestoreEntrySource, RestoreMonitor, RestoreProfile,
    UnixMillis,
};

use super::backend::LiveWindow;
use super::geometry::monitor_for_rect;
use super::matching::{LiveSession, match_entries, window_of};
use super::resume::{BYPASS_NOTE, agent_program, filter_args};

/// Everything the snapshot looks at.
#[derive(Debug, Clone, Copy)]
pub struct Observation<'a> {
    pub live: &'a [LiveSession],
    pub windows: &'a [LiveWindow],
    pub owners: &'a HashMap<u32, u64>,
    pub monitors: &'a [RestoreMonitor],
    pub now: UnixMillis,
    pub auto_register: bool,
}

fn clean_title(title: &str) -> String {
    let title: String = title
        .chars()
        .filter(|c| !vca_core::powershell::is_unsafe_title_char(*c))
        .take(MAX_RESTORE_TITLE_CHARS)
        .collect();
    let title = title.trim();
    if title.is_empty() {
        "Session".to_owned()
    } else {
        title.to_owned()
    }
}

/// `title`, or `title (2)` when an entry already uses it: the fixed title
/// is how the restore finds the window it opened.
fn unique_title(entries: &[RestoreEntry], title: &str) -> String {
    let used = |t: &str| entries.iter().any(|e| e.title == t);
    if !used(title) {
        return title.to_owned();
    }
    (2..)
        .map(|n| format!("{title} ({n})"))
        .find(|t| !used(t))
        .unwrap_or_else(|| title.to_owned())
}

/// Writes the window's rectangle into the entry; `true` when it changed.
fn measure(entry: &mut RestoreEntry, window: &LiveWindow, monitors: &[RestoreMonitor]) -> bool {
    // A minimized window reports a parking position, not its place.
    if window.minimized {
        return false;
    }
    let monitor = monitor_for_rect(&window.rect, monitors).cloned();
    let changed = entry.rect != Some(window.rect)
        || entry.maximized != window.maximized
        || entry.monitor != monitor;
    if changed {
        entry.rect = Some(window.rect);
        entry.maximized = window.maximized;
        entry.monitor = monitor;
    }
    changed
}

/// Copies what the running session knows into the entry; `true` when
/// something changed.
fn absorb(entry: &mut RestoreEntry, session: &LiveSession) -> bool {
    let mut changed = false;
    if session.session_id.is_some() && entry.session_id != session.session_id {
        entry.session_id.clone_from(&session.session_id);
        changed = true;
    }
    if session.custom_name {
        let title = clean_title(&session.title);
        if entry.title != title {
            entry.title = title;
            changed = true;
        }
    }
    if entry.agent != session.agent {
        entry.agent = session.agent.clone();
        changed = true;
    }
    if session.transcript.is_some() && entry.transcript != session.transcript {
        entry.transcript.clone_from(&session.transcript);
        changed = true;
    }
    let (args, bypass) = filter_args(&session.agent, &session.args);
    if entry.args != args {
        entry.args = args;
        changed = true;
    }
    let note = bypass.then(|| BYPASS_NOTE.to_owned());
    if bypass && entry.note != note {
        entry.note = note;
        changed = true;
    }
    changed
}

/// Applies one observation; `true` when the profile changed.
pub fn apply(profile: &mut RestoreProfile, seen: Observation<'_>) -> bool {
    let live = seen.live;
    let matches = match_entries(&profile.entries, live);
    let mut changed = false;
    let mut measured = vec![false; profile.entries.len()];
    for (session, hit) in live.iter().zip(&matches) {
        let Some(index) = *hit else { continue };
        let title = profile.entries[index].title.clone();
        let window = window_of(session, Some(&title), seen.windows, seen.owners);
        let entry = &mut profile.entries[index];
        let mut touched = absorb(entry, session);
        if let Some(window) = window {
            touched |= measure(entry, window, seen.monitors);
            measured[index] = true;
        }
        if touched {
            entry.updated_at = seen.now;
            changed = true;
        }
    }
    // Entries with no session yet: a window with their exact title (what
    // the old script's -Save did).
    for (index, entry) in profile.entries.iter_mut().enumerate() {
        if measured[index] {
            continue;
        }
        let mut same = seen.windows.iter().filter(|w| w.title == entry.title);
        if let (Some(window), None) = (same.next(), same.next()) {
            if measure(entry, window, seen.monitors) {
                entry.updated_at = seen.now;
                changed = true;
            }
        }
    }
    if seen.auto_register {
        for (session, hit) in live.iter().zip(&matches) {
            if hit.is_some() || agent_program(&session.agent).is_none() {
                continue;
            }
            let Some(window) = window_of(session, None, seen.windows, seen.owners) else {
                continue;
            };
            let (args, bypass) = filter_args(&session.agent, &session.args);
            let mut entry = RestoreEntry {
                id: format!("e{}", uuid::Uuid::new_v4().simple()),
                title: unique_title(&profile.entries, &clean_title(&session.title)),
                agent: session.agent.clone(),
                folder: session.folder.clone(),
                session_id: session.session_id.clone(),
                rect: None,
                monitor: None,
                maximized: false,
                enabled: true,
                order: next_order(profile),
                args,
                transcript: session.transcript.clone(),
                note: bypass.then(|| BYPASS_NOTE.to_owned()),
                source: RestoreEntrySource::Auto,
                updated_at: seen.now,
            };
            measure(&mut entry, window, seen.monitors);
            profile.entries.push(entry);
            changed = true;
        }
    }
    changed
}

/// One past the largest `order`.
pub fn next_order(profile: &RestoreProfile) -> u32 {
    profile
        .entries
        .iter()
        .map(|e| e.order + 1)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use vca_core::{AgentKind, WindowRect};

    use super::super::matching::tests::{entry, live, window};
    use super::*;

    fn empty() -> RestoreProfile {
        RestoreProfile {
            id: "p".into(),
            name: "Work".into(),
            delay_secs: 6,
            auto_prompt: None,
            entries: Vec::new(),
        }
    }

    fn seen<'a>(
        live: &'a [LiveSession],
        windows: &'a [LiveWindow],
        owners: &'a HashMap<u32, u64>,
    ) -> Observation<'a> {
        Observation {
            live,
            windows,
            owners,
            monitors: &[],
            now: 5,
            auto_register: true,
        }
    }

    #[test]
    fn a_session_with_a_window_registers_itself() {
        let mut profile = empty();
        let mut session = live("Payments", "E:\\pay", Some("s1"));
        session.pids = vec![10, 20];
        let windows = [window(7, "whatever")];
        let owners = HashMap::from([(20, 7)]);
        assert!(apply(&mut profile, seen(&[session], &windows, &owners)));
        let e = &profile.entries[0];
        assert_eq!(e.title, "Payments");
        assert_eq!(e.session_id.as_deref(), Some("s1"));
        assert_eq!(e.rect, Some(windows[0].rect));
        assert_eq!(e.source, RestoreEntrySource::Auto);
        assert!(e.enabled);
    }

    #[test]
    fn sessions_without_a_window_stay_off_the_list() {
        let mut profile = empty();
        let session = live("desktop app agent", "E:\\x", Some("s1"));
        assert!(!apply(&mut profile, seen(&[session], &[], &HashMap::new())));
        assert!(profile.entries.is_empty());
    }

    #[test]
    fn plain_shells_and_generic_programs_are_not_listed() {
        let mut profile = empty();
        let mut shell = live("pwsh", "E:\\x", None);
        shell.agent = AgentKind::Shell("powershell".into());
        let mut generic = live("tool", "E:\\x", None);
        generic.agent = AgentKind::Generic("tool".into());
        let windows = [window(1, "pwsh"), window(2, "tool")];
        assert!(!apply(
            &mut profile,
            seen(&[shell, generic], &windows, &HashMap::new())
        ));
        assert!(profile.entries.is_empty());
    }

    #[test]
    fn nothing_changes_when_nothing_moved() {
        let mut profile = empty();
        let session = live("Payments", "E:\\pay", Some("s1"));
        let windows = [window(7, "Payments")];
        let owners = HashMap::new();
        assert!(apply(
            &mut profile,
            seen(std::slice::from_ref(&session), &windows, &owners)
        ));
        let before = profile.clone();
        let later = Observation {
            now: 99,
            ..seen(std::slice::from_ref(&session), &windows, &owners)
        };
        assert!(!apply(&mut profile, later));
        assert_eq!(profile, before);
    }

    #[test]
    fn a_moved_window_updates_its_entry() {
        let mut profile = empty();
        profile
            .entries
            .push(entry("a", "Payments", "E:\\pay", None));
        let session = live("Payments", "E:\\pay", Some("s1"));
        let mut w = window(7, "Payments");
        w.rect = WindowRect {
            x: -1929,
            y: 0,
            w: 1938,
            h: 1053,
        };
        w.maximized = true;
        assert!(apply(
            &mut profile,
            seen(&[session], &[w.clone()], &HashMap::new())
        ));
        let e = &profile.entries[0];
        assert_eq!(e.rect, Some(w.rect));
        assert!(e.maximized);
        assert_eq!(e.session_id.as_deref(), Some("s1"));
        assert_eq!(e.updated_at, 5);
    }

    #[test]
    fn a_minimized_window_keeps_the_last_place() {
        let mut profile = empty();
        let mut saved = entry("a", "Payments", "E:\\pay", None);
        let rect = WindowRect {
            x: 100,
            y: 100,
            w: 800,
            h: 600,
        };
        saved.rect = Some(rect);
        profile.entries.push(saved);
        let mut w = window(7, "Payments");
        w.rect = WindowRect {
            x: -32000,
            y: -32000,
            w: 160,
            h: 28,
        };
        w.minimized = true;
        let session = live("Payments", "E:\\pay", None);
        assert!(!apply(
            &mut profile,
            seen(&[session], &[w], &HashMap::new())
        ));
        assert_eq!(profile.entries[0].rect, Some(rect));
    }

    #[test]
    fn the_custom_name_becomes_the_window_title() {
        let mut profile = empty();
        profile
            .entries
            .push(entry("a", "app · Claude Code", "E:\\app", Some("s1")));
        let mut session = live("Billing API", "E:\\app", Some("s1"));
        session.custom_name = false;
        assert!(!apply(
            &mut profile,
            seen(std::slice::from_ref(&session), &[], &HashMap::new())
        ));
        assert_eq!(profile.entries[0].title, "app · Claude Code");
        session.custom_name = true;
        assert!(apply(&mut profile, seen(&[session], &[], &HashMap::new())));
        assert_eq!(profile.entries[0].title, "Billing API");
    }

    #[test]
    fn two_sessions_with_one_name_get_distinct_titles() {
        let mut profile = empty();
        let mut a = live("app", "E:\\app", Some("s1"));
        a.pids = vec![1];
        let mut b = live("app", "E:\\app", Some("s2"));
        b.pids = vec![2];
        let windows = [window(7, "x"), window(8, "y")];
        let owners = HashMap::from([(1, 7), (2, 8)]);
        apply(&mut profile, seen(&[a, b], &windows, &owners));
        let titles: Vec<&str> = profile.entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["app", "app (2)"]);
    }

    #[test]
    fn an_entry_without_a_session_is_measured_by_its_exact_title() {
        let mut profile = empty();
        profile.entries.push(entry("a", "acme-pdf", "E:\\p", None));
        let windows = [window(7, "acme-pdf")];
        assert!(apply(&mut profile, seen(&[], &windows, &HashMap::new())));
        assert_eq!(profile.entries[0].rect, Some(windows[0].rect));
    }

    #[test]
    fn a_bypass_flag_is_dropped_with_a_note() {
        let mut profile = empty();
        let mut session = live("yolo", "E:\\p", Some("s1"));
        session.args = vec![
            "--dangerously-skip-permissions".into(),
            "--model".into(),
            "opus".into(),
        ];
        session.pids = vec![1];
        let windows = [window(7, "yolo")];
        let owners = HashMap::from([(1, 7)]);
        apply(&mut profile, seen(&[session], &windows, &owners));
        let e = &profile.entries[0];
        assert_eq!(e.args, ["--model", "opus"]);
        assert_eq!(e.note.as_deref(), Some(BYPASS_NOTE));
    }

    #[test]
    fn auto_register_can_be_turned_off() {
        let mut profile = empty();
        let session = live("x", "E:\\p", None);
        let windows = [window(7, "x")];
        let owners = HashMap::new();
        let off = Observation {
            auto_register: false,
            ..seen(std::slice::from_ref(&session), &windows, &owners)
        };
        assert!(!apply(&mut profile, off));
        assert!(profile.entries.is_empty());
    }

    #[test]
    fn titles_lose_control_characters() {
        assert_eq!(clean_title("a\u{7}b\n"), "ab");
        assert_eq!(clean_title("   "), "Session");
        assert_eq!(clean_title("a\u{202E}b\u{200B}c"), "abc");
        assert_eq!(clean_title("Bob\u{2019}s fix"), "Bob\u{2019}s fix");
    }
}
