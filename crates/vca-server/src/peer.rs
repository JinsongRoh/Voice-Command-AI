//! Who is on the other end of the local channel.
//!
//! The channel is open to every process of the owner, so identity here means
//! narrowing down which process. The operating system names the peer of a
//! named pipe or Unix socket, and the process tree tells whether it descends
//! from a session VCA started.
//!
//! The limits: a process can leave the tree (a daemon that outlives its
//! parent, a task the OS scheduler starts), so "not in a session" proves
//! nothing, while "in a session" is reliable.

use std::collections::HashMap;
use std::path::Path;

use crate::api::Api;
use crate::security::Caller;

/// Parents of every running process.
///
/// On Windows this is one `NtQuerySystemInformation` snapshot
/// ([`vca_core::proclist`]): no process handle is opened, so a call on every
/// local request never touches protected processes or trips process
/// enumeration alarms. A parent id is only kept when that parent was created
/// before the child: Windows keeps the id of an exited parent, and a newer
/// process may have been given the same id since.
fn process_parents() -> HashMap<u32, u32> {
    #[cfg(windows)]
    {
        vca_core::proclist::list()
            .map(|procs| verified_parents(&procs))
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Never),
        );
        system
            .processes()
            .iter()
            .filter_map(|(pid, p)| Some((pid.as_u32(), p.parent()?.as_u32())))
            .collect()
    }
}

/// Child to parent, for the parents that are provably the process that
/// created the child (created no later than it).
#[cfg_attr(not(windows), allow(dead_code))]
fn verified_parents(procs: &[vca_core::proclist::ProcEntry]) -> HashMap<u32, u32> {
    let created: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.created)).collect();
    procs
        .iter()
        .filter_map(|p| {
            let parent = p.parent?;
            let parent_created = *created.get(&parent)?;
            (parent_created <= p.created).then_some((p.pid, parent))
        })
        .collect()
}

/// `pid` and its ancestors, nearest first. Stops on a loop (process ids are
/// reused) and at an unknown parent.
pub fn lineage(pid: u32, parents: &HashMap<u32, u32>) -> Vec<u32> {
    let mut chain = vec![pid];
    let mut at = pid;
    while let Some(&parent) = parents.get(&at) {
        if parent == 0 || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        at = parent;
    }
    chain
}

/// What the local channel knows about process `pid`, given the process ids
/// of the managed sessions and of the remote shells.
pub fn describe(pid: Option<u32>, chain: &[u32], sessions: &[u32], shells: &[u32]) -> Caller {
    Caller {
        pid,
        in_session: chain.iter().any(|p| sessions.contains(p)),
        in_remote_shell: chain.iter().any(|p| shells.contains(p)),
    }
}

/// Looks up the process behind a connection.
pub async fn caller(api: &Api, pid: Option<u32>) -> Caller {
    let Some(known) = pid else {
        return Caller::default();
    };
    let (sessions, shells) = api.sessions().session_pids();
    if sessions.is_empty() && shells.is_empty() {
        return describe(pid, &[known], &sessions, &shells);
    }
    let chain = tokio::task::spawn_blocking(move || lineage(known, &process_parents()))
        .await
        .unwrap_or_else(|_| vec![known]);
    describe(pid, &chain, &sessions, &shells)
}

/// The `tty_nr` field of `/proc/<pid>/stat`. The command name in the second
/// field may hold spaces and parentheses, so fields count from the last `)`.
fn tty_of_stat(stat: &str) -> Option<i64> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(4)?.parse().ok()
}

/// Does the process have a controlling terminal? `None` where the system
/// cannot tell.
pub fn has_terminal(pid: u32) -> Option<bool> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    tty_of_stat(&stat).map(|tty| tty != 0)
}

/// The file names the desktop app is installed under (the cargo binary,
/// and the product name some packages use).
const DESKTOP_APP_NAMES: &[&str] = &["vca-desktop", "voice-command-ai"];

/// Directories on a search path that the owner (or any program the owner
/// runs) can write to. A file there named like the app proves nothing.
const USER_BIN_DIRS: &[&str] = &[".cargo/bin", ".local/bin", "bin", "go/bin"];

/// `/proc/<pid>/exe` of a file replaced since the process started reads
/// `<path> (deleted)`.
fn strip_deleted(exe: &Path) -> std::path::PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(stripped) => stripped.into(),
        None => exe.to_owned(),
    }
}

/// May a program in `dir` vouch for being the desktop app? Not in a
/// temporary folder or a bin folder in the home directory: anything can be
/// put there without administrator rights.
fn trusted_install_dir(dir: &Path, home: Option<&Path>) -> bool {
    if dir.starts_with("/tmp") || dir.starts_with("/var/tmp") || dir.starts_with("/dev/shm") {
        return false;
    }
    if let Some(home) = home {
        if USER_BIN_DIRS.iter().any(|d| dir == home.join(d)) {
            return false;
        }
    }
    true
}

/// Is `exe` the bundled desktop app: a program with the app's exact name,
/// next to this vcad, in a folder that is not shared by unrelated programs
/// anyone can drop there?
fn beside_vcad(exe: &Path, vcad: &Path, home: Option<&Path>) -> bool {
    let exe = strip_deleted(exe);
    let vcad = strip_deleted(vcad);
    let name = exe.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    let (Some(dir), Some(vcad_dir)) = (exe.parent(), vcad.parent()) else {
        return false;
    };
    exe != vcad
        && dir == vcad_dir
        && DESKTOP_APP_NAMES.contains(&name)
        && trusted_install_dir(dir, home)
        && !world_writable(dir)
}

/// Can every user write into `dir`?
fn world_writable(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(dir).is_ok_and(|m| m.permissions().mode() & 0o002 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// Is process `pid` the desktop app installed with this vcad? The app has no
/// terminal, so on Linux this ties its first-run confirmation to a program,
/// not to a claim. False where the system cannot tell.
pub fn is_desktop_app(pid: u32) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let (Ok(exe), Ok(vcad)) = (
        std::fs::read_link(format!("/proc/{pid}/exe")),
        std::env::current_exe(),
    ) else {
        return false;
    };
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_owned());
    beside_vcad(&exe, &vcad, home.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_of_parents_ends_at_the_top_and_survives_loops() {
        let parents = HashMap::from([(5, 4), (4, 3), (3, 0), (8, 9), (9, 8)]);
        assert_eq!(lineage(5, &parents), [5, 4, 3]);
        assert_eq!(lineage(7, &parents), [7]);
        assert_eq!(lineage(8, &parents), [8, 9]);
    }

    #[test]
    fn only_the_app_by_name_beside_vcad_is_the_app() {
        let home = Some(Path::new("/home/me"));
        let vcad = Path::new("/opt/vca/vcad");
        assert!(beside_vcad(
            Path::new("/opt/vca/voice-command-ai"),
            vcad,
            home
        ));
        assert!(beside_vcad(Path::new("/opt/vca/vca-desktop"), vcad, home));
        assert!(
            beside_vcad(Path::new("/opt/vca/vca-desktop (deleted)"), vcad, home),
            "replaced by an update while running"
        );
        assert!(
            !beside_vcad(Path::new("/opt/vca/vca"), vcad, home),
            "the CLI"
        );
        assert!(
            !beside_vcad(Path::new("/opt/vca/vca (deleted)"), vcad, home),
            "the replaced CLI"
        );
        assert!(!beside_vcad(Path::new("/opt/vca/python3"), vcad, home));
        assert!(!beside_vcad(vcad, vcad, home));
        assert!(!beside_vcad(Path::new("/tmp/voice-command-ai"), vcad, home));
        // A shared bin folder in the home directory proves nothing.
        let cargo = Path::new("/home/me/.cargo/bin/vcad");
        assert!(!beside_vcad(
            Path::new("/home/me/.cargo/bin/vca-desktop"),
            cargo,
            home
        ));
        let local = Path::new("/home/me/.local/bin/vcad");
        assert!(!beside_vcad(
            Path::new("/home/me/.local/bin/vca-desktop"),
            local,
            home
        ));
    }

    #[test]
    fn a_parent_younger_than_its_child_is_not_its_parent() {
        use vca_core::proclist::ProcEntry;
        let entry = |pid, parent, created| ProcEntry {
            pid,
            parent: Some(parent),
            name: String::new(),
            created,
        };
        // 20 was created by 10. 40's parent exited, and its id 50 now
        // belongs to a process created after 40.
        let procs = [
            entry(10, 1, 100),
            entry(20, 10, 200),
            entry(40, 50, 300),
            entry(50, 1, 400),
        ];
        let parents = verified_parents(&procs);
        assert_eq!(parents.get(&20), Some(&10));
        assert_eq!(parents.get(&40), None, "reused parent id");
        assert_eq!(lineage(40, &parents), [40]);
    }

    #[test]
    fn this_process_has_a_parent_on_record() {
        let chain = lineage(std::process::id(), &process_parents());
        assert_eq!(chain[0], std::process::id());
        assert!(chain.len() > 1, "{chain:?}");
    }

    #[test]
    fn descendants_of_sessions_and_shells_are_named() {
        let chain = [50, 40, 30];
        let plain = describe(Some(50), &chain, &[1], &[2]);
        assert_eq!(plain.pid, Some(50));
        assert!(!plain.in_session && !plain.in_remote_shell);
        assert!(describe(Some(50), &chain, &[30], &[2]).in_session);
        assert!(describe(Some(50), &chain, &[1], &[40]).in_remote_shell);
    }

    #[test]
    fn the_terminal_field_is_found_behind_a_strange_name() {
        let with = "123 (a b) c) S 1 123 123 34816 123 4194560";
        assert_eq!(tty_of_stat(with), Some(34816));
        let without = "123 (vca) S 1 123 123 0 -1 4194560";
        assert_eq!(tty_of_stat(without), Some(0));
        assert_eq!(tty_of_stat("nonsense"), None);
    }
}
