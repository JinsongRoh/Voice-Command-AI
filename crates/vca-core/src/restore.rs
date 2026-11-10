//! Session restore: the managed list of AI CLI windows of a computer and
//! the wire types to view, edit and run it (`RestoreList`, `RestoreEdit`,
//! `RestoreRun`, ...), and the screen layouts that arrange open windows in a
//! grid (`ArrangeList`, `ArrangeApply`, `ArrangeUndo`, ...).
//!
//! Coordinates are physical pixels of the virtual screen. They may be
//! negative (monitors left of or above the primary one).

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::session::{AgentKind, SessionId};
use crate::{UnixMillis, session::MAX_SESSION_NAME_CHARS};

/// Longest window title accepted, in characters.
pub const MAX_RESTORE_TITLE_CHARS: usize = MAX_SESSION_NAME_CHARS;
/// Longest profile name accepted, in characters.
pub const MAX_RESTORE_PROFILE_CHARS: usize = 40;
/// Longest "type after restore" message accepted, in characters.
pub const MAX_RESTORE_PROMPT_CHARS: usize = 200;
/// Largest pause between two windows, in seconds.
pub const MAX_RESTORE_DELAY_SECS: u32 = 120;
/// The default pause between two windows, in seconds.
pub const DEFAULT_RESTORE_DELAY_SECS: u32 = 6;

/// Most rows of a screen layout grid.
pub const MAX_ARRANGE_ROWS: u32 = 6;
/// Most columns of a screen layout grid.
pub const MAX_ARRANGE_COLS: u32 = 8;
/// Most saved screen layouts on a computer.
pub const MAX_ARRANGE_LAYOUTS: usize = 50;
/// Longest screen layout name accepted, in characters.
pub const MAX_ARRANGE_LAYOUT_CHARS: usize = 40;

/// A window rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// One monitor as the OS reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreMonitor {
    /// OS device name (for example `\\.\DISPLAY1`).
    pub id: String,
    /// The whole monitor.
    pub rect: WindowRect,
    /// The monitor without the task bar.
    pub work: WindowRect,
    /// 96 = 100 %, 144 = 150 %.
    pub dpi: u32,
}

/// How an entry got on the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreEntrySource {
    /// Found by the snapshot: a running agent session with a terminal window.
    Auto,
    Manual,
    /// From a `layout.json`.
    Imported,
}

/// One saved AI CLI window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreEntry {
    pub id: String,
    /// The fixed window title (the session's VCA name).
    pub title: String,
    pub agent: AgentKind,
    pub folder: PathBuf,
    /// The agent's own session id, resumed exactly when known.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub rect: Option<WindowRect>,
    /// The monitor `rect` was on when it was measured.
    #[serde(default)]
    pub monitor: Option<RestoreMonitor>,
    #[serde(default)]
    pub maximized: bool,
    pub enabled: bool,
    /// Restore order, lowest first.
    pub order: u32,
    /// Allow-listed agent arguments captured from the running session.
    #[serde(default)]
    pub args: Vec<String>,
    /// Where the agent keeps the conversation: when the file is gone, the
    /// restore falls back to the agent's latest conversation in the folder.
    #[serde(default)]
    pub transcript: Option<PathBuf>,
    /// Shown to the user (for example a flag that was dropped).
    #[serde(default)]
    pub note: Option<String>,
    pub source: RestoreEntrySource,
    pub updated_at: UnixMillis,
}

/// A named list of windows ("Work", "Home").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreProfile {
    pub id: String,
    pub name: String,
    /// Pause after each opened window, so memory use does not spike.
    pub delay_secs: u32,
    /// Off by default: sent to each restored agent as its first message, so
    /// it starts work unattended and spends the user's quota. Only the
    /// computer itself can set it.
    #[serde(default)]
    pub auto_prompt: Option<String>,
    #[serde(default)]
    pub entries: Vec<RestoreEntry>,
}

/// What became of one window in a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreItemStatus {
    Waiting,
    Opening,
    Placed,
    /// Opened, but not put in its place: the platform cannot move windows
    /// (Wayland, X11 without wmctrl or xdotool, tmux) or the entry has no
    /// saved position. `message` says which.
    Opened,
    SkippedOpen,
    SkippedMissing,
    /// Remote runs only: the folder is outside the workspace roots.
    SkippedOutside,
    Failed,
}

/// How the conversation is reopened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreResume {
    /// The exact conversation, by session id.
    Exact,
    /// The latest conversation in the folder (id unknown or transcript gone).
    Latest,
    /// A new conversation: the agent has no resume command.
    Fresh,
}

/// One window of a restore run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreItem {
    pub entry_id: String,
    pub title: String,
    pub folder: PathBuf,
    pub status: RestoreItemStatus,
    pub resume: RestoreResume,
    /// The agent command, for display.
    pub command: String,
    #[serde(default)]
    pub rect: Option<WindowRect>,
    /// Pause after this window, in seconds.
    #[serde(default)]
    pub delay_after_secs: u32,
    /// Why the item failed or was skipped, or what was adjusted.
    #[serde(default)]
    pub message: Option<String>,
}

/// A restore run (or, with `dry_run`, its plan).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreJob {
    pub id: String,
    pub profile: String,
    /// `local` or the device name.
    pub by: String,
    pub dry_run: bool,
    pub started_at: UnixMillis,
    #[serde(default)]
    pub finished_at: Option<UnixMillis>,
    pub items: Vec<RestoreItem>,
}

/// Whether an entry's window is open right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreEntryLive {
    pub entry_id: String,
    pub open: bool,
    /// The VCA session it belongs to, when known.
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// Everything the Windows menu shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOverview {
    /// Window placement works on this computer.
    pub supported: bool,
    /// `windows`, `macos`, `linux`, ...
    pub platform: String,
    /// Why it does not, or what is limited.
    #[serde(default)]
    pub message: Option<String>,
    /// 0 when the timed snapshot is off.
    pub snapshot_interval_secs: u32,
    #[serde(default)]
    pub last_snapshot_at: Option<UnixMillis>,
    pub active_profile: String,
    pub profiles: Vec<RestoreProfile>,
    pub live: Vec<RestoreEntryLive>,
    pub monitors: Vec<RestoreMonitor>,
    /// The current or last run.
    #[serde(default)]
    pub job: Option<RestoreJob>,
}

/// Starts a restore ([`crate::ClientMsg::RestoreRun`]).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreRunRequest {
    /// Profile id or name; the active profile when `None`.
    #[serde(default)]
    pub profile: Option<String>,
    /// Keep only entries whose title contains one of these (`--only`).
    #[serde(default)]
    pub only: Vec<String>,
    /// Keep only these entries ("restore selected"). They run even when
    /// disabled: the user picked them.
    #[serde(default)]
    pub entry_ids: Vec<String>,
    /// Plan only: nothing opens, no code is needed.
    #[serde(default)]
    pub dry_run: bool,
    /// A fresh TOTP code. Remote runs need it unless the device is inside
    /// its step-up window.
    #[serde(default)]
    pub totp: Option<String>,
}

impl fmt::Debug for RestoreRunRequest {
    /// Never prints the TOTP code.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestoreRunRequest")
            .field("profile", &self.profile)
            .field("only", &self.only)
            .field("entry_ids", &self.entry_ids)
            .field("dry_run", &self.dry_run)
            .field("totp", &self.totp.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// The editable fields of an entry. Agent arguments are not among them:
/// they only come from a running session, through the allow-list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreEntryDraft {
    pub title: String,
    pub agent: AgentKind,
    pub folder: PathBuf,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub rect: Option<WindowRect>,
    #[serde(default)]
    pub maximized: bool,
    pub enabled: bool,
}

/// One change to the managed list. Removing an entry never closes a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RestoreEdit {
    ProfileAdd {
        name: String,
    },
    ProfileRename {
        profile: String,
        name: String,
    },
    ProfileDelete {
        profile: String,
    },
    /// Which profile the snapshot and "restore" use by default.
    ProfileActivate {
        profile: String,
    },
    /// `auto_prompt` can only be changed from the computer itself.
    ProfileSettings {
        profile: String,
        delay_secs: u32,
        #[serde(default)]
        auto_prompt: Option<String>,
    },
    EntryAdd {
        profile: String,
        entry: RestoreEntryDraft,
    },
    EntryUpdate {
        profile: String,
        entry_id: String,
        entry: RestoreEntryDraft,
    },
    EntryDelete {
        profile: String,
        entry_id: String,
    },
    EntrySetEnabled {
        profile: String,
        entry_id: String,
        enabled: bool,
    },
    /// The profile's entries in their new order.
    EntryReorder {
        profile: String,
        entry_ids: Vec<String>,
    },
}

impl RestoreEdit {
    /// Stable name for the audit log.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ProfileAdd { .. } => "profile_add",
            Self::ProfileRename { .. } => "profile_rename",
            Self::ProfileDelete { .. } => "profile_delete",
            Self::ProfileActivate { .. } => "profile_activate",
            Self::ProfileSettings { .. } => "profile_settings",
            Self::EntryAdd { .. } => "entry_add",
            Self::EntryUpdate { .. } => "entry_update",
            Self::EntryDelete { .. } => "entry_delete",
            Self::EntrySetEnabled { .. } => "entry_set_enabled",
            Self::EntryReorder { .. } => "entry_reorder",
        }
    }
}

/// A session running in an open terminal window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeSession {
    pub id: SessionId,
    pub title: String,
    pub agent: AgentKind,
    pub folder: PathBuf,
}

/// An open terminal window. One window can host several sessions as tabs; it
/// moves as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeWindow {
    /// Names this window until it closes: only meaningful in a request
    /// answered from the current window list, never stored.
    pub id: String,
    pub title: String,
    /// The monitor holding most of it.
    #[serde(default)]
    pub monitor: Option<String>,
    pub maximized: bool,
    pub minimized: bool,
    /// At least one is an AI agent: the tab lists no other window (the
    /// server's `arrangeable`).
    pub sessions: Vec<ArrangeSession>,
}

/// One cell of a grid: `cell` counts row by row from the top left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeCell {
    pub cell: u32,
    /// [`ArrangeWindow::id`].
    pub window: String,
}

/// The grid of one monitor, as the person laid it out. The server works out
/// the rectangles; a client never sends one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeMonitorPlan {
    /// [`RestoreMonitor::id`].
    pub monitor: String,
    pub rows: u32,
    pub cols: u32,
    pub cells: Vec<ArrangeCell>,
}

/// What a saved layout remembers of a session. Window handles change, so a
/// layout never holds one: it finds the window again the way a restore finds
/// a session (session id, then folder and title).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeWindowKey {
    pub agent: AgentKind,
    pub folder: PathBuf,
    #[serde(default)]
    pub session_id: Option<String>,
    pub title: String,
}

/// A cell of a saved layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeSavedCell {
    pub cell: u32,
    /// The window title when saved: the fallback for a window with no
    /// session, and what a report calls it.
    pub title: String,
    /// One per session of the window (tabs).
    #[serde(default)]
    pub sessions: Vec<ArrangeWindowKey>,
}

/// The grid of one monitor in a saved layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeSavedMonitor {
    pub monitor: String,
    pub rows: u32,
    pub cols: u32,
    pub cells: Vec<ArrangeSavedCell>,
}

/// A named screen layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeLayout {
    pub id: String,
    pub name: String,
    pub saved_at: UnixMillis,
    pub monitors: Vec<ArrangeSavedMonitor>,
}

/// What became of one window of an arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrangeItemStatus {
    Moved,
    /// No such open terminal window: closed, or never one.
    SkippedNotOpen,
    /// Its monitor is not connected.
    SkippedMonitor,
    /// Another cell already holds the same window.
    SkippedDuplicate,
    /// `message` says why.
    Failed,
}

/// One window of an arrangement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeItem {
    /// Empty for a window the server does not know.
    pub title: String,
    pub status: ArrangeItemStatus,
    #[serde(default)]
    pub message: Option<String>,
}

/// The report of an arrangement. Windows that could not be moved are part of
/// the report, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeResult {
    pub moved: u32,
    pub skipped: u32,
    pub failed: u32,
    pub items: Vec<ArrangeItem>,
}

/// Everything the screen layout tab shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangeOverview {
    /// Windows can be moved on this computer.
    pub supported: bool,
    /// Why they cannot.
    #[serde(default)]
    pub message: Option<String>,
    pub monitors: Vec<RestoreMonitor>,
    pub windows: Vec<ArrangeWindow>,
    pub layouts: Vec<ArrangeLayout>,
    /// How many windows `ArrangeUndo` would put back; 0 when there is
    /// nothing to undo.
    pub undo_windows: u32,
    /// For each saved layout (by [`ArrangeLayout::id`]), its grids with the
    /// windows of [`Self::windows`] open now, for the editor to load: cells
    /// whose window is not found and monitors not connected are left out.
    /// Worked out for each answer, never saved.
    #[serde(default)]
    pub layout_plans: BTreeMap<String, Vec<ArrangeMonitorPlan>>,
}

/// One change to the saved screen layouts. Nothing here moves a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ArrangeEdit {
    /// Saves the plan under a new name. The server turns each window into
    /// its stable identity.
    LayoutSave {
        name: String,
        monitors: Vec<ArrangeMonitorPlan>,
    },
    LayoutRename {
        layout: String,
        name: String,
    },
    LayoutDelete {
        layout: String,
    },
}

impl ArrangeEdit {
    /// Stable name for the audit log.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::LayoutSave { .. } => "layout_save",
            Self::LayoutRename { .. } => "layout_rename",
            Self::LayoutDelete { .. } => "layout_delete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrange_shapes_survive_the_wire() {
        let edit = ArrangeEdit::LayoutRename {
            layout: "l1".into(),
            name: "Morning".into(),
        };
        let json = serde_json::to_value(&edit).unwrap();
        assert_eq!(json["op"], "layout_rename");
        assert_eq!(edit.kind(), "layout_rename");
        let bytes = rmp_serde::to_vec_named(&edit).unwrap();
        assert_eq!(rmp_serde::from_slice::<ArrangeEdit>(&bytes).unwrap(), edit);
        let status = serde_json::to_value(ArrangeItemStatus::SkippedNotOpen).unwrap();
        assert_eq!(status, "skipped_not_open");
    }

    #[test]
    fn overview_layout_plans_survive_the_wire_and_default_to_empty() {
        let plan = ArrangeMonitorPlan {
            monitor: "A".into(),
            rows: 1,
            cols: 2,
            cells: vec![ArrangeCell {
                cell: 1,
                window: "7".into(),
            }],
        };
        let overview = ArrangeOverview {
            supported: true,
            message: None,
            monitors: Vec::new(),
            windows: vec![ArrangeWindow {
                id: "7".into(),
                title: "API".into(),
                monitor: Some("A".into()),
                maximized: false,
                minimized: false,
                sessions: vec![ArrangeSession {
                    id: SessionId::from("vca-1"),
                    title: "api".into(),
                    agent: AgentKind::ClaudeCode,
                    folder: PathBuf::from("E:\\a"),
                }],
            }],
            layouts: Vec::new(),
            undo_windows: 0,
            layout_plans: BTreeMap::from([("l1".to_owned(), vec![plan])]),
        };
        let json = serde_json::to_value(&overview).unwrap();
        assert_eq!(json["layout_plans"]["l1"][0]["cells"][0]["window"], "7");
        let bytes = rmp_serde::to_vec_named(&overview).unwrap();
        assert_eq!(
            rmp_serde::from_slice::<ArrangeOverview>(&bytes).unwrap(),
            overview
        );
        // An older server does not send it.
        let old = r#"{"supported":true,"message":null,"monitors":[],"windows":[],
            "layouts":[],"undo_windows":0}"#;
        let back: ArrangeOverview = serde_json::from_str(old).unwrap();
        assert!(back.layout_plans.is_empty());
    }

    #[test]
    fn run_request_debug_hides_the_code() {
        let req = RestoreRunRequest {
            totp: Some("123456".into()),
            ..RestoreRunRequest::default()
        };
        assert!(!format!("{req:?}").contains("123456"));
    }

    #[test]
    fn run_request_fields_are_optional() {
        let req: RestoreRunRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(req, RestoreRunRequest::default());
    }

    #[test]
    fn edit_shape_is_tagged_by_op() {
        let edit = RestoreEdit::EntrySetEnabled {
            profile: "p".into(),
            entry_id: "e".into(),
            enabled: false,
        };
        let json = serde_json::to_value(&edit).unwrap();
        assert_eq!(json["op"], "entry_set_enabled");
        assert_eq!(edit.kind(), "entry_set_enabled");
        let bytes = rmp_serde::to_vec_named(&edit).unwrap();
        assert_eq!(rmp_serde::from_slice::<RestoreEdit>(&bytes).unwrap(), edit);
    }

    #[test]
    fn opened_but_not_placed_has_its_own_name() {
        let json = serde_json::to_value(RestoreItemStatus::Opened).unwrap();
        assert_eq!(json, "opened");
        let back: RestoreItemStatus = serde_json::from_str("\"opened\"").unwrap();
        assert_eq!(back, RestoreItemStatus::Opened);
    }

    #[test]
    fn negative_coordinates_survive_the_wire() {
        let rect = WindowRect {
            x: -1929,
            y: -2160,
            w: 1938,
            h: 1053,
        };
        let bytes = rmp_serde::to_vec_named(&rect).unwrap();
        assert_eq!(rmp_serde::from_slice::<WindowRect>(&bytes).unwrap(), rect);
    }
}
