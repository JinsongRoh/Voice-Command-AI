//! A cache of per-file metadata for discovery.
//!
//! Discovery runs every few seconds and looks at every stored transcript.
//! Reading each one again on every pass made the work grow with every
//! transcript ever stored. [`MetaCache`] keeps what was read from a file
//! together with the file's size and modification time and reads it again
//! only when either changed. Entries of files that were not seen in a pass
//! (deleted, or no longer looked at) are dropped at the end of the pass, so
//! the cache never holds more entries than there are files.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

/// Files modified within this time are checked with an exact `stat` of the
/// file instead of the directory listing. On Windows a directory listing can
/// report an old size and time for a file another process keeps open for
/// writing; an open file is a recently active one.
pub(crate) const HOT_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);

/// What identifies one version of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    pub(crate) fn of(meta: &Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }

    pub(crate) fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Modified within [`HOT_WINDOW`] of `now`.
    pub(crate) fn is_recent(&self, now: SystemTime) -> bool {
        self.modified
            .is_some_and(|m| now.duration_since(m).map_or(true, |age| age < HOT_WINDOW))
    }
}

/// The stamp of a directory entry: from the listing (free on Windows, one
/// `stat` elsewhere), or from the file itself when `exact`.
pub(crate) fn stamp(entry: &std::fs::DirEntry, exact: bool) -> Option<FileStamp> {
    if exact && let Ok(meta) = std::fs::metadata(entry.path()) {
        return Some(FileStamp::of(&meta));
    }
    entry.metadata().ok().map(|m| FileStamp::of(&m))
}

#[derive(Debug)]
struct Entry<M> {
    stamp: FileStamp,
    pass: u64,
    meta: M,
}

/// Metadata per file path, valid while the file's [`FileStamp`] is
/// unchanged.
#[derive(Debug)]
pub(crate) struct MetaCache<M> {
    entries: HashMap<PathBuf, Entry<M>>,
    pass: u64,
    reads: u64,
}

impl<M> Default for MetaCache<M> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            pass: 0,
            reads: 0,
        }
    }
}

impl<M: Clone> MetaCache<M> {
    /// Starts a pass; see [`MetaCache::finish`].
    pub(crate) fn begin(&mut self) {
        self.pass += 1;
    }

    /// The metadata of `path`, read with `read` only when the file is new
    /// to the cache or its stamp changed.
    pub(crate) fn get(&mut self, path: &Path, stamp: FileStamp, read: impl FnOnce() -> M) -> M {
        let pass = self.pass;
        if let Some(entry) = self.entries.get_mut(path)
            && entry.stamp == stamp
        {
            entry.pass = pass;
            return entry.meta.clone();
        }
        self.reads += 1;
        let meta = read();
        self.entries.insert(
            path.to_owned(),
            Entry {
                stamp,
                pass,
                meta: meta.clone(),
            },
        );
        meta
    }

    /// The cached metadata of `path`, without reading or marking it.
    pub(crate) fn peek(&self, path: &Path) -> Option<&M> {
        self.entries.get(path).map(|e| &e.meta)
    }

    /// Ends a pass: drops every entry not looked up since [`MetaCache::begin`].
    pub(crate) fn finish(&mut self) {
        let pass = self.pass;
        self.entries.retain(|_, e| e.pass == pass);
    }

    /// Number of cached files.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Number of file reads so far.
    #[cfg(test)]
    pub(crate) fn reads(&self) -> u64 {
        self.reads
    }
}

/// A [`MetaCache`] shared by clones of an adapter.
pub(crate) struct SharedCache<M>(Arc<Mutex<MetaCache<M>>>);

impl<M> Default for SharedCache<M> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(MetaCache::default())))
    }
}

impl<M> Clone for SharedCache<M> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<M> std::fmt::Debug for SharedCache<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.0.lock().map_or(0, |c| c.entries.len());
        f.debug_struct("SharedCache").field("files", &len).finish()
    }
}

impl<M> SharedCache<M> {
    pub(crate) fn lock(&self) -> MutexGuard<'_, MetaCache<M>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(len: u64, secs: u64) -> FileStamp {
        FileStamp {
            len,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
        }
    }

    #[test]
    fn rereads_only_changed_files_and_evicts_unseen_ones() {
        let mut cache = MetaCache::default();
        let (a, b) = (Path::new("a"), Path::new("b"));
        cache.begin();
        assert_eq!(cache.get(a, stamp(1, 1), || "a1"), "a1");
        assert_eq!(cache.get(b, stamp(1, 1), || "b1"), "b1");
        cache.finish();
        assert_eq!(cache.reads(), 2);

        // Unchanged: served from the cache.
        cache.begin();
        assert_eq!(cache.get(a, stamp(1, 1), || "never"), "a1");
        // Grown: read again.
        assert_eq!(cache.get(b, stamp(2, 1), || "b2"), "b2");
        cache.finish();
        assert_eq!(cache.reads(), 3);

        // Touched without growing (a rewrite of the same size): read again.
        cache.begin();
        assert_eq!(cache.get(a, stamp(1, 2), || "a2"), "a2");
        // `b` was deleted: not looked up, so evicted.
        cache.finish();
        assert_eq!(cache.len(), 1);
        assert!(cache.peek(b).is_none());
        assert_eq!(cache.peek(a), Some(&"a2"));
    }

    #[test]
    fn recent_means_within_the_hot_window() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(stamp(1, 1_000_000 - 60).is_recent(now));
        assert!(!stamp(1, 1_000_000 - HOT_WINDOW.as_secs() - 1).is_recent(now));
        // A clock skewed into the future counts as recent.
        assert!(stamp(1, 1_000_100).is_recent(now));
        assert!(
            !FileStamp {
                len: 1,
                modified: None
            }
            .is_recent(now)
        );
    }
}
