//! Process containment: priority, suspend/resume and termination of process
//! trees (RES-03, RES-07).
//!
//! The default [`Scope`] is [`Scope::ChildrenOnly`]: the session's root
//! process (the agent itself) keeps running so that its API connections do
//! not time out, while builds and tests it started are paused.
//!
//! Platform notes:
//! - Unix: signals via `nix` (`SIGSTOP`/`SIGCONT`, `SIGTERM` then `SIGKILL`),
//!   priority via the `renice` tool (every thread on Linux). Lowering
//!   priority never needs privileges; restoring [`Priority::Normal`] after
//!   lowering it usually does, and fails with an error otherwise.
//! - Windows (**unverified on Windows**): suspension by suspending every
//!   thread, priority classes, `TerminateProcess` (no graceful phase), and a
//!   Job Object wrapper (`JobGroup`, Windows only) for grouping.
//! - Linux cgroups v2 are not used yet.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::proctree::current_tree;

/// Which processes of a tree an operation affects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Descendants only; the root process is untouched (default).
    #[default]
    ChildrenOnly,
    /// The root and all of its descendants.
    WholeTree,
}

/// Scheduling priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// nice 0 / `NORMAL_PRIORITY_CLASS`.
    Normal,
    /// nice 10 / `BELOW_NORMAL_PRIORITY_CLASS`.
    Low,
    /// nice 19 / `IDLE_PRIORITY_CLASS`.
    Background,
}

impl Priority {
    pub fn nice(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Low => 10,
            Self::Background => 19,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContainError {
    #[error("process {0} does not exist")]
    NoSuchProcess(u32),
    #[error("cannot control process {pid}: {message}")]
    Os { pid: u32, message: String },
    #[error("not supported on this platform")]
    Unsupported,
}

/// Result of a tree operation: the pids that were affected.
pub type ContainResult = Result<Vec<u32>, ContainError>;

/// Controls process trees. Implemented by [`OsProcessControl`]; tests and
/// dry runs can substitute their own.
pub trait ProcessControl: Send + Sync + 'static {
    fn set_priority(&self, root: u32, scope: Scope, priority: Priority) -> ContainResult;
    fn suspend(&self, root: u32, scope: Scope) -> ContainResult;
    fn resume(&self, root: u32, scope: Scope) -> ContainResult;
    /// Asks the processes to exit, then kills those still alive after
    /// `grace`. Blocks for up to `grace`; call from a blocking context.
    fn terminate(&self, root: u32, scope: Scope, grace: Duration) -> ContainResult;
}

/// The operating system's process control.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsProcessControl;

/// Lists the target pids in top-down order (parents before children).
fn targets(root: u32, scope: Scope) -> Vec<u32> {
    let mut pids = Vec::new();
    if scope == Scope::WholeTree {
        pids.push(root);
    }
    pids.extend(current_tree().descendants(root));
    pids
}

fn ensure_exists(root: u32) -> Result<(), ContainError> {
    if crate::admission::Liveness::is_alive(&crate::admission::SysLiveness::default(), root) {
        Ok(())
    } else {
        Err(ContainError::NoSuchProcess(root))
    }
}

/// Applies `op` to the tree, re-scanning for processes spawned while the
/// operation was in progress (up to three passes).
fn apply_to_tree(
    root: u32,
    scope: Scope,
    mut op: impl FnMut(u32) -> Result<bool, ContainError>,
) -> ContainResult {
    ensure_exists(root)?;
    let mut visited: Vec<u32> = Vec::new();
    let mut affected: Vec<u32> = Vec::new();
    for _ in 0..3 {
        let fresh: Vec<u32> = targets(root, scope)
            .into_iter()
            .filter(|p| !visited.contains(p))
            .collect();
        if fresh.is_empty() {
            break;
        }
        for pid in fresh {
            visited.push(pid);
            // `false` means the process vanished in the meantime.
            if op(pid)? {
                affected.push(pid);
            }
        }
    }
    Ok(affected)
}

#[cfg(unix)]
mod unix {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    use super::{
        ContainError, ContainResult, Priority, Scope, apply_to_tree, ensure_exists, targets,
    };
    use crate::admission::{Liveness, SysLiveness};

    /// Sends a signal; `Ok(false)` if the process no longer exists.
    pub fn signal(pid: u32, sig: Signal) -> Result<bool, ContainError> {
        let raw = i32::try_from(pid).map_err(|_| ContainError::NoSuchProcess(pid))?;
        match kill(Pid::from_raw(raw), sig) {
            Ok(()) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            Err(e) => Err(ContainError::Os {
                pid,
                message: e.desc().to_owned(),
            }),
        }
    }

    pub fn suspend(root: u32, scope: Scope) -> ContainResult {
        apply_to_tree(root, scope, |pid| signal(pid, Signal::SIGSTOP))
    }

    pub fn resume(root: u32, scope: Scope) -> ContainResult {
        ensure_exists(root)?;
        // Children first, so that a resumed parent finds its children running.
        let mut pids = targets(root, scope);
        pids.reverse();
        let mut done = Vec::new();
        for pid in pids {
            if signal(pid, Signal::SIGCONT)? {
                done.push(pid);
            }
        }
        Ok(done)
    }

    pub fn terminate(root: u32, scope: Scope, grace: Duration) -> ContainResult {
        ensure_exists(root)?;
        let pids = targets(root, scope);
        let liveness = SysLiveness::default();
        for &pid in &pids {
            // A stopped process must be continued to handle SIGTERM.
            signal(pid, Signal::SIGTERM)?;
            signal(pid, Signal::SIGCONT)?;
        }
        let deadline = Instant::now() + grace;
        loop {
            let alive: Vec<u32> = pids
                .iter()
                .copied()
                .filter(|&p| liveness.is_alive(p))
                .collect();
            if alive.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                for pid in alive {
                    signal(pid, Signal::SIGKILL)?;
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(pids)
    }

    /// Thread ids of a process (Linux), or just the pid elsewhere.
    fn thread_ids(pid: u32) -> Vec<u32> {
        #[cfg(target_os = "linux")]
        if let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/task")) {
            let tids: Vec<u32> = dir
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect();
            if !tids.is_empty() {
                return tids;
            }
        }
        vec![pid]
    }

    pub fn set_priority(root: u32, scope: Scope, priority: Priority) -> ContainResult {
        apply_to_tree(root, scope, |pid| {
            let ids: Vec<String> = thread_ids(pid).iter().map(u32::to_string).collect();
            let out = Command::new("renice")
                .arg("-n")
                .arg(priority.nice().to_string())
                .arg("-p")
                .args(&ids)
                .output()
                .map_err(|e| ContainError::Os {
                    pid,
                    message: format!("cannot run renice: {e}"),
                })?;
            if out.status.success() {
                Ok(true)
            } else if !SysLiveness::default().is_alive(pid) {
                Ok(false)
            } else {
                Err(ContainError::Os {
                    pid,
                    message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                })
            }
        })
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    //! Win32 implementation. **Unverified on Windows**: compiled in CI only.

    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, OpenProcess,
        OpenThread, PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE, ResumeThread,
        SetPriorityClass, SuspendThread, THREAD_SUSPEND_RESUME, TerminateProcess,
    };

    use super::{
        ContainError, ContainResult, Priority, Scope, apply_to_tree, ensure_exists, targets,
    };

    /// An owned Win32 handle, closed on drop.
    struct Handle(HANDLE);

    impl Handle {
        fn new(h: HANDLE) -> Option<Self> {
            (!h.is_null() && h != INVALID_HANDLE_VALUE).then_some(Self(h))
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a valid handle we own (checked in `new`) and
            // it is closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn last_error(pid: u32) -> ContainError {
        ContainError::Os {
            pid,
            message: std::io::Error::last_os_error().to_string(),
        }
    }

    fn open_process(pid: u32, access: u32) -> Option<Handle> {
        // SAFETY: plain FFI call with no pointer arguments.
        Handle::new(unsafe { OpenProcess(access, 0, pid) })
    }

    /// Thread ids belonging to `pid`.
    fn threads_of(pid: u32) -> Result<Vec<u32>, ContainError> {
        // SAFETY: plain FFI call; the returned handle is checked and owned.
        let snap = Handle::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })
            .ok_or_else(|| last_error(pid))?;
        // SAFETY: THREADENTRY32 is a plain C struct; all-zero is a valid value.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut tids = Vec::new();
        // SAFETY: `snap.0` is a valid snapshot handle and `entry` is a valid,
        // writable THREADENTRY32 with `dwSize` set, as the API requires.
        let mut ok = unsafe { Thread32First(snap.0, &mut entry) };
        while ok != 0 {
            if entry.th32OwnerProcessID == pid {
                tids.push(entry.th32ThreadID);
            }
            // SAFETY: as above.
            ok = unsafe { Thread32Next(snap.0, &mut entry) };
        }
        Ok(tids)
    }

    fn for_each_thread(pid: u32, suspend: bool) -> Result<bool, ContainError> {
        let tids = threads_of(pid)?;
        if tids.is_empty() {
            return Ok(false);
        }
        for tid in tids {
            // SAFETY: plain FFI call; the returned handle is checked and owned.
            let Some(t) = Handle::new(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) }) else {
                continue; // The thread exited.
            };
            // SAFETY: `t.0` is a valid thread handle opened with
            // THREAD_SUSPEND_RESUME access.
            let r = unsafe {
                if suspend {
                    SuspendThread(t.0)
                } else {
                    ResumeThread(t.0)
                }
            };
            if r == u32::MAX {
                return Err(last_error(pid));
            }
        }
        Ok(true)
    }

    pub fn suspend(root: u32, scope: Scope) -> ContainResult {
        apply_to_tree(root, scope, |pid| for_each_thread(pid, true))
    }

    pub fn resume(root: u32, scope: Scope) -> ContainResult {
        ensure_exists(root)?;
        let mut pids = targets(root, scope);
        pids.reverse();
        let mut done = Vec::new();
        for pid in pids {
            if for_each_thread(pid, false)? {
                done.push(pid);
            }
        }
        Ok(done)
    }

    pub fn terminate(root: u32, scope: Scope, _grace: Duration) -> ContainResult {
        // Windows has no SIGTERM equivalent for arbitrary processes, so the
        // tree is terminated immediately, leaves first.
        ensure_exists(root)?;
        let mut pids = targets(root, scope);
        pids.reverse();
        let mut done = Vec::new();
        for pid in pids {
            let Some(h) = open_process(pid, PROCESS_TERMINATE) else {
                continue;
            };
            // SAFETY: `h.0` is a valid process handle with PROCESS_TERMINATE.
            if unsafe { TerminateProcess(h.0, 1) } == 0 {
                return Err(last_error(pid));
            }
            done.push(pid);
        }
        Ok(done)
    }

    pub fn set_priority(root: u32, scope: Scope, priority: Priority) -> ContainResult {
        let class = match priority {
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::Low => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Background => IDLE_PRIORITY_CLASS,
        };
        apply_to_tree(root, scope, |pid| {
            let Some(h) = open_process(pid, PROCESS_SET_INFORMATION) else {
                return Ok(false);
            };
            // SAFETY: `h.0` is a valid process handle with PROCESS_SET_INFORMATION.
            if unsafe { SetPriorityClass(h.0, class) } == 0 {
                return Err(last_error(pid));
            }
            Ok(true)
        })
    }

    /// A Windows Job Object grouping a session's processes, so that the
    /// whole group can be terminated at once. Processes started by an
    /// assigned process join the job automatically. **Unverified on Windows.**
    pub struct JobGroup(Handle);

    impl JobGroup {
        pub fn new() -> Result<Self, ContainError> {
            // SAFETY: null security attributes and name are allowed (anonymous job).
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            Handle::new(h).map(Self).ok_or_else(|| last_error(0))
        }

        pub fn assign(&self, pid: u32) -> Result<(), ContainError> {
            let p = open_process(pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE)
                .ok_or(ContainError::NoSuchProcess(pid))?;
            // SAFETY: both handles are valid and have the required access rights.
            if unsafe { AssignProcessToJobObject((self.0).0, p.0) } == 0 {
                return Err(last_error(pid));
            }
            Ok(())
        }

        pub fn terminate(&self, exit_code: u32) -> Result<(), ContainError> {
            // SAFETY: `self.0` is a valid job handle.
            if unsafe { TerminateJobObject((self.0).0, exit_code) } == 0 {
                return Err(last_error(0));
            }
            Ok(())
        }
    }

    // SAFETY: a job handle may be used from any thread.
    unsafe impl Send for JobGroup {}
    // SAFETY: the Win32 job APIs used here are thread-safe.
    unsafe impl Sync for JobGroup {}
}

#[cfg(windows)]
pub use windows::JobGroup;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(any(unix, windows))]
impl ProcessControl for OsProcessControl {
    fn set_priority(&self, root: u32, scope: Scope, priority: Priority) -> ContainResult {
        platform::set_priority(root, scope, priority)
    }
    fn suspend(&self, root: u32, scope: Scope) -> ContainResult {
        platform::suspend(root, scope)
    }
    fn resume(&self, root: u32, scope: Scope) -> ContainResult {
        platform::resume(root, scope)
    }
    fn terminate(&self, root: u32, scope: Scope, grace: Duration) -> ContainResult {
        platform::terminate(root, scope, grace)
    }
}

#[cfg(not(any(unix, windows)))]
impl ProcessControl for OsProcessControl {
    fn set_priority(&self, _: u32, _: Scope, _: Priority) -> ContainResult {
        Err(ContainError::Unsupported)
    }
    fn suspend(&self, _: u32, _: Scope) -> ContainResult {
        Err(ContainError::Unsupported)
    }
    fn resume(&self, _: u32, _: Scope) -> ContainResult {
        Err(ContainError::Unsupported)
    }
    fn terminate(&self, _: u32, _: Scope, _: Duration) -> ContainResult {
        Err(ContainError::Unsupported)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::process::{Child, Command};
    use std::time::Instant;

    use super::*;

    /// The state letter from /proc/<pid>/stat (R, S, T, Z, ...).
    fn state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name is in parentheses and may contain spaces.
        stat.rsplit_once(')')?.1.trim_start().chars().next()
    }

    fn nice(pid: u32) -> i32 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields: Vec<&str> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        // Field 19 (nice) is index 16 after the command name.
        fields[16].parse().unwrap()
    }

    /// Spawns `sh` with two `sleep` children and waits until both exist.
    /// Callers terminate and wait for the returned child.
    #[allow(clippy::zombie_processes)]
    fn spawn_tree() -> (Child, Vec<u32>) {
        let child = Command::new("sh")
            .args(["-c", "sleep 30 & sleep 30 & wait"])
            .spawn()
            .unwrap();
        let root = child.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let kids = current_tree().descendants(root);
            if kids.len() == 2 {
                return (child, kids);
            }
            assert!(Instant::now() < deadline, "children did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_state(pid: u32, want: impl Fn(Option<char>) -> bool) -> Option<char> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = state(pid);
            if want(s) || Instant::now() >= deadline {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn suspend_and_resume_children_only() {
        let (mut root, kids) = spawn_tree();
        let ctl = OsProcessControl;
        let mut affected = ctl.suspend(root.id(), Scope::ChildrenOnly).unwrap();
        affected.sort_unstable();
        let mut expected = kids.clone();
        expected.sort_unstable();
        assert_eq!(affected, expected);
        for &k in &kids {
            assert_eq!(
                wait_state(k, |s| s == Some('T')),
                Some('T'),
                "child {k} stopped"
            );
        }
        assert_ne!(state(root.id()), Some('T'), "root keeps running");

        ctl.resume(root.id(), Scope::ChildrenOnly).unwrap();
        for &k in &kids {
            assert_eq!(
                wait_state(k, |s| s == Some('S')),
                Some('S'),
                "child {k} resumed"
            );
        }
        ctl.terminate(root.id(), Scope::WholeTree, Duration::from_secs(2))
            .unwrap();
        root.wait().unwrap();
    }

    #[test]
    fn suspend_whole_tree_includes_root() {
        let (mut root, _) = spawn_tree();
        let ctl = OsProcessControl;
        let affected = ctl.suspend(root.id(), Scope::WholeTree).unwrap();
        assert_eq!(affected[0], root.id(), "root is stopped first");
        assert_eq!(wait_state(root.id(), |s| s == Some('T')), Some('T'));
        ctl.resume(root.id(), Scope::WholeTree).unwrap();
        assert_eq!(wait_state(root.id(), |s| s == Some('S')), Some('S'));
        ctl.terminate(root.id(), Scope::WholeTree, Duration::from_secs(2))
            .unwrap();
        root.wait().unwrap();
    }

    #[test]
    fn terminate_children_escalates_to_kill() {
        // The child ignores SIGTERM, so it must be killed after the grace period.
        let mut root = Command::new("sh")
            .args(["-c", "sh -c 'trap \"\" TERM; sleep 30' & wait"])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while current_tree().descendants(root.id()).len() < 2 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        let kids = current_tree().descendants(root.id());
        let start = Instant::now();
        OsProcessControl
            .terminate(root.id(), Scope::ChildrenOnly, Duration::from_millis(300))
            .unwrap();
        assert!(start.elapsed() >= Duration::from_millis(300));
        // The root shell exits once its child is gone.
        root.wait().unwrap();
        // SIGKILL delivery is asynchronous, so give the kernel a moment.
        for k in kids {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !matches!(state(k), None | Some('Z')) {
                assert!(Instant::now() < deadline, "{k} still alive");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    #[test]
    fn lower_priority_of_children() {
        let (mut root, kids) = spawn_tree();
        OsProcessControl
            .set_priority(root.id(), Scope::ChildrenOnly, Priority::Low)
            .unwrap();
        for &k in &kids {
            assert_eq!(nice(k), 10);
        }
        assert_eq!(nice(root.id()), nice(std::process::id()));
        OsProcessControl
            .terminate(root.id(), Scope::WholeTree, Duration::from_secs(1))
            .unwrap();
        root.wait().unwrap();
    }

    #[test]
    fn missing_root_is_an_error() {
        let mut c = Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        assert!(matches!(
            OsProcessControl.suspend(pid, Scope::ChildrenOnly),
            Err(ContainError::NoSuchProcess(_))
        ));
    }
}

/// Tests that terminate real processes on Windows. They only run when
/// `VCA_PROCESS_CONTROL_TESTS=1`, which CI sets on its disposable VMs;
/// never run them on a workstation with other work in progress.
#[cfg(all(test, windows))]
mod windows_tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::{OsProcessControl, ProcessControl, Scope};
    use crate::proctree::current_tree;

    fn allowed() -> bool {
        let ok = std::env::var("VCA_PROCESS_CONTROL_TESTS").as_deref() == Ok("1");
        if !ok {
            eprintln!("skipped: set VCA_PROCESS_CONTROL_TESTS=1 (isolated VMs only)");
        }
        ok
    }

    #[test]
    fn terminate_children_only_ends_own_tree() {
        if !allowed() {
            return;
        }
        let mut root = Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let kids = loop {
            let kids = current_tree().descendants(root.id());
            if !kids.is_empty() {
                break kids;
            }
            assert!(Instant::now() < deadline, "child did not start");
            std::thread::sleep(Duration::from_millis(50));
        };
        let me = std::process::id();
        assert!(!kids.contains(&me));
        let affected = OsProcessControl
            .terminate(root.id(), Scope::ChildrenOnly, Duration::from_secs(1))
            .unwrap();
        assert!(!affected.contains(&root.id()));
        assert!(
            affected.iter().all(|p| kids.contains(p)),
            "{affected:?} vs {kids:?}"
        );
        // cmd exits once ping is gone.
        root.wait().unwrap();
    }
}
