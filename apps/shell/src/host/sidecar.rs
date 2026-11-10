//! The vcad bundled with the desktop app (a Tauri sidecar, `externalBin`).
//!
//! Tauri installs the sidecar next to the app's executable without its
//! target-triple suffix (`vcad.exe` / `vcad`), in the bundle as in
//! `tauri dev`.
//!
//! Ownership: the app only ever stops a vcad it started itself. While the
//! app runs it holds the [`Child`] handle (on Windows an open process handle,
//! on Unix an unreaped child, so the PID cannot be reused by an unrelated
//! process). vcad keeps running when the app quits (agent sessions and paired
//! phones depend on it), so the PID, the executable path and the OS start
//! time are also written to `vcad.json` in the app's data directory; after a
//! restart that record only counts when a live process with that PID still
//! runs the very same executable and started at the recorded time.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const RECORD_FILE: &str = "vcad.json";
const LOG_FILE: &str = "vcad.log";
/// The log of the run before, kept when vcad is started again.
const PREVIOUS_LOG_FILE: &str = "vcad.previous.log";
/// Allowed difference between recorded and observed start times, in seconds.
const START_SLACK_SECS: u64 = 2;

/// The bundled vcad next to the app's executable, when present.
pub fn bundled_vcad() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe
        .parent()?
        .join(format!("vcad{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

/// What identifies a vcad the app started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub pid: u32,
    pub exe: PathBuf,
    /// OS start time, Unix seconds (0 when it could not be read).
    pub started_at: u64,
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The OS view of a process: its executable and start time.
fn observe(pid: u32) -> Option<(Option<PathBuf>, u64)> {
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let p = sys.process(pid)?;
    Some((p.exe().map(Path::to_owned), p.start_time()))
}

/// True when `record` still describes a live process running `expected_exe`.
pub fn verify(record: &Record, expected_exe: &Path) -> bool {
    if !same_path(&record.exe, expected_exe) {
        return false;
    }
    let Some((exe, started_at)) = observe(record.pid) else {
        return false;
    };
    let Some(exe) = exe else {
        // The executable of a process we may not inspect is not ours.
        return false;
    };
    if !same_path(&exe, expected_exe) {
        return false;
    }
    record.started_at == 0 || started_at.abs_diff(record.started_at) <= START_SLACK_SECS
}

/// Kills the process of a verified record.
fn kill_verified(record: &Record, expected_exe: &Path) -> bool {
    if !verify(record, expected_exe) {
        return false;
    }
    let pid = Pid::from_u32(record.pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid).is_some_and(|p| p.kill())
}

#[derive(Default)]
struct Owned {
    child: Option<Child>,
    record: Option<Record>,
}

/// The vcad started by this app, if any.
pub struct Sidecar {
    dir: PathBuf,
    exe: Option<PathBuf>,
    owned: Mutex<Owned>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sidecar {
    /// `dir` holds the ownership record and vcad's log; `exe` is the bundled
    /// vcad ([`bundled_vcad`]).
    pub fn open(dir: PathBuf, exe: Option<PathBuf>) -> Self {
        let record = std::fs::read(dir.join(RECORD_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Record>(&b).ok());
        let sidecar = Self {
            dir,
            exe,
            owned: Mutex::new(Owned::default()),
        };
        if let Some(record) = record {
            let valid = sidecar.exe.as_deref().is_some_and(|e| verify(&record, e));
            if valid {
                lock(&sidecar.owned).record = Some(record);
            } else {
                sidecar.forget_record();
            }
        }
        sidecar
    }

    pub fn exe(&self) -> Option<&Path> {
        self.exe.as_deref()
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.join(LOG_FILE)
    }

    fn forget_record(&self) {
        let _ = std::fs::remove_file(self.dir.join(RECORD_FILE));
    }

    /// Starts the bundled vcad on `endpoint` with the user's default
    /// configuration file and data directory, without a console window.
    pub fn spawn(&self, endpoint: &Path) -> std::io::Result<u32> {
        let exe = self.exe.clone().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the VCA server (vcad) is not bundled with this build",
            )
        })?;
        let mut owned = lock(&self.owned);
        if let Some(pid) = self.live_pid(&mut owned, &exe) {
            return Ok(pid);
        }
        std::fs::create_dir_all(&self.dir)?;
        // Keep the last run's log (it may say why that run ended).
        let _ = std::fs::rename(self.log_path(), self.dir.join(PREVIOUS_LOG_FILE));
        let log = File::create(self.log_path())?;
        let mut cmd = Command::new(&exe);
        cmd.arg("--control")
            .arg(endpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log));
        no_window(&mut cmd);
        let child = cmd.spawn()?;
        let pid = child.id();
        let started_at = observe(pid).map(|(_, t)| t).unwrap_or(0);
        let record = Record {
            pid,
            exe,
            started_at,
        };
        if let Ok(json) = serde_json::to_vec(&record) {
            if let Err(e) = std::fs::write(self.dir.join(RECORD_FILE), json) {
                tracing::warn!("cannot save the vcad ownership record: {e}");
            }
        }
        owned.child = Some(child);
        owned.record = Some(record);
        tracing::info!(pid, "started the bundled vcad");
        Ok(pid)
    }

    fn live_pid(&self, owned: &mut Owned, exe: &Path) -> Option<u32> {
        if let Some(child) = owned.child.as_mut() {
            match child.try_wait() {
                Ok(None) => return Some(child.id()),
                // Exited (and now reaped): the PID is no longer ours.
                result => {
                    let status = match result {
                        Ok(Some(status)) => status.to_string(),
                        Err(e) => format!("unknown status ({e})"),
                        Ok(None) => unreachable!(),
                    };
                    self.note_exit(child.id(), &status);
                    owned.child = None;
                    owned.record = None;
                    return None;
                }
            }
        }
        match &owned.record {
            Some(r) if verify(r, exe) => Some(r.pid),
            Some(_) => {
                owned.record = None;
                None
            }
            None => None,
        }
    }

    /// PID of the vcad this app started, while it runs.
    pub fn owned_pid(&self) -> Option<u32> {
        let exe = self.exe.as_deref()?;
        let mut owned = lock(&self.owned);
        let pid = self.live_pid(&mut owned, exe);
        if pid.is_none() {
            drop(owned);
            self.forget_record();
        }
        pid
    }

    /// Records that the vcad this app started has exited, in the app's log
    /// and at the end of vcad's own log. A process ended from outside (an
    /// antivirus, the task manager) cannot write that line itself.
    fn note_exit(&self, pid: u32, status: &str) {
        tracing::warn!(pid, %status, "the vcad this app started has exited");
        let line = format!(
            "{} WARN vca_desktop: vcad (pid {pid}) exited: {status}; noticed by the desktop app\n",
            utc_now()
        );
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .open(self.log_path())
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Exit status text when the started vcad already exited (startup
    /// failures), without forgetting it.
    pub fn exited(&self) -> Option<String> {
        let mut owned = lock(&self.owned);
        let child = owned.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(status.to_string()),
            _ => None,
        }
    }

    /// Forcibly ends the vcad this app started (after a graceful shutdown
    /// request timed out). Never touches any other process.
    pub fn kill(&self) -> bool {
        let Some(exe) = self.exe.clone() else {
            return false;
        };
        let mut owned = lock(&self.owned);
        if let Some(child) = owned.child.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return child.kill().is_ok();
            }
            return false;
        }
        match owned.record.clone() {
            Some(r) => kill_verified(&r, &exe),
            None => false,
        }
    }
}

/// The current time as `2026-11-02T14:54:12Z` (UTC), like vcad's log.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc_text(secs)
}

fn utc_text(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Ctrl-C/Ctrl-Break sent to the app's console (in `tauri dev`) must not
    // reach vcad.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn no_window(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Terminal signals to the app's process group must not reach vcad.
    cmd.process_group(0);
}

#[cfg(not(any(unix, windows)))]
fn no_window(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Record {
        let pid = std::process::id();
        let (_, started_at) = observe(pid).expect("the test process is visible");
        Record {
            pid,
            exe: std::env::current_exe().unwrap(),
            started_at,
        }
    }

    #[test]
    fn verifies_a_live_process_by_exe_and_start_time() {
        let record = me();
        let exe = record.exe.clone();
        assert!(verify(&record, &exe));
        // Without a recorded start time, the executable alone decides.
        assert!(verify(
            &Record {
                started_at: 0,
                ..record.clone()
            },
            &exe
        ));
    }

    #[test]
    fn rejects_other_executables_start_times_and_dead_pids() {
        let record = me();
        let exe = record.exe.clone();
        let other = exe.with_file_name("not-vcad");
        assert!(!verify(&record, &other), "expected executable differs");
        assert!(!verify(
            &Record {
                exe: other.clone(),
                ..record.clone()
            },
            &exe
        ));
        assert!(!verify(
            &Record {
                started_at: record.started_at.saturating_sub(3600),
                ..record.clone()
            },
            &exe
        ));
        // PIDs are multiples of 4 on Windows and below 2^22 on Linux; this one
        // is not in use.
        assert!(!verify(
            &Record {
                pid: u32::MAX - 2,
                ..record
            },
            &exe
        ));
    }

    #[test]
    fn stale_records_are_dropped_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let stale = Record {
            pid: u32::MAX - 2,
            exe: exe.clone(),
            started_at: 1,
        };
        std::fs::write(
            dir.path().join(RECORD_FILE),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();
        let sidecar = Sidecar::open(dir.path().to_owned(), Some(exe));
        assert_eq!(sidecar.owned_pid(), None);
        assert!(!dir.path().join(RECORD_FILE).exists());
        assert!(!sidecar.kill(), "nothing to stop");
    }

    #[test]
    fn a_verified_record_survives_open_and_is_owned() {
        let dir = tempfile::tempdir().unwrap();
        let record = me();
        std::fs::write(
            dir.path().join(RECORD_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let sidecar = Sidecar::open(dir.path().to_owned(), Some(record.exe.clone()));
        assert_eq!(sidecar.owned_pid(), Some(record.pid));
        // (Not killed here: that would end the test process.)
    }

    #[test]
    fn utc_text_matches_the_log_format() {
        assert_eq!(utc_text(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_text(1_793_631_252), "2026-11-02T14:54:12Z");
        assert_eq!(utc_text(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn spawning_needs_the_bundled_binary() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = Sidecar::open(dir.path().to_owned(), None);
        let err = sidecar.spawn(Path::new("vca-test-endpoint")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(sidecar.owned_pid(), None);
    }
}
