//! Workspace allow-list (SEC-05): launch targets must be below an approved root.

use std::path::{Component, Path, PathBuf};

use vca_core::Error;
use vca_core::error::ErrorCode;

/// Approved root folders for new sessions and project creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoots {
    roots: Vec<PathBuf>,
}

impl WorkspaceRoots {
    /// Roots are canonicalized when they exist; missing roots are kept
    /// lexically normalized (and can never match an existing path through a
    /// symlink).
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let roots = roots
            .into_iter()
            .filter(|r| r.is_absolute())
            .map(|r| std::fs::canonicalize(&r).unwrap_or_else(|_| normalize(&r)))
            .collect();
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Checks `dir` against the allow-list and, with `create`, creates it.
    /// Returns the canonical directory.
    ///
    /// The check resolves symlinks of the longest existing ancestor before
    /// anything is created, so `..` components and symlinks cannot escape a
    /// root. Errors: `InvalidRequest` (relative path), `Forbidden` (outside
    /// every root), `NotFound` (missing and `create` is false).
    pub fn prepare(&self, dir: &Path, create: bool) -> Result<PathBuf, Error> {
        if !dir.is_absolute() {
            return Err(Error::api(
                ErrorCode::InvalidRequest,
                format!("project directory must be absolute: {}", dir.display()),
            ));
        }
        let resolved = resolve_existing_prefix(&normalize(dir));
        if !self.allows(&resolved) {
            return Err(Error::api(
                ErrorCode::Forbidden,
                format!("{} is outside the allowed workspace roots", dir.display()),
            ));
        }
        if !resolved.exists() {
            if !create {
                return Err(Error::api(
                    ErrorCode::NotFound,
                    format!("project directory does not exist: {}", dir.display()),
                ));
            }
            std::fs::create_dir_all(&resolved)?;
        }
        if !resolved.is_dir() {
            return Err(Error::api(
                ErrorCode::InvalidRequest,
                format!("not a directory: {}", dir.display()),
            ));
        }
        let canonical = std::fs::canonicalize(&resolved)?;
        if !self.allows(&canonical) {
            return Err(Error::api(
                ErrorCode::Forbidden,
                format!("{} is outside the allowed workspace roots", dir.display()),
            ));
        }
        Ok(canonical)
    }

    fn allows(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }
}

/// Removes `.` and resolves `..` lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Canonicalizes the longest existing ancestor of a normalized absolute
/// path and appends the remaining components.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_owned();
    let mut rest = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&existing) {
            let mut out = canon;
            for c in rest.iter().rev() {
                out.push(c);
            }
            return out;
        }
        match (
            existing.file_name().map(|n| n.to_owned()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_owned();
            }
            _ => return path.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_inside_and_creates() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = WorkspaceRoots::new([tmp.path().to_owned()]);
        let target = tmp.path().join("a/b");
        assert_eq!(
            roots.prepare(&target, false).unwrap_err().code(),
            ErrorCode::NotFound
        );
        let made = roots.prepare(&target, true).unwrap();
        assert!(made.is_dir());
        assert!(made.ends_with("a/b"));
    }

    #[test]
    fn rejects_outside_and_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let roots = WorkspaceRoots::new([root.clone()]);
        let outside = tmp.path().join("outside");
        assert_eq!(
            roots.prepare(&outside, true).unwrap_err().code(),
            ErrorCode::Forbidden
        );
        assert!(!outside.exists(), "nothing is created outside the roots");
        let sneaky = root.join("../outside2");
        assert_eq!(
            roots.prepare(&sneaky, true).unwrap_err().code(),
            ErrorCode::Forbidden
        );
        assert_eq!(
            roots
                .prepare(Path::new("rel/dir"), true)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let roots = WorkspaceRoots::new([root.clone()]);
        assert_eq!(
            roots
                .prepare(&root.join("link/new"), true)
                .unwrap_err()
                .code(),
            ErrorCode::Forbidden
        );
        assert!(!outside.join("new").exists());
    }
}
