//! The remote shell audit log (rule 9): append-only JSON lines in
//! `<data dir>/audit.jsonl`, rotated at 5 MB (one `audit.jsonl.1` kept).
//! Entries never contain TOTP codes; command text is redacted with the
//! transport's secret patterns.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use vca_core::{AuditEntry, SessionId, UnixMillis};

use crate::util::lock;

/// File name below the data directory.
pub const AUDIT_FILE: &str = "audit.jsonl";
/// Rotation threshold.
pub const AUDIT_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Largest `ListAudit` answer.
pub const MAX_LIST: usize = 1000;
/// `ListAudit { limit: 0 }`.
pub const DEFAULT_LIST: usize = 100;

/// Audit event names.
pub mod events {
    pub const SHELL_OPEN: &str = "shell_open";
    pub const SHELL_CLOSE: &str = "shell_close";
    pub const SHELL_RUN: &str = "shell_run";
    /// Another admin device joined a shell with a code.
    pub const SHELL_JOIN: &str = "shell_join";
    pub const STEP_UP_OK: &str = "step_up_ok";
    pub const STEP_UP_FAILED: &str = "step_up_failed";
    pub const RESTORE_RUN: &str = "restore_run";
    pub const RESTORE_EDIT: &str = "restore_edit";
    /// Windows arranged in a grid, or a saved screen layout changed.
    pub const WINDOW_ARRANGE: &str = "window_arrange";
    /// An ended session's card removed before its retention time.
    pub const SESSION_DISMISS: &str = "session_dismiss";
    /// The commonly misheard words saved. The detail holds counts only: the
    /// words themselves can be private names.
    pub const PERSONAL_TERMS_SAVE: &str = "personal_terms_save";
}

/// Append-only JSONL audit log.
#[derive(Debug)]
pub struct ShellAudit {
    path: PathBuf,
    max_bytes: u64,
    write: Mutex<()>,
}

/// `audit.jsonl` -> `audit.jsonl.1`.
fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

impl ShellAudit {
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            path: path.into(),
            max_bytes: max_bytes.max(1),
            write: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one entry. Failures are logged, never fatal.
    pub fn record(
        &self,
        ts: UnixMillis,
        device_id: &str,
        device_name: &str,
        event: &str,
        session_id: Option<&SessionId>,
        detail: &str,
    ) {
        let entry = AuditEntry {
            ts,
            device_id: device_id.to_owned(),
            device_name: device_name.to_owned(),
            event: event.to_owned(),
            session_id: session_id.cloned(),
            detail: detail.to_owned(),
        };
        if let Err(err) = self.append(&entry) {
            tracing::warn!(%err, path = %self.path.display(), "cannot write the shell audit log");
        }
    }

    fn append(&self, entry: &AuditEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        line.push('\n');
        let _guard = lock(&self.write);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size + line.len() as u64 > self.max_bytes {
            // Keeps exactly one older file.
            let old = rotated(&self.path);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&self.path, &old)?;
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.flush()
    }

    /// The newest `limit` entries (0 = default), newest first. Lines that
    /// do not parse are skipped.
    pub fn recent(&self, limit: u32) -> Vec<AuditEntry> {
        let limit = match limit as usize {
            0 => DEFAULT_LIST,
            n => n.min(MAX_LIST),
        };
        let _guard = lock(&self.write);
        let mut out: Vec<AuditEntry> = Vec::new();
        for path in [self.path.clone(), rotated(&self.path)] {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines().rev() {
                if out.len() >= limit {
                    return out;
                }
                if let Ok(entry) = serde_json::from_str::<AuditEntry>(line) {
                    out.push(entry);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lists_newest_first_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(AUDIT_FILE);
        let audit = ShellAudit::new(&path, 600);
        for i in 0..12 {
            audit.record(
                i,
                "dev_1",
                "phone",
                events::SHELL_RUN,
                Some(&SessionId::from("s1")),
                &format!("command {i}"),
            );
        }
        assert!(path.exists());
        assert!(rotated(&path).exists());
        assert!(std::fs::metadata(&path).unwrap().len() <= 600);
        assert!(std::fs::metadata(rotated(&path)).unwrap().len() <= 600);
        // Only one older file is kept.
        assert!(!dir.path().join("data").join("audit.jsonl.2").exists());
        let recent = audit.recent(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].ts, 11);
        assert_eq!(recent[2].ts, 9);
        assert_eq!(recent[0].detail, "command 11");
        // Entries span both files.
        let all = audit.recent(0);
        assert!(all.len() > std::fs::read_to_string(&path).unwrap().lines().count());
        assert!(all.windows(2).all(|w| w[0].ts > w[1].ts));
        // The lines are the protocol's entry shape.
        let first = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
        for key in [
            "ts",
            "device_id",
            "device_name",
            "event",
            "session_id",
            "detail",
        ] {
            assert!(v.get(key).is_some(), "{key} in {v}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_missing_log_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let audit = ShellAudit::new(dir.path().join(AUDIT_FILE), AUDIT_MAX_BYTES);
        assert!(audit.recent(10).is_empty());
    }
}
