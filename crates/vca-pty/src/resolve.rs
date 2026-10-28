//! Program lookup on Windows.
//!
//! `cmd.exe` finds `name` by trying each `PATHEXT` extension in every `PATH`
//! directory. portable-pty instead tries the bare name first, so npm-style
//! installs that put an extensionless shell script (`codex`) next to the
//! real launcher (`codex.cmd`) fail to start. Batch files also cannot be
//! started directly by `CreateProcessW`; they run through `cmd.exe`, which
//! re-parses the command line, so arguments with cmd metacharacters are
//! refused rather than escaped (see "BatBadBut").

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::error::PtyError;

const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The program and arguments to hand to the pseudo-terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub program: OsString,
    pub args: Vec<OsString>,
}

/// Finds `program` like `cmd.exe` does, except that the working directory
/// is not searched (a project folder must not be able to shadow the agent):
/// names with a directory part or an extension are used as given; bare
/// names are searched in `path`, trying each extension from `pathext`.
pub(crate) fn find_program(
    program: &OsStr,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    let p = Path::new(program);
    let text = program.to_string_lossy();
    if text.contains(['/', '\\']) || p.extension().is_some() {
        return None;
    }
    let pathext = pathext
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_PATHEXT.to_owned());
    let exts: Vec<&str> = pathext.split(';').filter(|e| !e.is_empty()).collect();
    for dir in path.map(std::env::split_paths).into_iter().flatten() {
        for ext in &exts {
            let candidate = dir.join(format!("{text}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_batch(program: &Path) -> bool {
    program
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| e == "bat" || e == "cmd")
}

/// Characters that `cmd.exe` interprets even inside quotes, or that would
/// let an argument escape its quotes.
fn unsafe_for_cmd(arg: &str) -> bool {
    arg.contains(['%', '^', '&', '|', '<', '>', '!', '"', '\n', '\r'])
}

/// Resolves the program and wraps batch files in `cmd.exe /d /s /c`.
pub(crate) fn resolve(
    program: &OsStr,
    args: &[OsString],
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
    comspec: Option<&OsStr>,
) -> Result<Resolved, PtyError> {
    let found = find_program(program, path, pathext);
    let program_path = found.clone().unwrap_or_else(|| PathBuf::from(program));
    if !is_batch(&program_path) {
        return Ok(Resolved {
            program: found.map_or_else(|| program.to_owned(), PathBuf::into_os_string),
            args: args.to_vec(),
        });
    }
    for arg in std::iter::once(program_path.as_os_str()).chain(args.iter().map(OsString::as_os_str))
    {
        let text = arg.to_string_lossy();
        if unsafe_for_cmd(&text) {
            return Err(PtyError::Spawn(format!(
                "argument {text:?} cannot be passed safely to the batch file {}",
                program_path.display()
            )));
        }
    }
    let shell = comspec
        .filter(|c| !c.is_empty())
        .map_or_else(|| OsString::from("cmd.exe"), OsStr::to_owned);
    // The PTY layer quotes each argument that contains spaces with plain
    // double quotes (none of them contains a quote, see above). Starting the
    // command with `call` keeps cmd from stripping the first and last quote
    // of the line when the batch path itself is quoted.
    let mut out: Vec<OsString> = vec!["/d".into(), "/c".into(), "call".into()];
    out.push(program_path.into_os_string());
    out.extend(args.iter().cloned());
    Ok(Resolved {
        program: shell,
        args: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// File systems on Windows and macOS ignore case.
    fn same(a: &Path, b: &Path) -> bool {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn prefers_pathext_over_extensionless_scripts() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "codex");
        touch(dir.path(), "codex.cmd");
        let path = dir.path().as_os_str();
        let found = find_program(OsStr::new("codex"), Some(path), None).unwrap();
        assert!(same(&found, &dir.path().join("codex.cmd")), "{found:?}");
        assert!(find_program(OsStr::new("missing"), Some(path), None).is_none());
        // Names with an extension or a directory are left alone.
        assert!(find_program(OsStr::new("codex.cmd"), Some(path), None).is_none());
    }

    #[test]
    fn working_directory_is_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "claude.cmd");
        let empty = tempfile::tempdir().unwrap();
        // Even with the planted file in the current directory, only PATH counts.
        let found = find_program(OsStr::new("claude"), Some(empty.path().as_os_str()), None);
        assert!(found.is_none());
    }

    #[test]
    fn exe_wins_over_cmd_in_pathext_order() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "tool.cmd");
        touch(dir.path(), "tool.exe");
        let found = find_program(OsStr::new("tool"), Some(dir.path().as_os_str()), None).unwrap();
        assert!(same(&found, &dir.path().join("tool.exe")), "{found:?}");
    }

    #[test]
    fn batch_files_run_through_cmd() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "codex.cmd");
        let r = resolve(
            OsStr::new("codex"),
            &["--model".into(), "a b".into()],
            Some(dir.path().as_os_str()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.program, "cmd.exe");
        assert_eq!(&r.args[..3], &["/d", "/c", "call"]);
        assert!(same(Path::new(&r.args[3]), &dir.path().join("codex.cmd")));
        assert_eq!(&r.args[4..], &["--model", "a b"]);
    }

    #[test]
    fn batch_arguments_with_metacharacters_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "x.bat");
        for bad in ["a&calc", "%PATH%", "\"q\"", "a|b", "x^y", "!v!"] {
            let r = resolve(
                OsStr::new("x"),
                &[bad.into()],
                Some(dir.path().as_os_str()),
                None,
                None,
            );
            assert!(r.is_err(), "{bad}");
        }
    }

    #[test]
    fn executables_keep_their_arguments() {
        let r = resolve(OsStr::new("prog.exe"), &["a&b".into()], None, None, None).unwrap();
        assert_eq!(r.program, "prog.exe");
        assert_eq!(r.args, vec![OsString::from("a&b")]);
    }
}
