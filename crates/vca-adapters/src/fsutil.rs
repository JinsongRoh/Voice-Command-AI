//! Safe editing of user configuration files.

use std::io;
use std::path::{Path, PathBuf};

/// Copies `path` to `<path>.vca-backup-<millis>` when it exists and returns
/// the backup path.
pub(crate) fn backup(path: &Path) -> io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".vca-backup-{}", vca_core::now_millis()));
    let backup = path.with_file_name(name);
    std::fs::copy(path, &backup)?;
    Ok(Some(backup))
}

/// Writes `contents` to `path` through a temporary file in the same
/// directory followed by a rename, creating parent directories.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".vca-tmp-{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, contents)?;
    // Keep the original file's permissions (settings may hold secrets).
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Reads a file, returning `None` when it does not exist.
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Quotes one argument for a POSIX shell when needed.
pub(crate) fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if safe {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(shell_quote("vca"), "vca");
        assert_eq!(shell_quote("/opt/my tools/vca"), "'/opt/my tools/vca'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn backup_and_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("f.json");
        assert_eq!(backup(&path).unwrap(), None);
        write_atomic(&path, "one").unwrap();
        let b = backup(&path).unwrap().unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "one");
        assert_eq!(read_optional(&path).unwrap().as_deref(), Some("two"));
    }
}
