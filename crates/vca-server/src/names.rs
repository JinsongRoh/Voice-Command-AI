//! Session names chosen by the user, kept in `data_dir/session-names.json`.
//!
//! Names are keyed by a stable session key (see `Session::name_key`):
//! managed and cloud sessions by their session id, observed sessions by the
//! agent kind plus the agent's own session id (or transcript, or project
//! directory and process start identity). The file is rewritten atomically
//! (temporary file + rename) on every change.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vca_core::{UnixMillis, now_millis};

use crate::util::lock;

/// File name below the data directory.
pub const NAMES_FILE: &str = "session-names.json";
const FILE_VERSION: u32 = 1;
/// Upper bound on stored names; the oldest are dropped beyond it.
const MAX_ENTRIES: usize = 1000;
/// Keys of managed sessions. Managed sessions end with vcad, so their
/// names are dropped when the file is loaded.
pub(crate) const MANAGED_PREFIX: &str = "managed:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NameEntry {
    name: String,
    updated_at: UnixMillis,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct NameFile {
    version: u32,
    #[serde(default)]
    names: BTreeMap<String, NameEntry>,
}

/// The persisted custom names. Without a path, names live in memory only.
pub(crate) struct NameStore {
    path: Option<PathBuf>,
    names: Mutex<BTreeMap<String, NameEntry>>,
}

impl NameStore {
    /// Loads the names. A missing file is empty; an unreadable one is
    /// moved aside (`.bad`) so it is not lost, and the store starts empty.
    pub(crate) fn open(path: Option<PathBuf>) -> Self {
        let mut names = path.as_deref().map(load).unwrap_or_default();
        let before = names.len();
        names.retain(|k, _| !k.starts_with(MANAGED_PREFIX));
        let pruned = names.len() != before;
        let store = Self {
            path,
            names: Mutex::new(names),
        };
        if pruned {
            let names = store.names();
            store.save_logged(&names);
        }
        store
    }

    fn names(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, NameEntry>> {
        lock(&self.names)
    }

    pub(crate) fn get(&self, key: &str) -> Option<String> {
        self.names().get(key).map(|e| e.name.clone())
    }

    /// Stores (`Some`) or removes (`None`) the name of `key` and writes the
    /// file. On a write error the in-memory state is left unchanged.
    pub(crate) fn set(&self, key: &str, name: Option<String>) -> std::io::Result<()> {
        let mut names = self.names();
        let mut next = names.clone();
        match name {
            Some(name) => {
                next.insert(
                    key.to_owned(),
                    NameEntry {
                        name,
                        updated_at: now_millis(),
                    },
                );
                prune(&mut next);
            }
            None => {
                if next.remove(key).is_none() {
                    return Ok(());
                }
            }
        }
        self.save(&next)?;
        *names = next;
        Ok(())
    }

    /// Drops the name of a session that is gone for good. Errors are
    /// logged.
    pub(crate) fn forget(&self, key: &str) {
        let mut names = self.names();
        if names.remove(key).is_some() {
            self.save_logged(&names);
        }
    }

    fn save_logged(&self, names: &BTreeMap<String, NameEntry>) {
        if let Err(err) = self.save(names) {
            tracing::warn!(%err, "cannot write {NAMES_FILE}");
        }
    }

    fn save(&self, names: &BTreeMap<String, NameEntry>) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = NameFile {
            version: FILE_VERSION,
            names: names.clone(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        write_atomic(path, &json)
    }
}

/// Keeps the newest [`MAX_ENTRIES`] names.
fn prune(names: &mut BTreeMap<String, NameEntry>) {
    while names.len() > MAX_ENTRIES {
        let oldest = names
            .iter()
            .min_by_key(|(_, e)| e.updated_at)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => names.remove(&k),
            None => break,
        };
    }
}

fn load(path: &Path) -> BTreeMap<String, NameEntry> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "cannot read session names");
            return BTreeMap::new();
        }
    };
    match serde_json::from_slice::<NameFile>(&bytes) {
        Ok(file) if file.version == FILE_VERSION => file.names,
        Ok(file) => {
            tracing::warn!(
                version = file.version,
                "unknown {NAMES_FILE} version; ignoring"
            );
            BTreeMap::new()
        }
        Err(err) => {
            let bad = path.with_extension("json.bad");
            tracing::warn!(%err, moved_to = %bad.display(), "{NAMES_FILE} is damaged");
            let _ = std::fs::rename(path, bad);
            BTreeMap::new()
        }
    }
}

/// Writes `data` to a temporary file next to `path`, then renames it over
/// `path`, so readers see either the old or the new file.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| NAMES_FILE.to_owned());
    let tmp = dir.join(format!(
        ".{file_name}.{}.tmp",
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
    use super::*;

    #[test]
    fn names_persist_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(NAMES_FILE);
        let store = NameStore::open(Some(path.clone()));
        assert_eq!(store.get("observed:claude_code:session:abc"), None);
        store
            .set("observed:claude_code:session:abc", Some("API work".into()))
            .unwrap();
        store.set("managed:s1", Some("temp".into())).unwrap();
        drop(store);

        let store = NameStore::open(Some(path.clone()));
        assert_eq!(
            store.get("observed:claude_code:session:abc").as_deref(),
            Some("API work")
        );
        // Managed sessions do not outlive vcad.
        assert_eq!(store.get("managed:s1"), None);
        store.set("observed:claude_code:session:abc", None).unwrap();
        drop(store);
        assert_eq!(
            NameStore::open(Some(path)).get("observed:claude_code:session:abc"),
            None
        );
        // No temporary files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("data"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn damaged_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(NAMES_FILE);
        std::fs::write(&path, b"{not json").unwrap();
        let store = NameStore::open(Some(path.clone()));
        assert_eq!(store.get("x"), None);
        assert!(path.with_extension("json.bad").exists());
        store.set("x", Some("y".into())).unwrap();
        assert_eq!(NameStore::open(Some(path)).get("x").as_deref(), Some("y"));
    }

    #[test]
    fn oldest_names_are_pruned() {
        let mut names = BTreeMap::new();
        for i in 0..=MAX_ENTRIES {
            names.insert(
                format!("k{i}"),
                NameEntry {
                    name: "n".into(),
                    updated_at: i as u64,
                },
            );
        }
        prune(&mut names);
        assert_eq!(names.len(), MAX_ENTRIES);
        assert!(!names.contains_key("k0"));
    }
}
