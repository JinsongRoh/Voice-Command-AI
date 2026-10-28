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

/// Reads the current process table (pids and parents only).
pub(crate) fn current_tree() -> ProcTree {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    ProcTree::from_pairs(
        sys.processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .map(|p| (p.pid().as_u32(), p.parent().map(|pp| pp.as_u32()))),
    )
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
