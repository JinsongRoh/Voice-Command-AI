//! The dry-run planner: what a restore would do, as pure data. Every skip
//! (already open, folder missing, outside the workspace) is decided here;
//! the job runner only carries out the items left `waiting`.

use std::collections::HashSet;
use std::path::Path;

use vca_core::{
    RestoreEntry, RestoreEntryLive, RestoreItem, RestoreItemStatus, RestoreMonitor, RestoreProfile,
    RestoreResume, WindowRect,
};

use super::backend::{LiveWindow, OpenRequest};
use super::geometry::{Placement, resolve_rect};
use super::matching::{LiveSession, match_entries};
use super::resume::resume_command;

/// What the file system says about an entry's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderCheck {
    Ok,
    Missing,
    /// Remote runs: outside the workspace roots.
    Outside,
}

/// Which windows of a profile to restore.
#[derive(Debug, Clone, Copy, Default)]
pub struct Selection<'a> {
    /// Keep titles containing one of these, ignoring case (`--only`).
    pub only: &'a [String],
    /// Keep these entries; they run even when disabled.
    pub entry_ids: &'a [String],
}

/// One planned window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWindow {
    pub item: RestoreItem,
    /// `Some` for items that will open.
    pub open: Option<OpenRequest>,
    /// Where it goes, and whether maximized.
    pub place: Option<(WindowRect, bool)>,
}

/// What the machine looks like right now.
#[derive(Debug, Clone, Copy)]
pub struct Situation<'a> {
    pub live: &'a [LiveSession],
    pub windows: &'a [LiveWindow],
    pub monitors: &'a [RestoreMonitor],
}

/// Whether each entry is open: its session runs, or a window has its exact
/// title. In entry order.
pub fn live_status(entries: &[RestoreEntry], now: Situation<'_>) -> Vec<RestoreEntryLive> {
    let matches = match_entries(entries, now.live);
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let session = matches
                .iter()
                .position(|hit| *hit == Some(index))
                .map(|s| now.live[s].id.clone());
            RestoreEntryLive {
                entry_id: entry.id.clone(),
                open: session.is_some() || now.windows.iter().any(|w| w.title == entry.title),
                session_id: session,
            }
        })
        .collect()
}

fn selected(entry: &RestoreEntry, pick: &Selection<'_>) -> bool {
    if !pick.entry_ids.is_empty() {
        return pick.entry_ids.contains(&entry.id);
    }
    if !entry.enabled {
        return false;
    }
    if pick.only.is_empty() {
        return true;
    }
    let title = entry.title.to_lowercase();
    pick.only.iter().any(|o| title.contains(&o.to_lowercase()))
}

fn item(entry: &RestoreEntry, status: RestoreItemStatus, resume: RestoreResume) -> RestoreItem {
    RestoreItem {
        entry_id: entry.id.clone(),
        title: entry.title.clone(),
        folder: entry.folder.clone(),
        status,
        resume,
        command: String::new(),
        rect: None,
        delay_after_secs: 0,
        message: None,
    }
}

/// Plans a restore of `profile`, in list order. `folder` and
/// `transcript_exists` look at the file system.
pub fn plan(
    profile: &RestoreProfile,
    pick: Selection<'_>,
    now: Situation<'_>,
    folder: &dyn Fn(&Path) -> FolderCheck,
    transcript_exists: &dyn Fn(&Path) -> bool,
) -> Vec<PlannedWindow> {
    let live = live_status(&profile.entries, now);
    let open_ids: HashSet<&str> = live
        .iter()
        .filter(|l| l.open)
        .map(|l| l.entry_id.as_str())
        .collect();
    let mut entries: Vec<&RestoreEntry> = profile
        .entries
        .iter()
        .filter(|e| selected(e, &pick))
        .collect();
    entries.sort_by_key(|e| e.order);
    let prompt = profile.auto_prompt.as_deref();

    let mut out: Vec<PlannedWindow> = Vec::with_capacity(entries.len());
    for entry in entries {
        let transcript_ok = entry.transcript.as_deref().is_none_or(transcript_exists);
        let Some(command) = resume_command(
            &entry.agent,
            entry.session_id.as_deref(),
            transcript_ok,
            &entry.args,
            prompt,
        ) else {
            let mut failed = item(entry, RestoreItemStatus::Failed, RestoreResume::Fresh);
            failed.message = Some("this agent cannot be restored".into());
            out.push(PlannedWindow {
                item: failed,
                open: None,
                place: None,
            });
            continue;
        };
        let status = if open_ids.contains(entry.id.as_str()) {
            RestoreItemStatus::SkippedOpen
        } else {
            match folder(&entry.folder) {
                FolderCheck::Ok => RestoreItemStatus::Waiting,
                FolderCheck::Missing => RestoreItemStatus::SkippedMissing,
                FolderCheck::Outside => RestoreItemStatus::SkippedOutside,
            }
        };
        let mut planned = item(entry, status, command.mode);
        planned.command = command.display();
        if entry.session_id.is_some() && command.mode == RestoreResume::Latest {
            planned.message = Some("the conversation file is gone: using the latest one".into());
        }
        let mut place = None;
        if let Some(rect) = entry.rect {
            let (rect, placement) = resolve_rect(&rect, entry.monitor.as_ref(), now.monitors);
            planned.rect = Some(rect);
            place = Some((rect, entry.maximized));
            if status == RestoreItemStatus::Waiting && placement != Placement::Exact {
                planned.message = Some(match placement {
                    Placement::Clamped => "its monitor is gone: moved to the nearest one".into(),
                    _ => "its monitor changed: same place on it".into(),
                });
            }
        }
        let open = (status == RestoreItemStatus::Waiting).then(|| OpenRequest {
            entry_id: entry.id.clone(),
            title: entry.title.clone(),
            folder: entry.folder.clone(),
            program: command.program.clone(),
            args: command.args.clone(),
            managed: None,
        });
        out.push(PlannedWindow {
            item: planned,
            open,
            place,
        });
    }
    // The pause separates windows: nothing after the last one that opens.
    let opening: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, p)| p.open.is_some())
        .map(|(i, _)| i)
        .collect();
    if let Some((_, rest)) = opening.split_last() {
        for i in rest {
            out[*i].item.delay_after_secs = profile.delay_secs;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use vca_core::{AgentKind, WindowRect};

    use super::super::matching::tests::{entry, live, window};
    use super::*;

    fn profile(entries: Vec<RestoreEntry>) -> RestoreProfile {
        RestoreProfile {
            id: "p".into(),
            name: "Work".into(),
            delay_secs: 6,
            auto_prompt: None,
            entries: entries
                .into_iter()
                .enumerate()
                .map(|(i, e)| RestoreEntry {
                    order: i as u32,
                    ..e
                })
                .collect(),
        }
    }

    fn nothing_open<'a>() -> Situation<'a> {
        Situation {
            live: &[],
            windows: &[],
            monitors: &[],
        }
    }

    fn all_folders(_: &Path) -> FolderCheck {
        FolderCheck::Ok
    }

    fn all_exist(_: &Path) -> bool {
        true
    }

    fn statuses(items: &[PlannedWindow]) -> Vec<(String, RestoreItemStatus)> {
        items
            .iter()
            .map(|p| (p.item.title.clone(), p.item.status))
            .collect()
    }

    #[test]
    fn plans_in_list_order_with_the_delay_between_windows() {
        let mut p = profile(vec![
            entry("a", "first", "E:\\a", Some("s1")),
            entry("b", "second", "E:\\b", None),
            entry("c", "third", "E:\\c", None),
        ]);
        p.entries[0].order = 2;
        p.entries[2].order = 0;
        let items = plan(
            &p,
            Selection::default(),
            nothing_open(),
            &all_folders,
            &all_exist,
        );
        let titles: Vec<&str> = items.iter().map(|i| i.item.title.as_str()).collect();
        assert_eq!(titles, ["third", "second", "first"]);
        let delays: Vec<u32> = items.iter().map(|i| i.item.delay_after_secs).collect();
        // Nothing to wait for after the last window.
        assert_eq!(delays, [6, 6, 0]);
        assert_eq!(items[2].item.command, "claude --resume s1");
        assert_eq!(items[2].item.resume, RestoreResume::Exact);
        assert_eq!(items[1].item.command, "claude --continue");
    }

    #[test]
    fn a_session_in_a_window_with_another_title_counts_as_open() {
        let p = profile(vec![entry("a", "API", "E:\\a", Some("s1"))]);
        let sessions = [live("renamed", "E:\\a", Some("s1"))];
        let items = plan(
            &p,
            Selection::default(),
            Situation {
                live: &sessions,
                ..nothing_open()
            },
            &all_folders,
            &all_exist,
        );
        assert_eq!(items[0].item.status, RestoreItemStatus::SkippedOpen);
        assert_eq!(items[0].open, None);
    }

    #[test]
    fn a_window_with_the_exact_title_counts_as_open() {
        let p = profile(vec![
            entry("a", "API", "E:\\a", None),
            entry("b", "UI", "E:\\b", None),
        ]);
        let windows = [window(1, "API")];
        let items = plan(
            &p,
            Selection::default(),
            Situation {
                windows: &windows,
                ..nothing_open()
            },
            &all_folders,
            &all_exist,
        );
        assert_eq!(
            statuses(&items),
            [
                ("API".to_owned(), RestoreItemStatus::SkippedOpen),
                ("UI".to_owned(), RestoreItemStatus::Waiting)
            ]
        );
        // The skipped window takes no pause.
        assert_eq!(items[0].item.delay_after_secs, 0);
        assert_eq!(items[1].item.delay_after_secs, 0);
    }

    #[test]
    fn a_missing_folder_is_skipped_and_an_outside_folder_too() {
        let p = profile(vec![
            entry("a", "gone", "E:\\gone", None),
            entry("b", "out", "E:\\out", None),
            entry("c", "here", "E:\\here", None),
        ]);
        let folder = |path: &Path| match path.to_string_lossy().as_ref() {
            "E:\\gone" => FolderCheck::Missing,
            "E:\\out" => FolderCheck::Outside,
            _ => FolderCheck::Ok,
        };
        let items = plan(
            &p,
            Selection::default(),
            nothing_open(),
            &folder,
            &all_exist,
        );
        assert_eq!(
            statuses(&items),
            [
                ("gone".to_owned(), RestoreItemStatus::SkippedMissing),
                ("out".to_owned(), RestoreItemStatus::SkippedOutside),
                ("here".to_owned(), RestoreItemStatus::Waiting),
            ]
        );
    }

    #[test]
    fn only_filters_titles_ignoring_case() {
        let p = profile(vec![
            entry("a", "acme-pdf", "E:\\a", None),
            entry("b", "acme-office", "E:\\b", None),
            entry("c", "harbor", "E:\\c", None),
        ]);
        let only = vec!["PDF".to_owned(), "HAR".to_owned()];
        let items = plan(
            &p,
            Selection {
                only: &only,
                entry_ids: &[],
            },
            nothing_open(),
            &all_folders,
            &all_exist,
        );
        let titles: Vec<&str> = items.iter().map(|i| i.item.title.as_str()).collect();
        assert_eq!(titles, ["acme-pdf", "harbor"]);
    }

    #[test]
    fn disabled_entries_stay_closed_unless_picked() {
        let mut p = profile(vec![
            entry("a", "on", "E:\\a", None),
            entry("b", "off", "E:\\b", None),
        ]);
        p.entries[1].enabled = false;
        let items = plan(
            &p,
            Selection::default(),
            nothing_open(),
            &all_folders,
            &all_exist,
        );
        assert_eq!(items.len(), 1);
        let picked = vec!["b".to_owned()];
        let items = plan(
            &p,
            Selection {
                only: &[],
                entry_ids: &picked,
            },
            nothing_open(),
            &all_folders,
            &all_exist,
        );
        assert_eq!(items[0].item.title, "off");
    }

    #[test]
    fn a_gone_transcript_falls_back_with_a_message() {
        let mut e = entry("a", "API", "E:\\a", Some("s1"));
        e.transcript = Some(PathBuf::from("E:\\t\\s1.jsonl"));
        let items = plan(
            &profile(vec![e]),
            Selection::default(),
            nothing_open(),
            &all_folders,
            &|_| false,
        );
        assert_eq!(items[0].item.resume, RestoreResume::Latest);
        assert_eq!(items[0].item.command, "claude --continue");
        assert!(items[0].item.message.is_some());
    }

    #[test]
    fn generic_entries_never_open() {
        let mut e = entry("a", "tool", "E:\\a", None);
        e.agent = AgentKind::Generic("tool".into());
        let items = plan(
            &profile(vec![e]),
            Selection::default(),
            nothing_open(),
            &all_folders,
            &all_exist,
        );
        assert_eq!(items[0].item.status, RestoreItemStatus::Failed);
        assert_eq!(items[0].open, None);
    }

    #[test]
    fn the_auto_prompt_is_off_unless_the_profile_sets_it() {
        let mut p = profile(vec![entry("a", "API", "E:\\a", Some("s1"))]);
        let items = plan(
            &p,
            Selection::default(),
            nothing_open(),
            &all_folders,
            &all_exist,
        );
        assert_eq!(items[0].open.as_ref().unwrap().args, ["--resume", "s1"]);
        p.auto_prompt = Some("go on".into());
        let items = plan(
            &p,
            Selection::default(),
            nothing_open(),
            &all_folders,
            &all_exist,
        );
        assert_eq!(
            items[0].open.as_ref().unwrap().args,
            ["--resume", "s1", "go on"]
        );
    }

    #[test]
    fn an_off_screen_rect_is_planned_on_the_nearest_monitor() {
        let mut e = entry("a", "API", "E:\\a", None);
        e.rect = Some(WindowRect {
            x: 9000,
            y: 0,
            w: 800,
            h: 600,
        });
        e.maximized = true;
        let monitor = RestoreMonitor {
            id: "A".into(),
            rect: WindowRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            work: WindowRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1040,
            },
            dpi: 144,
        };
        let items = plan(
            &profile(vec![e]),
            Selection::default(),
            Situation {
                monitors: std::slice::from_ref(&monitor),
                ..nothing_open()
            },
            &all_folders,
            &all_exist,
        );
        let (rect, maximized) = items[0].place.unwrap();
        assert!(rect.x + rect.w <= 1920);
        assert!(maximized);
        assert!(items[0].item.message.as_ref().unwrap().contains("nearest"));
    }

    #[test]
    fn live_status_reports_the_session_of_each_entry() {
        let entries = [
            entry("a", "API", "E:\\a", Some("s1")),
            entry("b", "UI", "E:\\b", None),
            entry("c", "Docs", "E:\\c", None),
        ];
        let sessions = [live("x", "E:\\a", Some("s1"))];
        let windows = [window(1, "UI")];
        let status = live_status(
            &entries,
            Situation {
                live: &sessions,
                windows: &windows,
                monitors: &[],
            },
        );
        assert!(status[0].open);
        assert_eq!(status[0].session_id.as_ref().unwrap().0, "vca-x");
        assert!(status[1].open);
        assert_eq!(status[1].session_id, None);
        assert!(!status[2].open);
    }
}
