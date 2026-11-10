//! The managed list on disk: `<data dir>/restore.json`, written atomically
//! (temporary file + rename), keeping the previous good file as `.bak`, so a
//! power cut leaves the last good layout.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vca_core::{ArrangeLayout, DEFAULT_RESTORE_DELAY_SECS, RestoreProfile, UnixMillis};

/// File name below the data directory.
pub const RESTORE_FILE: &str = "restore.json";
const FILE_VERSION: u32 = 1;
/// Id of the profile every computer starts with.
pub const DEFAULT_PROFILE: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFile {
    pub version: u32,
    /// Id of the profile the snapshot updates and "restore" uses.
    pub active: String,
    pub profiles: Vec<RestoreProfile>,
    /// When the list last changed.
    #[serde(default)]
    pub saved_at: UnixMillis,
    /// Pause after sign-in when restore at sign-in is on.
    #[serde(default)]
    pub sign_in_delay_secs: u32,
    /// The named screen layouts. In this file for its atomic write, backup
    /// and recovery; a file written before they existed has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layouts: Vec<ArrangeLayout>,
}

impl Default for RestoreFile {
    fn default() -> Self {
        Self {
            version: FILE_VERSION,
            active: DEFAULT_PROFILE.into(),
            profiles: vec![RestoreProfile {
                id: DEFAULT_PROFILE.into(),
                name: "Work".into(),
                delay_secs: DEFAULT_RESTORE_DELAY_SECS,
                auto_prompt: None,
                entries: Vec::new(),
            }],
            saved_at: 0,
            sign_in_delay_secs: 60,
            layouts: Vec::new(),
        }
    }
}

/// `restore.json` -> `restore.json.bak`.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

fn bad_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bad");
    PathBuf::from(name)
}

fn parse(bytes: &[u8]) -> Option<RestoreFile> {
    let file: RestoreFile = serde_json::from_slice(bytes).ok()?;
    let usable = file.version == FILE_VERSION
        && !file.profiles.is_empty()
        && file.profiles.iter().any(|p| p.id == file.active);
    usable.then_some(file)
}

/// Loads the list. A damaged file (a write cut short) is moved aside as
/// `.bad` and the `.bak` of the last good state takes over; with neither,
/// the list starts empty.
pub fn load(path: &Path) -> RestoreFile {
    match std::fs::read(path) {
        Ok(bytes) => {
            if let Some(file) = parse(&bytes) {
                return file;
            }
            tracing::warn!(path = %path.display(), "the restore list is damaged; using the backup");
            let _ = std::fs::rename(path, bad_path(path));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(%err, path = %path.display(), "cannot read the restore list"),
    }
    std::fs::read(backup_path(path))
        .ok()
        .as_deref()
        .and_then(parse)
        .unwrap_or_default()
}

/// Writes `file`: new content to a temporary file, the current file to
/// `.bak`, the temporary file into place. At every step one of the files
/// holds a complete list (see [`load`]).
pub fn save(path: &Path, file: &RestoreFile) -> std::io::Result<()> {
    let data = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{RESTORE_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        drop(f);
        if path.exists() {
            std::fs::rename(path, backup_path(path))?;
        }
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

    fn named(name: &str) -> RestoreFile {
        let mut file = RestoreFile::default();
        file.profiles[0].name = name.into();
        file
    }

    #[test]
    fn a_file_from_before_screen_layouts_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESTORE_FILE);
        let mut old = serde_json::to_value(RestoreFile::default()).unwrap();
        assert!(old.get("layouts").is_none(), "{old}");
        old["sign_in_delay_secs"] = 30.into();
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let file = load(&path);
        assert_eq!(file.sign_in_delay_secs, 30);
        assert!(file.layouts.is_empty());
    }

    #[test]
    fn a_missing_file_gives_the_default_profile() {
        let dir = tempfile::tempdir().unwrap();
        let file = load(&dir.path().join(RESTORE_FILE));
        assert_eq!(file.active, DEFAULT_PROFILE);
        assert_eq!(file.profiles[0].delay_secs, 6);
        assert_eq!(file.profiles[0].auto_prompt, None);
    }

    #[test]
    fn save_keeps_the_previous_file_as_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(RESTORE_FILE);
        save(&path, &named("one")).unwrap();
        assert!(!backup_path(&path).exists());
        save(&path, &named("two")).unwrap();
        assert_eq!(load(&path).profiles[0].name, "two");
        let bak = std::fs::read(backup_path(&path)).unwrap();
        assert_eq!(parse(&bak).unwrap().profiles[0].name, "one");
        // No temporary files stay behind.
        let leftovers = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn a_truncated_file_recovers_from_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESTORE_FILE);
        save(&path, &named("good")).unwrap();
        save(&path, &named("newer")).unwrap();
        // A power cut in the middle of a write.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert_eq!(load(&path).profiles[0].name, "good");
        assert!(!path.exists());
        assert!(bad_path(&path).exists());
        // The next save rebuilds the file.
        save(&path, &named("after")).unwrap();
        assert_eq!(load(&path).profiles[0].name, "after");
    }

    #[test]
    fn a_cut_between_the_two_renames_recovers_from_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESTORE_FILE);
        save(&path, &named("good")).unwrap();
        std::fs::rename(&path, backup_path(&path)).unwrap();
        assert_eq!(load(&path).profiles[0].name, "good");
    }

    #[test]
    fn nothing_usable_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESTORE_FILE);
        std::fs::write(&path, b"{").unwrap();
        std::fs::write(backup_path(&path), b"not json").unwrap();
        assert!(load(&path).profiles[0].entries.is_empty());
    }

    #[test]
    fn a_file_without_its_active_profile_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESTORE_FILE);
        let mut file = named("x");
        file.active = "gone".into();
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(load(&path).active, DEFAULT_PROFILE);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESTORE_FILE);
        save(&path, &named("x")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
