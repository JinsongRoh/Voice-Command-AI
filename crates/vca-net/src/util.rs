//! Small shared helpers: randomness, base64url, private file writes.

use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::error::{NetError, Result};

/// Fills `buf` from the operating system CSPRNG.
pub(crate) fn fill_random(buf: &mut [u8]) {
    // The OS RNG failing is unrecoverable for a security component.
    getrandom::fill(buf).expect("operating system random number generator failed");
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    fill_random(&mut b);
    hex(&b)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

pub(crate) fn b64e(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn b64d(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s.trim())
        .map_err(|e| NetError::Encoding(format!("invalid base64url: {e}")))
}

/// Atomically writes `data` to `path` with owner-only permissions
/// (`0600` on Unix). The parent directory is created if needed (`0700` on
/// Unix).
///
/// On Windows the file inherits the ACL of its directory, which for the
/// per-user application data directory is owner-only; this is
/// unverified on Windows.
pub(crate) fn write_private_file(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_private_dir(dir)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| NetError::InvalidArgument("path has no file name".into()))?
        .to_string_lossy()
        .into_owned();
    let tmp = dir.join(format!(".{file_name}.{}.tmp", random_hex(6)));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(NetError::from)
}

/// Creates a directory (and parents) that only the owner can access on Unix.
pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(NetError::from)
}

/// Removes ASCII/Unicode control characters and limits the length of a
/// user-supplied display string.
pub(crate) fn sanitize_display(s: &str, max_chars: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_roundtrip() {
        let data = [0u8, 255, 254, 1, 2, 3];
        let s = b64e(&data);
        assert!(!s.contains('=') && !s.contains('+') && !s.contains('/'));
        assert_eq!(b64d(&s).unwrap(), data);
        assert!(b64d("not base64!").is_err());
    }

    #[test]
    fn random_values_differ() {
        assert_ne!(random_hex(16), random_hex(16));
    }

    #[test]
    fn sanitize_strips_controls() {
        assert_eq!(sanitize_display("  a\u{1b}[31mb\n ", 64), "a[31mb");
        assert_eq!(sanitize_display("abcdef", 3), "abc");
    }

    #[test]
    fn private_file_is_written_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("secret.json");
        write_private_file(&p, b"one").unwrap();
        write_private_file(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // No temp files left behind.
        let entries: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }
}
