//! Process discovery: running agent processes, their working directories
//! and their process trees.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// A snapshot of one process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub cmd: Vec<String>,
    pub exe: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// Start time in seconds since the Unix epoch.
    pub start_time: u64,
}

/// A root process and all of its descendants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessTree {
    pub root: u32,
    /// Descendants in breadth-first order (root excluded).
    pub descendants: Vec<u32>,
}

impl ProcessTree {
    /// Root followed by all descendants.
    pub fn pids(&self) -> impl Iterator<Item = u32> + '_ {
        std::iter::once(self.root).chain(self.descendants.iter().copied())
    }
}

/// All processes visible to the current user at one point in time.
#[derive(Debug, Clone, Default)]
pub struct ProcessTable {
    procs: HashMap<u32, ProcInfo>,
    children: HashMap<u32, Vec<u32>>,
}

impl ProcessTable {
    /// Takes a snapshot of the running processes (threads excluded). This
    /// is blocking and takes a few milliseconds to tens of milliseconds;
    /// call it from `spawn_blocking` in async code.
    pub fn snapshot() -> Self {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cwd(UpdateKind::Always)
                .without_tasks(),
        );
        let procs = sys
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .map(|p| ProcInfo {
                pid: p.pid().as_u32(),
                parent: p.parent().map(Pid::as_u32),
                name: p.name().to_string_lossy().into_owned(),
                cmd: p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
                exe: p.exe().map(Path::to_path_buf),
                cwd: p.cwd().map(Path::to_path_buf),
                start_time: p.start_time(),
            })
            .collect();
        Self::from_procs(procs)
    }

    /// Builds a table from explicit entries (used in tests and by callers
    /// that already hold process data).
    pub fn from_procs(procs: Vec<ProcInfo>) -> Self {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in &procs {
            if let Some(parent) = p.parent {
                children.entry(parent).or_default().push(p.pid);
            }
        }
        for list in children.values_mut() {
            list.sort_unstable();
        }
        Self {
            procs: procs.into_iter().map(|p| (p.pid, p)).collect(),
            children,
        }
    }

    pub fn get(&self, pid: u32) -> Option<&ProcInfo> {
        self.procs.get(&pid)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProcInfo> {
        self.procs.values()
    }

    /// All descendants of `pid` in breadth-first order.
    pub fn descendants(&self, pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([pid]);
        let mut queue = VecDeque::from([pid]);
        while let Some(current) = queue.pop_front() {
            for child in self.children.get(&current).into_iter().flatten() {
                if seen.insert(*child) {
                    out.push(*child);
                    queue.push_back(*child);
                }
            }
        }
        out
    }

    pub fn tree(&self, root: u32) -> ProcessTree {
        ProcessTree {
            root,
            descendants: self.descendants(root),
        }
    }

    /// Parent chain of `pid`, nearest first (`pid` itself excluded).
    pub fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([pid]);
        let mut current = self.procs.get(&pid).and_then(|p| p.parent);
        while let Some(p) = current {
            if !seen.insert(p) {
                break;
            }
            out.push(p);
            current = self.procs.get(&p).and_then(|info| info.parent);
        }
        out
    }

    /// Processes that match `matcher`, keeping only the topmost match of
    /// each chain (a wrapper script and the binary it starts count once).
    pub fn find(&self, matcher: &ProcessMatcher) -> Vec<&ProcInfo> {
        let own_pid = std::process::id();
        let matched: HashSet<u32> = self
            .procs
            .values()
            .filter(|p| p.pid != own_pid && matcher.matches(p))
            .map(|p| p.pid)
            .collect();
        let mut out: Vec<&ProcInfo> = matched
            .iter()
            .filter(|pid| !self.ancestors(**pid).iter().any(|a| matched.contains(a)))
            .filter_map(|pid| self.procs.get(pid))
            .collect();
        out.sort_by_key(|p| p.pid);
        out
    }
}

/// Reads the environment of one process. Returns `None` when it is not
/// readable (other users' processes, or platforms where sysinfo cannot read
/// it, such as recent macOS versions for most processes).
pub fn process_environ(pid: u32) -> Option<Vec<(String, String)>> {
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
    );
    let proc = sys.process(pid)?;
    let env: Vec<(String, String)> = proc
        .environ()
        .iter()
        .filter_map(|kv| {
            let kv = kv.to_string_lossy();
            let (k, v) = kv.split_once('=')?;
            Some((k.to_owned(), v.to_owned()))
        })
        .collect();
    if env.is_empty() { None } else { Some(env) }
}

/// Matches processes of one agent by executable name, including agents
/// that run as a script under an interpreter such as `node` or `python`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProcessMatcher {
    /// Executable or script names, without extension, compared
    /// case-insensitively (for example `claude`, `codex`, `gemini`).
    pub names: Vec<String>,
    /// Substrings that identify the agent's script path when it runs under
    /// an interpreter (for example `@google/gemini-cli`).
    pub script_markers: Vec<String>,
}

const INTERPRETERS: &[&str] = &[
    "node", "nodejs", "bun", "deno", "python", "python3", "pypy3",
];

impl ProcessMatcher {
    pub fn new(names: &[&str], script_markers: &[&str]) -> Self {
        Self {
            names: names.iter().map(|s| s.to_ascii_lowercase()).collect(),
            script_markers: script_markers.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    pub fn matches(&self, p: &ProcInfo) -> bool {
        if self.name_matches(&p.name) {
            return true;
        }
        let Some(first) = p.cmd.first() else {
            return false;
        };
        if self.name_matches(first) {
            return true;
        }
        if !is_interpreter(first) && !is_interpreter(&p.name) {
            return false;
        }
        // The first non-option argument is the script.
        let Some(script) = p.cmd.iter().skip(1).find(|a| !a.starts_with('-')) else {
            return false;
        };
        self.name_matches(script)
            || self
                .script_markers
                .iter()
                .any(|m| script.replace('\\', "/").contains(m.as_str()))
    }

    fn name_matches(&self, raw: &str) -> bool {
        let base = normalized_basename(raw);
        self.names.contains(&base)
    }
}

fn normalized_basename(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".js", ".mjs", ".cjs", ".py"] {
        if let Some(stripped) = base.strip_suffix(ext) {
            return stripped.to_owned();
        }
    }
    base
}

fn is_interpreter(raw: &str) -> bool {
    let base = normalized_basename(raw);
    INTERPRETERS.iter().any(|i| base == *i)
        || (base.starts_with("python") && base[6..].chars().all(|c| c.is_ascii_digit() || c == '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: Option<u32>, name: &str, cmd: &[&str]) -> ProcInfo {
        ProcInfo {
            pid,
            parent,
            name: name.into(),
            cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
            exe: None,
            cwd: Some(PathBuf::from("/work")),
            start_time: 0,
        }
    }

    fn table() -> ProcessTable {
        ProcessTable::from_procs(vec![
            proc(1, None, "init", &["init"]),
            proc(10, Some(1), "bash", &["bash"]),
            proc(
                11,
                Some(10),
                "node",
                &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"],
            ),
            proc(
                12,
                Some(11),
                "codex",
                &["/usr/lib/node_modules/@openai/codex/vendor/codex"],
            ),
            proc(13, Some(12), "cargo", &["cargo", "build"]),
            proc(14, Some(13), "rustc", &["rustc"]),
            proc(
                20,
                Some(1),
                "node",
                &[
                    "node",
                    "--no-warnings",
                    "/opt/gemini/node_modules/@google/gemini-cli/dist/index.js",
                ],
            ),
            proc(30, Some(1), "claude", &["claude", "--resume", "abc"]),
            proc(40, Some(1), "vca", &["vca", "hook", "claude", "stop"]),
            proc(
                50,
                Some(1),
                "python3.12",
                &["python3.12", "/home/u/.local/bin/aider"],
            ),
        ])
    }

    #[test]
    fn descendants_and_ancestors() {
        let t = table();
        assert_eq!(t.descendants(11), vec![12, 13, 14]);
        assert_eq!(t.tree(12).pids().collect::<Vec<_>>(), vec![12, 13, 14]);
        assert_eq!(t.ancestors(14), vec![13, 12, 11, 10, 1]);
    }

    #[test]
    fn finds_topmost_agent_processes() {
        let t = table();
        let codex = ProcessMatcher::new(&["codex"], &["@openai/codex"]);
        let found: Vec<u32> = t.find(&codex).iter().map(|p| p.pid).collect();
        assert_eq!(found, vec![11]);

        let gemini = ProcessMatcher::new(&["gemini"], &["@google/gemini-cli"]);
        assert_eq!(
            t.find(&gemini).iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![20]
        );

        let claude = ProcessMatcher::new(&["claude"], &["@anthropic-ai/claude-code"]);
        assert_eq!(
            t.find(&claude).iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![30]
        );

        let aider = ProcessMatcher::new(&["aider"], &[]);
        assert_eq!(
            t.find(&aider).iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![50]
        );
    }

    #[test]
    fn snapshot_contains_current_process() {
        let t = ProcessTable::snapshot();
        let me = t.get(std::process::id()).expect("own process listed");
        assert!(me.cwd.is_some());
        let tree = t.tree(std::process::id());
        assert!(!tree.pids().any(|p| p == 1) || std::process::id() == 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_own_environment() {
        let env = process_environ(std::process::id()).expect("own environ readable");
        assert!(env.iter().any(|(k, _)| k == "PATH"));
    }
}
