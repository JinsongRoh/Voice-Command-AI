//! Files and folders sent from a client into a local session.
//!
//! Files are stored below the session's project folder in
//! `.vca/attachments/<YYYYMMDD-HHMMSS>-<name>` (UTC time), and the agent is
//! told with an `@path` mention typed into managed sessions (without Enter
//! unless the client asks for it). Observed sessions cannot receive input
//! on every platform, so there the file is only saved and the client shows
//! the mention to the user.
//!
//! # Transfer
//!
//! Uploads are chunked: `AttachBegin` declares name, type and size,
//! `AttachChunk` messages carry at most [`ATTACH_CHUNK_BYTES`] each, in
//! order, and `AttachEnd` completes. The Noise transport limits one message
//! to 16 MiB, below the 25 MiB file limit; raising that limit for everyone
//! would let any paired device make the server buffer huge messages, so
//! instead the data streams to a temporary file next to its destination
//! and is renamed into place at the end. Chunking also gives per-chunk
//! progress and cancellation on slow mobile links. Small files may use the
//! single-message `Attach`. Folders are a group of uploads opened with
//! `AttachFolderBegin` and closed with `AttachFolderEnd`, which mentions the
//! folder once.
//!
//! # Batches
//!
//! Several files picked together (a multi-photo pick) carry the same
//! client-chosen batch id in `AttachBegin`. They are saved into one new
//! folder, `.vca/attachments/<YYYYMMDD-HHMMSS>-batch/`, and are not
//! mentioned one by one: `AttachBatchEnd` types ONE compact mention for all
//! of them, listing the files when that stays short
//! ([`MAX_INLINE_MENTION_CHARS`]) and otherwise naming the folder and a
//! `files.txt` manifest. Long input bursts are what lose text in a Windows
//! console agent (Claude Code kept only the last 1024-character read of a
//! 1426-character prompt of 23 mentions), so the mention stays short.
//!
//! # Path safety
//!
//! * Client names are reduced to one sanitized path component (no
//!   separators, `..`, control, reserved or shell-special characters, no
//!   Windows device names, bounded length); the server picks the final
//!   name and never overwrites an existing file.
//! * The project folder and every directory written to are resolved
//!   through [`WorkspaceRoots::prepare`] (symlinks resolved, must be inside
//!   the workspace roots), and must stay inside the canonical project
//!   folder. The destination is checked again before the final rename.
//! * Folder uploads apply the same rules to every relative path component
//!   and refuse hidden and system entries.
//! * Unfinished uploads are deleted when cancelled or when the client
//!   disconnects.
//!
//! With `git_exclude` (default on), `/.vca/` is added to the project's
//! `.git/info/exclude` (never `.gitignore`), so attachments are not
//! committed by accident. Worktrees whose `.git` is a file are left alone.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vca_core::error::ErrorCode;
use vca_core::{
    ATTACH_CHUNK_BYTES, AttachFolderEntry, ClientMsg, Error, Input, ServerMsg, SessionId,
};
use vca_net::{AuditEvent, AuditLog, redact};

use crate::sessions::{Session, SessionManager};
use crate::util::{error_text, lock};
use crate::workspace::{WorkspaceRoots, without_verbatim_prefix};

/// Chunk size the server suggests to clients.
pub const PREFERRED_CHUNK_BYTES: u32 = 512 * 1024;
/// Uploads one client may have open at the same time.
const MAX_OPEN_UPLOADS: usize = 4;
/// Folder uploads one client may have open at the same time.
const MAX_OPEN_FOLDERS: usize = 2;
/// Directory depth inside an uploaded folder.
const MAX_FOLDER_DEPTH: usize = 32;
const MAX_STEM_CHARS: usize = 80;
const MAX_STEM_BYTES: usize = 150;
const MAX_EXT_CHARS: usize = 16;
const MAX_NOTE_CHARS: usize = 500;
/// Batches one client may have open at the same time.
const MAX_OPEN_BATCHES: usize = 2;
/// Files in one batch (the client caps a message at 50 photos and 10 other
/// files).
pub const MAX_BATCH_FILES: u32 = 100;
/// Longest batch id a client may choose.
const MAX_BATCH_ID_CHARS: usize = 64;
/// A batch mention that lists every file stays at or below this many
/// characters (before the note); longer ones name the batch folder and its
/// manifest instead. Well below the 1024 characters of one console read.
pub const MAX_INLINE_MENTION_CHARS: usize = 400;
/// The manifest of a batch mentioned by its folder.
pub const BATCH_MANIFEST: &str = "files.txt";
/// How long typing the mention into a session may take (typing is paced,
/// see `vca_adapters::terminal::console`).
const TYPE_TIMEOUT: Duration = Duration::from_secs(20);
/// The attachments folder, relative to the project folder.
pub const ATTACHMENTS_DIR: &str = ".vca/attachments";

/// `[attachments]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttachmentsConfig {
    pub enabled: bool,
    /// Largest single file (default 25 MiB).
    pub max_bytes: u64,
    /// Largest folder upload in total (default 100 MiB).
    pub max_folder_bytes: u64,
    /// Most files in one folder upload (default 500).
    pub max_folder_files: u32,
    /// Add `/.vca/` to the project's `.git/info/exclude`.
    pub git_exclude: bool,
}

impl Default for AttachmentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: 25 * 1024 * 1024,
            max_folder_bytes: 100 * 1024 * 1024,
            max_folder_files: 500,
            git_exclude: true,
        }
    }
}

fn api(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

fn invalid(message: impl Into<String>) -> Error {
    api(ErrorCode::InvalidRequest, message)
}

fn mib(bytes: u64) -> String {
    let m = bytes as f64 / (1024.0 * 1024.0);
    if m >= 10.0 || m.fract() == 0.0 {
        format!("{m:.0} MiB")
    } else {
        format!("{m:.1} MiB")
    }
}

// ------------------------------------------------------------------ names

/// Combining marks kept in names (accents and kana sound marks, as found
/// in decomposed names from macOS).
fn is_combining_mark(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036F}' | '\u{3099}'..='\u{309A}')
}

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9",
];

fn truncate_stem(stem: &str) -> String {
    let mut out = String::new();
    for c in stem.chars().take(MAX_STEM_CHARS) {
        if out.len() + c.len_utf8() > MAX_STEM_BYTES {
            break;
        }
        out.push(c);
    }
    out.trim_end_matches(['-', '.']).to_owned()
}

/// Reduces a client-supplied file name to one safe path component.
///
/// Keeps letters and digits of any script plus `.`, `-` and `_`; anything
/// else (path separators, control, reserved, whitespace and shell-special
/// characters) becomes `-`. Runs of `.` collapse, so `..` never survives;
/// leading and trailing dots and dashes are dropped; Windows device names
/// get a `_` prefix; the stem is limited to 80 characters (150 bytes) and
/// the extension to 16. Never returns an empty string.
pub fn sanitize_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut cleaned = String::with_capacity(base.len());
    for c in base.chars() {
        let c = if c.is_alphanumeric() || is_combining_mark(c) || matches!(c, '.' | '-' | '_') {
            c
        } else {
            '-'
        };
        if (c == '-' || c == '.') && cleaned.ends_with(c) {
            continue;
        }
        cleaned.push(c);
    }
    let cleaned = cleaned.trim_matches(['.', '-']);
    let (stem, ext) = match cleaned.rfind('.') {
        Some(i) if i > 0 => {
            let ext = &cleaned[i + 1..];
            let ok = (1..=MAX_EXT_CHARS).contains(&ext.chars().count())
                && ext.chars().all(char::is_alphanumeric);
            if ok {
                (&cleaned[..i], Some(ext))
            } else {
                (cleaned, None)
            }
        }
        _ => (cleaned, None),
    };
    let mut stem = truncate_stem(stem);
    if stem.is_empty() {
        stem = "file".into();
    }
    let device = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if WINDOWS_RESERVED.contains(&device.as_str()) {
        stem.insert(0, '_');
    }
    match ext {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem,
    }
}

/// Folder entries that are never uploaded (clients skip them; the server
/// refuses them).
pub fn is_hidden_or_system(component: &str) -> bool {
    let lower = component.to_ascii_lowercase();
    component.starts_with('.')
        || matches!(
            lower.as_str(),
            "node_modules" | "thumbs.db" | "desktop.ini" | "$recycle.bin" | "__macosx"
        )
}

/// Splits a folder entry path (`/` or `\` separated) into sanitized
/// components. Refuses empty paths, `.`/`..`, hidden and system entries
/// and excessive depth.
pub fn sanitize_relative_path(raw: &str) -> Result<Vec<String>, Error> {
    let parts: Vec<&str> = raw.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(invalid("empty path in the folder"));
    }
    if parts.len() > MAX_FOLDER_DEPTH {
        return Err(invalid("the folder is nested too deeply"));
    }
    parts
        .iter()
        .map(|p| {
            if *p == "." || *p == ".." {
                Err(invalid("relative path components are not allowed"))
            } else if is_hidden_or_system(p) {
                Err(invalid(format!("hidden or system entry skipped: {p}")))
            } else {
                Ok(sanitize_name(p))
            }
        })
        .collect()
}

fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Removes control characters (a newline would submit the line) and
/// limits the length of a note typed after the mention.
fn clean_note(note: Option<&str>) -> Option<String> {
    let note = note?;
    let cleaned: String = note
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned: String = cleaned.chars().take(MAX_NOTE_CHARS).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

// ------------------------------------------------------------------ types

const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe",
    "dll",
    "com",
    "scr",
    "msi",
    "msp",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "psd1",
    "vbs",
    "vbe",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "cpl",
    "lnk",
    "pif",
    "reg",
    "scf",
    "sys",
    "drv",
    "ocx",
    "gadget",
    "application",
    "appref-ms",
    "so",
    "dylib",
    "app",
    "dmg",
    "pkg",
    "deb",
    "rpm",
    "apk",
    "aab",
    "xapk",
    "ipa",
    "jar",
    "class",
    "elf",
    "out",
];

const EXECUTABLE_MIMES: &[&str] = &[
    "application/x-msdownload",
    "application/x-msdos-program",
    "application/x-dosexec",
    "application/x-executable",
    "application/x-sharedlib",
    "application/x-mach-binary",
    "application/x-elf",
    "application/vnd.microsoft.portable-executable",
    "application/x-msi",
    "application/x-ms-installer",
    "application/x-ms-shortcut",
    "application/java-archive",
    "application/x-java-archive",
    "application/vnd.android.package-archive",
    "application/x-apple-diskimage",
    "application/x-bat",
    "application/x-sh-executable",
];

const ALLOWED_MIMES: &[&str] = &[
    "application/pdf",
    "application/json",
    "application/ld+json",
    "application/xml",
    "application/yaml",
    "application/x-yaml",
    "application/toml",
    "application/javascript",
    "application/x-javascript",
    "application/typescript",
    "application/x-typescript",
    "application/sql",
    "application/graphql",
    "application/x-ndjson",
    "application/rtf",
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.apple.pages",
    "application/vnd.apple.numbers",
    "application/vnd.apple.keynote",
    "application/zip",
    "application/x-zip-compressed",
];

const ALLOWED_MIME_PREFIXES: &[&str] = &[
    "image/",
    "text/",
    "application/vnd.openxmlformats-officedocument.",
    "application/vnd.oasis.opendocument.",
];

/// Extensions accepted when the client reports no specific type (common
/// for code files, and on Android).
const ALLOWED_EXTENSIONS: &[&str] = &[
    // images
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "heic",
    "heif",
    "bmp",
    "tif",
    "tiff",
    "svg",
    "avif",
    // documents
    "pdf",
    "rtf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "odt",
    "ods",
    "odp",
    "pages",
    "numbers",
    "key",
    "zip", // text and data
    "txt",
    "md",
    "markdown",
    "rst",
    "adoc",
    "json",
    "jsonl",
    "ndjson",
    "csv",
    "tsv",
    "xml",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "log",
    "env",
    "lock",
    "diff",
    "patch",
    "proto",
    "graphql",
    "gql",
    "sql",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less", // code
    "rs",
    "py",
    "pyi",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "jsx",
    "java",
    "kt",
    "kts",
    "swift",
    "go",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hh",
    "m",
    "mm",
    "cs",
    "fs",
    "vb",
    "rb",
    "php",
    "pl",
    "sh",
    "bash",
    "zsh",
    "fish",
    "lua",
    "r",
    "dart",
    "scala",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "vue",
    "svelte",
    "gradle",
    "properties",
    "tf",
    "nix",
    "zig",
    "sol",
    "ipynb",
];

/// Extensionless names accepted with a generic type.
const ALLOWED_NAMES: &[&str] = &[
    "dockerfile",
    "makefile",
    "license",
    "readme",
    "changelog",
    "procfile",
    "gemfile",
    "rakefile",
    "justfile",
];

fn is_generic_mime(mime: &str) -> bool {
    mime.is_empty() || mime == "application/octet-stream" || mime == "binary/octet-stream"
}

/// Normalized MIME type: lowercase, parameters dropped.
fn normalize_mime(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Refuses executables by type and extension (all uploads).
fn check_not_executable(mime: &str, name: &str) -> Result<(), Error> {
    let ext = extension(name);
    if EXECUTABLE_MIMES.contains(&mime)
        || ext
            .as_deref()
            .is_some_and(|e| EXECUTABLE_EXTENSIONS.contains(&e))
    {
        return Err(api(
            ErrorCode::Forbidden,
            format!("executable files cannot be attached: {name}"),
        ));
    }
    Ok(())
}

/// The allow-list for single files: images, PDF, text, data and code,
/// office documents and zip archives.
pub fn check_type(mime: &str, name: &str) -> Result<(), Error> {
    let mime = normalize_mime(mime);
    check_not_executable(&mime, name)?;
    let allowed = if is_generic_mime(&mime) {
        match extension(name) {
            Some(ext) => ALLOWED_EXTENSIONS.contains(&ext.as_str()),
            None => ALLOWED_NAMES.contains(&name.to_ascii_lowercase().as_str()),
        }
    } else {
        ALLOWED_MIMES.contains(&mime.as_str())
            || ALLOWED_MIME_PREFIXES.iter().any(|p| mime.starts_with(p))
    };
    if !allowed {
        let what = if mime.is_empty() { "unknown" } else { &mime };
        return Err(api(
            ErrorCode::Unsupported,
            format!(
                "this file type ({what}) cannot be attached; send images, PDF, text, code, \
                 office documents or zip archives"
            ),
        ));
    }
    Ok(())
}

/// True when the first bytes are a native executable (PE, ELF, Mach-O or a
/// universal binary / Java class).
pub fn looks_executable(head: &[u8]) -> bool {
    head.starts_with(b"MZ")
        || head.starts_with(b"\x7fELF")
        || [
            [0xFE, 0xED, 0xFA, 0xCE],
            [0xFE, 0xED, 0xFA, 0xCF],
            [0xCE, 0xFA, 0xED, 0xFE],
            [0xCF, 0xFA, 0xED, 0xFE],
            [0xCA, 0xFE, 0xBA, 0xBE],
        ]
        .iter()
        .any(|m| head.starts_with(m))
}

// ------------------------------------------------------------------ paths

/// UTC `YYYYMMDD-HHMMSS` for a Unix time in seconds.
pub fn timestamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs = unix_secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

fn now_stamp() -> String {
    timestamp(vca_core::now_millis() / 1000)
}

fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(without_verbatim_prefix)
}

/// `path` relative to `base`, with `/` separators.
fn relative(base: &Path, path: &Path) -> Result<String, Error> {
    let rel = path
        .strip_prefix(base)
        .map_err(|_| api(ErrorCode::Forbidden, "the file left the project folder"))?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => Ok(s.to_string_lossy().into_owned()),
            _ => Err(api(ErrorCode::Forbidden, "unexpected path component")),
        })
        .collect::<Result<_, _>>()?;
    Ok(parts.join("/"))
}

/// Candidate names `stem.ext`, `stem-2.ext`, `stem-3.ext`, ...
fn candidate(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{n}.{ext}"),
        _ => format!("{name}-{n}"),
    }
}

/// The canonical project folder of a session, checked against the
/// workspace roots.
pub fn project_root(roots: &WorkspaceRoots, session: &Session) -> Result<PathBuf, Error> {
    let Some(dir) = session.project_dir() else {
        return Err(api(
            ErrorCode::Unsupported,
            "this session has no project folder on this PC (cloud session); \
             files can only be attached to local sessions",
        ));
    };
    roots.prepare(&dir, false).map_err(|e| match e.code() {
        ErrorCode::Forbidden => api(
            ErrorCode::Forbidden,
            format!(
                "the project folder {} is outside the allowed workspace roots",
                dir.display()
            ),
        ),
        _ => e,
    })
}

/// Creates (if needed) and checks a directory that must stay inside
/// `project`. Returns its canonical path.
fn prepare_inside(roots: &WorkspaceRoots, project: &Path, dir: &Path) -> Result<PathBuf, Error> {
    // Checked before `prepare` creates anything: a symlinked `.vca` must not
    // leave a directory at its target, even when the request is refused.
    if !roots.resolved(dir).starts_with(project) {
        return Err(api(
            ErrorCode::Forbidden,
            "the attachments folder resolves outside the project folder",
        ));
    }
    let dir = roots.prepare(dir, true)?;
    if !dir.starts_with(project) {
        return Err(api(
            ErrorCode::Forbidden,
            "the attachments folder resolves outside the project folder",
        ));
    }
    Ok(dir)
}

/// `<project>/.vca/attachments`, created and checked.
pub fn attachments_dir(roots: &WorkspaceRoots, project: &Path) -> Result<PathBuf, Error> {
    prepare_inside(roots, project, &project.join(ATTACHMENTS_DIR))
}

/// Adds `/.vca/` to `.git/info/exclude` when the project is a plain git
/// checkout. Failures are logged, never fatal.
pub fn ensure_git_exclude(project: &Path) {
    let git = project.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&git) else {
        return;
    };
    if !meta.is_dir() {
        return; // A worktree or submodule (`.git` file), or a symlink.
    }
    let info = git.join("info");
    if let Err(e) = std::fs::create_dir_all(&info) {
        tracing::debug!("cannot create {}: {e}", info.display());
        return;
    }
    match canonical(&info) {
        Ok(c) if c.starts_with(project) => {}
        _ => return,
    }
    let exclude = info.join("exclude");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing
        .lines()
        .any(|l| matches!(l.trim(), "/.vca/" | ".vca/" | "/.vca" | ".vca"))
    {
        return;
    }
    let mut text = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("# Voice Command AI attachments\n/.vca/\n");
    let result = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if let Err(e) = result {
        tracing::debug!("cannot update {}: {e}", exclude.display());
    }
}

// ------------------------------------------------------------------ state

struct Upload {
    session_id: SessionId,
    project: PathBuf,
    /// Canonical destination directory.
    dir: PathBuf,
    part: PathBuf,
    file: Option<File>,
    /// Final (sanitized) file name.
    name: String,
    mime: String,
    size: u64,
    received: u64,
    head: Vec<u8>,
    note: Option<String>,
    submit: bool,
    folder: Option<String>,
    batch: Option<String>,
    done: bool,
}

impl Drop for Upload {
    fn drop(&mut self) {
        self.file.take();
        if !self.done {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

struct Folder {
    session_id: SessionId,
    project: PathBuf,
    dir: PathBuf,
    name: String,
    files_started: u32,
    files_done: u32,
    bytes_reserved: u64,
    bytes_done: u64,
    note: Option<String>,
    submit: bool,
    finished: bool,
}

impl Drop for Folder {
    fn drop(&mut self) {
        if !self.finished {
            // The directory was created new for this upload and only holds
            // its files; `remove_dir_all` does not follow symlinks.
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Files sent together, mentioned once at `AttachBatchEnd`.
struct Batch {
    session_id: SessionId,
    project: PathBuf,
    /// Canonical batch folder (new for this batch).
    dir: PathBuf,
    files_started: u32,
    /// Project-relative paths of the finished files, in order.
    files: Vec<String>,
    bytes: u64,
}

impl Drop for Batch {
    fn drop(&mut self) {
        // Finished files stay (each upload succeeded on its own); a folder
        // that never got a file is removed (`remove_dir` only removes empty
        // directories).
        if self.files.is_empty() {
            let _ = std::fs::remove_dir(&self.dir);
        }
    }
}

/// Open uploads of one client. Dropping it (the client disconnected)
/// deletes everything unfinished.
#[derive(Default)]
pub struct Uploads {
    files: Mutex<HashMap<String, Upload>>,
    folders: Mutex<HashMap<String, Folder>>,
    batches: Mutex<HashMap<String, Batch>>,
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Saves attachments and tells the agents. One per server.
pub struct Attachments {
    cfg: AttachmentsConfig,
    audit: OnceLock<Arc<AuditLog>>,
}

/// What `AttachBegin` asks for.
struct BeginRequest<'a> {
    session_id: SessionId,
    name: &'a str,
    mime: &'a str,
    size: u64,
    note: Option<String>,
    submit: bool,
    folder: Option<AttachFolderEntry>,
    batch: Option<String>,
}

impl Attachments {
    pub fn new(cfg: AttachmentsConfig) -> Self {
        Self {
            cfg,
            audit: OnceLock::new(),
        }
    }

    pub fn config(&self) -> &AttachmentsConfig {
        &self.cfg
    }

    /// Records attachments from paired devices in the remote audit log.
    pub fn set_audit(&self, audit: Arc<AuditLog>) {
        let _ = self.audit.set(audit);
    }

    /// Handles one attachment message. `device` is the paired device, or
    /// `None` for the local owner.
    pub async fn handle(
        &self,
        mgr: &SessionManager,
        uploads: &Uploads,
        device: Option<&str>,
        msg: ClientMsg,
    ) -> ServerMsg {
        let result = match msg {
            ClientMsg::Attach {
                session_id,
                name,
                mime,
                data,
                note,
                submit,
            } => {
                self.attach_whole(
                    mgr, uploads, device, session_id, &name, &mime, data, note, submit,
                )
                .await
            }
            ClientMsg::AttachBegin {
                session_id,
                name,
                mime,
                size,
                note,
                submit,
                folder,
                batch,
            } => self
                .begin(
                    mgr,
                    uploads,
                    BeginRequest {
                        session_id,
                        name: &name,
                        mime: &mime,
                        size,
                        note,
                        submit,
                        folder,
                        batch,
                    },
                )
                .map(|upload_id| ServerMsg::AttachReady {
                    upload_id,
                    chunk_size: PREFERRED_CHUNK_BYTES,
                }),
            ClientMsg::AttachChunk {
                upload_id,
                offset,
                data,
            } => self
                .chunk(uploads, &upload_id, offset, &data)
                .map(|()| ServerMsg::Ok),
            ClientMsg::AttachEnd { upload_id } => self.end(mgr, uploads, device, &upload_id).await,
            ClientMsg::AttachCancel { upload_id } => {
                cancel(uploads, &upload_id).map(|()| ServerMsg::Ok)
            }
            ClientMsg::AttachFolderBegin {
                session_id,
                name,
                files,
                total_bytes,
                note,
                submit,
            } => self
                .folder_begin(
                    mgr,
                    uploads,
                    session_id,
                    &name,
                    files,
                    total_bytes,
                    note,
                    submit,
                )
                .map(|folder_id| ServerMsg::AttachFolderReady {
                    folder_id,
                    max_files: self.cfg.max_folder_files,
                    max_total_bytes: self.cfg.max_folder_bytes,
                }),
            ClientMsg::AttachFolderEnd { folder_id } => {
                self.folder_end(mgr, uploads, device, &folder_id).await
            }
            ClientMsg::AttachBatchEnd {
                batch_id,
                note,
                submit,
            } => {
                self.batch_end(mgr, uploads, &batch_id, note.as_deref(), submit)
                    .await
            }
            _ => Err(invalid("not an attachment message")),
        };
        result.unwrap_or_else(|err| ServerMsg::Error {
            code: err.code(),
            message: error_text(&err),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn attach_whole(
        &self,
        mgr: &SessionManager,
        uploads: &Uploads,
        device: Option<&str>,
        session_id: SessionId,
        name: &str,
        mime: &str,
        data: Vec<u8>,
        note: Option<String>,
        submit: bool,
    ) -> Result<ServerMsg, Error> {
        if data.len() > ATTACH_CHUNK_BYTES {
            return Err(invalid(format!(
                "single-message attachments are limited to {}; use a chunked upload",
                mib(ATTACH_CHUNK_BYTES as u64)
            )));
        }
        let upload_id = self.begin(
            mgr,
            uploads,
            BeginRequest {
                session_id,
                name,
                mime,
                size: data.len() as u64,
                note,
                submit,
                folder: None,
                batch: None,
            },
        )?;
        if let Err(e) = self.chunk(uploads, &upload_id, 0, &data) {
            let _ = cancel(uploads, &upload_id);
            return Err(e);
        }
        self.end(mgr, uploads, device, &upload_id).await
    }

    fn check_size(&self, size: u64) -> Result<(), Error> {
        if size > self.cfg.max_bytes {
            return Err(invalid(format!(
                "the file is too large ({}; the limit is {})",
                mib(size),
                mib(self.cfg.max_bytes)
            )));
        }
        Ok(())
    }

    fn enabled(&self) -> Result<(), Error> {
        if self.cfg.enabled {
            Ok(())
        } else {
            Err(api(
                ErrorCode::Unsupported,
                "attachments are off on this server ([attachments] enabled = false)",
            ))
        }
    }

    fn session(mgr: &SessionManager, id: &SessionId) -> Result<Arc<Session>, Error> {
        mgr.get(id)
            .ok_or_else(|| api(ErrorCode::NotFound, format!("no session {id}")))
    }

    fn begin(
        &self,
        mgr: &SessionManager,
        uploads: &Uploads,
        req: BeginRequest<'_>,
    ) -> Result<String, Error> {
        self.enabled()?;
        let roots = &mgr.inner.cfg.workspace_roots;
        let session = Self::session(mgr, &req.session_id)?;
        self.check_size(req.size)?;
        let mime = normalize_mime(req.mime);
        if lock(&uploads.files).len() >= MAX_OPEN_UPLOADS {
            return Err(api(
                ErrorCode::Unavailable,
                "too many uploads at once; wait for the others to finish",
            ));
        }
        let mut batch_id = None;
        let (project, dir, name, folder_id) = match &req.folder {
            Some(entry) => {
                let parts = sanitize_relative_path(&entry.path)?;
                let (file_name, subdirs) = parts.split_last().expect("not empty");
                check_not_executable(&mime, file_name)?;
                let mut folders = lock(&uploads.folders);
                let folder = folders
                    .get_mut(&entry.folder_id)
                    .ok_or_else(|| api(ErrorCode::NotFound, "no such folder upload"))?;
                if folder.session_id != req.session_id {
                    return Err(invalid("the folder upload belongs to another session"));
                }
                if folder.files_started >= self.cfg.max_folder_files {
                    return Err(invalid(format!(
                        "the folder has too many files (the limit is {})",
                        self.cfg.max_folder_files
                    )));
                }
                if folder.bytes_reserved.saturating_add(req.size) > self.cfg.max_folder_bytes {
                    return Err(invalid(format!(
                        "the folder is too large (the limit is {})",
                        mib(self.cfg.max_folder_bytes)
                    )));
                }
                let mut target = folder.dir.clone();
                for s in subdirs {
                    target.push(s);
                }
                let dir = prepare_inside(roots, &folder.dir, &target)?;
                folder.files_started += 1;
                folder.bytes_reserved += req.size;
                (
                    folder.project.clone(),
                    dir,
                    file_name.clone(),
                    Some(entry.folder_id.clone()),
                )
            }
            None => {
                let file_name = sanitize_name(req.name);
                check_type(&mime, &file_name)?;
                let project = project_root(roots, &session)?;
                let dir = attachments_dir(roots, &project)?;
                match &req.batch {
                    Some(id) => {
                        let batch_dir =
                            Self::join_batch(uploads, id, &req.session_id, &project, &dir)?;
                        batch_id = Some(id.clone());
                        // The folder carries the time; the files keep their names.
                        (project, batch_dir, file_name, None)
                    }
                    None => (project, dir, format!("{}-{file_name}", now_stamp()), None),
                }
            }
        };
        let upload_id = new_id("up");
        let part = dir.join(format!(".{upload_id}.part"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)?;
        let upload = Upload {
            session_id: req.session_id,
            project,
            dir,
            part,
            file: Some(file),
            name,
            mime,
            size: req.size,
            received: 0,
            head: Vec::new(),
            note: clean_note(req.note.as_deref()),
            submit: req.submit,
            folder: folder_id,
            batch: batch_id,
            done: false,
        };
        lock(&uploads.files).insert(upload_id.clone(), upload);
        Ok(upload_id)
    }

    fn chunk(
        &self,
        uploads: &Uploads,
        upload_id: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error> {
        let mut files = lock(&uploads.files);
        let upload = files
            .get_mut(upload_id)
            .ok_or_else(|| api(ErrorCode::NotFound, "no such upload"))?;
        let result = apply_chunk(upload, offset, data);
        if result.is_err() {
            // A broken upload is not resumable; drop it (deletes the part).
            files.remove(upload_id);
        }
        result
    }

    async fn end(
        &self,
        mgr: &SessionManager,
        uploads: &Uploads,
        device: Option<&str>,
        upload_id: &str,
    ) -> Result<ServerMsg, Error> {
        let mut upload = lock(&uploads.files)
            .remove(upload_id)
            .ok_or_else(|| api(ErrorCode::NotFound, "no such upload"))?;
        if upload.received != upload.size {
            return Err(invalid(format!(
                "upload incomplete ({} of {} bytes)",
                upload.received, upload.size
            )));
        }
        if let Some(mut f) = upload.file.take() {
            f.flush()?;
        }
        let roots = &mgr.inner.cfg.workspace_roots;
        let path = place(roots, &upload)?;
        upload.done = true;
        let rel = relative(&upload.project, &path)?;
        let size = upload.size;

        if let Some(folder_id) = &upload.folder {
            if let Some(folder) = lock(&uploads.folders).get_mut(folder_id) {
                folder.files_done += 1;
                folder.bytes_done += size;
            }
            return Ok(ServerMsg::Attached {
                session_id: upload.session_id.clone(),
                path: rel,
                size,
                mention: String::new(),
                typed: false,
            });
        }

        if self.cfg.git_exclude {
            ensure_git_exclude(&upload.project);
        }
        self.audit(
            device,
            &upload.session_id,
            "file",
            &upload.name,
            &upload.mime,
            size,
            1,
        );
        if let Some(batch_id) = &upload.batch {
            if let Some(batch) = lock(&uploads.batches).get_mut(batch_id) {
                batch.files.push(rel.clone());
                batch.bytes += size;
                // Mentioned with the others at `AttachBatchEnd`.
                return Ok(ServerMsg::Attached {
                    session_id: upload.session_id.clone(),
                    path: rel,
                    size,
                    mention: String::new(),
                    typed: false,
                });
            }
            // The batch was completed already: mention this file on its own
            // rather than not at all.
            tracing::warn!(batch = %batch_id, "a file finished after its batch; mentioned alone");
        }
        let mention = mention("Attached file", &rel, upload.note.as_deref(), upload.submit);
        let typed = type_mention(mgr, &upload.session_id, &mention, upload.submit).await;
        Ok(ServerMsg::Attached {
            session_id: upload.session_id.clone(),
            path: rel,
            size,
            mention: mention.trim_end().to_owned(),
            typed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn folder_begin(
        &self,
        mgr: &SessionManager,
        uploads: &Uploads,
        session_id: SessionId,
        name: &str,
        files: u32,
        total_bytes: u64,
        note: Option<String>,
        submit: bool,
    ) -> Result<String, Error> {
        self.enabled()?;
        let roots = &mgr.inner.cfg.workspace_roots;
        let session = Self::session(mgr, &session_id)?;
        if files == 0 {
            return Err(invalid("the folder has no files to send"));
        }
        if files > self.cfg.max_folder_files {
            return Err(invalid(format!(
                "the folder has too many files ({files}; the limit is {})",
                self.cfg.max_folder_files
            )));
        }
        if total_bytes > self.cfg.max_folder_bytes {
            return Err(invalid(format!(
                "the folder is too large ({}; the limit is {})",
                mib(total_bytes),
                mib(self.cfg.max_folder_bytes)
            )));
        }
        if lock(&uploads.folders).len() >= MAX_OPEN_FOLDERS {
            return Err(api(
                ErrorCode::Unavailable,
                "another folder is still uploading; wait for it to finish",
            ));
        }
        let project = project_root(roots, &session)?;
        let base = attachments_dir(roots, &project)?;
        let folder_name = format!("{}-{}", now_stamp(), sanitize_name(name));
        let dir = create_unique_dir(&base, &folder_name)?;
        let dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folder_id = new_id("fd");
        lock(&uploads.folders).insert(
            folder_id.clone(),
            Folder {
                session_id,
                project,
                dir,
                name: dir_name,
                files_started: 0,
                files_done: 0,
                bytes_reserved: 0,
                bytes_done: 0,
                note: clean_note(note.as_deref()),
                submit,
                finished: false,
            },
        );
        Ok(folder_id)
    }

    async fn folder_end(
        &self,
        mgr: &SessionManager,
        uploads: &Uploads,
        device: Option<&str>,
        folder_id: &str,
    ) -> Result<ServerMsg, Error> {
        let open = lock(&uploads.files)
            .values()
            .any(|u| u.folder.as_deref() == Some(folder_id));
        if open {
            return Err(api(
                ErrorCode::Conflict,
                "files of this folder are still uploading",
            ));
        }
        let mut folder = lock(&uploads.folders)
            .remove(folder_id)
            .ok_or_else(|| api(ErrorCode::NotFound, "no such folder upload"))?;
        if folder.files_done == 0 {
            return Err(invalid("no file of the folder was uploaded"));
        }
        folder.finished = true;
        let rel = relative(&folder.project, &folder.dir)?;
        if self.cfg.git_exclude {
            ensure_git_exclude(&folder.project);
        }
        self.audit(
            device,
            &folder.session_id,
            "folder",
            &folder.name,
            "inode/directory",
            folder.bytes_done,
            folder.files_done,
        );
        let mention = mention(
            "Attached folder",
            &format!("{rel}/"),
            folder.note.as_deref(),
            folder.submit,
        );
        let typed = type_mention(mgr, &folder.session_id, &mention, folder.submit).await;
        Ok(ServerMsg::Attached {
            session_id: folder.session_id.clone(),
            path: rel,
            size: folder.bytes_done,
            mention: mention.trim_end().to_owned(),
            typed,
        })
    }

    /// The folder of batch `id`, opened (and created) by its first file.
    fn join_batch(
        uploads: &Uploads,
        id: &str,
        session_id: &SessionId,
        project: &Path,
        base: &Path,
    ) -> Result<PathBuf, Error> {
        check_batch_id(id)?;
        let mut batches = lock(&uploads.batches);
        if let Some(batch) = batches.get_mut(id) {
            if &batch.session_id != session_id {
                return Err(invalid("the batch belongs to another session"));
            }
            if batch.files_started >= MAX_BATCH_FILES {
                return Err(invalid(format!(
                    "the batch has too many files (the limit is {MAX_BATCH_FILES})"
                )));
            }
            batch.files_started += 1;
            return Ok(batch.dir.clone());
        }
        if batches.len() >= MAX_OPEN_BATCHES {
            return Err(api(
                ErrorCode::Unavailable,
                "other batches are still uploading; wait for them to finish",
            ));
        }
        let dir = create_unique_dir(base, &format!("{}-batch", now_stamp()))?;
        batches.insert(
            id.to_owned(),
            Batch {
                session_id: session_id.clone(),
                project: project.to_owned(),
                dir: dir.clone(),
                files_started: 1,
                files: Vec::new(),
                bytes: 0,
            },
        );
        Ok(dir)
    }

    /// Mentions every uploaded file of a batch once.
    async fn batch_end(
        &self,
        mgr: &SessionManager,
        uploads: &Uploads,
        batch_id: &str,
        note: Option<&str>,
        submit: bool,
    ) -> Result<ServerMsg, Error> {
        let open = lock(&uploads.files)
            .values()
            .any(|u| u.batch.as_deref() == Some(batch_id));
        if open {
            return Err(api(
                ErrorCode::Conflict,
                "files of this batch are still uploading",
            ));
        }
        let batch = lock(&uploads.batches)
            .remove(batch_id)
            .ok_or_else(|| api(ErrorCode::NotFound, "no such batch"))?;
        if batch.files.is_empty() {
            return Err(invalid("no file of the batch was uploaded"));
        }
        let folder = relative(&batch.project, &batch.dir)?;
        let note = clean_note(note);
        let mut text = batch_mention(&batch.files, &folder, None, None, true);
        if text.chars().count() > MAX_INLINE_MENTION_CHARS {
            let manifest = write_manifest(&batch.dir, &batch.files)?;
            let manifest = format!("{folder}/{manifest}");
            text = batch_mention(
                &batch.files,
                &folder,
                Some(&manifest),
                note.as_deref(),
                submit,
            );
        } else {
            text = batch_mention(&batch.files, &folder, None, note.as_deref(), submit);
        }
        let typed = type_mention(mgr, &batch.session_id, &text, submit).await;
        tracing::info!(
            session = %batch.session_id,
            files = batch.files.len(),
            chars = text.chars().count(),
            typed,
            "batch mention"
        );
        Ok(ServerMsg::Attached {
            session_id: batch.session_id.clone(),
            path: folder,
            size: batch.bytes,
            mention: text.trim_end().to_owned(),
            typed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        device: Option<&str>,
        session_id: &SessionId,
        kind: &str,
        name: &str,
        mime: &str,
        size: u64,
        files: u32,
    ) {
        tracing::info!(session = %session_id, kind, name, size, files, by = device.unwrap_or("local"), "attachment saved");
        if let (Some(audit), Some(device)) = (self.audit.get(), device) {
            audit.record(AuditEvent::Attachment {
                device_id: device.to_owned(),
                session_id: session_id.to_string(),
                kind: kind.to_owned(),
                name: redact(name),
                mime: mime.to_owned(),
                size,
                files,
            });
        }
    }
}

fn apply_chunk(upload: &mut Upload, offset: u64, data: &[u8]) -> Result<(), Error> {
    if data.len() > ATTACH_CHUNK_BYTES {
        return Err(invalid("chunk too large"));
    }
    if offset != upload.received {
        return Err(invalid(format!(
            "chunk out of order (expected offset {}, got {offset})",
            upload.received
        )));
    }
    if upload.received + data.len() as u64 > upload.size {
        return Err(invalid("more data than the declared size"));
    }
    if upload.head.len() < 8 {
        let take = (8 - upload.head.len()).min(data.len());
        upload.head.extend_from_slice(&data[..take]);
        if looks_executable(&upload.head) {
            return Err(api(
                ErrorCode::Forbidden,
                "executable files cannot be attached",
            ));
        }
    }
    let file = upload
        .file
        .as_mut()
        .ok_or_else(|| invalid("upload closed"))?;
    file.write_all(data)?;
    upload.received += data.len() as u64;
    Ok(())
}

fn cancel(uploads: &Uploads, id: &str) -> Result<(), Error> {
    if lock(&uploads.files).remove(id).is_some() {
        return Ok(());
    }
    let folder = lock(&uploads.folders).remove(id);
    match folder {
        Some(folder) => {
            // Close the folder's open files before its directory goes away.
            lock(&uploads.files).retain(|_, u| u.folder.as_deref() != Some(id));
            drop(folder);
            Ok(())
        }
        None => Err(api(ErrorCode::NotFound, "no such upload")),
    }
}

/// Moves a finished upload to a free final name in its directory, after
/// checking again that the directory is where it was. Never overwrites.
fn place(roots: &WorkspaceRoots, upload: &Upload) -> Result<PathBuf, Error> {
    let again = roots.prepare(&upload.dir, false)?;
    if again != upload.dir || !again.starts_with(&upload.project) {
        return Err(api(
            ErrorCode::Forbidden,
            "the attachments folder changed during the upload",
        ));
    }
    for n in 1..1000 {
        let target = upload.dir.join(candidate(&upload.name, n));
        // A hard link fails when the target exists, so nothing is replaced;
        // file systems without hard links fall back to a checked rename.
        match std::fs::hard_link(&upload.part, &target) {
            Ok(()) => {
                let _ = std::fs::remove_file(&upload.part);
                return verify_placed(upload, target);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                if std::fs::symlink_metadata(&target).is_ok() {
                    continue;
                }
                std::fs::rename(&upload.part, &target)?;
                return verify_placed(upload, target);
            }
        }
    }
    Err(api(ErrorCode::Conflict, "no free file name"))
}

fn verify_placed(upload: &Upload, target: PathBuf) -> Result<PathBuf, Error> {
    let real = canonical(&target)?;
    if real.parent() != Some(upload.dir.as_path()) {
        let _ = std::fs::remove_file(&target);
        return Err(api(
            ErrorCode::Forbidden,
            "the file left the project folder",
        ));
    }
    Ok(target)
}

/// `Attached file: @<path>[ <note>]`, with a trailing space when the user
/// is expected to continue the line.
pub fn mention(label: &str, rel: &str, note: Option<&str>, submit: bool) -> String {
    let mut text = format!("{label}: @{rel}");
    if let Some(note) = note {
        text.push(' ');
        text.push_str(note);
    }
    if !submit {
        text.push(' ');
    }
    text
}

/// The one mention of a batch: `Attached file: @a` for one file,
/// `Attached files (3): @a @b @c` without a `manifest`, otherwise
/// `Attached 23 files: @<folder>/ (list: @<manifest>)`.
pub fn batch_mention(
    files: &[String],
    folder: &str,
    manifest: Option<&str>,
    note: Option<&str>,
    submit: bool,
) -> String {
    if let [only] = files {
        return mention("Attached file", only, note, submit);
    }
    let n = files.len();
    let mut text = match manifest {
        Some(manifest) => format!("Attached {n} files: @{folder}/ (list: @{manifest})"),
        None => {
            let list: Vec<String> = files.iter().map(|f| format!("@{f}")).collect();
            format!("Attached files ({n}): {}", list.join(" "))
        }
    };
    if let Some(note) = note {
        text.push(' ');
        text.push_str(note);
    }
    if !submit {
        text.push(' ');
    }
    text
}

/// Writes the list of a batch's files (one project-relative path per line)
/// into a new manifest in the batch folder; returns its file name.
fn write_manifest(dir: &Path, files: &[String]) -> Result<String, Error> {
    let mut body = files.join("\n");
    body.push('\n');
    for n in 1..1000 {
        let name = candidate(BATCH_MANIFEST, n);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&name))
        {
            Ok(mut f) => {
                f.write_all(body.as_bytes())?;
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(api(ErrorCode::Conflict, "no free manifest name"))
}

/// A batch id is a short token of letters, digits, `-` and `_`.
fn check_batch_id(id: &str) -> Result<(), Error> {
    let ok = !id.is_empty()
        && id.len() <= MAX_BATCH_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(invalid("invalid batch id"))
    }
}

/// Creates a new directory `name` (or `name-2`, ...) in the canonical
/// `base`. `create_dir` fails when the name exists, so the folder is always
/// new and only ever holds this upload's files. Returns its canonical path.
fn create_unique_dir(base: &Path, name: &str) -> Result<PathBuf, Error> {
    let mut created = None;
    for n in 1..1000 {
        let path = base.join(candidate(name, n));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                created = Some(path);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let created = created.ok_or_else(|| api(ErrorCode::Conflict, "no free folder name"))?;
    let dir = canonical(&created)?;
    if !dir.starts_with(base) {
        let _ = std::fs::remove_dir(&created);
        return Err(api(ErrorCode::Forbidden, "the folder left the project"));
    }
    Ok(dir)
}

/// Types the mention into a managed session. Observed and cloud sessions
/// only get the saved file.
async fn type_mention(mgr: &SessionManager, id: &SessionId, mention: &str, submit: bool) -> bool {
    let Some(session) = mgr.get(id) else {
        return false;
    };
    if session.managed().is_none() {
        return false;
    }
    let input = Input::Text {
        text: mention.to_owned(),
        submit,
        at_prompt: false,
    };
    let Ok(done) = mgr.send_input(id, input) else {
        return false;
    };
    matches!(
        tokio::time::timeout(TYPE_TIMEOUT, done).await,
        Ok(Ok(Ok(())))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::LogLimits;
    use crate::sessions::{ManagerConfig, Observed, ObservedIdentity};
    use vca_core::{AgentKind, ControlTier, SessionInfo, SessionOrigin, SessionState};

    #[test]
    fn sanitizes_traversal_and_separators() {
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name(r"..\..\Windows\win.ini"), "win.ini");
        assert_eq!(sanitize_name(".."), "file");
        assert_eq!(sanitize_name("a..b"), "a.b");
        assert_eq!(sanitize_name(".bashrc"), "bashrc");
        assert_eq!(sanitize_name("my photo (1).JPG"), "my-photo-1.JPG");
        assert_eq!(sanitize_name("a\u{0}b\nc\u{202e}d.txt"), "a-b-c-d.txt");
        assert_eq!(sanitize_name("x<>:\"|?*y.md"), "x-y.md");
        assert_eq!(sanitize_name("name. . ."), "name");
        assert_eq!(sanitize_name(""), "file");
        assert_eq!(sanitize_name("@foo;rm -rf.sh"), "foo-rm-rf.sh");
    }

    #[test]
    fn sanitizes_windows_names() {
        assert_eq!(sanitize_name("CON"), "_CON");
        assert_eq!(sanitize_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_name("Lpt1.tar.gz"), "_Lpt1.tar.gz");
        assert_eq!(sanitize_name("console.txt"), "console.txt");
        assert_eq!(sanitize_name("nul."), "_nul");
    }

    #[test]
    fn sanitizes_unicode_and_length() {
        assert_eq!(sanitize_name("회의록 2026.pdf"), "회의록-2026.pdf");
        assert_eq!(sanitize_name("写真.heic"), "写真.heic");
        // Decomposed (NFD) names keep their combining marks.
        assert_eq!(sanitize_name("cafe\u{301}.txt"), "cafe\u{301}.txt");
        let long = format!("{}.png", "가".repeat(300));
        let s = sanitize_name(&long);
        assert!(s.ends_with(".png"));
        assert!(s.len() <= MAX_STEM_BYTES + 4, "{} bytes", s.len());
        let long_ext = format!("a.{}", "x".repeat(40));
        assert!(sanitize_name(&long_ext).chars().count() <= MAX_STEM_CHARS);
    }

    #[test]
    fn relative_paths_refuse_hidden_and_traversal() {
        assert_eq!(
            sanitize_relative_path("src/main.rs").unwrap(),
            vec!["src", "main.rs"]
        );
        assert_eq!(
            sanitize_relative_path(r"a\b c\d.txt").unwrap(),
            vec!["a", "b-c", "d.txt"]
        );
        for bad in [
            "../x",
            "a/../../x",
            ".git/config",
            "node_modules/x/index.js",
            "a/.DS_Store",
            "Thumbs.db",
            "",
            "/",
        ] {
            assert!(sanitize_relative_path(bad).is_err(), "{bad}");
        }
        let deep = vec!["d"; MAX_FOLDER_DEPTH + 1].join("/");
        assert!(sanitize_relative_path(&deep).is_err());
    }

    #[test]
    fn type_allow_list() {
        for (mime, name) in [
            ("image/jpeg", "a.jpg"),
            ("image/heic", "a.heic"),
            ("application/pdf", "a.pdf"),
            ("text/markdown", "a.md"),
            ("text/plain; charset=utf-8", "a.txt"),
            ("application/json", "a.json"),
            (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "a.docx",
            ),
            ("application/zip", "a.zip"),
            ("", "main.rs"),
            ("application/octet-stream", "data.csv"),
            ("", "Dockerfile"),
        ] {
            assert!(check_type(mime, name).is_ok(), "{mime} {name}");
        }
        for (mime, name, code) in [
            ("application/x-msdownload", "a.exe", ErrorCode::Forbidden),
            (
                "application/octet-stream",
                "setup.exe",
                ErrorCode::Forbidden,
            ),
            ("text/plain", "run.bat", ErrorCode::Forbidden),
            ("image/png", "x.ps1", ErrorCode::Forbidden),
            (
                "application/vnd.android.package-archive",
                "a.apk",
                ErrorCode::Forbidden,
            ),
            (
                "application/octet-stream",
                "blob.bin",
                ErrorCode::Unsupported,
            ),
            ("video/mp4", "clip.mp4", ErrorCode::Unsupported),
            ("", "noext", ErrorCode::Unsupported),
        ] {
            assert_eq!(
                check_type(mime, name).unwrap_err().code(),
                code,
                "{mime} {name}"
            );
        }
        assert!(looks_executable(b"MZ\x90\0"));
        assert!(looks_executable(b"\x7fELF\x02"));
        assert!(looks_executable(&[0xCF, 0xFA, 0xED, 0xFE]));
        assert!(!looks_executable(b"\x89PNG"));
        assert!(!looks_executable(b"%PDF"));
    }

    #[test]
    fn timestamps_are_utc_civil_time() {
        assert_eq!(timestamp(0), "19700101-000000");
        assert_eq!(timestamp(951_782_400), "20000229-000000");
        assert_eq!(timestamp(1_793_349_296), "20261030-083456");
    }

    #[test]
    fn config_section() {
        let cfg = crate::config::Config::from_toml_str(
            "[attachments]
max_bytes = 10
max_folder_files = 3
git_exclude = false
",
        )
        .unwrap();
        assert_eq!(cfg.attachments.max_bytes, 10);
        assert_eq!(cfg.attachments.max_folder_files, 3);
        assert!(!cfg.attachments.git_exclude);
        assert!(cfg.attachments.enabled);
        let defaults = crate::config::Config::default().attachments;
        assert_eq!(defaults.max_bytes, 25 * 1024 * 1024);
        assert!(
            crate::config::Config::from_toml_str(
                "[attachments]
nope = 1"
            )
            .is_err()
        );
    }

    #[test]
    fn mentions() {
        assert_eq!(
            mention("Attached file", ".vca/attachments/a.png", None, false),
            "Attached file: @.vca/attachments/a.png "
        );
        assert_eq!(
            mention(
                "Attached file",
                "x",
                clean_note(Some("look\nat this")).as_deref(),
                true
            ),
            "Attached file: @x look at this"
        );
        assert_eq!(clean_note(Some(" \n ")), None);
    }

    #[test]
    fn git_exclude_once() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        std::fs::create_dir_all(project.join(".git/info")).unwrap();
        std::fs::write(project.join(".git/info/exclude"), "*.log").unwrap();
        let project = canonical(project).unwrap();
        ensure_git_exclude(&project);
        ensure_git_exclude(&project);
        let text = std::fs::read_to_string(project.join(".git/info/exclude")).unwrap();
        assert_eq!(text.matches("/.vca/").count(), 1, "{text}");
        assert!(text.starts_with("*.log\n"));
        assert!(!project.join(".gitignore").exists());
        // Not a git checkout: nothing is created.
        let plain = tempfile::tempdir().unwrap();
        ensure_git_exclude(plain.path());
        assert!(!plain.path().join(".git").exists());
    }

    // ----------------------------------------------------- with sessions

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        mgr: SessionManager,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path()).unwrap().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        let mgr = SessionManager::new(
            ManagerConfig {
                limits: LogLimits {
                    max_events: 100,
                    max_bytes: 1 << 20,
                },
                exited_retention: Duration::from_secs(60),
                pause_scope: vca_broker::contain::Scope::ChildrenOnly,
                heuristics_interval: Duration::from_millis(500),
                control_endpoint: tmp.path().join("ctl"),
                attach_dir: None,
                workspace_roots: WorkspaceRoots::new([root.clone()]),
                session_names: None,
                allow_generic_launch: false,
            },
            None,
        );
        Fixture {
            _tmp: tmp,
            root,
            mgr,
        }
    }

    fn observed(mgr: &SessionManager, id: &str, project: &Path) -> SessionId {
        let info = SessionInfo {
            id: id.into(),
            origin: SessionOrigin::Local {
                agent: AgentKind::ClaudeCode,
                project_dir: project.to_owned(),
                pid: None,
            },
            display_name: id.into(),
            state: SessionState::Running,
            control: ControlTier::ObserveOnly,
            profile: None,
            created_at: 0,
            last_activity: 0,
            unread: 0,
            preview: None,
            auto_name: None,
            custom_name: false,
            shell: None,
            pending_decision: None,
            subagents: 0,
            conductor: false,
            terminal_prompt: None,
            question: None,
            progress: None,
            problem: None,
        };
        mgr.insert_observed(
            info,
            Observed {
                terminal: None,
                identity: ObservedIdentity::default(),
            },
            None,
        );
        id.into()
    }

    fn attach(sid: &SessionId, name: &str, mime: &str, data: &[u8]) -> ClientMsg {
        ClientMsg::Attach {
            session_id: sid.clone(),
            name: name.into(),
            mime: mime.into(),
            data: data.to_vec(),
            note: None,
            submit: false,
        }
    }

    fn error_code(msg: &ServerMsg) -> ErrorCode {
        match msg {
            ServerMsg::Error { code, .. } => *code,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observed_session_saves_only_and_audits() {
        let fx = fixture();
        let project = fx.root.join("proj");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let sid = observed(&fx.mgr, "obs", &project);
        let att = Attachments::new(AttachmentsConfig::default());
        let audit = Arc::new(AuditLog::memory(vca_net::system_clock()));
        att.set_audit(Arc::clone(&audit));
        let uploads = Uploads::default();

        let reply = att
            .handle(
                &fx.mgr,
                &uploads,
                Some("dev_1"),
                attach(&sid, "../screen shot.png", "image/png", b"\x89PNG data"),
            )
            .await;
        let ServerMsg::Attached {
            path,
            size,
            mention,
            typed,
            ..
        } = reply
        else {
            panic!("{reply:?}");
        };
        assert!(!typed, "observed sessions get no input");
        assert_eq!(size, 9);
        assert!(path.starts_with(".vca/attachments/"), "{path}");
        assert!(path.ends_with("-screen-shot.png"), "{path}");
        assert_eq!(mention, format!("Attached file: @{path}"));
        assert_eq!(std::fs::read(project.join(&path)).unwrap(), b"\x89PNG data");
        let exclude = std::fs::read_to_string(project.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains("/.vca/"));
        // No leftover temporary files.
        let dir = project.join(ATTACHMENTS_DIR);
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");

        // Same name again: deduplicated, never overwritten.
        let again = att
            .handle(
                &fx.mgr,
                &uploads,
                Some("dev_1"),
                attach(&sid, "screen shot.png", "image/png", b"\x89PNG other"),
            )
            .await;
        let ServerMsg::Attached { path: path2, .. } = again else {
            panic!("{again:?}");
        };
        assert_ne!(path, path2);

        let records = audit.recent(10);
        let AuditEvent::Attachment {
            device_id,
            session_id,
            kind,
            name,
            size,
            ..
        } = &records[0].event
        else {
            panic!("{records:?}");
        };
        assert_eq!(device_id, "dev_1");
        assert_eq!(session_id, "obs");
        assert_eq!(kind, "file");
        assert!(name.ends_with("screen-shot.png"));
        assert_eq!(*size, 9);
        let line = serde_json::to_string(&records[0]).unwrap();
        assert!(!line.contains("PNG data"), "no content in the audit log");
    }

    #[tokio::test]
    async fn chunked_upload_limits_and_order() {
        let fx = fixture();
        let project = fx.root.join("p");
        std::fs::create_dir_all(&project).unwrap();
        let sid = observed(&fx.mgr, "s", &project);
        let att = Attachments::new(AttachmentsConfig {
            max_bytes: 10,
            ..AttachmentsConfig::default()
        });
        let uploads = Uploads::default();
        let begin = |size| ClientMsg::AttachBegin {
            session_id: sid.clone(),
            name: "notes.txt".into(),
            mime: "text/plain".into(),
            size,
            note: Some("see this".into()),
            submit: false,
            folder: None,
            batch: None,
        };
        let big = att.handle(&fx.mgr, &uploads, None, begin(11)).await;
        assert_eq!(error_code(&big), ErrorCode::InvalidRequest);

        let ready = att.handle(&fx.mgr, &uploads, None, begin(6)).await;
        let ServerMsg::AttachReady { upload_id, .. } = ready else {
            panic!("{ready:?}");
        };
        let chunk = |offset, data: &[u8]| ClientMsg::AttachChunk {
            upload_id: upload_id.clone(),
            offset,
            data: data.to_vec(),
        };
        let ok = att.handle(&fx.mgr, &uploads, None, chunk(0, b"abc")).await;
        assert_eq!(ok, ServerMsg::Ok);
        let early = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                ClientMsg::AttachEnd {
                    upload_id: upload_id.clone(),
                },
            )
            .await;
        assert_eq!(error_code(&early), ErrorCode::InvalidRequest);
        // The failed end dropped the upload and its temporary file.
        let gone = att.handle(&fx.mgr, &uploads, None, chunk(3, b"def")).await;
        assert_eq!(error_code(&gone), ErrorCode::NotFound);
        let dir = project.join(ATTACHMENTS_DIR);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        // Out of order and oversize chunks end the upload.
        let ServerMsg::AttachReady { upload_id, .. } =
            att.handle(&fx.mgr, &uploads, None, begin(6)).await
        else {
            panic!()
        };
        let skip = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                ClientMsg::AttachChunk {
                    upload_id: upload_id.clone(),
                    offset: 2,
                    data: b"x".to_vec(),
                },
            )
            .await;
        assert_eq!(error_code(&skip), ErrorCode::InvalidRequest);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        // A complete upload, with the note in the mention.
        let ServerMsg::AttachReady { upload_id, .. } =
            att.handle(&fx.mgr, &uploads, None, begin(6)).await
        else {
            panic!()
        };
        for (off, part) in [(0, b"abc"), (3, b"def")] {
            let r = att
                .handle(
                    &fx.mgr,
                    &uploads,
                    None,
                    ClientMsg::AttachChunk {
                        upload_id: upload_id.clone(),
                        offset: off,
                        data: part.to_vec(),
                    },
                )
                .await;
            assert_eq!(r, ServerMsg::Ok);
        }
        let done = att
            .handle(&fx.mgr, &uploads, None, ClientMsg::AttachEnd { upload_id })
            .await;
        let ServerMsg::Attached { path, mention, .. } = done else {
            panic!("{done:?}");
        };
        assert!(mention.ends_with(" see this"), "{mention}");
        assert_eq!(std::fs::read(project.join(path)).unwrap(), b"abcdef");

        // Dropping the client's uploads removes unfinished files.
        let ServerMsg::AttachReady { .. } = att.handle(&fx.mgr, &uploads, None, begin(6)).await
        else {
            panic!()
        };
        let before = std::fs::read_dir(&dir).unwrap().count();
        drop(uploads);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before - 1);
    }

    #[tokio::test]
    async fn rejects_types_executables_and_cloud_or_outside_sessions() {
        let fx = fixture();
        let project = fx.root.join("p");
        std::fs::create_dir_all(&project).unwrap();
        let sid = observed(&fx.mgr, "s", &project);
        let att = Attachments::new(AttachmentsConfig::default());
        let uploads = Uploads::default();
        let exe = att
            .handle(&fx.mgr, &uploads, None, attach(&sid, "a.exe", "", b"MZ"))
            .await;
        assert_eq!(error_code(&exe), ErrorCode::Forbidden);
        // Executable content under an innocent name.
        let disguised = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                attach(&sid, "photo.png", "image/png", b"MZ\x90\0\x03"),
            )
            .await;
        assert_eq!(error_code(&disguised), ErrorCode::Forbidden);
        let video = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                attach(&sid, "a.mp4", "video/mp4", b"x"),
            )
            .await;
        assert_eq!(error_code(&video), ErrorCode::Unsupported);
        let missing = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                attach(&"nope".into(), "a.txt", "text/plain", b"x"),
            )
            .await;
        assert_eq!(error_code(&missing), ErrorCode::NotFound);

        // A session whose project is outside the workspace roots.
        let outside = fx.root.parent().unwrap().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let out = observed(&fx.mgr, "out", &outside);
        let refused = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                attach(&out, "a.txt", "text/plain", b"x"),
            )
            .await;
        assert_eq!(error_code(&refused), ErrorCode::Forbidden);
        assert!(!outside.join(".vca").exists());

        // Disabled by configuration.
        let off = Attachments::new(AttachmentsConfig {
            enabled: false,
            ..AttachmentsConfig::default()
        });
        let r = off
            .handle(
                &fx.mgr,
                &uploads,
                None,
                attach(&sid, "a.txt", "text/plain", b"x"),
            )
            .await;
        assert_eq!(error_code(&r), ErrorCode::Unsupported);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_attachments_dir_symlinked_out_of_the_project() {
        let fx = fixture();
        let project = fx.root.join("p");
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join(".vca")).unwrap();
        let sid = observed(&fx.mgr, "s", &project);
        let att = Attachments::new(AttachmentsConfig::default());
        let uploads = Uploads::default();
        let r = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                attach(&sid, "a.txt", "text/plain", b"x"),
            )
            .await;
        assert_eq!(error_code(&r), ErrorCode::Forbidden);
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn folder_upload_paths_caps_and_one_mention() {
        let fx = fixture();
        let project = fx.root.join("p");
        std::fs::create_dir_all(&project).unwrap();
        let sid = observed(&fx.mgr, "s", &project);
        let att = Attachments::new(AttachmentsConfig {
            max_folder_files: 5,
            max_folder_bytes: 20,
            ..AttachmentsConfig::default()
        });
        let audit = Arc::new(AuditLog::memory(vca_net::system_clock()));
        att.set_audit(Arc::clone(&audit));
        let uploads = Uploads::default();
        let folder_begin = |files, total_bytes| ClientMsg::AttachFolderBegin {
            session_id: sid.clone(),
            name: "My Docs".into(),
            files,
            total_bytes,
            note: None,
            submit: false,
        };
        let too_many = att
            .handle(&fx.mgr, &uploads, None, folder_begin(6, 1))
            .await;
        assert_eq!(error_code(&too_many), ErrorCode::InvalidRequest);
        let too_big = att
            .handle(&fx.mgr, &uploads, None, folder_begin(1, 21))
            .await;
        assert_eq!(error_code(&too_big), ErrorCode::InvalidRequest);

        let ready = att
            .handle(&fx.mgr, &uploads, None, folder_begin(3, 12))
            .await;
        let ServerMsg::AttachFolderReady { folder_id, .. } = ready else {
            panic!("{ready:?}");
        };
        let send = |path: &str, data: &[u8]| {
            let msg = ClientMsg::AttachBegin {
                session_id: sid.clone(),
                name: path.rsplit('/').next().unwrap().into(),
                mime: String::new(),
                size: data.len() as u64,
                note: None,
                submit: false,
                folder: Some(AttachFolderEntry {
                    folder_id: folder_id.clone(),
                    path: path.into(),
                }),
                batch: None,
            };
            let data = data.to_vec();
            let att = &att;
            let uploads = &uploads;
            let mgr = &fx.mgr;
            async move {
                let r = att.handle(mgr, uploads, Some("dev_9"), msg).await;
                let ServerMsg::AttachReady { upload_id, .. } = r else {
                    return r;
                };
                let c = att
                    .handle(
                        mgr,
                        uploads,
                        Some("dev_9"),
                        ClientMsg::AttachChunk {
                            upload_id: upload_id.clone(),
                            offset: 0,
                            data,
                        },
                    )
                    .await;
                if c != ServerMsg::Ok {
                    return c;
                }
                att.handle(
                    mgr,
                    uploads,
                    Some("dev_9"),
                    ClientMsg::AttachEnd { upload_id },
                )
                .await
            }
        };
        // Files keep their relative paths; any extension is fine in a folder.
        let a = send("src/main.rs", b"fn main").await;
        let ServerMsg::Attached {
            path,
            mention,
            typed,
            ..
        } = a
        else {
            panic!("{a:?}");
        };
        assert!(
            mention.is_empty() && !typed,
            "files of a folder are not mentioned"
        );
        assert!(path.ends_with("-My-Docs/src/main.rs"), "{path}");
        let b = send("deep/dir name/Makefile.unknownext", b"all").await;
        assert!(matches!(b, ServerMsg::Attached { .. }), "{b:?}");
        // Refused entries: traversal, hidden, executables, over the caps.
        for bad in ["../escape.txt", ".git/config", "node_modules/a.js"] {
            assert_eq!(
                error_code(&send(bad, b"x").await),
                ErrorCode::InvalidRequest,
                "{bad}"
            );
        }
        assert_eq!(
            error_code(&send("bin/tool.exe", b"x").await),
            ErrorCode::Forbidden
        );
        assert_eq!(
            error_code(&send("x.dat", b"MZ\x90\0").await),
            ErrorCode::Forbidden
        );
        let over = send("big.txt", &[b'a'; 15]).await;
        assert_eq!(error_code(&over), ErrorCode::InvalidRequest);
        assert!(!fx.root.join("escape.txt").exists());

        let done = att
            .handle(
                &fx.mgr,
                &uploads,
                Some("dev_9"),
                ClientMsg::AttachFolderEnd {
                    folder_id: folder_id.clone(),
                },
            )
            .await;
        let ServerMsg::Attached {
            path,
            size,
            mention,
            ..
        } = done
        else {
            panic!("{done:?}");
        };
        assert_eq!(size, 10);
        assert!(
            path.starts_with(".vca/attachments/") && path.ends_with("-My-Docs"),
            "{path}"
        );
        assert_eq!(mention, format!("Attached folder: @{path}/"));
        assert!(
            project
                .join(&path)
                .join("deep/dir-name/Makefile.unknownext")
                .is_file()
        );
        let folder_audits: Vec<_> = audit
            .recent(10)
            .into_iter()
            .filter(|r| matches!(&r.event, AuditEvent::Attachment { kind, files: 2, .. } if kind == "folder"))
            .collect();
        assert_eq!(folder_audits.len(), 1);

        // A cancelled folder upload leaves nothing behind.
        let ServerMsg::AttachFolderReady { folder_id, .. } = att
            .handle(&fx.mgr, &uploads, None, folder_begin(1, 1))
            .await
        else {
            panic!()
        };
        let base = project.join(ATTACHMENTS_DIR);
        let before = std::fs::read_dir(&base).unwrap().count();
        let r = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                ClientMsg::AttachCancel {
                    upload_id: folder_id,
                },
            )
            .await;
        assert_eq!(r, ServerMsg::Ok);
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), before - 1);
    }

    /// Uploads `data` as one file of batch `batch` (begin, one chunk, end).
    async fn batch_file(
        att: &Attachments,
        mgr: &SessionManager,
        uploads: &Uploads,
        sid: &SessionId,
        batch: &str,
        name: &str,
        data: &[u8],
    ) -> ServerMsg {
        let ready = att
            .handle(
                mgr,
                uploads,
                Some("dev_1"),
                ClientMsg::AttachBegin {
                    session_id: sid.clone(),
                    name: name.into(),
                    mime: "image/jpeg".into(),
                    size: data.len() as u64,
                    note: None,
                    submit: true,
                    folder: None,
                    batch: Some(batch.into()),
                },
            )
            .await;
        let ServerMsg::AttachReady { upload_id, .. } = ready else {
            return ready;
        };
        let ok = att
            .handle(
                mgr,
                uploads,
                Some("dev_1"),
                ClientMsg::AttachChunk {
                    upload_id: upload_id.clone(),
                    offset: 0,
                    data: data.to_vec(),
                },
            )
            .await;
        assert_eq!(ok, ServerMsg::Ok);
        att.handle(
            mgr,
            uploads,
            Some("dev_1"),
            ClientMsg::AttachEnd { upload_id },
        )
        .await
    }

    fn batch_end(batch: &str, submit: bool) -> ClientMsg {
        ClientMsg::AttachBatchEnd {
            batch_id: batch.into(),
            note: None,
            submit,
        }
    }

    #[tokio::test]
    async fn batch_is_mentioned_once() {
        let fx = fixture();
        let project = fx.root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let sid = observed(&fx.mgr, "obs", &project);
        let att = Attachments::new(AttachmentsConfig::default());
        let uploads = Uploads::default();

        let mut paths = Vec::new();
        for name in ["IMG_1.jpg", "IMG_2.jpg", "IMG_1.jpg"] {
            let reply =
                batch_file(&att, &fx.mgr, &uploads, &sid, "b1", name, b"\xFF\xD8 jpg").await;
            let ServerMsg::Attached {
                path,
                mention,
                typed,
                ..
            } = reply
            else {
                panic!("{reply:?}");
            };
            // No mention per file: the batch end makes the only one.
            assert!(mention.is_empty() && !typed, "{mention:?}");
            paths.push(path);
        }
        // One new folder holds the batch; the names stay, clashes get `-2`.
        let folder = paths[0].rsplit_once('/').unwrap().0.to_owned();
        assert!(folder.starts_with(".vca/attachments/") && folder.ends_with("-batch"));
        assert_eq!(
            paths,
            [
                format!("{folder}/IMG_1.jpg"),
                format!("{folder}/IMG_2.jpg"),
                format!("{folder}/IMG_1-2.jpg"),
            ]
        );
        for p in &paths {
            assert!(project.join(p).is_file(), "{p}");
        }

        let done = att
            .handle(&fx.mgr, &uploads, Some("dev_1"), batch_end("b1", false))
            .await;
        let ServerMsg::Attached {
            path,
            size,
            mention,
            typed,
            ..
        } = done
        else {
            panic!("{done:?}");
        };
        assert_eq!(path, folder);
        assert_eq!(size, 3 * 6);
        assert!(!typed, "observed sessions get no input");
        assert_eq!(
            mention,
            format!(
                "Attached files (3): @{folder}/IMG_1.jpg @{folder}/IMG_2.jpg @{folder}/IMG_1-2.jpg"
            )
        );
        // Short lists need no manifest.
        assert!(!project.join(&folder).join(BATCH_MANIFEST).exists());
        // The batch is gone once ended.
        let again = att
            .handle(&fx.mgr, &uploads, None, batch_end("b1", false))
            .await;
        assert_eq!(error_code(&again), ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn fifty_file_batch_names_the_folder_and_a_manifest() {
        let fx = fixture();
        let project = fx.root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let sid = observed(&fx.mgr, "obs", &project);
        let att = Attachments::new(AttachmentsConfig::default());
        let uploads = Uploads::default();

        let mut paths = Vec::new();
        for i in 0..50 {
            let name = format!("IMG_{}.jpg", 6700 + i);
            let reply = batch_file(&att, &fx.mgr, &uploads, &sid, "big", &name, b"jpg").await;
            let ServerMsg::Attached { path, mention, .. } = reply else {
                panic!("{reply:?}");
            };
            assert!(mention.is_empty());
            paths.push(path);
        }
        let done = att
            .handle(&fx.mgr, &uploads, None, batch_end("big", true))
            .await;
        let ServerMsg::Attached {
            path: folder,
            size,
            mention,
            ..
        } = done
        else {
            panic!("{done:?}");
        };
        assert_eq!(size, 150);
        assert_eq!(
            mention,
            format!("Attached 50 files: @{folder}/ (list: @{folder}/{BATCH_MANIFEST})")
        );
        // One short line instead of fifty mentions (about 3100 characters).
        assert!(mention.chars().count() < 120, "{mention}");
        let manifest = std::fs::read_to_string(project.join(&folder).join(BATCH_MANIFEST)).unwrap();
        let listed: Vec<&str> = manifest.lines().collect();
        assert_eq!(listed, paths);
        assert_eq!(
            std::fs::read_dir(project.join(&folder)).unwrap().count(),
            51
        );
    }

    #[tokio::test]
    async fn batch_rules() {
        let fx = fixture();
        let project = fx.root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let sid = observed(&fx.mgr, "obs", &project);
        let other = observed(&fx.mgr, "obs2", &project);
        let att = Attachments::new(AttachmentsConfig::default());
        let uploads = Uploads::default();

        // Ids are short tokens.
        for bad in ["", "../x", "a b", "x".repeat(65).as_str()] {
            let r = batch_file(&att, &fx.mgr, &uploads, &sid, bad, "a.jpg", b"jpg").await;
            assert_eq!(error_code(&r), ErrorCode::InvalidRequest, "{bad:?}");
        }
        // A batch belongs to one session.
        batch_file(&att, &fx.mgr, &uploads, &sid, "b", "a.jpg", b"jpg").await;
        let r = batch_file(&att, &fx.mgr, &uploads, &other, "b", "b.jpg", b"jpg").await;
        assert_eq!(error_code(&r), ErrorCode::InvalidRequest);
        // Not ended while one of its files is still open.
        let ready = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                ClientMsg::AttachBegin {
                    session_id: sid.clone(),
                    name: "c.jpg".into(),
                    mime: "image/jpeg".into(),
                    size: 3,
                    note: None,
                    submit: false,
                    folder: None,
                    batch: Some("b".into()),
                },
            )
            .await;
        let ServerMsg::AttachReady { upload_id, .. } = ready else {
            panic!("{ready:?}");
        };
        let r = att
            .handle(&fx.mgr, &uploads, None, batch_end("b", false))
            .await;
        assert_eq!(error_code(&r), ErrorCode::Conflict);
        att.handle(
            &fx.mgr,
            &uploads,
            None,
            ClientMsg::AttachCancel { upload_id },
        )
        .await;
        // One file left: a plain single mention.
        let r = att
            .handle(&fx.mgr, &uploads, None, batch_end("b", false))
            .await;
        let ServerMsg::Attached { mention, path, .. } = r else {
            panic!("{r:?}");
        };
        assert_eq!(mention, format!("Attached file: @{path}/a.jpg"));

        // A batch whose files all failed has nothing to mention, and its
        // empty folder is removed.
        let ready = att
            .handle(
                &fx.mgr,
                &uploads,
                None,
                ClientMsg::AttachBegin {
                    session_id: sid.clone(),
                    name: "d.jpg".into(),
                    mime: "image/jpeg".into(),
                    size: 3,
                    note: None,
                    submit: false,
                    folder: None,
                    batch: Some("empty".into()),
                },
            )
            .await;
        let ServerMsg::AttachReady { upload_id, .. } = ready else {
            panic!("{ready:?}");
        };
        att.handle(
            &fx.mgr,
            &uploads,
            None,
            ClientMsg::AttachCancel { upload_id },
        )
        .await;
        let r = att
            .handle(&fx.mgr, &uploads, None, batch_end("empty", false))
            .await;
        assert_eq!(error_code(&r), ErrorCode::InvalidRequest);
        let dirs: Vec<_> = std::fs::read_dir(project.join(ATTACHMENTS_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
    }

    #[test]
    fn batch_mentions() {
        let files = ["a/1.jpg".to_owned(), "a/2.jpg".to_owned()];
        assert_eq!(
            batch_mention(&files, "a", None, Some("see these"), false),
            "Attached files (2): @a/1.jpg @a/2.jpg see these "
        );
        assert_eq!(
            batch_mention(&files, "a", Some("a/files.txt"), None, true),
            "Attached 2 files: @a/ (list: @a/files.txt)"
        );
        assert_eq!(
            batch_mention(&files[..1], "a", Some("a/files.txt"), None, true),
            "Attached file: @a/1.jpg"
        );
    }

    #[tokio::test]
    async fn cloud_sessions_are_refused() {
        let fx = fixture();
        let info = SessionInfo {
            id: "c1".into(),
            origin: SessionOrigin::Cloud {
                provider: vca_core::CloudProvider::Jules,
                remote_id: "r".into(),
                url: None,
            },
            display_name: "c".into(),
            state: SessionState::Running,
            control: ControlTier::Cloud,
            profile: None,
            created_at: 0,
            last_activity: 0,
            unread: 0,
            preview: None,
            auto_name: None,
            custom_name: false,
            shell: None,
            pending_decision: None,
            subagents: 0,
            conductor: false,
            terminal_prompt: None,
            question: None,
            progress: None,
            problem: None,
        };
        // Observed stands in for the cloud handle: only the origin matters.
        fx.mgr.insert_observed(
            info,
            Observed {
                terminal: None,
                identity: ObservedIdentity::default(),
            },
            None,
        );
        let att = Attachments::new(AttachmentsConfig::default());
        let r = att
            .handle(
                &fx.mgr,
                &Uploads::default(),
                None,
                attach(&"c1".into(), "a.txt", "text/plain", b"x"),
            )
            .await;
        let ServerMsg::Error { code, message } = r else {
            panic!("{r:?}");
        };
        assert_eq!(code, ErrorCode::Unsupported);
        assert!(message.contains("cloud"), "{message}");
    }
}
