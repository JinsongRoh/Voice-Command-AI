//! `vca_save_text_file`: saves text (backup codes, exports) through the OS
//! "Save as" dialog. Tauri webviews ignore `<a download>` blob links, so the
//! UI (`apps/ui/src/native/saveFile.ts`) asks the desktop shell instead.
//!
//! The dialog runs from Rust, so the webview has no dialog or file system
//! permission. Only the path the user picked in the dialog is written; the
//! webview only suggests a file name, never a directory.
//!
//! The name and write helpers are shared with the phones' share sheet
//! (`share_file`); the dialog command itself is desktop only.

use std::path::Path;
#[cfg(desktop)]
use std::path::PathBuf;

#[cfg(desktop)]
use serde::Serialize;
#[cfg(desktop)]
use tauri::{AppHandle, Window};
#[cfg(desktop)]
use tauri_plugin_dialog::DialogExt;

#[cfg(not(target_os = "android"))]
use crate::{CommandError, CommandResult};

/// Largest text accepted (backup codes and exports are a few KiB).
pub(crate) const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// Longest suggested file name kept, in characters.
const MAX_NAME_CHARS: usize = 120;
const FALLBACK_NAME: &str = "vca.txt";
/// File types the suggested name may have: plain text, never a type the
/// system would run when the file is opened (`.cmd`, `.ps1`, `.hta`, ...).
/// Any other extension becomes `.txt`.
const TEXT_EXTENSIONS: &[&str] = &["txt", "md", "csv", "json"];
/// Windows device names, which are not files under any extension.
const RESERVED_STEMS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// What happened, returned to the webview. Failures are a [`CommandError`].
#[cfg(desktop)]
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SaveOutcome {
    /// Written; `file_name` is the picked file's name (not its directory).
    Saved {
        #[serde(rename = "fileName")]
        file_name: String,
    },
    /// The user closed the dialog without choosing a file.
    Cancelled,
}

/// Invisible formatting characters that can make a name read differently
/// from what it is: bidirectional overrides (`U+202E` shows `txt.cmd` as
/// `dmc.txt`), zero-width characters and line separators.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// A safe default name for the dialog: no directories, no characters
/// Windows or macOS reject or that disguise the name, a plain text
/// extension ([`TEXT_EXTENSIONS`]), not a device name, not empty, not too
/// long (the extension survives shortening).
pub(crate) fn sanitize_name(suggested: &str) -> String {
    // Keep only the last path component a caller might have sent.
    let base = suggested.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|&c| {
            !c.is_control()
                && !is_invisible(c)
                && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        return FALLBACK_NAME.to_owned();
    }
    // Another file type's extension goes (`x.cmd` -> `x.txt`, not
    // `x.cmd.txt`, which Explorer shows as `x.cmd`).
    let stem = match extension(cleaned) {
        Some(_) => Path::new(cleaned)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(cleaned),
        None => cleaned,
    };
    let ext = extension(cleaned)
        .map(str::to_ascii_lowercase)
        .filter(|e| TEXT_EXTENSIONS.contains(&e.as_str()))
        .unwrap_or_else(|| "txt".to_owned());
    let stem: String = stem.chars().take(MAX_NAME_CHARS - ext.len() - 1).collect();
    let stem = stem.trim().trim_end_matches('.').trim();
    if stem.is_empty() {
        return FALLBACK_NAME.to_owned();
    }
    let device = stem.split('.').next().unwrap_or(stem).trim();
    if RESERVED_STEMS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(device))
    {
        return format!("vca-{stem}.{ext}");
    }
    format!("{stem}.{ext}")
}

/// The extension of `name` for the dialog's file type filter, if it has one.
pub(crate) fn extension(name: &str) -> Option<&str> {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
}

#[cfg(any(desktop, test))]
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Writes `text` to `path` (the path the user picked), replacing the file.
/// A new file is readable by its owner only on Unix (backup codes). Android
/// shares write their file in Kotlin.
#[cfg(not(target_os = "android"))]
pub(crate) fn write_text(path: &Path, text: &str) -> CommandResult<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| CommandError::new("save_failed", format!("could not write the file: {e}")))
}

/// Opens the OS "Save as" dialog with `suggested_name` and writes `text` to
/// the file the user picks.
#[cfg(desktop)]
#[tauri::command]
pub async fn vca_save_text_file(
    app: AppHandle,
    window: Window,
    suggested_name: String,
    text: String,
) -> CommandResult<SaveOutcome> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(CommandError::new(
            "save_too_large",
            "the text is too large to save",
        ));
    }
    let name = sanitize_name(&suggested_name);
    let mut dialog = app.dialog().file().set_file_name(&name).set_parent(&window);
    if let Some(ext) = extension(&name) {
        dialog = dialog.add_filter(ext.to_uppercase(), &[ext]);
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.save_file(move |picked| {
        let _ = tx.send(picked);
    });
    let picked = rx
        .await
        .map_err(|_| CommandError::new("save_failed", "the save dialog closed unexpectedly"))?;
    let Some(picked) = picked else {
        return Ok(SaveOutcome::Cancelled);
    };
    let path: PathBuf = picked
        .into_path()
        .map_err(|e| CommandError::new("save_failed", format!("unusable file path: {e}")))?;
    let file_name = file_name_of(&path);
    tauri::async_runtime::spawn_blocking(move || write_text(&path, &text))
        .await
        .map_err(|e| CommandError::new("save_failed", e.to_string()))??;
    Ok(SaveOutcome::Saved { file_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_directories_and_reserved_characters() {
        assert_eq!(
            sanitize_name("vca-backup-codes.txt"),
            "vca-backup-codes.txt"
        );
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd.txt");
        assert_eq!(sanitize_name("C:\\Windows\\evil.txt"), "evil.txt");
        assert_eq!(sanitize_name("a<b>c:d\"e|f?g*h\u{0}.txt"), "abcdefgh.txt");
        assert_eq!(sanitize_name(""), FALLBACK_NAME);
        assert_eq!(sanitize_name(" .. "), FALLBACK_NAME);
        assert_eq!(sanitize_name("dir/"), FALLBACK_NAME);
        assert_eq!(
            sanitize_name(&"x".repeat(500)).chars().count(),
            MAX_NAME_CHARS
        );
    }

    #[test]
    fn names_cannot_disguise_a_program() {
        // U+202E shows "vca-backup-txt.cmd" as "vca-backup-dmc.txt".
        assert_eq!(
            sanitize_name("vca-backup-\u{202E}txt.cmd"),
            "vca-backup-txt.txt"
        );
        for name in [
            "run.cmd", "run.BAT", "run.ps1", "run.exe", "run.hta", "run.reg",
        ] {
            let safe = sanitize_name(name);
            assert_eq!(extension(&safe), Some("txt"), "{name} -> {safe}");
        }
        assert_eq!(sanitize_name("a\u{200B}b\u{2066}c.md"), "abc.md");
        assert_eq!(sanitize_name("vca-tasks.CSV"), "vca-tasks.csv");
        assert_eq!(sanitize_name("vca-tasks.md"), "vca-tasks.md");
        assert_eq!(sanitize_name("README"), "README.txt");
        // Windows device names are not files.
        assert_eq!(sanitize_name("CON.txt"), "vca-CON.txt");
        assert_eq!(sanitize_name("nul"), "vca-nul.txt");
        assert_eq!(sanitize_name("console.txt"), "console.txt");
        // Shortening keeps the extension.
        let long = sanitize_name(&format!("{}.json", "y".repeat(500)));
        assert_eq!(long.chars().count(), MAX_NAME_CHARS);
        assert!(long.ends_with(".json"), "{long}");
    }

    #[cfg(unix)]
    #[test]
    fn new_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codes.txt");
        write_text(&path, "AAAA\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    #[test]
    fn extensions_feed_the_filter() {
        assert_eq!(extension("codes.txt"), Some("txt"));
        assert_eq!(extension("tasks.json"), Some("json"));
        assert_eq!(extension("README"), None);
        assert_eq!(extension("odd.t x"), None);
    }

    #[test]
    fn writes_replace_the_picked_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codes.txt");
        std::fs::write(&path, "old contents that are longer").unwrap();
        write_text(&path, "AAAA-BBBB\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "AAAA-BBBB\n");
        assert_eq!(file_name_of(&path), "codes.txt");
    }

    #[test]
    fn write_errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_text(&dir.path().join("missing").join("codes.txt"), "x").unwrap_err();
        assert_eq!(err.code, "save_failed");
    }

    #[test]
    fn outcomes_serialize_for_the_webview() {
        assert_eq!(
            serde_json::to_value(SaveOutcome::Saved {
                file_name: "a.txt".into()
            })
            .unwrap(),
            serde_json::json!({ "status": "saved", "fileName": "a.txt" })
        );
        assert_eq!(
            serde_json::to_value(SaveOutcome::Cancelled).unwrap(),
            serde_json::json!({ "status": "cancelled" })
        );
    }
}
