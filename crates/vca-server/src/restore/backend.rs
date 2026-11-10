//! The platform behind session restore. The planner and the job runner only
//! see this trait, so tests run on the [`FakeBackend`] and later platforms
//! (macOS: Terminal.app and iTerm2 through AppleScript; Linux: gnome-terminal,
//! konsole and kitty with wmctrl/xdotool on X11, open-only on Wayland; a
//! headless GPU server and other headless machines: tmux sessions named after
//! the entry, no placement) only add a backend.
//!
//! Rules every backend keeps: reading windows changes nothing; a restore run
//! calls `place` only on a window it opened itself; a screen layout
//! ([`super::arrange`]) moves any open terminal window, after finding it in
//! `windows` again; `focus` only after the person clicked for it; nothing is
//! closed.
//!
//! A backend that cannot move windows (Wayland, X11 without wmctrl or
//! xdotool, tmux) answers `place` with `unsupported` and a message: the run
//! then reports the window as opened but not placed, not as failed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use vca_core::error::ErrorCode;
use vca_core::{Error, RestoreMonitor, WindowRect};

use crate::util::lock;

/// A top-level terminal window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWindow {
    /// OS window handle.
    pub id: u64,
    pub title: String,
    pub rect: WindowRect,
    pub maximized: bool,
    /// Minimized windows report no usable rectangle.
    pub minimized: bool,
}

/// One window to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// The list entry it restores: stable across renames, so a backend can
    /// name what it opens after it (tmux sessions).
    pub entry_id: String,
    pub title: String,
    pub folder: PathBuf,
    /// Agent executable name (`claude`, `codex`, ...).
    pub program: String,
    pub args: Vec<String>,
    /// Run the agent as a session managed by vcad (`vca run`) instead of
    /// directly, where the backend supports it (Windows for now).
    pub managed: Option<ManagedLaunch>,
}

/// How a window starts a managed session: `vca --endpoint <endpoint> run
/// --name <title> -- <agent> <args>`, with the `vca` installed next to
/// vcad. A backend falls back to the direct launch when that file is
/// missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedLaunch {
    /// Absolute path of `vca` next to vcad; never looked up on `PATH`.
    pub vca: PathBuf,
    /// vcad's control endpoint, so the window reaches this vcad.
    pub endpoint: PathBuf,
}

pub trait WindowBackend: Send + Sync {
    /// `windows`, `macos`, `linux`, ...
    fn platform(&self) -> &'static str;
    /// `Some(reason)` when windows cannot be restored here.
    fn unsupported(&self) -> Option<String>;
    fn monitors(&self) -> Result<Vec<RestoreMonitor>, Error>;
    /// Open terminal windows. Read-only.
    fn windows(&self) -> Result<Vec<LiveWindow>, Error>;
    /// Which window each console belongs to: the pid of the first process
    /// on a console to the window handle of its terminal. Read-only.
    fn console_owners(&self) -> Result<HashMap<u32, u64>, Error>;
    /// Opens a terminal running the agent in the folder, titled `title`.
    fn open(&self, request: &OpenRequest) -> Result<(), Error>;
    /// Moves a terminal window from `windows`, un-maximizing or restoring it
    /// first when needed, without taking the focus. `rect` is the outer
    /// rectangle `windows` reports. `unsupported` (with the reason as
    /// message) when this platform cannot move windows.
    fn place(&self, id: u64, rect: WindowRect, maximized: bool) -> Result<(), Error>;
    /// Like `place`, but `rect` is what should be seen on screen: a backend
    /// whose windows have invisible borders widens the outer rectangle by
    /// them, so neighbouring windows touch.
    fn place_visible(&self, id: u64, rect: WindowRect) -> Result<(), Error> {
        self.place(id, rect, false)
    }
    /// Minimizes a window without activating another one, to undo an
    /// arrangement that un-minimized it.
    fn minimize(&self, _id: u64) -> Result<(), Error> {
        Err(Error::api(
            ErrorCode::Unsupported,
            "minimizing a window works on Windows only for now",
        ))
    }
    /// Brings a terminal window to the front, only ever after the person
    /// clicked for it. The OS may refuse to hand over the focus: that is an
    /// `unavailable` error, never worked around.
    fn focus(&self, _id: u64) -> Result<(), Error> {
        Err(Error::api(
            ErrorCode::Unsupported,
            FOCUS_UNSUPPORTED.to_owned(),
        ))
    }
}

/// What platforms without [`WindowBackend::focus`] answer.
pub const FOCUS_UNSUPPORTED: &str =
    "bringing a terminal to the front works on Windows only for now";

/// What other platforms answer until they get a backend.
pub struct UnsupportedBackend {
    platform: &'static str,
}

impl UnsupportedBackend {
    pub fn new(platform: &'static str) -> Self {
        Self { platform }
    }

    fn message(&self) -> String {
        format!(
            "session restore is available on Windows only for now (this computer runs {})",
            self.platform
        )
    }

    fn error(&self) -> Error {
        Error::api(ErrorCode::Unsupported, self.message())
    }
}

impl WindowBackend for UnsupportedBackend {
    fn platform(&self) -> &'static str {
        self.platform
    }

    fn unsupported(&self) -> Option<String> {
        Some(self.message())
    }

    fn monitors(&self) -> Result<Vec<RestoreMonitor>, Error> {
        Ok(Vec::new())
    }

    fn windows(&self) -> Result<Vec<LiveWindow>, Error> {
        Ok(Vec::new())
    }

    fn console_owners(&self) -> Result<HashMap<u32, u64>, Error> {
        Ok(HashMap::new())
    }

    fn open(&self, _: &OpenRequest) -> Result<(), Error> {
        Err(self.error())
    }

    fn place(&self, _: u64, _: WindowRect, _: bool) -> Result<(), Error> {
        Err(self.error())
    }
}

/// The backend of this computer.
pub fn platform_backend() -> Arc<dyn WindowBackend> {
    #[cfg(windows)]
    {
        Arc::new(super::windows::WindowsBackend::system())
    }
    #[cfg(target_os = "macos")]
    {
        super::macos::backend()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        super::linux::backend()
    }
    #[cfg(not(any(windows, unix)))]
    {
        Arc::new(UnsupportedBackend::new(std::env::consts::OS))
    }
}

/// What happened on a [`FakeBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    Open(OpenRequest),
    Place {
        id: u64,
        rect: WindowRect,
        maximized: bool,
    },
    Focus {
        id: u64,
    },
    Minimize {
        id: u64,
    },
}

#[derive(Default)]
struct FakeState {
    windows: Vec<LiveWindow>,
    owners: HashMap<u32, u64>,
    monitors: Vec<RestoreMonitor>,
    calls: Vec<FakeCall>,
    next_id: u64,
    /// `open` of these titles fails.
    fail_open: Vec<String>,
    /// Opened windows of these titles never show up.
    never_appears: Vec<String>,
    /// `place` answers `unsupported` with this message.
    cannot_place: Option<String>,
    /// `focus` is refused, as Windows does when another app has the focus.
    refuse_focus: bool,
    /// `place` of these windows fails.
    fail_place: Vec<u64>,
}

/// An in-memory desktop for tests and dry runs: no real window, process or
/// monitor is touched.
#[derive(Default)]
pub struct FakeBackend {
    state: Mutex<FakeState>,
}

impl FakeBackend {
    pub fn new() -> Arc<Self> {
        let backend = Self::default();
        lock(&backend.state).next_id = 1000;
        Arc::new(backend)
    }

    pub fn set_monitors(&self, monitors: Vec<RestoreMonitor>) {
        lock(&self.state).monitors = monitors;
    }

    pub fn set_windows(&self, windows: Vec<LiveWindow>) {
        lock(&self.state).windows = windows;
    }

    pub fn set_owner(&self, pid: u32, window: u64) {
        lock(&self.state).owners.insert(pid, window);
    }

    pub fn fail_open(&self, title: &str) {
        lock(&self.state).fail_open.push(title.to_owned());
    }

    pub fn never_appears(&self, title: &str) {
        lock(&self.state).never_appears.push(title.to_owned());
    }

    /// Makes this desktop one that cannot move windows (Wayland, tmux).
    pub fn cannot_place(&self, reason: &str) {
        lock(&self.state).cannot_place = Some(reason.to_owned());
    }

    pub fn refuse_focus(&self) {
        lock(&self.state).refuse_focus = true;
    }

    pub fn fail_place(&self, id: u64) {
        lock(&self.state).fail_place.push(id);
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        lock(&self.state).calls.clone()
    }

    pub fn opened_titles(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                FakeCall::Open(req) => Some(req.title),
                FakeCall::Place { .. } | FakeCall::Focus { .. } | FakeCall::Minimize { .. } => None,
            })
            .collect()
    }
}

impl WindowBackend for FakeBackend {
    fn platform(&self) -> &'static str {
        "windows"
    }

    fn unsupported(&self) -> Option<String> {
        None
    }

    fn monitors(&self) -> Result<Vec<RestoreMonitor>, Error> {
        Ok(lock(&self.state).monitors.clone())
    }

    fn windows(&self) -> Result<Vec<LiveWindow>, Error> {
        Ok(lock(&self.state).windows.clone())
    }

    fn console_owners(&self) -> Result<HashMap<u32, u64>, Error> {
        Ok(lock(&self.state).owners.clone())
    }

    fn open(&self, request: &OpenRequest) -> Result<(), Error> {
        let mut state = lock(&self.state);
        state.calls.push(FakeCall::Open(request.clone()));
        if state.fail_open.contains(&request.title) {
            return Err(Error::api(ErrorCode::Unavailable, "cannot start wt.exe"));
        }
        if state.never_appears.contains(&request.title) {
            return Ok(());
        }
        state.next_id += 1;
        let id = state.next_id;
        state.windows.push(LiveWindow {
            id,
            title: request.title.clone(),
            rect: WindowRect {
                x: 10,
                y: 10,
                w: 800,
                h: 600,
            },
            maximized: false,
            minimized: false,
        });
        Ok(())
    }

    fn place(&self, id: u64, rect: WindowRect, maximized: bool) -> Result<(), Error> {
        let mut state = lock(&self.state);
        if let Some(reason) = &state.cannot_place {
            return Err(Error::api(ErrorCode::Unsupported, reason.clone()));
        }
        state.calls.push(FakeCall::Place {
            id,
            rect,
            maximized,
        });
        if state.fail_place.contains(&id) {
            return Err(Error::api(
                ErrorCode::Unavailable,
                "the window could not be moved",
            ));
        }
        if let Some(window) = state.windows.iter_mut().find(|w| w.id == id) {
            window.rect = rect;
            window.maximized = maximized;
            window.minimized = false;
        }
        Ok(())
    }

    fn minimize(&self, id: u64) -> Result<(), Error> {
        let mut state = lock(&self.state);
        state.calls.push(FakeCall::Minimize { id });
        if let Some(window) = state.windows.iter_mut().find(|w| w.id == id) {
            window.minimized = true;
        }
        Ok(())
    }

    fn focus(&self, id: u64) -> Result<(), Error> {
        let mut state = lock(&self.state);
        if !state.windows.iter().any(|w| w.id == id) {
            return Err(Error::api(ErrorCode::NotFound, "the window is gone"));
        }
        state.calls.push(FakeCall::Focus { id });
        if state.refuse_focus {
            return Err(Error::api(
                ErrorCode::Unavailable,
                "Windows did not let VCA bring it forward",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_platforms_answer_unsupported_with_a_clear_message() {
        let backend = UnsupportedBackend::new("linux");
        let reason = backend.unsupported().unwrap();
        assert!(reason.contains("Windows only"), "{reason}");
        assert!(reason.contains("linux"), "{reason}");
        assert!(backend.windows().unwrap().is_empty());
        let request = OpenRequest {
            entry_id: "e1".into(),
            title: "t".into(),
            folder: PathBuf::from("/tmp"),
            program: "claude".into(),
            args: Vec::new(),
            managed: None,
        };
        assert_eq!(
            backend.open(&request).unwrap_err().code(),
            ErrorCode::Unsupported
        );
        assert_eq!(backend.focus(1).unwrap_err().code(), ErrorCode::Unsupported);
    }
}
