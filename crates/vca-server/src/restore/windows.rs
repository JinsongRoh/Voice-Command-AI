//! The Windows backend.
//!
//! Reading is strictly read-only: `EnumWindows`, class names, titles,
//! rectangles, monitors. The console of a session is found without
//! attaching to it: every console has a hidden `PseudoConsoleWindow` (or,
//! outside Windows Terminal, a `ConsoleWindowClass` window) whose reported
//! process is the first process on that console, usually the tab's shell,
//! and `GetAncestor(GA_ROOTOWNER)` of that window is the terminal's window.
//! `AttachConsole` is never used.
//!
//! All coordinates are physical pixels: every call runs with per-monitor v2
//! DPI awareness set on the calling thread only.
//!
//! Only `open`, `place`, `place_visible`, `minimize` and `focus` change
//! anything: `open` starts a new terminal (Windows Terminal, or a console
//! host window when `wt.exe` is missing); `place` moves a terminal window: a
//! restore run moves the ones it opened, a screen layout any open terminal
//! window the caller found in `windows`, and the class check below refuses
//! everything else; `place_visible` does the same with the window's
//! invisible borders outside the rectangle; `minimize` puts a window back
//! when an arrangement is undone; `focus` brings a terminal window forward
//! after the person clicked for it. None is called by the snapshot, and no
//! code here closes a window or ends a process.
//!
//! `place` and `minimize` never activate a window on purpose. Maximizing has
//! no form that leaves the activation alone, so `place` hands the foreground
//! back to the window that held it (Windows may refuse that too).
//!
//! `focus` uses `SetForegroundWindow` and nothing else. Windows refuses it
//! when another app holds the focus (it flashes the taskbar button instead);
//! that is reported as "could not bring it forward", never worked around
//! with input simulation or by attaching to another thread's input.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use vca_core::error::ErrorCode;
use vca_core::{Error, RestoreMonitor, WindowRect};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
    MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOTOWNER, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, IsZoomed, SW_RESTORE, SW_SHOWMAXIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow,
};
use windows_sys::core::BOOL;

use super::arrange::outer_for_visible;
use super::backend::{LiveWindow, OpenRequest, WindowBackend};
use super::launch::{console_args, restored_env, window_command, wt_args};

const TERMINAL_CLASS: &str = "CASCADIA_HOSTING_WINDOW_CLASS";
const PSEUDO_CONSOLE_CLASS: &str = "PseudoConsoleWindow";
const CONSOLE_CLASS: &str = "ConsoleWindowClass";

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What `focus` says when Windows keeps the focus where it is.
pub const FOCUS_REFUSED: &str = "Windows did not let VCA bring the window forward: click its \
                                 taskbar button, which is flashing";

fn unavailable(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::Unavailable, message)
}

/// Per-monitor v2 DPI awareness on this thread, until dropped.
struct DpiScope(DPI_AWARENESS_CONTEXT);

impl DpiScope {
    fn enter() -> Self {
        // SAFETY: changes only the calling thread's awareness; restored on drop.
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for DpiScope {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: restores the value returned by the call in `enter`.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

fn rect_of(r: &RECT) -> WindowRect {
    WindowRect {
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
    }
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer outlives the call and its length is passed.
    let len = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn title_of(hwnd: HWND) -> String {
    // SAFETY: both calls only read the cached title of the window.
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..copied.max(0) as usize])
    }
}

/// What is seen of a window, without its invisible resize borders.
fn visible_frame(hwnd: HWND) -> Option<WindowRect> {
    // SAFETY: an all-zero RECT is valid; the buffer and its size are passed
    // and it outlives the call, which only reads the window.
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        let read = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        );
        (read == 0).then(|| rect_of(&rect))
    }
}

fn is_terminal_window(hwnd: HWND) -> bool {
    let class = class_of(hwnd);
    class == TERMINAL_CLASS || class == CONSOLE_CLASS
}

fn all_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Vec` below, alive for the whole enumeration.
        unsafe { (*(lparam as *mut Vec<HWND>)).push(hwnd) };
        TRUE
    }
    let mut out: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes to `out`.
    unsafe { EnumWindows(Some(collect), &mut out as *mut Vec<HWND> as LPARAM) };
    out
}

fn monitor_entry(handle: HMONITOR) -> Option<RestoreMonitor> {
    // SAFETY: an all-zero MONITORINFOEXW is valid and its size field is set.
    unsafe {
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(handle, &mut info.monitorInfo) == 0 {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
        if GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) != 0 {
            dpi_x = 96;
        }
        let name = &info.szDevice;
        let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        Some(RestoreMonitor {
            id: String::from_utf16_lossy(&name[..len]),
            rect: rect_of(&info.monitorInfo.rcMonitor),
            work: rect_of(&info.monitorInfo.rcWork),
            dpi: dpi_x,
        })
    }
}

/// Looks `name` up on `PATH` with each executable extension. The working
/// directory is not searched: a project must not shadow the agent.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions = [".exe", ".cmd", ".bat", ".com"];
    for dir in std::env::split_paths(&path).filter(|d| d.is_absolute()) {
        for ext in extensions {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The agent's executable: `PATH`, then the per-user install location that
/// a server started at sign-in may not have on its `PATH`.
fn find_agent(program: &str) -> Option<PathBuf> {
    find_on_path(program).or_else(|| {
        let home = PathBuf::from(std::env::var_os("USERPROFILE")?);
        let local = home.join(".local").join("bin");
        let npm = PathBuf::from(std::env::var_os("APPDATA")?).join("npm");
        [local, npm].into_iter().find_map(|dir| {
            [".exe", ".cmd"]
                .iter()
                .map(|ext| dir.join(format!("{program}{ext}")))
                .find(|p| p.is_file())
        })
    })
}

fn find_wt() -> Option<PathBuf> {
    find_on_path("wt").or_else(|| {
        let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        Some(local.join("Microsoft").join("WindowsApps").join("wt.exe")).filter(|p| p.is_file())
    })
}

/// Splits an environment block (`NAME=VALUE\0...\0\0`, UTF-16) into
/// variables. Entries whose name starts with `=` (the per-drive current
/// directories, `=C:=C:\...`) keep that `=` in the name.
fn parse_env_block(block: &[u16]) -> Vec<(OsString, OsString)> {
    use std::os::windows::ffi::OsStringExt;
    block
        .split(|c| *c == 0)
        .take_while(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let at = entry.iter().skip(1).position(|c| *c == u16::from(b'='))? + 1;
            Some((
                OsString::from_wide(&entry[..at]),
                OsString::from_wide(&entry[at + 1..]),
            ))
        })
        .collect()
}

/// The person's default environment, as Explorer gives a program started
/// from the desktop: the system and user variables from the registry and
/// the profile's (`CreateEnvironmentBlock` with this process's token,
/// without inheriting vcad's environment). Read-only; `None` when Windows
/// does not give it.
fn user_default_env() -> Option<Vec<(OsString, OsString)>> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY};
    use windows_sys::Win32::System::Environment::{
        CreateEnvironmentBlock, DestroyEnvironmentBlock,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of this process needs no closing; `token`
    // receives a handle closed below.
    let opened = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
            &mut token,
        )
    };
    if opened == 0 {
        return None;
    }
    let mut block: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `token` is valid until closed; `block` receives memory freed
    // with `DestroyEnvironmentBlock`. FALSE: nothing of vcad's environment.
    let created = unsafe { CreateEnvironmentBlock(&mut block, token, 0) };
    // SAFETY: closes the handle opened above, once.
    unsafe { CloseHandle(token) };
    if created == 0 || block.is_null() {
        return None;
    }
    // SAFETY: the block is a sequence of NUL-terminated UTF-16 strings
    // ending with an empty one; the length is found before slicing.
    let vars = unsafe {
        let start = block as *const u16;
        let mut len = 0usize;
        while !(*start.add(len) == 0 && *start.add(len + 1) == 0) {
            len += 1;
        }
        parse_env_block(std::slice::from_raw_parts(start, len + 2))
    };
    // SAFETY: frees the block created above, once.
    unsafe { DestroyEnvironmentBlock(block) };
    (!vars.is_empty()).then_some(vars)
}

/// One process to start for `open`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub flags: u32,
}

/// What `open` needs from the computer. The real one looks at `PATH` and
/// starts processes; tests give a fake one, so the choice between Windows
/// Terminal and the console host is checked without starting anything.
pub trait Launcher: Send + Sync {
    fn agent(&self, program: &str) -> Option<PathBuf>;
    fn powershell(&self) -> Result<PathBuf, Error>;
    /// `wt.exe`, when Windows Terminal is installed.
    fn wt(&self) -> Option<PathBuf>;
    /// Whether `path` is an existing file (the installed `vca`).
    fn is_file(&self, path: &Path) -> bool;
    fn spawn(&self, launch: &Launch) -> Result<(), Error>;
}

/// This computer.
pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn agent(&self, program: &str) -> Option<PathBuf> {
        find_agent(program)
    }

    fn powershell(&self) -> Result<PathBuf, Error> {
        Ok(crate::shell::resolve_shell("powershell")?.program)
    }

    fn wt(&self) -> Option<PathBuf> {
        find_wt()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn spawn(&self, launch: &Launch) -> Result<(), Error> {
        let mut cmd = Command::new(&launch.program);
        cmd.args(&launch.args)
            .creation_flags(launch.flags)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = &launch.cwd {
            cmd.current_dir(dir);
        }
        // The window starts with the person's default environment, as if
        // opened from the desktop, not with vcad's (which may come from a
        // Claude Code session or an installer). `wt.exe` hands it to the new
        // tab when Windows Terminal inherits environments; the tab's
        // PowerShell clears the session variables again in case it does not
        // (see `launch`).
        let base = user_default_env().unwrap_or_else(|| {
            tracing::warn!(
                "restore: cannot read the user's default environment; the window gets vcad's"
            );
            std::env::vars_os().collect()
        });
        cmd.env_clear().envs(restored_env(base));
        cmd.spawn()
            .map(drop)
            .map_err(|e| unavailable(format!("cannot start {}: {e}", launch.program.display())))
    }
}

/// What `open` starts: Windows Terminal with a fixed title, or, where it is
/// not installed, PowerShell in a console host window of its own that sets
/// the title itself. Inside, the window runs `vca run` with the agent as a
/// managed session when the request asks for it and `vca` is installed at
/// its absolute path, else the agent itself.
pub fn launch_for(launcher: &dyn Launcher, request: &OpenRequest) -> Result<Launch, Error> {
    let agent = launcher.agent(&request.program).ok_or_else(|| {
        unavailable(format!(
            "{} is not installed on this computer",
            request.program
        ))
    })?;
    let managed = request.managed.as_ref().filter(|m| {
        let installed = m.vca.is_absolute() && launcher.is_file(&m.vca);
        if !installed {
            tracing::warn!(
                vca = %m.vca.display(),
                "restore: vca is not installed next to vcad; the window runs the agent directly"
            );
        }
        installed
    });
    let (program, args) = window_command(request, &agent, managed);
    let powershell = launcher.powershell()?;
    Ok(match launcher.wt() {
        Some(wt) => Launch {
            args: wt_args(request, &powershell, &program, &args),
            program: wt,
            cwd: None,
            flags: CREATE_NO_WINDOW,
        },
        None => Launch {
            args: console_args(request, &program, &args),
            program: powershell,
            cwd: Some(request.folder.clone()),
            flags: CREATE_NEW_CONSOLE,
        },
    })
}

/// Windows Terminal, or a plain console window where it is not installed.
pub struct WindowsBackend<L: Launcher = SystemLauncher> {
    launcher: L,
}

impl WindowsBackend {
    pub fn system() -> Self {
        Self {
            launcher: SystemLauncher,
        }
    }
}

#[cfg(test)]
impl<L: Launcher> WindowsBackend<L> {
    pub fn with_launcher(launcher: L) -> Self {
        Self { launcher }
    }
}

impl<L: Launcher> WindowBackend for WindowsBackend<L> {
    fn platform(&self) -> &'static str {
        "windows"
    }

    fn unsupported(&self) -> Option<String> {
        None
    }

    fn monitors(&self) -> Result<Vec<RestoreMonitor>, Error> {
        unsafe extern "system" fn collect(
            monitor: HMONITOR,
            _: HDC,
            _: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            // SAFETY: `lparam` is the `Vec` below, alive for the whole call.
            let out = unsafe { &mut *(lparam as *mut Vec<RestoreMonitor>) };
            out.extend(monitor_entry(monitor));
            TRUE
        }
        let _dpi = DpiScope::enter();
        let mut out: Vec<RestoreMonitor> = Vec::new();
        // SAFETY: the callback only pushes to `out`.
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(collect),
                &mut out as *mut Vec<RestoreMonitor> as LPARAM,
            )
        };
        Ok(out)
    }

    fn windows(&self) -> Result<Vec<LiveWindow>, Error> {
        let _dpi = DpiScope::enter();
        let mut out = Vec::new();
        for hwnd in all_windows() {
            let class = class_of(hwnd);
            if class != TERMINAL_CLASS && class != CONSOLE_CLASS {
                continue;
            }
            // SAFETY: read-only queries of a window handle; a stale handle
            // makes them fail, which is handled.
            unsafe {
                if IsWindowVisible(hwnd) == 0 {
                    continue;
                }
                let mut rect: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut rect) == 0 {
                    continue;
                }
                out.push(LiveWindow {
                    id: hwnd as usize as u64,
                    title: title_of(hwnd),
                    rect: rect_of(&rect),
                    maximized: IsZoomed(hwnd) != 0,
                    minimized: IsIconic(hwnd) != 0,
                });
            }
        }
        Ok(out)
    }

    fn console_owners(&self) -> Result<HashMap<u32, u64>, Error> {
        let mut out = HashMap::new();
        for hwnd in all_windows() {
            let class = class_of(hwnd);
            if class != PSEUDO_CONSOLE_CLASS && class != CONSOLE_CLASS {
                continue;
            }
            let mut pid = 0u32;
            // SAFETY: read-only queries; `pid` outlives the call.
            let root = unsafe {
                GetWindowThreadProcessId(hwnd, &mut pid);
                GetAncestor(hwnd, GA_ROOTOWNER)
            };
            if pid != 0 && !root.is_null() {
                out.insert(pid, root as usize as u64);
            }
        }
        Ok(out)
    }

    fn open(&self, request: &OpenRequest) -> Result<(), Error> {
        let launch = launch_for(&self.launcher, request)?;
        self.launcher.spawn(&launch)
    }

    fn place(&self, id: u64, rect: WindowRect, maximized: bool) -> Result<(), Error> {
        let _dpi = DpiScope::enter();
        let hwnd = id as usize as HWND;
        // SAFETY: `hwnd` came from `windows()`; `IsWindow` and the class check
        // reject a handle that went away or was reused.
        unsafe {
            if IsWindow(hwnd) == 0 {
                return Err(unavailable("the window is gone"));
            }
            let class = class_of(hwnd);
            if class != TERMINAL_CLASS && class != CONSOLE_CLASS {
                return Err(unavailable("that is no longer a terminal window"));
            }
            if IsZoomed(hwnd) != 0 || IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW;
            let place = || {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h,
                    flags,
                )
            };
            if place() == 0 {
                return Err(unavailable("the window could not be moved"));
            }
            // Crossing monitors with another scaling resizes the window once
            // more; set the size again when it did.
            let mut now: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut now) != 0 && rect_of(&now) != rect {
                place();
            }
            if maximized {
                let foreground = GetForegroundWindow();
                ShowWindow(hwnd, SW_SHOWMAXIMIZED);
                if !foreground.is_null() && foreground != hwnd {
                    SetForegroundWindow(foreground);
                }
            }
        }
        Ok(())
    }

    fn place_visible(&self, id: u64, rect: WindowRect) -> Result<(), Error> {
        self.place(id, rect, false)?;
        // Measured after the move: the borders follow the monitor's scaling.
        let _dpi = DpiScope::enter();
        let hwnd = id as usize as HWND;
        // SAFETY: an all-zero RECT is valid and outlives the read-only query;
        // a handle that went away makes it fail, which is handled.
        let outer = unsafe {
            let mut now: RECT = std::mem::zeroed();
            (GetWindowRect(hwnd, &mut now) != 0).then(|| rect_of(&now))
        };
        let (Some(outer), Some(visible)) = (outer, visible_frame(hwnd)) else {
            return Ok(());
        };
        let wanted = outer_for_visible(&rect, &outer, &visible);
        if wanted == rect {
            return Ok(());
        }
        self.place(id, wanted, false)
    }

    fn minimize(&self, id: u64) -> Result<(), Error> {
        let hwnd = id as usize as HWND;
        // SAFETY: `hwnd` came from `windows()`; `IsWindow` and the class check
        // reject a handle that went away or was reused.
        unsafe {
            if IsWindow(hwnd) == 0 {
                return Err(unavailable("the window is gone"));
            }
            if !is_terminal_window(hwnd) {
                return Err(unavailable("that is no longer a terminal window"));
            }
            ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
        }
        Ok(())
    }

    fn focus(&self, id: u64) -> Result<(), Error> {
        let hwnd = id as usize as HWND;
        // SAFETY: `hwnd` came from `windows()`; `IsWindow` and the class check
        // reject a handle that went away or was reused. Only documented
        // calls: Windows decides whether the focus moves.
        unsafe {
            if IsWindow(hwnd) == 0 {
                return Err(Error::api(ErrorCode::NotFound, "the window is gone"));
            }
            let class = class_of(hwnd);
            if class != TERMINAL_CLASS && class != CONSOLE_CLASS {
                return Err(Error::api(
                    ErrorCode::NotFound,
                    "that is no longer a terminal window",
                ));
            }
            if SetForegroundWindow(hwnd) == 0 {
                return Err(unavailable(FOCUS_REFUSED));
            }
            // Only once it has the focus: a refused request changes nothing.
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::Mutex;

    /// A computer that starts nothing: it records what `open` would start.
    #[derive(Default)]
    struct FakeLauncher {
        wt: Option<PathBuf>,
        no_agent: bool,
        /// `vca` is installed at [`VCA`].
        vca: bool,
        started: Mutex<Vec<Launch>>,
    }

    const PS: &str = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
    const CLAUDE: &str = "C:\\Users\\me\\.local\\bin\\claude.exe";
    const WT: &str = "C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps\\wt.exe";
    const VCA: &str = "C:\\Users\\me\\AppData\\Local\\Voice Command AI\\vca.exe";

    impl Launcher for FakeLauncher {
        fn agent(&self, _: &str) -> Option<PathBuf> {
            (!self.no_agent).then(|| PathBuf::from(CLAUDE))
        }

        fn powershell(&self) -> Result<PathBuf, Error> {
            Ok(PathBuf::from(PS))
        }

        fn wt(&self) -> Option<PathBuf> {
            self.wt.clone()
        }

        fn is_file(&self, path: &Path) -> bool {
            self.vca && path == Path::new(VCA)
        }

        fn spawn(&self, launch: &Launch) -> Result<(), Error> {
            self.started.lock().unwrap().push(launch.clone());
            Ok(())
        }
    }

    fn request() -> OpenRequest {
        OpenRequest {
            entry_id: "e1".into(),
            title: "Payments".into(),
            folder: PathBuf::from("E:\\work\\app"),
            program: "claude".into(),
            args: vec!["--resume".into(), "ab-1".into()],
            managed: None,
        }
    }

    fn managed_request(vca: &str) -> OpenRequest {
        OpenRequest {
            managed: Some(super::super::backend::ManagedLaunch {
                vca: PathBuf::from(vca),
                endpoint: PathBuf::from("vca-control-me"),
            }),
            ..request()
        }
    }

    fn direct_args() -> Vec<String> {
        request().args
    }

    #[test]
    fn without_windows_terminal_it_opens_a_console_host_window() {
        let backend = WindowsBackend::with_launcher(FakeLauncher::default());
        backend.open(&request()).unwrap();
        let started = backend.launcher.started.lock().unwrap().clone();
        assert_eq!(
            started,
            [Launch {
                program: PathBuf::from(PS),
                args: console_args(&request(), Path::new(CLAUDE), &direct_args()),
                cwd: Some(PathBuf::from("E:\\work\\app")),
                flags: CREATE_NEW_CONSOLE,
            }]
        );
        // The console sets its own title, clears the variables of a Claude
        // Code session vcad may run under, then resumes the exact
        // conversation.
        assert_eq!(
            started[0].args[2],
            format!(
                "$Host.UI.RawUI.WindowTitle='Payments'; {}; \
                 Set-Location -LiteralPath 'E:\\work\\app'; \
                 & 'C:\\Users\\me\\.local\\bin\\claude.exe' '--resume' 'ab-1'",
                vca_core::child_env::powershell_cleanup()
            )
        );
    }

    #[test]
    fn with_windows_terminal_it_opens_a_new_wt_window() {
        let backend = WindowsBackend::with_launcher(FakeLauncher {
            wt: Some(PathBuf::from(WT)),
            ..FakeLauncher::default()
        });
        backend.open(&request()).unwrap();
        let started = backend.launcher.started.lock().unwrap().clone();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].program, PathBuf::from(WT));
        assert_eq!(
            started[0].args,
            wt_args(&request(), Path::new(PS), Path::new(CLAUDE), &direct_args())
        );
        assert_eq!(started[0].cwd, None);
        assert_eq!(started[0].flags, CREATE_NO_WINDOW);
    }

    #[test]
    fn a_managed_request_opens_vca_run_in_windows_terminal() {
        let backend = WindowsBackend::with_launcher(FakeLauncher {
            wt: Some(PathBuf::from(WT)),
            vca: true,
            ..FakeLauncher::default()
        });
        backend.open(&managed_request(VCA)).unwrap();
        let started = backend.launcher.started.lock().unwrap().clone();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].program, PathBuf::from(WT));
        // Same window, title and folder as a direct launch.
        let direct = wt_args(&request(), Path::new(PS), Path::new(CLAUDE), &direct_args());
        assert_eq!(started[0].args[..11], direct[..11]);
        assert!(
            started[0].args[11].ends_with(
                "\\; & 'C:\\Users\\me\\AppData\\Local\\Voice Command AI\\vca.exe' \
                 '--endpoint' 'vca-control-me' 'run' '--name=Payments' '--' \
                 'C:\\Users\\me\\.local\\bin\\claude.exe' '--resume' 'ab-1'"
            ),
            "{}",
            started[0].args[11]
        );
    }

    #[test]
    fn a_managed_request_in_a_console_host_window_keeps_title_and_folder() {
        let backend = WindowsBackend::with_launcher(FakeLauncher {
            vca: true,
            ..FakeLauncher::default()
        });
        backend.open(&managed_request(VCA)).unwrap();
        let started = backend.launcher.started.lock().unwrap().clone();
        assert_eq!(started[0].program, PathBuf::from(PS));
        assert_eq!(started[0].cwd, Some(PathBuf::from("E:\\work\\app")));
        assert!(
            started[0].args[2].starts_with("$Host.UI.RawUI.WindowTitle='Payments'; "),
            "{}",
            started[0].args[2]
        );
        assert!(
            started[0].args[2].contains("'run' '--name=Payments' '--'"),
            "{}",
            started[0].args[2]
        );
    }

    #[test]
    fn without_vca_installed_a_managed_request_runs_the_agent_directly() {
        for (vca_installed, path) in [(false, VCA), (true, "vca.exe")] {
            let backend = WindowsBackend::with_launcher(FakeLauncher {
                wt: Some(PathBuf::from(WT)),
                vca: vca_installed,
                ..FakeLauncher::default()
            });
            backend.open(&managed_request(path)).unwrap();
            let started = backend.launcher.started.lock().unwrap().clone();
            assert_eq!(
                started[0].args,
                wt_args(&request(), Path::new(PS), Path::new(CLAUDE), &direct_args()),
                "{path}"
            );
        }
    }

    #[test]
    fn a_missing_agent_starts_nothing() {
        let backend = WindowsBackend::with_launcher(FakeLauncher {
            no_agent: true,
            ..FakeLauncher::default()
        });
        let err = backend.open(&request()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(backend.launcher.started.lock().unwrap().is_empty());
    }

    /// Read-only: lists monitors and terminal windows of this PC.
    #[test]
    fn reading_the_desktop_changes_nothing_and_does_not_fail() {
        let backend = WindowsBackend::system();
        assert!(backend.monitors().is_ok());
        assert!(backend.windows().is_ok());
        assert!(backend.console_owners().is_ok());
    }

    #[test]
    fn an_environment_block_is_split_into_variables() {
        let text = "=C:=C:\\work\0Path=C:\\Windows;C:\\bin\0EMPTY=\0A=b=c\0\0";
        let block: Vec<u16> = text.encode_utf16().collect();
        let vars = parse_env_block(&block);
        let pairs: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            [
                ("=C:".to_owned(), "C:\\work".to_owned()),
                ("Path".to_owned(), "C:\\Windows;C:\\bin".to_owned()),
                ("EMPTY".to_owned(), String::new()),
                ("A".to_owned(), "b=c".to_owned()),
            ]
        );
        assert!(parse_env_block(&[0, 0]).is_empty());
    }

    #[test]
    fn the_default_environment_is_the_users_not_vcads() {
        // Read-only: builds the block a desktop-started program would get.
        let Some(env) = user_default_env() else {
            return;
        };
        let has = |name: &str| {
            env.iter()
                .any(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case(name))
        };
        assert!(has("SystemRoot"), "{env:?}");
        assert!(has("Path"));
        // What only this process has (cargo sets it for the test run, and
        // a Claude Code session its own variables) is not in it.
        for name in ["CARGO_MANIFEST_DIR", "CLAUDECODE"] {
            if std::env::var_os(name).is_some() {
                assert!(!has(name), "{name}");
            }
        }
        // And the floor removes session variables whatever the base.
        let restored = restored_env(env);
        assert!(
            !restored
                .iter()
                .any(|(k, _)| super::super::launch::is_stripped(k))
        );
    }

    #[test]
    fn stripped_variables_are_matched_by_prefix() {
        use std::ffi::OsStr;

        use super::super::launch::is_stripped;

        assert!(is_stripped(OsStr::new("CLAUDECODE")));
        assert!(is_stripped(OsStr::new("claude_code_entrypoint")));
        assert!(is_stripped(OsStr::new("VCA_CONTROL")));
        assert!(!is_stripped(OsStr::new("PATH")));
        assert!(!is_stripped(OsStr::new("CLAUDE_CONFIG_DIR")));
        assert!(!is_stripped(OsStr::new("CLAUDE_CODE_USE_BEDROCK")));
        assert!(is_stripped(OsStr::new("CLAUDE_CODE_MESSAGING_TOKEN")));
    }
}
