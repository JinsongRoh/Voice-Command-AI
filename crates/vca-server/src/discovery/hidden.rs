//! Observed sessions the person took off the list, kept in the server's
//! data directory so they stay hidden across server restarts.
//!
//! An entry names one process (pid and start time): when that process ends
//! the entry is dropped, so a later session in the same folder is listed
//! again. Hiding never touches the process.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vca_adapters::ProcessTable;
use vca_core::{AgentKind, SessionId, UnixMillis};

use crate::util::lock;

/// File name in the data directory.
pub(crate) const HIDDEN_FILE: &str = "hidden-sessions.json";
/// Entries kept at most (the oldest go first).
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HiddenEntry {
    pub pid: u32,
    /// Start time of the process, so a reused pid is not hidden.
    #[serde(default)]
    pub process_start: Option<u64>,
    pub agent: AgentKind,
    #[serde(default)]
    pub project_dir: Option<String>,
    /// The session id it had when it was hidden.
    #[serde(default)]
    pub session_id: Option<SessionId>,
    pub hidden_at: UnixMillis,
}

impl HiddenEntry {
    fn is(&self, pid: u32, process_start: Option<u64>) -> bool {
        self.pid == pid
            && match (self.process_start, process_start) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HiddenFile {
    #[serde(default)]
    hidden: Vec<HiddenEntry>,
}

/// The hidden list. Without a path it lives in memory only.
#[derive(Debug)]
pub(crate) struct HiddenStore {
    path: Option<PathBuf>,
    entries: Mutex<Vec<HiddenEntry>>,
}

impl HiddenStore {
    pub(crate) fn open(path: Option<PathBuf>) -> Self {
        let entries = path.as_deref().map(load).unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    pub(crate) fn contains(&self, pid: u32, process_start: Option<u64>) -> bool {
        lock(&self.entries).iter().any(|e| e.is(pid, process_start))
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<HiddenEntry> {
        lock(&self.entries).clone()
    }

    pub(crate) fn add(&self, entry: HiddenEntry) -> std::io::Result<()> {
        let mut entries = lock(&self.entries);
        entries.retain(|e| !e.is(entry.pid, entry.process_start));
        entries.push(entry);
        if entries.len() > MAX_ENTRIES {
            let extra = entries.len() - MAX_ENTRIES;
            entries.drain(..extra);
        }
        self.save(&entries)
    }

    /// Removes the entries `matches` accepts; returns how many went.
    pub(crate) fn remove_where(
        &self,
        mut matches: impl FnMut(&HiddenEntry) -> bool,
    ) -> std::io::Result<usize> {
        let mut entries = lock(&self.entries);
        let before = entries.len();
        entries.retain(|e| !matches(e));
        let removed = before - entries.len();
        if removed > 0 {
            self.save(&entries)?;
        }
        Ok(removed)
    }

    /// Drops entries whose process ended. An empty table (a failed
    /// snapshot) changes nothing.
    pub(crate) fn prune(&self, procs: &ProcessTable) {
        if procs.iter().next().is_none() {
            return;
        }
        let result = self.remove_where(|e| match procs.get(e.pid) {
            Some(p) => e.process_start.is_some_and(|s| s != p.start_time),
            None => true,
        });
        if let Err(err) = result {
            tracing::warn!(%err, "cannot save {HIDDEN_FILE}");
        }
    }

    fn save(&self, entries: &[HiddenEntry]) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = HiddenFile {
            hidden: entries.to_vec(),
        };
        let data = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        write_atomic(path, &data)
    }
}

fn load(path: &Path) -> Vec<HiddenEntry> {
    match std::fs::read(path) {
        Ok(data) => match serde_json::from_slice::<HiddenFile>(&data) {
            Ok(file) => file.hidden,
            Err(err) => {
                tracing::warn!(%err, "{HIDDEN_FILE} is damaged; starting empty");
                Vec::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            tracing::warn!(%err, "cannot read {HIDDEN_FILE}");
            Vec::new()
        }
    }
}

/// Writes `data` to a temporary file next to `path`, then renames it over
/// `path`, so readers see either the old or the new file.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{HIDDEN_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use vca_adapters::ProcInfo;

    use super::*;

    fn entry(pid: u32, start: u64) -> HiddenEntry {
        HiddenEntry {
            pid,
            process_start: Some(start),
            agent: AgentKind::ClaudeCode,
            project_dir: Some("/code/shop".into()),
            session_id: None,
            hidden_at: 1,
        }
    }

    fn running(pid: u32, start: u64) -> ProcInfo {
        ProcInfo {
            pid,
            parent: None,
            name: "claude".into(),
            cmd: vec!["claude".into()],
            exe: None,
            cwd: None,
            start_time: start,
        }
    }

    #[test]
    fn persists_and_prunes_ended_processes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(HIDDEN_FILE);
        let store = HiddenStore::open(Some(path.clone()));
        store.add(entry(10, 100)).unwrap();
        store.add(entry(11, 200)).unwrap();
        store.add(entry(10, 100)).unwrap();
        assert_eq!(store.entries().len(), 2);

        let reopened = HiddenStore::open(Some(path.clone()));
        assert!(reopened.contains(10, Some(100)));
        // The pid was reused by another process.
        assert!(!reopened.contains(10, Some(101)));

        // An empty snapshot (failure) keeps everything.
        reopened.prune(&ProcessTable::default());
        assert_eq!(reopened.entries().len(), 2);
        // Process 11 ended; 10 still runs.
        reopened.prune(&ProcessTable::from_procs(vec![running(10, 100)]));
        assert_eq!(reopened.entries(), vec![entry(10, 100)]);
        assert_eq!(HiddenStore::open(Some(path)).entries().len(), 1);
    }

    #[test]
    fn damaged_file_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(HIDDEN_FILE);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(HiddenStore::open(Some(path)).entries().is_empty());
    }
}
