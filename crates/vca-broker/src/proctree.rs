//! Process tree helpers shared by the monitor and containment.

use std::collections::{HashMap, HashSet, VecDeque};

/// One process as seen by a sampler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ProcInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    /// CPU usage as reported by `sysinfo`: percent of one core (may exceed 100).
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// Parent → children index over a process list.
#[derive(Debug, Default)]
pub(crate) struct ProcTree {
    children: HashMap<u32, Vec<u32>>,
}

impl ProcTree {
    pub fn from_pairs(pairs: impl IntoIterator<Item = (u32, Option<u32>)>) -> Self {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, parent) in pairs {
            if let Some(parent) = parent.filter(|&p| p != pid) {
                children.entry(parent).or_default().push(pid);
            }
        }
        for list in children.values_mut() {
            list.sort_unstable();
        }
        Self { children }
    }

    /// Like [`from_pairs`](Self::from_pairs), but a process only counts as
    /// a child of its recorded parent when both start times are known and
    /// the child started no earlier than the parent.
    ///
    /// Windows keeps a process's parent pid after the parent exits and
    /// reuses pids, so an unrelated process that was created by a
    /// long-gone parent would otherwise look like a child of whatever new
    /// process received that pid. Edges that cannot be verified are dropped:
    /// missing a child is safe, adopting a stranger is not.
    pub fn from_timed(entries: impl IntoIterator<Item = (u32, Option<u32>, Option<u64>)>) -> Self {
        let entries: Vec<_> = entries.into_iter().collect();
        let started: HashMap<u32, u64> = entries
            .iter()
            .filter_map(|&(pid, _, t)| t.map(|t| (pid, t)))
            .collect();
        Self::from_pairs(entries.into_iter().map(|(pid, parent, t)| {
            let verified = parent.filter(|p| match (t, started.get(p)) {
                (Some(child), Some(&parent)) => child >= parent,
                _ => false,
            });
            (pid, verified)
        }))
    }

    /// All descendants of `root` in breadth-first order (parents before
    /// children), excluding `root` itself. Robust against cycles.
    pub fn descendants(&self, root: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([root]);
        let mut queue = VecDeque::from([root]);
        while let Some(pid) = queue.pop_front() {
            for &child in self.children.get(&pid).into_iter().flatten() {
                if seen.insert(child) {
                    out.push(child);
                    queue.push_back(child);
                }
            }
        }
        out
    }
}

/// Reads the current process table (pids and parents only). On Windows the
/// parent links are verified with precise creation times (see
/// [`ProcTree::from_timed`]); on Unix, orphans are re-parented, so parent
/// pids are reliable.
pub(crate) fn current_tree() -> ProcTree {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    let procs = sys
        .processes()
        .values()
        .filter(|p| p.thread_kind().is_none())
        .map(|p| (p.pid().as_u32(), p.parent().map(|pp| pp.as_u32())));
    if cfg!(windows) {
        ProcTree::from_timed(procs.map(|(pid, parent)| (pid, parent, creation_time(pid))))
    } else {
        ProcTree::from_pairs(procs)
    }
}

/// Creation time of a process in 100 ns units, when it can be queried.
#[cfg(windows)]
#[allow(unsafe_code)]
fn creation_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: plain FFI call with no pointer arguments.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `handle` is a valid process handle opened above, and all four
    // pointers refer to live, writable FILETIME values.
    let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    // SAFETY: `handle` is valid and closed exactly once.
    unsafe { CloseHandle(handle) };
    (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

#[cfg(not(windows))]
fn creation_time(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_bfs_excluding_root() {
        let t = ProcTree::from_pairs([
            (1, None),
            (10, Some(1)),
            (11, Some(10)),
            (12, Some(10)),
            (13, Some(11)),
            (20, Some(1)),
        ]);
        assert_eq!(t.descendants(10), vec![11, 12, 13]);
        assert_eq!(t.descendants(1), vec![10, 20, 11, 12, 13]);
        assert!(t.descendants(13).is_empty());
        assert!(t.descendants(999).is_empty());
    }

    #[test]
    fn timed_tree_ignores_stale_parent_pids() {
        // 500 exited long ago; its pid was reused by a new process (start
        // 900). Its old child 501 (start 100) must not become a child of
        // the new process 500, while the new process's real child 502 is.
        let t = ProcTree::from_timed([
            (500, Some(1), Some(900)),
            (501, Some(500), Some(100)),
            (502, Some(500), Some(950)),
            (503, Some(502), Some(960)),
        ]);
        assert_eq!(t.descendants(500), vec![502, 503]);
    }

    #[test]
    fn timed_tree_drops_unverifiable_links() {
        let t = ProcTree::from_timed([
            (10, None, Some(5)),
            (11, Some(10), None),
            (12, Some(10), Some(6)),
            (13, Some(99), Some(7)),
        ]);
        assert_eq!(t.descendants(10), vec![12]);
        assert!(t.descendants(99).is_empty());
    }

    #[test]
    fn cycles_terminate() {
        let t = ProcTree::from_pairs([(1, Some(2)), (2, Some(1)), (3, Some(3))]);
        assert_eq!(t.descendants(1), vec![2]);
        assert!(t.descendants(3).is_empty());
    }

    #[test]
    fn current_tree_contains_own_children() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sleep" })
            .args(if cfg!(windows) {
                vec!["/c", "timeout", "5"]
            } else {
                vec!["5"]
            })
            .spawn()
            .unwrap();
        let t = current_tree();
        assert!(t.descendants(std::process::id()).contains(&child.id()));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
