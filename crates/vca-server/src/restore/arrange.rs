//! Screen layouts: puts already open terminal windows in the cells of a grid,
//! one monitor at a time, a freer Windows Snap for watching many AI CLI
//! windows side by side.
//!
//! The grid is worked out here, in physical pixels inside a monitor's `work`
//! rectangle, from rows and columns only. A client names a window by the id
//! the current window list gave it, and that id is only ever looked up in the
//! live list of terminal windows ([`WindowBackend::windows`]): it never
//! reaches the backend as a handle, and no client sends a rectangle. A window
//! that is not in the list is reported as skipped, so nothing but an open
//! terminal window can move. Nothing is opened, closed, ended or focused.
//!
//! A terminal window hosting several sessions as tabs moves as a whole.
//!
//! Saved layouts live in `restore.json` for its atomic write and backup. They
//! hold no window handle (handles change): a saved cell keeps the sessions of
//! its window and its title, and finds the window again with the matching a
//! restore uses ([`match_entries`], [`window_of`]), then by a unique exact
//! title. What is not open is reported as skipped.
//!
//! The undo memory holds the positions from just before the last arrangement,
//! in memory only: handles do not survive anyway once windows close.
//!
//! The tab offers only the windows with an AI agent in them
//! ([`arrangeable`]): plain terminals and plain shells are left out of the
//! list and of what a saved layout loads, but an arrangement still moves any
//! open terminal window it names, so an undo or an older layout keeps
//! working. Sessions with no window of their own (vcad's pseudo consoles)
//! never are a list item: the backend lists visible terminal windows only,
//! and [`window_of`] does not lend them a window through vcad's ancestors.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ArrangeCell, ArrangeEdit, ArrangeItem, ArrangeItemStatus, ArrangeLayout,
    ArrangeMonitorPlan, ArrangeOverview, ArrangeResult, ArrangeSavedCell, ArrangeSavedMonitor,
    ArrangeSession, ArrangeWindow, ArrangeWindowKey, Error, MAX_ARRANGE_COLS,
    MAX_ARRANGE_LAYOUT_CHARS, MAX_ARRANGE_LAYOUTS, MAX_ARRANGE_ROWS, RestoreEntry,
    RestoreEntrySource, RestoreMonitor, WindowRect, now_millis,
};

use super::backend::{LiveWindow, WindowBackend};
use super::geometry::{clamp_into, monitor_for_rect, resolve_rect};
use super::matching::{LiveSession, match_entries, window_of};
use super::store::RestoreFile;
use super::{Restore, StopCheck, api_error};
use crate::util::{error_text, lock};

/// Most monitors of one plan.
const MAX_PLAN_MONITORS: usize = 16;
/// An invisible border wider than this is a measuring error.
const MAX_FRAME_INSET: i32 = 64;

const STOPPED_NOTE: &str = "stopped by the kill switch";

fn invalid(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::InvalidRequest, message)
}

/// Where the `count` cells along a side of `length` pixels start and end:
/// `count + 1` edges, so neighbours share one and the leftover pixels spread
/// over the cells, at most one each.
fn edges(start: i32, length: i32, count: u32) -> Vec<i32> {
    (0..=i64::from(count))
        .map(|i| start + (i * i64::from(length) / i64::from(count)) as i32)
        .collect()
}

fn check_grid(rows: u32, cols: u32) -> Result<(), Error> {
    if rows == 0 || cols == 0 || rows > MAX_ARRANGE_ROWS || cols > MAX_ARRANGE_COLS {
        return Err(invalid(format!(
            "a grid has 1 to {MAX_ARRANGE_ROWS} rows and 1 to {MAX_ARRANGE_COLS} columns"
        )));
    }
    Ok(())
}

/// The cells of a `rows` by `cols` grid, row by row from the top left. They
/// fill `area` exactly: no gap, no overlap, nothing left over.
pub fn grid(area: &WindowRect, rows: u32, cols: u32) -> Result<Vec<WindowRect>, Error> {
    check_grid(rows, cols)?;
    if i64::from(area.w) < i64::from(cols) || i64::from(area.h) < i64::from(rows) {
        return Err(invalid("the monitor is too small for that grid"));
    }
    let xs = edges(area.x, area.w, cols);
    let ys = edges(area.y, area.h, rows);
    let mut cells = Vec::with_capacity(rows as usize * cols as usize);
    for y in ys.windows(2) {
        for x in xs.windows(2) {
            cells.push(WindowRect {
                x: x[0],
                y: y[0],
                w: x[1] - x[0],
                h: y[1] - y[0],
            });
        }
    }
    Ok(cells)
}

/// The outer rectangle that shows `cell` on screen, for a window measured at
/// `outer` that shows `visible` (its invisible resize borders are the
/// difference). A measurement that makes no sense changes nothing.
pub fn outer_for_visible(
    cell: &WindowRect,
    outer: &WindowRect,
    visible: &WindowRect,
) -> WindowRect {
    let left = visible.x - outer.x;
    let top = visible.y - outer.y;
    let right = (outer.x + outer.w) - (visible.x + visible.w);
    let bottom = (outer.y + outer.h) - (visible.y + visible.h);
    if [left, top, right, bottom]
        .iter()
        .any(|inset| !(0..=MAX_FRAME_INSET).contains(inset))
    {
        return *cell;
    }
    WindowRect {
        x: cell.x - left,
        y: cell.y - top,
        w: cell.w + left + right,
        h: cell.h + top + bottom,
    }
}

/// The shape of a plan, before anything is read: a client mistake is an
/// error, unlike a window that closed in the meantime.
pub fn check_plans(plans: &[ArrangeMonitorPlan]) -> Result<(), Error> {
    if plans.len() > MAX_PLAN_MONITORS {
        return Err(invalid("too many monitors in the plan"));
    }
    let mut monitors = HashSet::new();
    let mut windows = HashSet::new();
    for plan in plans {
        check_grid(plan.rows, plan.cols)?;
        if !monitors.insert(plan.monitor.as_str()) {
            return Err(invalid("a monitor appears twice in the plan"));
        }
        let mut cells = HashSet::new();
        for cell in &plan.cells {
            if cell.cell >= plan.rows * plan.cols {
                return Err(invalid("a cell is outside the grid"));
            }
            if !cells.insert(cell.cell) {
                return Err(invalid("a cell holds two windows"));
            }
            if !windows.insert(cell.window.as_str()) {
                return Err(invalid("a window is in two cells"));
            }
        }
    }
    Ok(())
}

/// Where a window goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A grid cell: this much of the screen should show the window.
    Cell(WindowRect),
    /// Back to an outer rectangle read from the window before.
    Back { rect: WindowRect, maximized: bool },
    /// Back to minimized: it reported no usable rectangle.
    Minimized,
}

/// One window to move, as it is right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub window: LiveWindow,
    pub to: Target,
}

/// A plan checked against the live windows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moves {
    pub moves: Vec<Move>,
    /// Windows that will not move, and why.
    pub skipped: Vec<ArrangeItem>,
}

impl Moves {
    fn skip(&mut self, title: &str, status: ArrangeItemStatus) {
        self.skipped.push(ArrangeItem {
            title: title.to_owned(),
            status,
            message: None,
        });
    }

    /// `window`: found among the live windows. `cell`: the place in a grid
    /// of a connected monitor.
    fn add(
        &mut self,
        taken: &mut HashSet<u64>,
        window: Option<&LiveWindow>,
        cell: Option<(&WindowRect, &RestoreMonitor)>,
        title: &str,
    ) {
        let Some(window) = window else {
            return self.skip(title, ArrangeItemStatus::SkippedNotOpen);
        };
        let Some((rect, monitor)) = cell else {
            return self.skip(&window.title, ArrangeItemStatus::SkippedMonitor);
        };
        if !taken.insert(window.id) {
            return self.skip(&window.title, ArrangeItemStatus::SkippedDuplicate);
        }
        self.moves.push(Move {
            window: window.clone(),
            to: Target::Cell(clamp_into(rect, &monitor.work)),
        });
    }
}

/// The grid of a monitor; `None` when the monitor is not connected.
fn monitor_grid<'a>(
    id: &str,
    rows: u32,
    cols: u32,
    monitors: &'a [RestoreMonitor],
) -> Result<Option<(&'a RestoreMonitor, Vec<WindowRect>)>, Error> {
    monitors
        .iter()
        .find(|m| m.id == id)
        .map(|m| Ok((m, grid(&m.work, rows, cols)?)))
        .transpose()
}

/// A client's id is compared with the live list, never used as a handle.
fn live_window<'a>(id: &str, windows: &'a [LiveWindow]) -> Option<&'a LiveWindow> {
    windows.iter().find(|w| w.id.to_string() == id)
}

/// Turns a plan into moves. Only windows of `windows` move, each once, inside
/// the work area of a connected monitor.
pub fn resolve_plans(
    plans: &[ArrangeMonitorPlan],
    windows: &[LiveWindow],
    monitors: &[RestoreMonitor],
) -> Result<Moves, Error> {
    let mut out = Moves::default();
    let mut taken = HashSet::new();
    for plan in plans {
        let target = monitor_grid(&plan.monitor, plan.rows, plan.cols, monitors)?;
        for cell in &plan.cells {
            let place = target
                .as_ref()
                .and_then(|(m, cells)| Some((cells.get(cell.cell as usize)?, *m)));
            let window = live_window(&cell.window, windows);
            out.add(&mut taken, window, place, "");
        }
    }
    Ok(out)
}

/// For each live session, the title of its list entry, active profile first.
pub fn entry_titles(file: &RestoreFile, live: &[LiveSession]) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = vec![None; live.len()];
    let mut profiles: Vec<_> = file.profiles.iter().collect();
    profiles.sort_by_key(|p| p.id != file.active);
    for profile in profiles {
        let hits = match_entries(&profile.entries, live);
        for (title, hit) in out.iter_mut().zip(hits) {
            if title.is_none() {
                *title = hit
                    .and_then(|e| profile.entries.get(e))
                    .map(|e| e.title.clone());
            }
        }
    }
    out
}

/// The live sessions (their indexes) in each window.
pub fn sessions_by_window(
    live: &[LiveSession],
    titles: &[Option<String>],
    windows: &[LiveWindow],
    owners: &HashMap<u32, u64>,
) -> HashMap<u64, Vec<usize>> {
    let mut out: HashMap<u64, Vec<usize>> = HashMap::new();
    for (index, session) in live.iter().enumerate() {
        let title = titles.get(index).and_then(Option::as_deref);
        if let Some(window) = window_of(session, title, windows, owners) {
            out.entry(window.id).or_default().push(index);
        }
    }
    out
}

/// Whether the tab offers a window: it hosts at least one AI agent session,
/// whoever started it (a VCA restore window is as good as the person's own).
/// Left out: plain terminals (no session found in them) and windows of plain
/// shells only (`AgentKind::Shell`, opened from the app).
pub fn arrangeable(sessions: &[ArrangeSession]) -> bool {
    sessions
        .iter()
        .any(|s| !matches!(s.agent, AgentKind::Shell(_)))
}

/// The window list of the tab: the [`arrangeable`] windows.
pub fn window_list(
    windows: &[LiveWindow],
    monitors: &[RestoreMonitor],
    live: &[LiveSession],
    in_window: &HashMap<u64, Vec<usize>>,
) -> Vec<ArrangeWindow> {
    windows
        .iter()
        .map(|w| ArrangeWindow {
            id: w.id.to_string(),
            title: w.title.clone(),
            monitor: monitor_for_rect(&w.rect, monitors).map(|m| m.id.clone()),
            maximized: w.maximized,
            minimized: w.minimized,
            sessions: in_window
                .get(&w.id)
                .into_iter()
                .flatten()
                .filter_map(|i| live.get(*i))
                .map(|s| ArrangeSession {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    agent: s.agent.clone(),
                    folder: s.folder.clone(),
                })
                .collect(),
        })
        .filter(|w| arrangeable(&w.sessions))
        .collect()
}

/// A plan in the form a layout keeps it: every window by its stable identity.
/// Windows not in the live list and monitors not connected are left out.
pub fn saved_monitors(
    plans: &[ArrangeMonitorPlan],
    monitors: &[RestoreMonitor],
    windows: &[LiveWindow],
    live: &[LiveSession],
    titles: &[Option<String>],
    in_window: &HashMap<u64, Vec<usize>>,
) -> Vec<ArrangeSavedMonitor> {
    let key = |index: &usize| {
        let session = live.get(*index)?;
        let title = titles.get(*index).cloned().flatten();
        Some(ArrangeWindowKey {
            agent: session.agent.clone(),
            folder: session.folder.clone(),
            session_id: session.session_id.clone(),
            title: title.unwrap_or_else(|| session.title.clone()),
        })
    };
    plans
        .iter()
        .filter(|plan| monitors.iter().any(|m| m.id == plan.monitor))
        .map(|plan| ArrangeSavedMonitor {
            monitor: plan.monitor.clone(),
            rows: plan.rows,
            cols: plan.cols,
            cells: plan
                .cells
                .iter()
                .filter_map(|cell| {
                    let window = live_window(&cell.window, windows)?;
                    Some(ArrangeSavedCell {
                        cell: cell.cell,
                        title: window.title.clone(),
                        sessions: in_window
                            .get(&window.id)
                            .into_iter()
                            .flatten()
                            .filter_map(key)
                            .collect(),
                    })
                })
                .collect(),
        })
        .collect()
}

/// A saved session as a list entry, for [`match_entries`].
fn as_entry(id: String, key: &ArrangeWindowKey) -> RestoreEntry {
    RestoreEntry {
        id,
        title: key.title.clone(),
        agent: key.agent.clone(),
        folder: key.folder.clone(),
        session_id: key.session_id.clone(),
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

/// The window of each saved cell, by (monitor index, cell index): its
/// sessions first, then one window with exactly its title.
fn find_layout_windows<'a>(
    layout: &ArrangeLayout,
    live: &[LiveSession],
    windows: &'a [LiveWindow],
    owners: &HashMap<u32, u64>,
) -> HashMap<(usize, usize), &'a LiveWindow> {
    let mut owners_of = Vec::new();
    let mut entries = Vec::new();
    for (m, monitor) in layout.monitors.iter().enumerate() {
        for (c, cell) in monitor.cells.iter().enumerate() {
            for key in &cell.sessions {
                owners_of.push((m, c));
                entries.push(as_entry(format!("{m}.{c}.{}", entries.len()), key));
            }
        }
    }
    let mut found = HashMap::new();
    for (session, hit) in live.iter().zip(match_entries(&entries, live)) {
        let Some(hit) = hit else { continue };
        let title = entries.get(hit).map(|e| e.title.as_str());
        if let (Some(place), Some(window)) = (
            owners_of.get(hit),
            window_of(session, title, windows, owners),
        ) {
            found.entry(*place).or_insert(window);
        }
    }
    let claimed: HashSet<u64> = found.values().map(|w: &&LiveWindow| w.id).collect();
    for (m, monitor) in layout.monitors.iter().enumerate() {
        for (c, cell) in monitor.cells.iter().enumerate() {
            if found.contains_key(&(m, c)) {
                continue;
            }
            let mut same = windows
                .iter()
                .filter(|w| w.title == cell.title && !claimed.contains(&w.id));
            if let (Some(window), None) = (same.next(), same.next()) {
                found.insert((m, c), window);
            }
        }
    }
    found
}

/// Turns a saved layout into moves. The rectangles come from the monitors of
/// today, so a layout survives a new resolution.
pub fn resolve_layout(
    layout: &ArrangeLayout,
    live: &[LiveSession],
    windows: &[LiveWindow],
    owners: &HashMap<u32, u64>,
    monitors: &[RestoreMonitor],
) -> Result<Moves, Error> {
    let found = find_layout_windows(layout, live, windows, owners);
    let mut out = Moves::default();
    let mut taken = HashSet::new();
    for (m, saved) in layout.monitors.iter().enumerate() {
        let target = monitor_grid(&saved.monitor, saved.rows, saved.cols, monitors)?;
        for (c, cell) in saved.cells.iter().enumerate() {
            let place = target
                .as_ref()
                .and_then(|(monitor, cells)| Some((cells.get(cell.cell as usize)?, *monitor)));
            let window = found.get(&(m, c)).copied();
            out.add(&mut taken, window, place, &cell.title);
        }
    }
    Ok(out)
}

/// A saved layout as the editor loads it: the grid of each connected
/// monitor with the windows found open now, by the same matching an apply
/// uses. Cells whose window is not found, or not in `offered` (the window
/// list), are left out, and a window fills one cell at most.
pub fn layout_plan(
    layout: &ArrangeLayout,
    live: &[LiveSession],
    windows: &[LiveWindow],
    owners: &HashMap<u32, u64>,
    monitors: &[RestoreMonitor],
    offered: &HashSet<u64>,
) -> Vec<ArrangeMonitorPlan> {
    let found = find_layout_windows(layout, live, windows, owners);
    let mut taken = HashSet::new();
    let mut out = Vec::new();
    for (m, saved) in layout.monitors.iter().enumerate() {
        if !monitors.iter().any(|monitor| monitor.id == saved.monitor) {
            continue;
        }
        let mut cells = Vec::new();
        for (c, cell) in saved.cells.iter().enumerate() {
            let Some(window) = found.get(&(m, c)) else {
                continue;
            };
            let fits = cell.cell < saved.rows.saturating_mul(saved.cols);
            if fits && offered.contains(&window.id) && taken.insert(window.id) {
                cells.push(ArrangeCell {
                    cell: cell.cell,
                    window: window.id.to_string(),
                });
            }
        }
        out.push(ArrangeMonitorPlan {
            monitor: saved.monitor.clone(),
            rows: saved.rows,
            cols: saved.cols,
            cells,
        });
    }
    out
}

/// A window's place from just before an arrangement. In memory only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoWindow {
    id: u64,
    title: String,
    rect: WindowRect,
    maximized: bool,
    minimized: bool,
}

impl UndoWindow {
    fn of(window: &LiveWindow) -> Self {
        Self {
            id: window.id,
            title: window.title.clone(),
            rect: window.rect,
            maximized: window.maximized,
            minimized: window.minimized,
        }
    }
}

/// The moves that put windows back. A closed window is skipped; a place that
/// is off every monitor of today lands on the nearest one.
pub fn resolve_undo(
    undo: &[UndoWindow],
    windows: &[LiveWindow],
    monitors: &[RestoreMonitor],
) -> Moves {
    let mut out = Moves::default();
    for saved in undo {
        let Some(window) = windows.iter().find(|w| w.id == saved.id) else {
            out.skip(&saved.title, ArrangeItemStatus::SkippedNotOpen);
            continue;
        };
        let to = if saved.minimized {
            Target::Minimized
        } else {
            Target::Back {
                rect: resolve_rect(&saved.rect, None, monitors).0,
                maximized: saved.maximized,
            }
        };
        out.moves.push(Move {
            window: window.clone(),
            to,
        });
    }
    out
}

/// How the moves went.
struct Carried {
    items: Vec<ArrangeItem>,
    moved: Vec<UndoWindow>,
}

/// Moves one window after the other. A failure stays with its window, except
/// that a desktop that cannot move windows at all ends the run with its
/// reason.
fn carry_out(
    backend: &dyn WindowBackend,
    moves: &[Move],
    stop: Option<&StopCheck>,
) -> Result<Carried, Error> {
    let mut items = Vec::with_capacity(moves.len());
    let mut moved = Vec::new();
    for planned in moves {
        let window = &planned.window;
        let done = if stop.is_some_and(|s| s()) {
            Err(api_error(ErrorCode::Forbidden, STOPPED_NOTE))
        } else {
            match planned.to {
                Target::Cell(rect) => backend.place_visible(window.id, rect),
                Target::Back { rect, maximized } => backend.place(window.id, rect, maximized),
                Target::Minimized => backend.minimize(window.id),
            }
        };
        let unsupported = |err: &Error| err.code() == ErrorCode::Unsupported;
        let item = match done {
            Ok(()) => {
                moved.push(UndoWindow::of(window));
                ArrangeItem {
                    title: window.title.clone(),
                    status: ArrangeItemStatus::Moved,
                    message: None,
                }
            }
            Err(err) if unsupported(&err) && !matches!(planned.to, Target::Minimized) => {
                return Err(err);
            }
            Err(err) => ArrangeItem {
                title: window.title.clone(),
                status: ArrangeItemStatus::Failed,
                message: Some(error_text(&err)),
            },
        };
        items.push(item);
    }
    Ok(Carried { items, moved })
}

fn checked_layout_name(name: &str) -> Result<String, Error> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_ARRANGE_LAYOUT_CHARS
        || name.chars().any(vca_core::powershell::is_unsafe_title_char)
    {
        return Err(invalid(format!(
            "a layout name has 1 to {MAX_ARRANGE_LAYOUT_CHARS} characters"
        )));
    }
    Ok(name.to_owned())
}

fn name_taken(file: &RestoreFile, name: &str, except: Option<&str>) -> bool {
    let wanted = name.to_lowercase();
    file.layouts
        .iter()
        .any(|l| Some(l.id.as_str()) != except && l.name.to_lowercase() == wanted)
}

fn layout_mut<'a>(file: &'a mut RestoreFile, id: &str) -> Result<&'a mut ArrangeLayout, Error> {
    file.layouts
        .iter_mut()
        .find(|l| l.id == id)
        .ok_or_else(|| api_error(ErrorCode::NotFound, format!("no layout {id}")))
}

fn conflict(name: &str) -> Error {
    api_error(
        ErrorCode::Conflict,
        format!("another layout is already named {name}"),
    )
}

/// Applies an edit to the layouts. `saved`: what a save stores, made by the
/// caller from the live windows.
fn apply_edit(
    file: &mut RestoreFile,
    edit: &ArrangeEdit,
    saved: Vec<ArrangeSavedMonitor>,
) -> Result<(), Error> {
    match edit {
        ArrangeEdit::LayoutSave { name, .. } => {
            let name = checked_layout_name(name)?;
            if name_taken(file, &name, None) {
                return Err(conflict(&name));
            }
            if file.layouts.len() >= MAX_ARRANGE_LAYOUTS {
                return Err(invalid(format!(
                    "at most {MAX_ARRANGE_LAYOUTS} layouts can be saved"
                )));
            }
            let monitors: Vec<ArrangeSavedMonitor> =
                saved.into_iter().filter(|m| !m.cells.is_empty()).collect();
            if monitors.is_empty() {
                return Err(invalid("the layout has no open window"));
            }
            file.layouts.push(ArrangeLayout {
                id: format!("l{}", uuid::Uuid::new_v4().simple()),
                name,
                saved_at: now_millis(),
                monitors,
            });
        }
        ArrangeEdit::LayoutRename { layout, name } => {
            let name = checked_layout_name(name)?;
            if name_taken(file, &name, Some(layout)) {
                return Err(conflict(&name));
            }
            layout_mut(file, layout)?.name = name;
        }
        ArrangeEdit::LayoutDelete { layout } => {
            layout_mut(file, layout)?;
            file.layouts.retain(|l| &l.id != layout);
        }
    }
    Ok(())
}

impl Restore {
    /// Whether windows can be moved here, and why not.
    fn arrange_blocked(&self) -> Option<String> {
        self.inner
            .backend
            .unsupported()
            .or_else(|| lock(&self.inner.state).cannot_place.clone())
    }

    /// Everything the screen layout tab shows. Reads the process table: a
    /// window's sessions are found through their consoles.
    pub async fn arrange_overview(&self) -> Result<ArrangeOverview, Error> {
        let sight = self.sight(true).await?;
        let state = lock(&self.inner.state);
        let titles = entry_titles(&state.file, &sight.live);
        let in_window = sessions_by_window(&sight.live, &titles, &sight.windows, &sight.owners);
        let reason = self
            .inner
            .backend
            .unsupported()
            .or_else(|| state.cannot_place.clone());
        let windows = window_list(&sight.windows, &sight.monitors, &sight.live, &in_window);
        let offered: HashSet<u64> = windows.iter().filter_map(|w| w.id.parse().ok()).collect();
        let layout_plans = state
            .file
            .layouts
            .iter()
            .map(|layout| {
                let plan = layout_plan(
                    layout,
                    &sight.live,
                    &sight.windows,
                    &sight.owners,
                    &sight.monitors,
                    &offered,
                );
                (layout.id.clone(), plan)
            })
            .collect::<BTreeMap<_, _>>();
        Ok(ArrangeOverview {
            supported: reason.is_none(),
            message: reason,
            windows,
            monitors: sight.monitors,
            layouts: state.file.layouts.clone(),
            undo_windows: state.undo.len() as u32,
            layout_plans,
        })
    }

    /// Refuses while windows cannot move, or are moving for a restore.
    fn arrange_ready(&self) -> Result<(), Error> {
        if let Some(reason) = self.arrange_blocked() {
            return Err(api_error(ErrorCode::Unsupported, reason));
        }
        let running = lock(&self.inner.state)
            .job
            .as_ref()
            .is_some_and(|j| j.finished_at.is_none());
        if running {
            return Err(api_error(
                ErrorCode::Conflict,
                "a restore is running: arrange the windows when it has finished",
            ));
        }
        Ok(())
    }

    /// Carries out moves and reports them with `plan.skipped`. `remember`:
    /// these positions are the new undo memory.
    async fn arrange_moves(
        &self,
        plan: Moves,
        stop: Option<StopCheck>,
        remember: bool,
    ) -> Result<ArrangeResult, Error> {
        if stop.as_ref().is_some_and(|s| s()) {
            return Err(api_error(ErrorCode::Forbidden, STOPPED_NOTE));
        }
        let Moves { moves, skipped } = plan;
        let backend = Arc::clone(&self.inner.backend);
        let carried =
            tokio::task::spawn_blocking(move || carry_out(backend.as_ref(), &moves, stop.as_ref()))
                .await
                .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
        let carried = match carried {
            Ok(carried) => carried,
            Err(err) => {
                if err.code() == ErrorCode::Unsupported {
                    lock(&self.inner.state).cannot_place = Some(error_text(&err));
                }
                return Err(err);
            }
        };
        if remember && !carried.moved.is_empty() {
            lock(&self.inner.state).undo = carried.moved;
        }
        let count = |wanted: fn(&ArrangeItemStatus) -> bool, items: &[ArrangeItem]| {
            items.iter().filter(|i| wanted(&i.status)).count() as u32
        };
        Ok(ArrangeResult {
            moved: count(|s| *s == ArrangeItemStatus::Moved, &carried.items),
            skipped: skipped.len() as u32,
            failed: count(|s| *s == ArrangeItemStatus::Failed, &carried.items),
            items: skipped.into_iter().chain(carried.items).collect(),
        })
    }

    /// Puts open terminal windows in the cells of `plans`.
    pub async fn arrange_apply(
        &self,
        plans: &[ArrangeMonitorPlan],
        stop: Option<StopCheck>,
    ) -> Result<ArrangeResult, Error> {
        self.arrange_ready()?;
        check_plans(plans)?;
        let _one_at_a_time = self.inner.arranging.lock().await;
        let sight = self.sight(false).await?;
        let moves = resolve_plans(plans, &sight.windows, &sight.monitors)?;
        self.arrange_moves(moves, stop, true).await
    }

    /// Applies a saved layout to the windows open now.
    pub async fn arrange_apply_layout(
        &self,
        id: &str,
        stop: Option<StopCheck>,
    ) -> Result<ArrangeResult, Error> {
        self.arrange_ready()?;
        let _one_at_a_time = self.inner.arranging.lock().await;
        let sight = self.sight(true).await?;
        let layout = lock(&self.inner.state)
            .file
            .layouts
            .iter()
            .find(|l| l.id == id)
            .cloned()
            .ok_or_else(|| api_error(ErrorCode::NotFound, format!("no layout {id}")))?;
        let moves = resolve_layout(
            &layout,
            &sight.live,
            &sight.windows,
            &sight.owners,
            &sight.monitors,
        )?;
        self.arrange_moves(moves, stop, true).await
    }

    /// Puts the windows of the last arrangement back. One step: the memory
    /// is used up.
    pub async fn arrange_undo(&self, stop: Option<StopCheck>) -> Result<ArrangeResult, Error> {
        self.arrange_ready()?;
        let _one_at_a_time = self.inner.arranging.lock().await;
        let undo = std::mem::take(&mut lock(&self.inner.state).undo);
        if undo.is_empty() {
            return Err(api_error(ErrorCode::Conflict, "there is nothing to undo"));
        }
        let sight = match self.sight(false).await {
            Ok(sight) => sight,
            Err(err) => {
                lock(&self.inner.state).undo = undo;
                return Err(err);
            }
        };
        let moves = resolve_undo(&undo, &sight.windows, &sight.monitors);
        let done = self.arrange_moves(moves, stop, false).await;
        if done.is_err() {
            lock(&self.inner.state).undo = undo;
        }
        done
    }

    /// Saves, renames or deletes a layout. Moves nothing.
    pub async fn arrange_edit(&self, edit: &ArrangeEdit) -> Result<(), Error> {
        let saved = match edit {
            ArrangeEdit::LayoutSave { monitors, .. } => {
                check_plans(monitors)?;
                let sight = self.sight(true).await?;
                let titles = entry_titles(&lock(&self.inner.state).file, &sight.live);
                let in_window =
                    sessions_by_window(&sight.live, &titles, &sight.windows, &sight.owners);
                saved_monitors(
                    monitors,
                    &sight.monitors,
                    &sight.windows,
                    &sight.live,
                    &titles,
                    &in_window,
                )
            }
            ArrangeEdit::LayoutRename { .. } | ArrangeEdit::LayoutDelete { .. } => Vec::new(),
        };
        self.commit(|file| apply_edit(file, edit, saved))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tokio::sync::broadcast;
    use vca_core::SessionId;

    use super::super::backend::{FakeBackend, FakeCall};
    use super::super::matching::tests::{live, window};
    use super::super::{RestoreSettings, store};
    use super::*;
    use crate::workspace::WorkspaceRoots;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> WindowRect {
        WindowRect { x, y, w, h }
    }

    fn monitor(id: &str, r: WindowRect) -> RestoreMonitor {
        RestoreMonitor {
            id: id.into(),
            rect: r,
            work: WindowRect { h: r.h - 48, ..r },
            dpi: 144,
        }
    }

    fn two_monitors() -> Vec<RestoreMonitor> {
        vec![
            monitor("A", rect(0, 0, 1920, 1080)),
            monitor("B", rect(-1920, -200, 1920, 1080)),
        ]
    }

    fn cell(cell: u32, window: u64) -> ArrangeCell {
        ArrangeCell {
            cell,
            window: window.to_string(),
        }
    }

    fn plan(monitor: &str, rows: u32, cols: u32, cells: Vec<ArrangeCell>) -> ArrangeMonitorPlan {
        ArrangeMonitorPlan {
            monitor: monitor.into(),
            rows,
            cols,
            cells,
        }
    }

    fn assert_tiles(area: &WindowRect, cells: &[WindowRect], rows: usize, cols: usize) {
        assert_eq!(cells.len(), rows * cols);
        let total: i64 = cells.iter().map(|c| i64::from(c.w) * i64::from(c.h)).sum();
        assert_eq!(total, i64::from(area.w) * i64::from(area.h));
        for row in 0..rows {
            let line = &cells[row * cols..(row + 1) * cols];
            assert_eq!(line[0].x, area.x);
            for pair in line.windows(2) {
                assert_eq!(pair[0].x + pair[0].w, pair[1].x, "gap or overlap in a row");
                assert_eq!(pair[0].y, pair[1].y);
                assert_eq!(pair[0].h, pair[1].h);
                assert!((pair[0].w - pair[1].w).abs() <= 1, "uneven leftovers");
            }
            let last = line[cols - 1];
            assert_eq!(last.x + last.w, area.x + area.w);
        }
        for col in 0..cols {
            assert_eq!(cells[col].y, area.y);
            let bottom = cells[(rows - 1) * cols + col];
            assert_eq!(bottom.y + bottom.h, area.y + area.h);
            for row in 1..rows {
                let (up, down) = (cells[(row - 1) * cols + col], cells[row * cols + col]);
                assert_eq!(up.y + up.h, down.y, "gap or overlap in a column");
                assert_eq!(up.w, down.w);
                assert!((up.h - down.h).abs() <= 1, "uneven leftovers");
            }
        }
    }

    #[test]
    fn every_preset_fills_the_work_area_exactly() {
        let areas = [
            rect(0, 0, 1920, 1032),
            rect(-1921, -203, 2561, 1391),
            rect(5, 7, 3839, 2111),
        ];
        for area in areas {
            for (rows, cols) in [(1, 1), (1, 2), (2, 2), (2, 3), (2, 4), (2, 5)] {
                let cells = grid(&area, rows, cols).unwrap();
                assert_tiles(&area, &cells, rows as usize, cols as usize);
            }
        }
    }

    #[test]
    fn every_allowed_grid_fills_the_area() {
        let area = rect(-7, 11, 2023, 1037);
        for rows in 1..=MAX_ARRANGE_ROWS {
            for cols in 1..=MAX_ARRANGE_COLS {
                let cells = grid(&area, rows, cols).unwrap();
                assert_tiles(&area, &cells, rows as usize, cols as usize);
            }
        }
    }

    #[test]
    fn leftover_pixels_are_spread_not_dropped() {
        let cells = grid(&rect(0, 0, 10, 10), 3, 3).unwrap();
        let widths: Vec<i32> = cells[..3].iter().map(|c| c.w).collect();
        assert_eq!(widths, [3, 3, 4]);
        assert_eq!(cells[8], rect(6, 6, 4, 4));
    }

    #[test]
    fn grids_out_of_bounds_or_too_small_are_refused() {
        let area = rect(0, 0, 1920, 1032);
        for (rows, cols) in [
            (0, 2),
            (2, 0),
            (MAX_ARRANGE_ROWS + 1, 1),
            (1, MAX_ARRANGE_COLS + 1),
        ] {
            assert_eq!(
                grid(&area, rows, cols).unwrap_err().code(),
                ErrorCode::InvalidRequest
            );
        }
        assert!(grid(&rect(0, 0, 1, 1), 2, 2).is_err());
    }

    #[test]
    fn invisible_borders_stay_outside_the_cell() {
        // Windows Terminal at 150 %: 9 px left, right and bottom, none on top.
        let outer = rect(100, 100, 1938, 1053);
        let visible = rect(109, 100, 1920, 1044);
        let cell = rect(0, 0, 960, 516);
        assert_eq!(
            outer_for_visible(&cell, &outer, &visible),
            rect(-9, 0, 978, 525)
        );
        // No borders, or a reading that makes no sense: the cell as it is.
        assert_eq!(outer_for_visible(&cell, &outer, &outer), cell);
        let wrong = rect(500, 100, 800, 600);
        assert_eq!(outer_for_visible(&cell, &outer, &wrong), cell);
    }

    #[test]
    fn a_malformed_plan_is_an_error() {
        let code = |plans: &[ArrangeMonitorPlan]| check_plans(plans).unwrap_err().code();
        let bad = ErrorCode::InvalidRequest;
        assert_eq!(code(&[plan("A", 0, 2, vec![])]), bad);
        assert_eq!(code(&[plan("A", 1, 2, vec![cell(2, 1)])]), bad);
        assert_eq!(code(&[plan("A", 1, 2, vec![cell(0, 1), cell(0, 2)])]), bad);
        assert_eq!(code(&[plan("A", 1, 2, vec![cell(0, 1), cell(1, 1)])]), bad);
        assert_eq!(
            code(&[
                plan("A", 1, 1, vec![cell(0, 1)]),
                plan("B", 1, 1, vec![cell(0, 1)])
            ]),
            bad
        );
        assert_eq!(
            code(&[plan("A", 1, 1, vec![]), plan("A", 1, 1, vec![])]),
            bad
        );
        assert!(check_plans(&[plan("A", 2, 2, vec![cell(3, 1)])]).is_ok());
    }

    #[test]
    fn only_listed_windows_move_and_never_outside_the_work_area() {
        let monitors = two_monitors();
        let windows = [window(7, "API"), window(8, "UI")];
        let plans = [
            plan("A", 1, 2, vec![cell(0, 7), cell(1, 4242)]),
            plan("gone", 1, 1, vec![cell(0, 8)]),
        ];
        let out = resolve_plans(&plans, &windows, &monitors).unwrap();
        assert_eq!(out.moves.len(), 1);
        assert_eq!(out.moves[0].window.id, 7);
        assert_eq!(out.moves[0].to, Target::Cell(rect(0, 0, 960, 1032)));
        let statuses: Vec<_> = out.skipped.iter().map(|i| i.status).collect();
        assert_eq!(
            statuses,
            [
                ArrangeItemStatus::SkippedNotOpen,
                ArrangeItemStatus::SkippedMonitor
            ]
        );
        assert_eq!(out.skipped[0].title, "");
        assert_eq!(out.skipped[1].title, "UI");
    }

    #[test]
    fn very_small_cells_stay_usable_and_on_the_monitor() {
        let monitors = [monitor("A", rect(0, 0, 1366, 768))];
        let windows = [window(7, "API")];
        let plans = [plan("A", 1, 8, vec![cell(7, 7)])];
        let out = resolve_plans(&plans, &windows, &monitors).unwrap();
        let Target::Cell(placed) = out.moves[0].to else {
            panic!("{:?}", out.moves[0].to);
        };
        assert_eq!(placed.w, 320);
        assert_eq!(placed.x + placed.w, 1366);
    }

    struct Rig {
        restore: Restore,
        backend: Arc<FakeBackend>,
        live: Arc<std::sync::Mutex<Vec<LiveSession>>>,
        dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        rig_in(dir)
    }

    fn rig_in(dir: tempfile::TempDir) -> Rig {
        let backend = FakeBackend::new();
        backend.set_monitors(two_monitors());
        let sessions: Arc<std::sync::Mutex<Vec<LiveSession>>> = Arc::default();
        let source = Arc::clone(&sessions);
        let (notices, _) = broadcast::channel(16);
        let restore = Restore::new(
            RestoreSettings {
                path: dir.path().join(store::RESTORE_FILE),
                interval: None,
                auto_register: false,
                title_wait: Duration::from_millis(10),
                title_poll: Duration::from_millis(1),
                managed: None,
            },
            backend.clone(),
            Arc::new(move |_| source.lock().unwrap().clone()),
            WorkspaceRoots::new(Vec::new()),
            notices,
        );
        Rig {
            restore,
            backend,
            live: sessions,
            dir,
        }
    }

    fn placed(backend: &FakeBackend) -> Vec<(u64, WindowRect, bool)> {
        backend
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                FakeCall::Place {
                    id,
                    rect,
                    maximized,
                } => Some((id, rect, maximized)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_partial_result_moves_what_it_can_and_never_opens_or_focuses() {
        let rig = rig();
        rig.backend
            .set_windows(vec![window(7, "API"), window(8, "UI"), window(9, "Docs")]);
        rig.backend.fail_place(8);
        let plans = [plan(
            "A",
            2,
            2,
            vec![cell(0, 7), cell(1, 8), cell(2, 9), cell(3, 123)],
        )];
        let result = rig.restore.arrange_apply(&plans, None).await.unwrap();
        assert_eq!((result.moved, result.skipped, result.failed), (2, 1, 1));
        let failed = result
            .items
            .iter()
            .find(|i| i.status == ArrangeItemStatus::Failed)
            .unwrap();
        assert_eq!(failed.title, "UI");
        assert!(failed.message.as_ref().unwrap().contains("moved"));
        // Only place, and only windows of the live list.
        let calls = rig.backend.calls();
        assert!(calls.iter().all(|c| matches!(c, FakeCall::Place { .. })));
        let ids: Vec<u64> = placed(&rig.backend).iter().map(|p| p.0).collect();
        assert_eq!(ids, [7, 8, 9]);
    }

    #[tokio::test]
    async fn an_unknown_window_is_never_passed_to_the_backend() {
        let rig = rig();
        rig.backend.set_windows(vec![window(7, "API")]);
        // A browser window's handle, a garbage id, a name.
        let plans = [plan(
            "A",
            2,
            2,
            vec![
                cell(0, 999_999),
                ArrangeCell {
                    cell: 1,
                    window: "not-a-handle".into(),
                },
                ArrangeCell {
                    cell: 2,
                    window: "API".into(),
                },
            ],
        )];
        let result = rig.restore.arrange_apply(&plans, None).await.unwrap();
        assert_eq!((result.moved, result.skipped), (0, 3));
        assert!(rig.backend.calls().is_empty());
        assert_eq!(
            rig.restore.arrange_overview().await.unwrap().undo_windows,
            0
        );
    }

    #[tokio::test]
    async fn maximized_and_minimized_windows_end_up_in_their_cell() {
        let rig = rig();
        let mut big = window(7, "API");
        big.maximized = true;
        let mut small = window(8, "UI");
        small.minimized = true;
        small.rect = rect(-32000, -32000, 160, 28);
        rig.backend.set_windows(vec![big, small]);
        let plans = [plan("A", 1, 2, vec![cell(0, 7), cell(1, 8)])];
        rig.restore.arrange_apply(&plans, None).await.unwrap();
        let after = rig.backend.windows().unwrap();
        assert_eq!(after[0].rect, rect(0, 0, 960, 1032));
        assert!(!after[0].maximized);
        assert_eq!(after[1].rect, rect(960, 0, 960, 1032));
        assert!(!after[1].minimized);
        assert!(placed(&rig.backend).iter().all(|p| !p.2));
    }

    #[tokio::test]
    async fn undo_goes_one_step_back_and_reports_closed_windows() {
        let rig = rig();
        let start = rect(100, 100, 800, 600);
        let mut windows = vec![window(7, "API"), window(8, "Closing")];
        for w in &mut windows {
            w.rect = start;
        }
        rig.backend.set_windows(windows);
        let err = rig.restore.arrange_undo(None).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Conflict);

        let first = [plan("A", 1, 2, vec![cell(0, 7), cell(1, 8)])];
        rig.restore.arrange_apply(&first, None).await.unwrap();
        let overview = rig.restore.arrange_overview().await.unwrap();
        assert_eq!(overview.undo_windows, 2);

        // A second arrangement replaces the memory.
        let second = [plan("B", 1, 1, vec![cell(0, 7)])];
        rig.restore.arrange_apply(&second, None).await.unwrap();
        let overview = rig.restore.arrange_overview().await.unwrap();
        assert_eq!(overview.undo_windows, 1);
        let result = rig.restore.arrange_undo(None).await.unwrap();
        assert_eq!((result.moved, result.skipped, result.failed), (1, 0, 0));
        let cell_of_first = rect(0, 0, 960, 1032);
        assert_eq!(rig.backend.windows().unwrap()[0].rect, cell_of_first);

        // The memory is used up.
        let overview = rig.restore.arrange_overview().await.unwrap();
        assert_eq!(overview.undo_windows, 0);
        let err = rig.restore.arrange_undo(None).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Conflict);

        // A window closed in between is reported, the others go back.
        rig.restore.arrange_apply(&first, None).await.unwrap();
        let mut now = rig.backend.windows().unwrap();
        now.pop();
        rig.backend.set_windows(now);
        let result = rig.restore.arrange_undo(None).await.unwrap();
        assert_eq!((result.moved, result.skipped, result.failed), (1, 1, 0));
        assert!(
            result
                .items
                .iter()
                .any(|i| i.title == "Closing" && i.status == ArrangeItemStatus::SkippedNotOpen)
        );
    }

    #[tokio::test]
    async fn undo_restores_maximized_and_minimized_states() {
        let rig = rig();
        let mut big = window(7, "API");
        big.maximized = true;
        big.rect = rect(-9, -9, 1938, 1050);
        let mut small = window(8, "UI");
        small.minimized = true;
        small.rect = rect(-32000, -32000, 160, 28);
        let plain = window(9, "Docs");
        rig.backend
            .set_windows(vec![big.clone(), small, plain.clone()]);
        let plans = [plan("A", 1, 3, vec![cell(0, 7), cell(1, 8), cell(2, 9)])];
        rig.restore.arrange_apply(&plans, None).await.unwrap();
        let result = rig.restore.arrange_undo(None).await.unwrap();
        assert_eq!(result.moved, 3);
        let after = rig.backend.windows().unwrap();
        assert_eq!(after[0].rect, big.rect);
        assert!(after[0].maximized);
        assert!(after[1].minimized);
        assert_eq!(after[2].rect, plain.rect);
        assert!(rig.backend.calls().contains(&FakeCall::Minimize { id: 8 }));
    }

    #[tokio::test]
    async fn undo_lands_on_a_monitor_when_the_old_one_is_gone() {
        let rig = rig();
        let mut far = window(7, "API");
        far.rect = rect(-1900, 0, 800, 600);
        rig.backend.set_windows(vec![far]);
        let plans = [plan("A", 1, 1, vec![cell(0, 7)])];
        rig.restore.arrange_apply(&plans, None).await.unwrap();
        rig.backend
            .set_monitors(vec![monitor("A", rect(0, 0, 1920, 1080))]);
        rig.restore.arrange_undo(None).await.unwrap();
        let back = rig.backend.windows().unwrap()[0].rect;
        assert_eq!((back.x, back.y), (0, 0));
    }

    fn session(title: &str, folder: &str, id: Option<&str>, pid: u32) -> LiveSession {
        let mut s = live(title, folder, id);
        s.pids = vec![pid];
        s
    }

    fn layout_of(file: PathBuf) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_saved_layout_finds_windows_by_identity_after_handles_change() {
        let rig = rig();
        rig.backend
            .set_windows(vec![window(7, "One"), window(8, "Two"), window(9, "Plain")]);
        rig.backend.set_owner(100, 7);
        rig.backend.set_owner(200, 8);
        *rig.live.lock().unwrap() = vec![
            session("first", "E:\\a", Some("conv-1"), 100),
            session("second", "E:\\b", Some("conv-2"), 200),
        ];
        let edit = ArrangeEdit::LayoutSave {
            name: " Morning ".into(),
            monitors: vec![plan(
                "A",
                2,
                2,
                vec![cell(0, 7), cell(1, 8), cell(2, 9), cell(3, 4242)],
            )],
        };
        rig.restore.arrange_edit(&edit).await.unwrap();
        let overview = rig.restore.arrange_overview().await.unwrap();
        let layout = &overview.layouts[0];
        assert_eq!(layout.name, "Morning");
        // The window that was not open is not part of it.
        assert_eq!(layout.monitors[0].cells.len(), 3);
        let saved = layout_of(rig.dir.path().join(store::RESTORE_FILE));
        let text = saved["layouts"].to_string();
        assert!(text.contains("conv-1"), "{text}");
        for handle in ["\"7\"", "\"8\"", "\"9\"", ":7,", ":8,", ":9,"] {
            assert!(!text.contains(handle), "{handle} in {text}");
        }

        // A reboot: new handles, other titles, the window of conv-2 closed.
        rig.backend
            .set_windows(vec![window(501, "renamed"), window(502, "Plain")]);
        rig.backend.set_owner(100, 501);
        *rig.live.lock().unwrap() = vec![session("first", "E:\\a", Some("conv-1"), 100)];
        let id = layout.id.clone();
        let result = rig.restore.arrange_apply_layout(&id, None).await.unwrap();
        assert_eq!((result.moved, result.skipped, result.failed), (2, 1, 0));
        let skipped = result
            .items
            .iter()
            .find(|i| i.status == ArrangeItemStatus::SkippedNotOpen)
            .unwrap();
        assert_eq!(skipped.title, "Two");
        let moved: Vec<(u64, WindowRect)> =
            placed(&rig.backend).iter().map(|p| (p.0, p.1)).collect();
        assert_eq!(
            moved,
            [(501, rect(0, 0, 960, 516)), (502, rect(0, 516, 960, 516)),]
        );
    }

    #[tokio::test]
    async fn two_tabs_of_one_window_are_one_item_and_keep_both_identities() {
        let rig = rig();
        rig.backend.set_windows(vec![window(7, "active tab")]);
        rig.backend.set_owner(100, 7);
        rig.backend.set_owner(200, 7);
        *rig.live.lock().unwrap() = vec![
            session("first", "E:\\a", Some("conv-1"), 100),
            session("second", "E:\\b", Some("conv-2"), 200),
        ];
        let overview = rig.restore.arrange_overview().await.unwrap();
        assert_eq!(overview.windows.len(), 1);
        assert_eq!(overview.windows[0].sessions.len(), 2);
        let edit = ArrangeEdit::LayoutSave {
            name: "Tabs".into(),
            monitors: vec![plan("A", 1, 1, vec![cell(0, 7)])],
        };
        rig.restore.arrange_edit(&edit).await.unwrap();
        // One of the two tabs is enough to find the window again.
        *rig.live.lock().unwrap() = vec![session("second", "E:\\b", Some("conv-2"), 200)];
        let id = rig.restore.arrange_overview().await.unwrap().layouts[0]
            .id
            .clone();
        let result = rig.restore.arrange_apply_layout(&id, None).await.unwrap();
        assert_eq!(result.moved, 1);
    }

    #[tokio::test]
    async fn a_title_shared_by_two_windows_does_not_pick_one() {
        let rig = rig();
        rig.backend.set_windows(vec![window(7, "pwsh")]);
        let edit = ArrangeEdit::LayoutSave {
            name: "Shells".into(),
            monitors: vec![plan("A", 1, 1, vec![cell(0, 7)])],
        };
        rig.restore.arrange_edit(&edit).await.unwrap();
        rig.backend
            .set_windows(vec![window(21, "pwsh"), window(22, "pwsh")]);
        let id = rig.restore.arrange_overview().await.unwrap().layouts[0]
            .id
            .clone();
        let result = rig.restore.arrange_apply_layout(&id, None).await.unwrap();
        assert_eq!((result.moved, result.skipped), (0, 1));
        assert!(rig.backend.calls().is_empty());
    }

    #[tokio::test]
    async fn a_layout_uses_the_monitors_of_today_and_skips_missing_ones() {
        let rig = rig();
        rig.backend
            .set_windows(vec![window(7, "API"), window(8, "UI")]);
        let edit = ArrangeEdit::LayoutSave {
            name: "Both".into(),
            monitors: vec![
                plan("A", 1, 2, vec![cell(1, 7)]),
                plan("B", 1, 1, vec![cell(0, 8)]),
            ],
        };
        rig.restore.arrange_edit(&edit).await.unwrap();
        rig.backend
            .set_monitors(vec![monitor("A", rect(0, 0, 3840, 2160))]);
        let id = rig.restore.arrange_overview().await.unwrap().layouts[0]
            .id
            .clone();
        let result = rig.restore.arrange_apply_layout(&id, None).await.unwrap();
        assert_eq!((result.moved, result.skipped), (1, 1));
        assert_eq!(result.items[0].status, ArrangeItemStatus::SkippedMonitor);
        assert_eq!(placed(&rig.backend)[0].1, rect(1920, 0, 1920, 2112));
    }

    #[tokio::test]
    async fn layouts_are_renamed_deleted_and_survive_a_restart() {
        let rig = rig();
        rig.backend.set_windows(vec![window(7, "API")]);
        let save = |name: &str| ArrangeEdit::LayoutSave {
            name: name.into(),
            monitors: vec![plan("A", 1, 1, vec![cell(0, 7)])],
        };
        rig.restore.arrange_edit(&save("One")).await.unwrap();
        rig.restore.arrange_edit(&save("Two")).await.unwrap();
        let code = |e: Error| e.code();
        assert_eq!(
            code(rig.restore.arrange_edit(&save("one")).await.unwrap_err()),
            ErrorCode::Conflict
        );
        assert_eq!(
            code(rig.restore.arrange_edit(&save("  ")).await.unwrap_err()),
            ErrorCode::InvalidRequest
        );
        for (monitor, window) in [("A", 1234), ("not a monitor", 7)] {
            let empty = ArrangeEdit::LayoutSave {
                name: "Empty".into(),
                monitors: vec![plan(monitor, 1, 1, vec![cell(0, window)])],
            };
            assert_eq!(
                code(rig.restore.arrange_edit(&empty).await.unwrap_err()),
                ErrorCode::InvalidRequest
            );
        }
        let layouts = rig.restore.arrange_overview().await.unwrap().layouts;
        let (one, two) = (layouts[0].id.clone(), layouts[1].id.clone());
        let rename = |layout: &str, name: &str| ArrangeEdit::LayoutRename {
            layout: layout.into(),
            name: name.into(),
        };
        assert_eq!(
            code(
                rig.restore
                    .arrange_edit(&rename(&two, "ONE"))
                    .await
                    .unwrap_err()
            ),
            ErrorCode::Conflict
        );
        rig.restore
            .arrange_edit(&rename(&two, "Evening"))
            .await
            .unwrap();
        rig.restore
            .arrange_edit(&ArrangeEdit::LayoutDelete { layout: one })
            .await
            .unwrap();
        assert_eq!(
            code(
                rig.restore
                    .arrange_apply_layout("nope", None)
                    .await
                    .unwrap_err()
            ),
            ErrorCode::NotFound
        );
        assert!(rig.backend.calls().is_empty());

        let Rig { restore, dir, .. } = rig;
        drop(restore);
        let again = rig_in(dir);
        let layouts = again.restore.arrange_overview().await.unwrap().layouts;
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Evening");
    }

    #[tokio::test]
    async fn a_desktop_that_cannot_move_windows_says_so_from_then_on() {
        let rig = rig();
        rig.backend.set_windows(vec![window(7, "API")]);
        rig.backend
            .cannot_place("Wayland does not let apps move windows");
        let before = rig.restore.arrange_overview().await.unwrap();
        assert!(before.supported);
        let plans = [plan("A", 1, 1, vec![cell(0, 7)])];
        let err = rig.restore.arrange_apply(&plans, None).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unsupported);
        let after = rig.restore.arrange_overview().await.unwrap();
        assert!(!after.supported);
        assert!(after.message.unwrap().contains("Wayland"));
        assert_eq!(after.undo_windows, 0);
    }

    #[tokio::test]
    async fn the_kill_switch_stops_an_arrangement() {
        let rig = rig();
        rig.backend.set_windows(vec![window(7, "API")]);
        let plans = [plan("A", 1, 1, vec![cell(0, 7)])];
        let tripped: StopCheck = Arc::new(|| true);
        let err = rig
            .restore
            .arrange_apply(&plans, Some(tripped))
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Forbidden);
        assert!(rig.backend.calls().is_empty());
    }

    #[tokio::test]
    async fn the_overview_lists_ai_windows_with_their_monitor_and_sessions() {
        let rig = rig();
        let mut far = window(8, "UI");
        far.rect = rect(-1000, 0, 500, 500);
        rig.backend
            .set_windows(vec![window(7, "API"), far, window(9, "pwsh")]);
        rig.backend.set_owner(100, 7);
        rig.backend.set_owner(200, 8);
        let mut codex = session("api", "E:\\a", Some("c"), 100);
        codex.agent = AgentKind::Codex;
        *rig.live.lock().unwrap() = vec![codex, session("ui", "E:\\b", None, 200)];
        let overview = rig.restore.arrange_overview().await.unwrap();
        assert!(overview.supported);
        assert_eq!(overview.monitors.len(), 2);
        let api = &overview.windows[0];
        assert_eq!(api.id, "7");
        assert_eq!(api.monitor.as_deref(), Some("A"));
        assert_eq!(api.sessions[0].agent, AgentKind::Codex);
        assert_eq!(api.sessions[0].id, SessionId::from("vca-api"));
        assert_eq!(api.sessions[0].folder, Path::new("E:\\a"));
        assert_eq!(overview.windows[1].monitor.as_deref(), Some("B"));
        // The plain terminal is not offered.
        assert_eq!(overview.windows.len(), 2);
    }

    fn listed(sessions: Vec<ArrangeSession>) -> bool {
        arrangeable(&sessions)
    }

    fn arrange_session(agent: AgentKind) -> ArrangeSession {
        ArrangeSession {
            id: SessionId::from("vca-x"),
            title: "x".into(),
            agent,
            folder: PathBuf::from("E:\\a"),
        }
    }

    #[test]
    fn only_windows_with_an_ai_agent_are_arrangeable() {
        let shell = || arrange_session(AgentKind::Shell("pwsh".into()));
        assert!(!listed(vec![]));
        assert!(!listed(vec![shell()]));
        assert!(!listed(vec![shell(), shell()]));
        assert!(listed(vec![arrange_session(AgentKind::ClaudeCode)]));
        assert!(listed(vec![shell(), arrange_session(AgentKind::Codex)]));
        assert!(listed(vec![arrange_session(AgentKind::Generic(
            "x".into()
        ))]));
    }

    #[test]
    fn the_window_list_leaves_out_plain_and_shell_windows() {
        let windows = [window(7, "API"), window(8, "Shell"), window(9, "pwsh")];
        let mut shell = live("Shell", "E:\\s", None);
        shell.agent = AgentKind::Shell("powershell".into());
        let sessions = [live("API", "E:\\a", None), shell];
        let in_window = HashMap::from([(7, vec![0]), (8, vec![1])]);
        let list = window_list(&windows, &two_monitors(), &sessions, &in_window);
        let ids: Vec<&str> = list.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["7"]);
    }

    #[tokio::test]
    async fn a_vca_session_in_a_real_window_is_listed_but_lends_no_window_by_ancestry() {
        let rig = rig();
        // 7: a `vca run` window titled after its session. 8: the terminal
        // that started the app, and so vcad: an ancestor of every session
        // vcad hosts. Neither session's own process is on a listed console:
        // vcad runs them in pseudo consoles.
        rig.backend
            .set_windows(vec![window(7, "restored"), window(8, "dev shell")]);
        rig.backend.set_owner(50, 8);
        let mut restored = live("restored", "E:\\a", Some("conv-1"));
        restored.vca_started = true;
        restored.pids = vec![100, 40, 50];
        let mut headless = live("made in the app", "E:\\b", Some("conv-2"));
        headless.vca_started = true;
        headless.pids = vec![101, 40, 50];
        *rig.live.lock().unwrap() = vec![restored, headless];
        let overview = rig.restore.arrange_overview().await.unwrap();
        let ids: Vec<&str> = overview.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["7"]);
        assert_eq!(overview.windows[0].sessions.len(), 1);
    }

    /// Saves a layout of windows 7 (conv-1), 8 (conv-2) and 9 (conv-3, on
    /// monitor B), each in a window of its own.
    async fn saved_layout(rig: &Rig) -> String {
        rig.backend
            .set_windows(vec![window(7, "One"), window(8, "Two"), window(9, "Three")]);
        for (pid, id) in [(100, 7), (200, 8), (300, 9)] {
            rig.backend.set_owner(pid, id);
        }
        *rig.live.lock().unwrap() = vec![
            session("first", "E:\\a", Some("conv-1"), 100),
            session("second", "E:\\b", Some("conv-2"), 200),
            session("third", "E:\\c", Some("conv-3"), 300),
        ];
        let edit = ArrangeEdit::LayoutSave {
            name: "Morning".into(),
            monitors: vec![
                plan("A", 2, 3, vec![cell(4, 7), cell(1, 8)]),
                plan("B", 1, 2, vec![cell(1, 9)]),
            ],
        };
        rig.restore.arrange_edit(&edit).await.unwrap();
        rig.restore.arrange_overview().await.unwrap().layouts[0]
            .id
            .clone()
    }

    #[tokio::test]
    async fn a_saved_layout_comes_with_its_grids_of_the_windows_open_now() {
        let rig = rig();
        let id = saved_layout(&rig).await;
        // New handles; conv-2 now runs in a window with another session id
        // and folder, found by its title; conv-3 is closed.
        rig.backend.set_windows(vec![
            window(501, "renamed"),
            window(502, "Two"),
            window(503, "Three"),
        ]);
        rig.backend.set_owner(100, 501);
        rig.backend.set_owner(400, 502);
        *rig.live.lock().unwrap() = vec![
            session("first", "E:\\a", Some("conv-1"), 100),
            session("other", "E:\\z", Some("conv-9"), 400),
        ];
        let overview = rig.restore.arrange_overview().await.unwrap();
        let plans = &overview.layout_plans[&id];
        assert_eq!(
            plans,
            &vec![
                plan("A", 2, 3, vec![cell(4, 501), cell(1, 502)]),
                // Window 503 has the title but no AI session: not offered.
                plan("B", 1, 2, vec![]),
            ]
        );
        // Loading moves nothing.
        assert!(rig.backend.calls().is_empty());
    }

    #[tokio::test]
    async fn a_loaded_layout_leaves_out_monitors_not_connected() {
        let rig = rig();
        let id = saved_layout(&rig).await;
        rig.backend
            .set_monitors(vec![monitor("A", rect(0, 0, 3840, 2160))]);
        let overview = rig.restore.arrange_overview().await.unwrap();
        assert_eq!(
            overview.layout_plans[&id],
            [plan("A", 2, 3, vec![cell(4, 7), cell(1, 8)])]
        );
    }

    #[tokio::test]
    async fn loaded_grids_are_never_saved() {
        let rig = rig();
        let id = saved_layout(&rig).await;
        let overview = rig.restore.arrange_overview().await.unwrap();
        assert_eq!(overview.layout_plans[&id].len(), 2);
        // Something else is written after the plans were worked out.
        let rename = ArrangeEdit::LayoutRename {
            layout: id.clone(),
            name: "Evening".into(),
        };
        rig.restore.arrange_edit(&rename).await.unwrap();
        let text =
            String::from_utf8(std::fs::read(rig.dir.path().join(store::RESTORE_FILE)).unwrap())
                .unwrap();
        assert!(!text.contains("layout_plans"), "{text}");
        assert!(!text.contains("\"window\""), "{text}");
        let again = rig.restore.arrange_overview().await.unwrap();
        assert_eq!(again.layout_plans[&id], overview.layout_plans[&id]);
    }
}
