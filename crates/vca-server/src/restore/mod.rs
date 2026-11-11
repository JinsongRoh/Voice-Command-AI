//! Session restore: keeps a managed list of the AI CLI windows of this
//! computer and, after a reboot or power cut, reopens each one at the same
//! place, under the user's name, in the same conversation.
//!
//! * A snapshot on a timer folds the live sessions and their windows into
//!   the active profile, and writes the file only when something changed
//!   ([`capture`], [`store`]).
//! * A run is planned as pure data ([`plan`]; a dry run stops there), then
//!   carried out one window at a time with progress pushed to every client.
//! * The platform lives behind [`WindowBackend`]: Windows (Windows Terminal,
//!   console host fallback), macOS (Terminal.app, iTerm2), Linux desktops
//!   (gnome-terminal, konsole, kitty; placed with wmctrl or xdotool on X11,
//!   open-only on Wayland) and headless machines (tmux sessions named after
//!   the entry). Where windows cannot be moved, an item ends `opened`, not
//!   `placed` and not `failed`; other systems answer `unsupported`.
//! * "Bring to front" ([`Restore::focus_session`]) finds a session's window
//!   with the same map as the snapshot and asks the OS for the focus, on the
//!   person's click only.
//! * Screen layouts ([`arrange`]) put already open terminal windows in grid
//!   cells, one monitor at a time, on the person's request only.
//!
//! Rules: only known agents ([`resume`]), never a permission-bypass flag,
//! nothing closes a window or ends a process. A restore run moves only
//! windows it opened itself; a screen layout moves any terminal window the
//! backend lists, and nothing else. Scope, TOTP step-up and audit are
//! enforced in [`crate::api`] with the remote shell service.

pub mod arrange;
pub mod backend;
pub mod capture;
pub mod geometry;
pub mod import;
pub mod launch;
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
mod linux;
#[cfg(any(test, target_os = "macos"))]
mod macos;
pub mod matching;
pub mod plan;
pub mod resume;
pub mod shortcut;
pub mod store;
#[cfg(any(test, unix))]
mod tmux;
pub mod validate;
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use vca_adapters::ProcessTable;
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, ControlTier, Error, RestoreEdit, RestoreItemStatus, RestoreJob, RestoreMonitor,
    RestoreOverview, RestoreRunRequest, ServerMsg, SessionId, SessionOrigin, SessionState,
    now_millis,
};

use crate::config::{RestoreConfig, RestoreLaunchMode};
use crate::sessions::SessionManager;
use crate::util::lock;
use crate::workspace::WorkspaceRoots;

pub use self::backend::{
    FakeBackend, FakeCall, LiveWindow, ManagedLaunch, OpenRequest, UnsupportedBackend,
    WindowBackend, platform_backend,
};
pub use self::matching::LiveSession;
use self::plan::{FolderCheck, PlannedWindow, Selection, Situation, live_status};
use self::store::RestoreFile;

/// Why an item opened without being put in place.
const NO_POSITION_NOTE: &str = "opened, but not placed: no position is saved for it";

/// How long a run waits for the window it opened.
const TITLE_WAIT: Duration = Duration::from_secs(30);
const TITLE_POLL: Duration = Duration::from_millis(500);

/// Reads the running sessions; the process table is passed when the caller
/// needs process ancestors and command lines.
pub type LiveSource = Arc<dyn Fn(Option<&ProcessTable>) -> Vec<LiveSession> + Send + Sync>;

/// Stops a run early (the kill switch for remote runs).
pub type StopCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Settings of the restore service.
#[derive(Debug, Clone)]
pub struct RestoreSettings {
    pub path: PathBuf,
    /// `None`: no timed snapshot.
    pub interval: Option<Duration>,
    pub auto_register: bool,
    pub title_wait: Duration,
    pub title_poll: Duration,
    /// Opened windows run `vca run` (`launch_mode = "managed"`); `None`:
    /// they run the agent directly.
    pub managed: Option<ManagedLaunch>,
}

impl RestoreSettings {
    /// `control_endpoint`: where this vcad listens, handed to `vca run` in
    /// the windows it opens.
    pub fn new(data_dir: &Path, config: &RestoreConfig, control_endpoint: &Path) -> Self {
        let vcad = std::env::current_exe().ok();
        Self {
            path: data_dir.join(store::RESTORE_FILE),
            interval: config.enabled.then(|| config.snapshot_interval()),
            auto_register: config.auto_register,
            title_wait: TITLE_WAIT,
            title_poll: TITLE_POLL,
            managed: managed_launch(config.launch_mode, vcad.as_deref(), control_endpoint),
        }
    }
}

/// The managed launch for `mode`: `vca` next to `vcad` (an absolute path,
/// never a `PATH` lookup). Whether the file exists is checked when a
/// window opens, which then falls back to the direct launch.
pub fn managed_launch(
    mode: RestoreLaunchMode,
    vcad: Option<&Path>,
    control_endpoint: &Path,
) -> Option<ManagedLaunch> {
    if mode != RestoreLaunchMode::Managed {
        return None;
    }
    let dir = vcad?.parent()?;
    if !dir.is_absolute() {
        return None;
    }
    Some(ManagedLaunch {
        vca: dir.join(format!("vca{}", std::env::consts::EXE_SUFFIX)),
        endpoint: control_endpoint.to_owned(),
    })
}

/// The running sessions of the session manager that can be restored.
pub fn live_sessions(mgr: &SessionManager, procs: Option<&ProcessTable>) -> Vec<LiveSession> {
    mgr.all()
        .into_iter()
        .filter_map(|s| {
            let info = s.info();
            if matches!(info.state, SessionState::Exited { .. }) {
                return None;
            }
            let SessionOrigin::Local {
                agent,
                project_dir,
                pid,
            } = info.origin
            else {
                return None;
            };
            if matches!(agent, AgentKind::Shell(_)) {
                return None;
            }
            let vca_started = info.control == ControlTier::ManagedPty;
            let mut pids = Vec::new();
            let mut args = Vec::new();
            if let Some(pid) = pid {
                pids.push(pid);
                if let Some(procs) = procs {
                    pids.extend(procs.ancestors(pid));
                    if let Some(p) = procs.get(pid) {
                        // Snapshots read only candidate processes; a session
                        // of another program is read here, by its PID.
                        let cmd = if p.cmd.is_empty() {
                            vca_adapters::process::process_cmd(pid).unwrap_or_default()
                        } else {
                            p.cmd.clone()
                        };
                        args = cmd.into_iter().skip(1).collect();
                    }
                }
            }
            Some(LiveSession {
                id: info.id,
                agent,
                folder: project_dir,
                session_id: s.agent_session_id(),
                title: info.display_name,
                custom_name: info.custom_name,
                vca_started,
                pids,
                transcript: s.transcript_path(),
                args,
            })
        })
        .collect()
}

#[derive(Default)]
struct State {
    file: RestoreFile,
    last_snapshot_at: Option<u64>,
    job: Option<RestoreJob>,
    /// Where the windows of the last arrangement were before it.
    undo: Vec<arrange::UndoWindow>,
    /// What `place` answered when it turned out this desktop cannot move
    /// windows.
    cannot_place: Option<String>,
}

struct Inner {
    settings: RestoreSettings,
    backend: Arc<dyn WindowBackend>,
    live: LiveSource,
    roots: WorkspaceRoots,
    notices: broadcast::Sender<ServerMsg>,
    state: Mutex<State>,
    /// Writes of the list file, for the "skip when unchanged" check.
    writes: AtomicU64,
    timer: Mutex<Option<JoinHandle<()>>>,
    /// One arrangement at a time: each one reads the positions it may undo.
    arranging: tokio::sync::Mutex<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(t) = lock(&self.timer).take() {
            t.abort();
        }
    }
}

/// The session restore service. Cheap to clone.
#[derive(Clone)]
pub struct Restore {
    inner: Arc<Inner>,
}

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

/// What the machine looks like, read on a blocking thread.
struct Sight {
    live: Vec<LiveSession>,
    windows: Vec<LiveWindow>,
    owners: HashMap<u32, u64>,
    monitors: Vec<RestoreMonitor>,
}

impl Sight {
    fn situation(&self) -> Situation<'_> {
        Situation {
            live: &self.live,
            windows: &self.windows,
            monitors: &self.monitors,
        }
    }
}

impl Restore {
    pub fn new(
        settings: RestoreSettings,
        backend: Arc<dyn WindowBackend>,
        live: LiveSource,
        roots: WorkspaceRoots,
        notices: broadcast::Sender<ServerMsg>,
    ) -> Self {
        let file = store::load(&settings.path);
        Self {
            inner: Arc::new(Inner {
                settings,
                backend,
                live,
                roots,
                notices,
                state: Mutex::new(State {
                    file,
                    ..State::default()
                }),
                writes: AtomicU64::new(0),
                timer: Mutex::new(None),
                arranging: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Starts the timed snapshot (none when off or on an unsupported
    /// platform). The first one runs one interval after the start, when
    /// discovery has seen the sessions.
    pub fn start(&self) {
        let Some(interval) = self.inner.settings.interval else {
            return;
        };
        if self.inner.backend.unsupported().is_some() {
            return;
        }
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if let Err(err) = (Restore { inner }).snapshot(None).await {
                    tracing::warn!(%err, "the restore snapshot failed");
                }
            }
        });
        if let Some(old) = lock(&self.inner.timer).replace(task) {
            old.abort();
        }
    }

    /// How often the list file was written since the start.
    pub fn writes(&self) -> u64 {
        self.inner.writes.load(Ordering::Relaxed)
    }

    /// Changes the list and stores it; nothing is written when the change
    /// made no difference.
    fn commit<T>(
        &self,
        change: impl FnOnce(&mut RestoreFile) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut state = lock(&self.inner.state);
        let mut next = state.file.clone();
        let out = change(&mut next)?;
        if next != state.file {
            next.saved_at = now_millis();
            store::save(&self.inner.settings.path, &next)?;
            self.inner.writes.fetch_add(1, Ordering::Relaxed);
            state.file = next;
        }
        Ok(out)
    }

    async fn sight(&self, with_procs: bool) -> Result<Sight, Error> {
        let backend = Arc::clone(&self.inner.backend);
        let live = Arc::clone(&self.inner.live);
        tokio::task::spawn_blocking(move || {
            let procs = with_procs.then(ProcessTable::snapshot);
            Ok(Sight {
                live: live(procs.as_ref()),
                windows: backend.windows()?,
                owners: backend.console_owners()?,
                monitors: backend.monitors()?,
            })
        })
        .await
        .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?
    }

    /// Measures the open windows into `profile` (the active one by
    /// default). Returns whether the list changed.
    pub async fn snapshot(&self, profile: Option<&str>) -> Result<bool, Error> {
        let sight = self.sight(true).await?;
        let auto_register = self.inner.settings.auto_register;
        let changed = self.commit(|file| {
            let key = profile.map_or_else(|| file.active.clone(), str::to_owned);
            let id = validate::find_profile(file, &key)
                .map(|p| p.id.clone())
                .ok_or_else(|| api_error(ErrorCode::NotFound, format!("no profile {key}")))?;
            let Some(target) = file.profiles.iter_mut().find(|p| p.id == id) else {
                return Ok(false);
            };
            Ok(capture::apply(
                target,
                capture::Observation {
                    live: &sight.live,
                    windows: &sight.windows,
                    owners: &sight.owners,
                    monitors: &sight.monitors,
                    now: now_millis(),
                    auto_register,
                },
            ))
        })?;
        lock(&self.inner.state).last_snapshot_at = Some(now_millis());
        Ok(changed)
    }

    /// Everything the Windows menu shows.
    pub async fn overview(&self) -> Result<RestoreOverview, Error> {
        let sight = self.sight(false).await?;
        let state = lock(&self.inner.state);
        let live = state
            .file
            .profiles
            .iter()
            .flat_map(|p| live_status(&p.entries, sight.situation()))
            .collect();
        let backend = &self.inner.backend;
        Ok(RestoreOverview {
            supported: backend.unsupported().is_none(),
            platform: backend.platform().to_owned(),
            message: backend.unsupported(),
            snapshot_interval_secs: self
                .inner
                .settings
                .interval
                .map_or(0, |d| d.as_secs().min(u64::from(u32::MAX)) as u32),
            last_snapshot_at: state.last_snapshot_at,
            active_profile: state.file.active.clone(),
            profiles: state.file.profiles.clone(),
            live,
            monitors: sight.monitors,
            job: state.job.clone(),
        })
    }

    /// Applies an edit. `local`: the owner on this computer.
    pub fn edit(&self, edit: &RestoreEdit, local: bool) -> Result<(), Error> {
        self.commit(|file| validate::apply_edit(file, edit, local, now_millis()))
    }

    /// Imports a `layout.json` into `profile` (the active one by default).
    /// Returns how many windows the file had.
    pub async fn import(&self, layout: &str, profile: Option<&str>) -> Result<usize, Error> {
        let sight = self.sight(false).await?;
        self.commit(|file| {
            let key = profile.map_or_else(|| file.active.clone(), str::to_owned);
            let id = validate::find_profile(file, &key)
                .map(|p| p.id.clone())
                .ok_or_else(|| api_error(ErrorCode::NotFound, format!("no profile {key}")))?;
            let Some(target) = file.profiles.iter_mut().find(|p| p.id == id) else {
                return Ok(0);
            };
            import::import_layout(target, layout, &sight.monitors, now_millis())
        })
    }

    /// The sign-in delay stored for the toggle.
    pub fn sign_in_delay(&self) -> u32 {
        lock(&self.inner.state).file.sign_in_delay_secs
    }

    pub fn set_sign_in_delay(&self, delay_secs: u32) -> Result<(), Error> {
        self.commit(|file| {
            file.sign_in_delay_secs = delay_secs;
            Ok(())
        })
    }

    fn folder_check(&self, remote: bool) -> impl Fn(&Path) -> FolderCheck + '_ {
        move |dir: &Path| {
            if !dir.is_dir() {
                return FolderCheck::Missing;
            }
            if !remote {
                return FolderCheck::Ok;
            }
            match self.inner.roots.prepare(dir, false) {
                Ok(_) => FolderCheck::Ok,
                Err(err) if err.code() == ErrorCode::NotFound => FolderCheck::Missing,
                Err(_) => FolderCheck::Outside,
            }
        }
    }

    /// Plans a restore and, unless it is a dry run, starts it. `remote`
    /// runs only open folders inside the workspace roots. Answers with the
    /// job as it stands; progress follows as `RestoreProgress` pushes.
    pub async fn run(
        &self,
        by: &str,
        request: &RestoreRunRequest,
        remote: bool,
        stop: Option<StopCheck>,
    ) -> Result<RestoreJob, Error> {
        if let Some(reason) = self.inner.backend.unsupported() {
            return Err(api_error(ErrorCode::Unsupported, reason));
        }
        let sight = self.sight(false).await?;
        let (profile, plans) = {
            let state = lock(&self.inner.state);
            if !request.dry_run && state.job.as_ref().is_some_and(|j| j.finished_at.is_none()) {
                return Err(api_error(
                    ErrorCode::Conflict,
                    "a restore is already running",
                ));
            }
            let key = request.profile.as_deref().unwrap_or(&state.file.active);
            let profile = validate::find_profile(&state.file, key)
                .ok_or_else(|| api_error(ErrorCode::NotFound, format!("no profile {key}")))?
                .clone();
            let folder = self.folder_check(remote);
            let plans = plan::plan(
                &profile,
                Selection {
                    only: &request.only,
                    entry_ids: &request.entry_ids,
                },
                sight.situation(),
                &folder,
                &|path| path.is_file(),
            );
            (profile, plans)
        };
        let now = now_millis();
        let mut job = RestoreJob {
            id: format!("r{}", uuid::Uuid::new_v4().simple()),
            profile: profile.name,
            by: by.to_owned(),
            dry_run: request.dry_run,
            started_at: now,
            finished_at: None,
            items: plans.iter().map(|p| p.item.clone()).collect(),
        };
        if request.dry_run {
            job.finished_at = Some(now);
            return Ok(job);
        }
        lock(&self.inner.state).job = Some(job.clone());
        self.publish();
        let this = self.clone();
        tokio::spawn(async move { this.drive(plans, stop).await });
        Ok(job)
    }

    /// Tells every client how the run stands.
    fn publish(&self) {
        if let Some(job) = lock(&self.inner.state).job.clone() {
            let _ = self.inner.notices.send(ServerMsg::RestoreProgress { job });
        }
    }

    fn set_item(&self, index: usize, status: RestoreItemStatus, message: Option<String>) {
        if let Some(job) = lock(&self.inner.state).job.as_mut() {
            if let Some(item) = job.items.get_mut(index) {
                item.status = status;
                if message.is_some() {
                    item.message = message;
                }
            }
        }
        self.publish();
    }

    async fn windows_now(&self) -> Result<Vec<LiveWindow>, Error> {
        let backend = Arc::clone(&self.inner.backend);
        tokio::task::spawn_blocking(move || backend.windows())
            .await
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?
    }

    /// Opens one window and moves it. Other windows are only ever read.
    /// Answers the item's status and, when it opened without being placed,
    /// why.
    async fn restore_one(
        &self,
        planned: &PlannedWindow,
    ) -> Result<(RestoreItemStatus, Option<String>), Error> {
        let Some(mut open) = planned.open.clone() else {
            return Ok((planned.item.status, None));
        };
        open.managed = self.inner.settings.managed.clone();
        let before = self.windows_now().await?;
        if before.iter().any(|w| w.title == open.title) {
            return Ok((RestoreItemStatus::SkippedOpen, None));
        }
        let known: HashSet<u64> = before.iter().map(|w| w.id).collect();
        let title = open.title.clone();
        let backend = Arc::clone(&self.inner.backend);
        tokio::task::spawn_blocking(move || backend.open(&open))
            .await
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))??;
        let deadline = Instant::now() + self.inner.settings.title_wait;
        let window = loop {
            let found = self
                .windows_now()
                .await?
                .into_iter()
                .find(|w| w.title == title && !known.contains(&w.id));
            if let Some(window) = found {
                break window;
            }
            if Instant::now() >= deadline {
                return Err(api_error(
                    ErrorCode::Unavailable,
                    "it opened, but its window did not appear in time: not moved",
                ));
            }
            tokio::time::sleep(self.inner.settings.title_poll).await;
        };
        let Some((rect, maximized)) = planned.place else {
            return Ok((RestoreItemStatus::Opened, Some(NO_POSITION_NOTE.to_owned())));
        };
        let backend = Arc::clone(&self.inner.backend);
        let placed = tokio::task::spawn_blocking(move || backend.place(window.id, rect, maximized))
            .await
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
        match placed {
            Ok(()) => Ok((RestoreItemStatus::Placed, None)),
            // This platform cannot move windows: it opened all the same.
            Err(err) if err.code() == ErrorCode::Unsupported => Ok((
                RestoreItemStatus::Opened,
                Some(crate::util::error_text(&err)),
            )),
            Err(err) => Err(err),
        }
    }

    /// Brings the terminal window of a running session to the front. Only
    /// for the person's click on this computer (the caller checks that the
    /// request is local). The window is found as the snapshot finds it: the
    /// console owner first, then the one window with the exact title.
    pub async fn focus_session(&self, session: &SessionId) -> Result<(), Error> {
        if self.inner.backend.platform() != "windows" {
            return Err(api_error(
                ErrorCode::Unsupported,
                backend::FOCUS_UNSUPPORTED,
            ));
        }
        let sight = self.sight(true).await?;
        let Some(live) = sight.live.iter().find(|s| &s.id == session) else {
            return Err(api_error(
                ErrorCode::NotFound,
                "that session is not running in a terminal on this computer",
            ));
        };
        let entry_title = {
            let state = lock(&self.inner.state);
            let index = sight.live.iter().position(|s| &s.id == session);
            // The active profile's entry first: it is the list in use.
            let mut profiles: Vec<_> = state.file.profiles.iter().collect();
            profiles.sort_by_key(|p| p.id != state.file.active);
            profiles.into_iter().find_map(|p| {
                let hits = matching::match_entries(&p.entries, &sight.live);
                let entry = hits.get(index?).copied().flatten()?;
                p.entries.get(entry).map(|e| e.title.clone())
            })
        };
        let window =
            matching::window_of(live, entry_title.as_deref(), &sight.windows, &sight.owners)
                .ok_or_else(|| {
                    api_error(ErrorCode::NotFound, "its terminal window was not found")
                })?;
        let id = window.id;
        let backend = Arc::clone(&self.inner.backend);
        tokio::task::spawn_blocking(move || backend.focus(id))
            .await
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?
    }

    async fn drive(&self, plans: Vec<PlannedWindow>, stop: Option<StopCheck>) {
        for (index, planned) in plans.iter().enumerate() {
            if planned.open.is_none() {
                continue;
            }
            if stop.as_ref().is_some_and(|s| s()) {
                self.set_item(
                    index,
                    RestoreItemStatus::Failed,
                    Some("stopped by the kill switch".into()),
                );
                continue;
            }
            self.set_item(index, RestoreItemStatus::Opening, None);
            match self.restore_one(planned).await {
                Ok((status, message)) => self.set_item(index, status, message),
                Err(err) => self.set_item(
                    index,
                    RestoreItemStatus::Failed,
                    Some(crate::util::error_text(&err)),
                ),
            }
            let pause = planned.item.delay_after_secs;
            if pause > 0 {
                tokio::time::sleep(Duration::from_secs(u64::from(pause))).await;
            }
        }
        if let Some(job) = lock(&self.inner.state).job.as_mut() {
            job.finished_at = Some(now_millis());
        }
        self.publish();
    }
}
