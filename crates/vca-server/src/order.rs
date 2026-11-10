//! The order of session cards chosen by the user, kept in
//! `data_dir/session-order.json` so every client of this computer shows
//! the same order (and the same 1..N session numbers voice commands use).
//!
//! Session ids are new after every vcad restart and every window restore,
//! so the file stores stable keys instead, the same identity the windows
//! restore matches by (see [`crate::restore::matching`]):
//!
//! 1. `conv:<agent>:<agent session id>`: the exact conversation (survives a
//!    restart and a resumed restore). Cloud sessions use
//!    `cloud:<provider>:<remote id>`.
//! 2. `id:<session id>`: only within one vcad run, so two sessions that
//!    share every other key can still trade places.
//! 3. `place:<agent>:<folder>:<display name>`: the restore entry's agent,
//!    folder and window title.
//! 4. `folder:<agent>:<folder>`: the agent and folder alone, only settling
//!    a match while an entry is still free.
//!
//! Folders compare like the restore list does (case and separators on
//! Windows). Each stored entry is one card with every key it was seen
//! with, so a session found by its folder later learns its conversation
//! key, and a rename keeps its place. Sessions that match no entry come
//! after the ordered ones, in the default order (oldest first).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::names::write_atomic;
use crate::util::lock;

/// File name below the data directory.
pub const ORDER_FILE: &str = "session-order.json";
const FILE_VERSION: u32 = 1;
/// Keys remembered per card (the newest win).
const MAX_KEYS: usize = 8;
/// Cards remembered, live and gone.
const MAX_ENTRIES: usize = 300;

/// The stable keys of one live session, strongest first. Built by
/// [`crate::sessions::Session::order_keys`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderKeys {
    /// `conv:` or `cloud:` key.
    pub conversation: Option<String>,
    /// `id:<session id>`: tells apart sessions that share every other key,
    /// within one vcad run.
    pub session: Option<String>,
    /// `place:` key.
    pub place: Option<String>,
    /// `folder:` key.
    pub folder: Option<String>,
}

impl OrderKeys {
    fn tiers(&self) -> [Option<&str>; 4] {
        [
            self.conversation.as_deref(),
            self.session.as_deref(),
            self.place.as_deref(),
            self.folder.as_deref(),
        ]
    }

    fn all(&self) -> impl Iterator<Item = &str> {
        self.tiers().into_iter().flatten()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct OrderEntry {
    keys: Vec<String>,
}

impl OrderEntry {
    fn has(&self, key: &str) -> bool {
        self.keys.iter().any(|k| k == key)
    }

    /// Adds the keys it lacks; true when it changed.
    fn learn(&mut self, keys: &OrderKeys) -> bool {
        let mut changed = false;
        for key in keys.all() {
            if !self.has(key) {
                self.keys.push(key.to_owned());
                changed = true;
            }
        }
        // Over the cap, the oldest keys the session no longer has go first.
        while self.keys.len() > MAX_KEYS {
            let old = self
                .keys
                .iter()
                .position(|k| !keys.all().any(|c| c == k))
                .unwrap_or(0);
            self.keys.remove(old);
        }
        changed
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OrderFile {
    version: u32,
    #[serde(default)]
    entries: Vec<OrderEntry>,
}

/// The persisted card order. Without a path it lives in memory only.
pub(crate) struct OrderStore {
    path: Option<PathBuf>,
    entries: Mutex<Vec<OrderEntry>>,
}

/// For each session, the index of the stored entry it is, if any. Each
/// entry belongs to one session at most; stronger keys are matched first,
/// sessions in the given (default) order.
fn claim(entries: &[OrderEntry], sessions: &[OrderKeys]) -> Vec<Option<usize>> {
    let mut out = vec![None; sessions.len()];
    let mut taken = vec![false; entries.len()];
    for tier in 0..4 {
        for (s, keys) in sessions.iter().enumerate() {
            if out[s].is_some() {
                continue;
            }
            let Some(key) = keys.tiers()[tier] else {
                continue;
            };
            let hit = entries
                .iter()
                .enumerate()
                .position(|(e, entry)| !taken[e] && entry.has(key));
            if let Some(e) = hit {
                out[s] = Some(e);
                taken[e] = true;
            }
        }
    }
    out
}

impl OrderStore {
    /// Loads the order. A missing file is empty; a damaged one is moved
    /// aside (`.bad`) and the default order applies.
    pub(crate) fn open(path: Option<PathBuf>) -> Self {
        let entries = path.as_deref().map(load).unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    /// True when the user has set an order.
    pub(crate) fn is_custom(&self) -> bool {
        !lock(&self.entries).is_empty()
    }

    /// The display order of `sessions` (given in the default order), as
    /// indexes into it. Matched sessions also teach their entry the keys
    /// they have now (written to disk when that changed).
    pub(crate) fn arrange(&self, sessions: &[OrderKeys]) -> Vec<usize> {
        let mut entries = lock(&self.entries);
        if entries.is_empty() {
            return (0..sessions.len()).collect();
        }
        let claimed = claim(&entries, sessions);
        let mut changed = false;
        for (s, e) in claimed.iter().enumerate() {
            if let Some(e) = e {
                changed |= entries[*e].learn(&sessions[s]);
            }
        }
        if changed {
            self.save_logged(&entries);
        }
        let mut out: Vec<usize> = (0..sessions.len()).collect();
        // Stable: unmatched sessions keep the default order after the rest.
        out.sort_by_key(|s| claimed[*s].unwrap_or(usize::MAX));
        out
    }

    /// Stores `ordered` (the live sessions, first card first) as the order.
    /// Cards that are not running now keep their place after the card that
    /// preceded them, so they come back where they were.
    pub(crate) fn set(&self, ordered: &[OrderKeys]) -> std::io::Result<()> {
        let mut entries = lock(&self.entries);
        let claimed = claim(&entries, ordered);
        let mut owner: HashMap<usize, usize> = HashMap::new();
        for (s, e) in claimed.iter().enumerate() {
            if let Some(e) = e {
                owner.insert(*e, s);
            }
        }
        // Gone cards, grouped by the live card before them (`None`: first).
        let mut stale: HashMap<Option<usize>, Vec<OrderEntry>> = HashMap::new();
        let mut anchor = None;
        for (e, entry) in entries.iter().enumerate() {
            match owner.get(&e) {
                Some(s) => anchor = Some(*s),
                None => stale.entry(anchor).or_default().push(entry.clone()),
            }
        }
        let mut next: Vec<OrderEntry> = stale.remove(&None).unwrap_or_default();
        for (s, keys) in ordered.iter().enumerate() {
            let mut entry = claimed[s].map(|e| entries[e].clone()).unwrap_or_default();
            entry.learn(keys);
            if !entry.keys.is_empty() {
                next.push(entry);
            }
            next.extend(stale.remove(&Some(s)).unwrap_or_default());
        }
        if next.len() > MAX_ENTRIES {
            // Drop gone cards first, from the end.
            let live: Vec<bool> = next
                .iter()
                .map(|entry| ordered.iter().any(|keys| keys.all().any(|k| entry.has(k))))
                .collect();
            let mut excess = next.len() - MAX_ENTRIES;
            let mut keep = vec![true; next.len()];
            for i in (0..next.len()).rev() {
                if excess == 0 {
                    break;
                }
                if !live[i] {
                    keep[i] = false;
                    excess -= 1;
                }
            }
            let mut i = 0;
            next.retain(|_| {
                let k = keep[i];
                i += 1;
                k
            });
            next.truncate(MAX_ENTRIES);
        }
        self.save(&next)?;
        *entries = next;
        Ok(())
    }

    /// Forgets the custom order: the default order applies again.
    pub(crate) fn reset(&self) -> std::io::Result<()> {
        let mut entries = lock(&self.entries);
        if entries.is_empty() {
            return Ok(());
        }
        self.save(&[])?;
        entries.clear();
        Ok(())
    }

    fn save_logged(&self, entries: &[OrderEntry]) {
        if let Err(err) = self.save(entries) {
            tracing::warn!(%err, "cannot write {ORDER_FILE}");
        }
    }

    fn save(&self, entries: &[OrderEntry]) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = OrderFile {
            version: FILE_VERSION,
            entries: entries.to_vec(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        write_atomic(path, &json)
    }
}

fn load(path: &Path) -> Vec<OrderEntry> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "cannot read the session order");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<OrderFile>(&bytes) {
        Ok(file) if file.version == FILE_VERSION => file.entries,
        Ok(file) => {
            tracing::warn!(
                version = file.version,
                "unknown {ORDER_FILE} version; ignoring"
            );
            Vec::new()
        }
        Err(err) => {
            let bad = path.with_extension("json.bad");
            tracing::warn!(%err, moved_to = %bad.display(), "{ORDER_FILE} is damaged");
            let _ = std::fs::rename(path, bad);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(conv: Option<&str>, place: &str, folder: &str) -> OrderKeys {
        OrderKeys {
            conversation: conv.map(|c| format!("conv:claude_code:{c}")),
            session: None,
            place: Some(format!("place:claude_code:{folder}:{place}")),
            folder: Some(format!("folder:claude_code:{folder}")),
        }
    }

    #[test]
    fn without_an_order_the_default_order_applies() {
        let store = OrderStore::open(None);
        assert!(!store.is_custom());
        let s = [keys(None, "a", "/a"), keys(None, "b", "/b")];
        assert_eq!(store.arrange(&s), vec![0, 1]);
    }

    #[test]
    fn the_order_persists_and_unknown_sessions_go_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ORDER_FILE);
        let a = keys(Some("A"), "api", "/p/api");
        let b = keys(Some("B"), "web", "/p/web");
        let c = keys(None, "docs", "/p/docs");
        let store = OrderStore::open(Some(path.clone()));
        // The user puts c, a, b.
        store.set(&[c.clone(), a.clone(), b.clone()]).unwrap();
        assert!(store.is_custom());
        drop(store);

        // After a restart (new session ids, new default order) and with a
        // new session d, the stored order still decides; d goes last.
        let store = OrderStore::open(Some(path));
        let d = keys(None, "new", "/p/new");
        let live = [d.clone(), b.clone(), a.clone(), c.clone()];
        let order = store.arrange(&live);
        assert_eq!(order, vec![3, 2, 1, 0]);
    }

    #[test]
    fn keys_survive_a_restore_and_a_rename() {
        let store = OrderStore::open(None);
        let a = keys(None, "api", "C:\\p\\api");
        let b = keys(None, "web", "C:\\p\\web");
        store.set(&[b.clone(), a.clone()]).unwrap();
        // After a restore the agent reports its conversation id: the card is
        // still found by its place and learns the new key.
        let b2 = keys(Some("B"), "web", "C:\\p\\web");
        assert_eq!(store.arrange(&[a.clone(), b2.clone()]), vec![1, 0]);
        // Renamed: the conversation key still finds it.
        let b3 = keys(Some("B"), "renamed", "C:\\p\\web");
        assert_eq!(store.arrange(&[a.clone(), b3]), vec![1, 0]);
        // A session with neither key but the same folder takes a free entry.
        let a2 = keys(None, "other title", "C:\\p\\api");
        assert_eq!(store.arrange(&[a2, b2]), vec![1, 0]);
    }

    #[test]
    fn two_sessions_in_one_folder_keep_their_own_places() {
        let store = OrderStore::open(None);
        let x = keys(Some("X"), "same", "/p");
        let y = keys(Some("Y"), "same", "/p");
        store.set(&[y.clone(), x.clone()]).unwrap();
        assert_eq!(store.arrange(&[x, y]), vec![1, 0]);
    }

    #[test]
    fn gone_cards_keep_their_place() {
        let store = OrderStore::open(None);
        let a = keys(Some("A"), "a", "/a");
        let b = keys(Some("B"), "b", "/b");
        let c = keys(Some("C"), "c", "/c");
        store.set(&[a.clone(), b.clone(), c.clone()]).unwrap();
        // b is closed; the user swaps a and c.
        store.set(&[c.clone(), a.clone()]).unwrap();
        // b comes back: still after a.
        assert_eq!(store.arrange(&[a, b, c]), vec![2, 0, 1]);
    }

    #[test]
    fn reset_restores_the_default_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ORDER_FILE);
        let store = OrderStore::open(Some(path.clone()));
        let a = keys(Some("A"), "a", "/a");
        let b = keys(Some("B"), "b", "/b");
        store.set(&[b.clone(), a.clone()]).unwrap();
        store.reset().unwrap();
        assert!(!store.is_custom());
        assert_eq!(store.arrange(&[a.clone(), b.clone()]), vec![0, 1]);
        assert!(!OrderStore::open(Some(path)).is_custom());
    }

    #[test]
    fn a_damaged_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ORDER_FILE);
        std::fs::write(&path, b"{oops").unwrap();
        let store = OrderStore::open(Some(path.clone()));
        assert!(!store.is_custom());
        assert!(path.with_extension("json.bad").exists());
    }

    #[test]
    fn keys_per_card_are_capped() {
        let mut entry = OrderEntry::default();
        for i in 0..20 {
            entry.learn(&keys(None, &format!("n{i}"), "/a"));
        }
        assert!(entry.keys.len() <= MAX_KEYS);
        assert!(entry.has("place:claude_code:/a:n19"));
        // The keys the session has now are never the ones dropped.
        assert!(entry.has("folder:claude_code:/a"));
    }
}
