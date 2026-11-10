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
    /// call it from `spawn_blocking` in async code. Code that takes
    /// snapshots repeatedly should keep a [`ProcessScanner`] instead.
    pub fn snapshot() -> Self {
        ProcessScanner::new().snapshot()
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

/// Command line and working directory of one process, read once.
#[derive(Debug, Clone)]
struct Details {
    start_time: u64,
    cmd: Vec<String>,
    cwd: Option<PathBuf>,
    exe: Option<PathBuf>,
    /// Reads so far; an empty command line (a process caught while it
    /// starts) is read again a few times.
    attempts: u8,
}

/// Reads of a process whose command line stayed empty before giving up
/// (system processes and other users' processes have none to read).
const DETAIL_ATTEMPTS: u8 = 3;

/// Executable names (lower case, no extension) of processes whose command
/// line, directory and executable path are read: the built-in agents, the
/// interpreters agents run under, and the shells and terminals that host
/// them. Every other process is only listed (PID, parent, name, start
/// time); VCA never reads the memory of unrelated processes.
const DETAIL_NAMES: &[&str] = &[
    // Agents.
    "claude",
    "codex",
    "gemini",
    "antigravity",
    "grok",
    "cursor-agent",
    "copilot",
    "aider",
    "opencode",
    "qwen",
    // Interpreters (see `INTERPRETERS`; `python3.12` and the like match too).
    "node",
    "nodejs",
    "bun",
    "deno",
    "python",
    "python3",
    "pypy3",
    // Shells.
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "nu",
    "cmd",
    "powershell",
    "pwsh",
    "wsl",
    "wslhost",
    "git-bash",
    // Terminals and multiplexers.
    "windowsterminal",
    "openconsole",
    "conhost",
    "mintty",
    "alacritty",
    "wezterm",
    "wezterm-gui",
    "kitty",
    "ghostty",
    "tmux",
    "zellij",
    "screen",
];

/// The agents among [`DETAIL_NAMES`].
const AGENT_NAMES: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "antigravity",
    "grok",
    "cursor-agent",
    "copilot",
    "aider",
    "opencode",
    "qwen",
];

/// Ancestors of a candidate agent process that are read too (the host
/// detection looks this far up).
const DETAIL_ANCESTORS: usize = 8;

/// Processes whose memory is never read and that are never opened, even
/// as the ancestor of an agent: the kernel's own and the protected system
/// processes.
const NEVER_READ: &[&str] = &[
    "system",
    "registry",
    "memory compression",
    "secure system",
    "smss",
    "csrss",
    "wininit",
    "winlogon",
    "services",
    "lsass",
    "lsaiso",
];

/// One listed process: what every snapshot knows without reading it.
#[derive(Debug, Clone)]
struct Listed {
    pid: u32,
    parent: Option<u32>,
    name: String,
    start_time: u64,
}

/// Takes repeated process snapshots cheaply: the process list is refreshed
/// each time, but the command line, working directory and executable path
/// are read only for candidate processes ([`DETAIL_NAMES`], extra names,
/// and the ancestors of agent and interpreter processes), once per process
/// (by PID and start time), and forgotten when it exits.
///
/// On Windows the list comes from [`vca_core::proclist`], which opens no
/// process: the scanner holds no process handle between snapshots, and a
/// candidate is opened only while its details are read (a `sysinfo`
/// `System` kept across snapshots holds a `PROCESS_VM_READ` handle to
/// every process it lists). Elsewhere a persistent `sysinfo` list is kept.
pub struct ProcessScanner {
    sys: System,
    details: HashMap<u32, Details>,
    detail_reads: u64,
    extra_names: HashSet<String>,
}

impl Default for ProcessScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ProcessScanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessScanner")
            .field("processes", &self.details.len())
            .finish()
    }
}

impl ProcessScanner {
    pub fn new() -> Self {
        Self {
            sys: System::new(),
            details: HashMap::new(),
            detail_reads: 0,
            extra_names: HashSet::new(),
        }
    }

    /// Also reads processes with these executable names (an adapter's
    /// [`ProcessMatcher::names`]).
    pub fn watch_names<I: IntoIterator<Item = S>, S: AsRef<str>>(&mut self, names: I) {
        self.extra_names
            .extend(names.into_iter().map(|n| normalized_basename(n.as_ref())));
    }

    fn is_candidate(&self, name: &str) -> bool {
        let base = normalized_basename(name);
        DETAIL_NAMES.iter().any(|known| name_is(&base, known))
            || is_interpreter(&base)
            || self.extra_names.iter().any(|known| name_is(&base, known))
    }

    /// Agents and interpreters, whose host chain is looked at.
    fn is_agent_like(&self, name: &str) -> bool {
        let base = normalized_basename(name);
        is_interpreter(&base)
            || self.extra_names.iter().any(|known| name_is(&base, known))
            || AGENT_NAMES.iter().any(|known| name_is(&base, known))
    }

    /// Processes whose command line and working directory were read so far
    /// (a measure of the work done).
    pub fn detail_reads(&self) -> u64 {
        self.detail_reads
    }

    /// Every running process: PID, parent, name and start time.
    fn list(&mut self) -> Vec<Listed> {
        #[cfg(windows)]
        if let Some(list) = vca_core::proclist::list() {
            return list
                .into_iter()
                .map(|p| Listed {
                    pid: p.pid,
                    parent: p.parent,
                    name: p.name,
                    start_time: vca_core::proclist::filetime_to_unix_secs(p.created),
                })
                .collect();
        }
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        let list = self
            .sys
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .map(|p| Listed {
                pid: p.pid().as_u32(),
                parent: p.parent().map(Pid::as_u32),
                name: p.name().to_string_lossy().into_owned(),
                start_time: p.start_time(),
            })
            .collect();
        if cfg!(windows) {
            // The fallback list must not keep a handle to every process.
            self.sys = System::new();
        }
        list
    }

    /// Reads command line, directory and executable of `wanted` (PID and
    /// the start time it was listed with). A process whose start time
    /// differs is another one that got the PID: no details.
    fn read_details(&mut self, wanted: &[(u32, u64)]) -> Vec<(u32, u64, Option<Details>)> {
        let pids: Vec<Pid> = wanted.iter().map(|(pid, _)| Pid::from_u32(*pid)).collect();
        let kind = ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always)
            .without_tasks();
        // On Windows a throwaway `System`: its handles close when it drops.
        let mut throwaway = cfg!(windows).then(System::new);
        let sys = throwaway.as_mut().unwrap_or(&mut self.sys);
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), false, kind);
        self.detail_reads += wanted.len() as u64;
        wanted
            .iter()
            .map(|&(pid, start)| {
                let details = sys
                    .process(Pid::from_u32(pid))
                    .filter(|p| p.start_time() == start || p.start_time() == 0)
                    .map(|p| Details {
                        start_time: start,
                        cmd: p
                            .cmd()
                            .iter()
                            .map(|s| s.to_string_lossy().into_owned())
                            .collect(),
                        cwd: p.cwd().map(Path::to_path_buf),
                        exe: p.exe().map(Path::to_path_buf),
                        attempts: 0,
                    });
                (pid, start, details)
            })
            .collect()
    }

    /// The running processes now (blocking).
    pub fn snapshot(&mut self) -> ProcessTable {
        let listed = self.list();
        let by_pid: HashMap<u32, &Listed> = listed.iter().map(|p| (p.pid, p)).collect();
        // Exited processes, and PIDs another process got, are forgotten.
        self.details.retain(|pid, d| {
            by_pid
                .get(pid)
                .is_some_and(|p| p.start_time == d.start_time)
        });
        // Candidates by name, and the host chain of agent-like processes.
        let mut candidates: HashSet<u32> = HashSet::new();
        for p in &listed {
            if !self.is_candidate(&p.name) {
                continue;
            }
            candidates.insert(p.pid);
            if self.is_agent_like(&p.name) {
                let mut child = p;
                for _ in 0..DETAIL_ANCESTORS {
                    // A recorded parent that started after its child is a
                    // stranger that got a reused PID.
                    let Some(parent) = child
                        .parent
                        .and_then(|pp| by_pid.get(&pp).copied())
                        .filter(|pp| pp.start_time <= child.start_time)
                    else {
                        break;
                    };
                    candidates.insert(parent.pid);
                    child = parent;
                }
            }
        }
        let wanted: Vec<(u32, u64)> = listed
            .iter()
            .filter(|p| candidates.contains(&p.pid) && is_readable(p))
            .filter(|p| match self.details.get(&p.pid) {
                Some(d) => d.cmd.is_empty() && d.attempts < DETAIL_ATTEMPTS,
                None => true,
            })
            .map(|p| (p.pid, p.start_time))
            .collect();
        if !wanted.is_empty() {
            for (pid, start, read) in self.read_details(&wanted) {
                let attempts = self.details.get(&pid).map_or(0, |d| d.attempts);
                let mut details = read.unwrap_or(Details {
                    start_time: start,
                    cmd: Vec::new(),
                    cwd: None,
                    exe: None,
                    attempts: 0,
                });
                details.attempts = attempts.saturating_add(1);
                self.details.insert(pid, details);
            }
        }
        let procs = listed
            .into_iter()
            .map(|p| {
                let details = self
                    .details
                    .get(&p.pid)
                    .filter(|d| d.start_time == p.start_time);
                ProcInfo {
                    pid: p.pid,
                    parent: p.parent,
                    cmd: details.map(|d| d.cmd.clone()).unwrap_or_default(),
                    exe: details.and_then(|d| d.exe.clone()),
                    cwd: details.and_then(|d| d.cwd.clone()),
                    start_time: p.start_time,
                    name: p.name,
                }
            })
            .collect();
        ProcessTable::from_procs(procs)
    }
}

/// Whether a listed process may be opened to read its details: never the
/// kernel's own or the protected system processes ([`NEVER_READ`]).
fn is_readable(p: &Listed) -> bool {
    p.pid > 4 && !NEVER_READ.contains(&normalized_basename(&p.name).as_str())
}

/// Reads the command line of one process (for a session VCA already knows
/// about; a [`ProcessScanner`] reads only candidate processes).
pub fn process_cmd(pid: u32) -> Option<Vec<String>> {
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let cmd: Vec<String> = sys
        .process(pid)?
        .cmd()
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    (!cmd.is_empty()).then_some(cmd)
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
        // The script is the first argument that is a path; bare words before
        // it are option values (`--max-old-space-size 8192`) or a module
        // (`python -m aider`), which match by name. Code given inline
        // (`python -c ...`, `node -e ...`) runs no script: the words after
        // it are that code's arguments (`python -c "..." claude` is not
        // Claude Code).
        let mut words = 0;
        for arg in p.cmd.iter().skip(1) {
            if is_inline_code_flag(arg) {
                return false;
            }
            if arg.starts_with('-') {
                continue;
            }
            words += 1;
            if words > 2 {
                break;
            }
            if self.name_matches(arg)
                || self
                    .script_markers
                    .iter()
                    .any(|m| arg.replace('\\', "/").contains(m.as_str()))
            {
                return true;
            }
            if looks_like_script(arg) {
                return false;
            }
        }
        false
    }

    fn name_matches(&self, raw: &str) -> bool {
        let base = normalized_basename(raw);
        self.names.contains(&base)
    }
}

/// Linux lists a process under its `comm` name, cut to 15 bytes.
const COMM_LEN: usize = 15;

/// Whether the listed name `base` is the known name `known`, also when the
/// OS cut the listed name short (Linux).
fn name_is(base: &str, known: &str) -> bool {
    base == known
        || (cfg!(target_os = "linux") && base.len() == COMM_LEN && known.starts_with(base))
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

/// An interpreter option that runs code given on the command line instead
/// of a script: Python's `-c`, Node's, Bun's and Deno's `-e`/`--eval` and
/// `-p`/`--print`.
fn is_inline_code_flag(arg: &str) -> bool {
    matches!(arg, "-c" | "-e" | "--eval" | "-p" | "--print")
        || arg.starts_with("--eval=")
        || arg.starts_with("--print=")
}

/// A path or a script file name (as opposed to an option value or a module
/// name).
fn looks_like_script(arg: &str) -> bool {
    arg.contains(['/', '\\'])
        || [".js", ".mjs", ".cjs", ".ts", ".py"]
            .iter()
            .any(|ext| arg.to_ascii_lowercase().ends_with(ext))
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
    fn inline_code_is_not_an_agent() {
        let claude = ProcessMatcher::new(&["claude"], &["@anthropic-ai/claude-code"]);
        let codex = ProcessMatcher::new(&["codex"], &["@openai/codex"]);
        let p = |cmd: &[&str]| proc(7, None, cmd[0], cmd);
        assert!(!claude.matches(&p(&["python", "-c", "print(1)", "claude"])));
        assert!(!codex.matches(&p(&["node", "-e", "run()", "codex"])));
        assert!(!codex.matches(&p(&["node", "--eval=run()", "codex"])));
        assert!(!claude.matches(&p(&["bun", "-p", "1", "claude"])));
        // A script or module still matches.
        assert!(claude.matches(&p(&[
            "node",
            "--max-old-space-size",
            "8192",
            "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        ])));
        let aider = ProcessMatcher::new(&["aider"], &[]);
        assert!(aider.matches(&p(&["python3", "-m", "aider"])));
    }

    #[test]
    fn only_candidate_processes_are_read() {
        let scanner = ProcessScanner::new();
        assert!(scanner.is_candidate("claude.exe"));
        assert!(scanner.is_candidate("C:\\nvm\\node.exe"));
        assert!(scanner.is_candidate("python3.12"));
        assert!(scanner.is_candidate("pwsh.exe"));
        assert!(scanner.is_candidate("WindowsTerminal.exe"));
        assert!(!scanner.is_candidate("chrome.exe"));
        assert!(!scanner.is_candidate("svchost.exe"));
        assert!(scanner.is_agent_like("codex"));
        assert!(!scanner.is_agent_like("bash"));
        let mut scanner = ProcessScanner::new();
        scanner.watch_names(["my-agent"]);
        assert!(scanner.is_candidate("my-agent.exe"));
        assert!(scanner.is_agent_like("my-agent"));
    }

    #[test]
    fn names_cut_short_by_linux_still_match() {
        assert!(name_is("my-long-agent-n", "my-long-agent-name"));
        assert!(!name_is("my-long-agent", "my-long-agent-name"));
        assert!(!name_is("other-long-name", "my-long-agent-name"));
        if cfg!(target_os = "linux") {
            let mut scanner = ProcessScanner::new();
            scanner.watch_names(["my-long-agent-name"]);
            assert!(scanner.is_candidate("my-long-agent-n"));
        }
    }

    #[test]
    fn a_scanner_reads_command_lines_once_per_process() {
        // The test binary is not a candidate by name; name it one.
        let me_name = std::env::current_exe().unwrap();
        let mut scanner = ProcessScanner::new();
        scanner.watch_names([me_name.file_name().unwrap().to_string_lossy()]);
        let first = scanner.snapshot();
        let me = first.get(std::process::id()).expect("own process listed");
        assert!(!me.cmd.is_empty());
        assert!(me.cwd.is_some());
        let after_first = scanner.detail_reads();
        assert!(after_first > 0);

        let second = scanner.snapshot();
        let me_again = second.get(std::process::id()).expect("still listed");
        assert_eq!(me_again.cmd, me.cmd);
        assert_eq!(me_again.cwd, me.cwd);
        // Only processes started in between (and ones whose command line
        // could not be read yet) are read again, not every process.
        let again = scanner.detail_reads() - after_first;
        assert!(
            again < after_first / 2 + 1,
            "{again} reads on the second snapshot, {after_first} on the first"
        );
        // Exited processes are forgotten: the cache is bounded by the
        // processes alive at the last snapshot.
        assert!(scanner.details.len() <= second.iter().count());
    }

    #[test]
    fn snapshot_contains_current_process() {
        let t = ProcessTable::snapshot();
        let me = t.get(std::process::id()).expect("own process listed");
        // Not a candidate by name: listed, but its memory is not read.
        assert!(me.cmd.is_empty() && me.cwd.is_none());
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
