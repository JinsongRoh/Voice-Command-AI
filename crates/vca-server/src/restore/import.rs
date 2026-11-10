//! Import of a `layout.json` kept by hand or by the old restore script:
//! `{ delaySeconds, windows: [{ title, dir, x, y, w, h }] }`, physical
//! pixels. Titles and rectangles are kept; the agent is Claude Code (that
//! file knows nothing else) and the conversation the latest in the folder,
//! until the snapshot finds the running session.

use std::path::PathBuf;

use serde::Deserialize;
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, Error, MAX_RESTORE_DELAY_SECS, RestoreEntry, RestoreEntrySource, RestoreMonitor,
    RestoreProfile, UnixMillis, WindowRect,
};

use super::capture::next_order;
use super::geometry::monitor_for_rect;
use super::validate::{absolute_folder, checked_title};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutFile {
    #[serde(default)]
    delay_seconds: Option<u32>,
    #[serde(default)]
    windows: Vec<LayoutWindow>,
}

#[derive(Debug, Deserialize)]
struct LayoutWindow {
    title: String,
    dir: PathBuf,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::InvalidRequest, message)
}

/// Adds the layout to `profile`. A window whose title is on the list
/// updates that entry, so importing twice changes nothing. Returns how many
/// windows the file had.
pub fn import_layout(
    profile: &mut RestoreProfile,
    text: &str,
    monitors: &[RestoreMonitor],
    now: UnixMillis,
) -> Result<usize, Error> {
    // PowerShell writes UTF-8 with a byte order mark.
    let text = text.trim_start_matches('\u{feff}');
    let layout: LayoutFile =
        serde_json::from_str(text).map_err(|e| invalid(format!("not a layout file: {e}")))?;
    if layout.windows.is_empty() {
        return Err(invalid("the layout file has no windows"));
    }
    if let Some(delay) = layout.delay_seconds {
        profile.delay_secs = delay.min(MAX_RESTORE_DELAY_SECS);
    }
    let count = layout.windows.len();
    for window in layout.windows {
        let title = checked_title(&window.title)?;
        let folder = absolute_folder(&window.dir)?;
        if window.w <= 0 || window.h <= 0 {
            return Err(invalid(format!("{title}: the window has no size")));
        }
        let rect = WindowRect {
            x: window.x,
            y: window.y,
            w: window.w,
            h: window.h,
        };
        let monitor = monitor_for_rect(&rect, monitors).cloned();
        if let Some(entry) = profile.entries.iter_mut().find(|e| e.title == title) {
            if entry.rect != Some(rect) || entry.folder != folder || entry.monitor != monitor {
                entry.rect = Some(rect);
                entry.folder = folder;
                entry.monitor = monitor;
                entry.updated_at = now;
            }
            continue;
        }
        let order = next_order(profile);
        profile.entries.push(RestoreEntry {
            id: format!("e{}", uuid::Uuid::new_v4().simple()),
            title,
            agent: AgentKind::ClaudeCode,
            folder,
            session_id: None,
            rect: Some(rect),
            monitor,
            maximized: false,
            enabled: true,
            order,
            args: Vec::new(),
            transcript: None,
            note: None,
            source: RestoreEntrySource::Imported,
            updated_at: now,
        });
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the owner's file, with invented folders and titles.
    const FIXTURE: &str = r#"{
        "_note": "measured at 150 %",
        "delaySeconds": 8,
        "windows": [
            {"title": "Alpha (main)", "dir": "E:\\Alpha", "x": 3831, "y": 1033, "w": 1938, "h": 1053},
            {"title": "Beta", "dir": "D:\\Beta", "x": 1899, "y": -2160, "w": 1938, "h": 1053},
            {"title": "감마 발표자료", "dir": "E:\\Gamma", "x": -1929, "y": 0, "w": 1938, "h": 1053}
        ]
    }"#;

    fn empty() -> RestoreProfile {
        RestoreProfile {
            id: "p".into(),
            name: "Work".into(),
            delay_secs: 6,
            auto_prompt: None,
            entries: Vec::new(),
        }
    }

    #[test]
    fn imports_titles_folders_and_rects() {
        let mut profile = empty();
        let count = import_layout(&mut profile, FIXTURE, &[], 5).unwrap();
        assert_eq!(count, 3);
        assert_eq!(profile.delay_secs, 8);
        let e = &profile.entries[1];
        assert_eq!(e.title, "Beta");
        assert_eq!(e.folder, PathBuf::from("D:\\Beta"));
        assert_eq!(
            e.rect,
            Some(WindowRect {
                x: 1899,
                y: -2160,
                w: 1938,
                h: 1053
            })
        );
        assert_eq!(e.agent, AgentKind::ClaudeCode);
        assert_eq!(e.session_id, None);
        assert_eq!(e.source, RestoreEntrySource::Imported);
        assert_eq!(profile.entries[2].title, "감마 발표자료");
        let orders: Vec<u32> = profile.entries.iter().map(|e| e.order).collect();
        assert_eq!(orders, [0, 1, 2]);
    }

    #[test]
    fn a_byte_order_mark_is_accepted() {
        let mut profile = empty();
        let text = format!("\u{feff}{FIXTURE}");
        assert_eq!(import_layout(&mut profile, &text, &[], 5).unwrap(), 3);
    }

    #[test]
    fn importing_twice_changes_nothing() {
        let mut profile = empty();
        import_layout(&mut profile, FIXTURE, &[], 5).unwrap();
        let before = profile.clone();
        import_layout(&mut profile, FIXTURE, &[], 9).unwrap();
        assert_eq!(profile, before);
    }

    #[test]
    fn monitors_are_recorded_when_known() {
        let monitor = RestoreMonitor {
            id: "L".into(),
            rect: WindowRect {
                x: -1920,
                y: 0,
                w: 1920,
                h: 1080,
            },
            work: WindowRect {
                x: -1920,
                y: 0,
                w: 1920,
                h: 1040,
            },
            dpi: 144,
        };
        let mut profile = empty();
        import_layout(&mut profile, FIXTURE, std::slice::from_ref(&monitor), 5).unwrap();
        assert_eq!(profile.entries[2].monitor, Some(monitor));
        assert_eq!(profile.entries[0].monitor, None);
    }

    #[test]
    fn bad_files_are_refused() {
        let mut profile = empty();
        for bad in [
            "not json",
            r#"{"windows": []}"#,
            r#"{"windows": [{"title": "", "dir": "E:\\a", "x": 0, "y": 0, "w": 1, "h": 1}]}"#,
            r#"{"windows": [{"title": "a", "dir": "relative", "x": 0, "y": 0, "w": 1, "h": 1}]}"#,
            r#"{"windows": [{"title": "a", "dir": "E:\\a", "x": 0, "y": 0, "w": 0, "h": 1}]}"#,
        ] {
            let err = import_layout(&mut profile, bad, &[], 5).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRequest, "{bad}");
        }
    }
}
