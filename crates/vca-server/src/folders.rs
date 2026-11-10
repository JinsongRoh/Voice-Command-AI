//! Project folder picker: the workspace roots, recent project folders and
//! the sub-folders of a folder inside the roots ([`ClientMsg::ProjectFolders`],
//! [`ClientMsg::ListFolder`], [`ClientMsg::FolderPath`]).
//!
//! Every path a client sends goes through [`WorkspaceRoots::prepare`]
//! (symlinks and `..` resolved, must be inside a root), and listings only
//! return folders that stay inside the roots. Nothing here reads the
//! content of a project file: a listing checks existence only (`.git`).
//! Agent transcripts (Claude Code, Codex) are read for their recorded
//! working directory, the way session discovery does.
//!
//! [`ClientMsg::ProjectFolders`]: vca_core::ClientMsg::ProjectFolders
//! [`ClientMsg::ListFolder`]: vca_core::ClientMsg::ListFolder
//! [`ClientMsg::FolderPath`]: vca_core::ClientMsg::FolderPath

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vca_core::error::ErrorCode;
use vca_core::protocol::{FolderCrumb, FolderEntry, HostOs};
use vca_core::{Error, ServerMsg, SessionInfo, SessionOrigin, UnixMillis};

use crate::util::lock;
use crate::workspace::{WorkspaceRoots, without_verbatim_prefix};

/// Most recent project folders returned.
pub const MAX_RECENT: usize = 30;
/// Most sub-folders in one listing; the rest is reported as `truncated`.
pub const MAX_ENTRIES: usize = 500;
/// Directory entries looked at in one listing at most.
const MAX_SCAN: usize = 20_000;
/// Transcript scans are reused this long.
const TRANSCRIPT_CACHE: Duration = Duration::from_secs(30);
/// Transcript project folders considered (newest first).
const MAX_TRANSCRIPT_PROJECTS: usize = 80;
/// Codex rollout files looked at (newest first).
const MAX_ROLLOUTS: usize = 300;
/// Longest new folder name, in characters.
const MAX_NAME_CHARS: usize = 255;
/// Requests per client: burst size and refill per second.
const RATE_BURST: f64 = 40.0;
const RATE_PER_SEC: f64 = 15.0;
/// Clients tracked by the limiter at once.
const RATE_MAX_KEYS: usize = 512;

pub const RATE_LIMITED: &str = "too many folder requests; wait a moment";

/// Folders that are never listed, even with `show_hidden`.
const SYSTEM_FOLDERS: &[&str] = &["$recycle.bin", "system volume information"];
/// Folders skipped unless `show_hidden`.
const NOISE_FOLDERS: &[&str] = &["node_modules", "target"];

/// Where agents keep their transcripts. `None` turns a source off.
#[derive(Debug, Clone, Default)]
pub struct TranscriptSources {
    /// Claude Code's `projects` folder (`~/.claude/projects`).
    pub claude_projects: Option<PathBuf>,
    /// Codex's `sessions` folder (`~/.codex/sessions`).
    pub codex_sessions: Option<PathBuf>,
}

impl TranscriptSources {
    /// The agents' default folders (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`).
    pub fn from_env() -> Self {
        Self {
            claude_projects: Some(vca_adapters::claude::default_config_dir().join("projects")),
            codex_sessions: Some(vca_adapters::codex::default_home().join("sessions")),
        }
    }
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

/// A project folder an agent worked in, with its last activity.
type Seen = (PathBuf, UnixMillis);

/// The folder picker of one server.
pub struct Folders {
    roots: WorkspaceRoots,
    sources: TranscriptSources,
    cache: Mutex<Option<(Instant, Vec<Seen>)>>,
    limiter: Mutex<HashMap<String, Bucket>>,
}

impl Folders {
    pub fn new(roots: WorkspaceRoots, sources: TranscriptSources) -> Self {
        Self {
            roots,
            sources,
            cache: Mutex::new(None),
            limiter: Mutex::new(HashMap::new()),
        }
    }

    /// Light per-client rate limit (a token bucket).
    pub fn check_rate(&self, key: &str) -> Result<(), Error> {
        let now = Instant::now();
        let mut buckets = lock(&self.limiter);
        if buckets.len() >= RATE_MAX_KEYS && !buckets.contains_key(key) {
            buckets.retain(|_, b| now.duration_since(b.at) < Duration::from_secs(10));
            if buckets.len() >= RATE_MAX_KEYS {
                buckets.clear();
            }
        }
        let bucket = buckets.entry(key.to_owned()).or_insert(Bucket {
            tokens: RATE_BURST,
            at: now,
        });
        let elapsed = now.duration_since(bucket.at).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * RATE_PER_SEC).min(RATE_BURST);
        bucket.at = now;
        if bucket.tokens < 1.0 {
            return Err(Error::api(ErrorCode::Unavailable, RATE_LIMITED));
        }
        bucket.tokens -= 1.0;
        Ok(())
    }

    /// The existing workspace roots as shown to users (no `\\?\`).
    fn display_roots(&self) -> Vec<PathBuf> {
        self.roots
            .roots()
            .iter()
            .map(|r| without_verbatim_prefix(r.clone()))
            .filter(|r| r.is_dir())
            .collect()
    }

    /// The innermost root containing the canonical `path`.
    fn root_of(&self, path: &Path) -> Option<PathBuf> {
        self.display_roots()
            .into_iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
    }

    /// Answer to `ProjectFolders`. Blocking (file system).
    pub fn project_folders(&self, sessions: &[SessionInfo]) -> ServerMsg {
        let roots = self
            .display_roots()
            .into_iter()
            .filter_map(|r| entry(&r, true, false, None))
            .collect();
        let seen = self.seen(sessions);
        let mut recent = Vec::new();
        let mut keys = HashSet::new();
        for (path, at) in seen {
            if recent.len() >= MAX_RECENT {
                break;
            }
            let Ok(canonical) = self.roots.prepare(&path, false) else {
                continue;
            };
            if !keys.insert(path_key(&canonical)) {
                continue;
            }
            if let Some(e) = entry(&canonical, false, true, Some(at)) {
                recent.push(e);
            }
        }
        ServerMsg::ProjectFolders {
            roots,
            recent,
            os: HostOs::current(),
            separator: std::path::MAIN_SEPARATOR_STR.to_owned(),
        }
    }

    /// Project folders of sessions and transcripts, newest first (not yet
    /// filtered).
    fn seen(&self, sessions: &[SessionInfo]) -> Vec<Seen> {
        let mut seen: Vec<Seen> = sessions
            .iter()
            .filter_map(|s| match &s.origin {
                SessionOrigin::Local { project_dir, .. } if project_dir.is_absolute() => {
                    Some((project_dir.clone(), s.last_activity))
                }
                _ => None,
            })
            .collect();
        seen.extend(self.transcripts());
        seen.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
        seen
    }

    /// Transcript project folders, cached for [`TRANSCRIPT_CACHE`].
    fn transcripts(&self) -> Vec<Seen> {
        let mut cache = lock(&self.cache);
        if let Some((at, list)) = cache.as_ref() {
            if at.elapsed() < TRANSCRIPT_CACHE {
                return list.clone();
            }
        }
        let mut list = Vec::new();
        if let Some(dir) = &self.sources.claude_projects {
            list.extend(claude_projects(dir));
        }
        if let Some(dir) = &self.sources.codex_sessions {
            list.extend(codex_projects(dir));
        }
        *cache = Some((Instant::now(), list.clone()));
        list
    }

    /// Answer to `ListFolder`. Blocking (file system).
    pub fn list_folder(
        &self,
        path: &str,
        show_hidden: bool,
        sessions: &[SessionInfo],
    ) -> Result<ServerMsg, Error> {
        let dir = self.roots.prepare(Path::new(path), false)?;
        let Some(root) = self.root_of(&dir) else {
            return Err(outside(path));
        };
        let parent = if dir == root {
            None
        } else {
            dir.parent().and_then(path_string)
        };
        let mut breadcrumbs = vec![FolderCrumb {
            name: path_string(&root).unwrap_or_default(),
            path: path_string(&root).unwrap_or_default(),
        }];
        let mut walk = root.clone();
        if let Ok(rest) = dir.strip_prefix(&root) {
            for comp in rest.components() {
                walk.push(comp);
                breadcrumbs.push(FolderCrumb {
                    name: comp.as_os_str().to_string_lossy().into_owned(),
                    path: path_string(&walk).unwrap_or_default(),
                });
            }
        }

        let history = self.history_index(sessions);
        let mut entries = Vec::new();
        let mut more = false;
        let read = std::fs::read_dir(&dir)?;
        for (scanned, item) in read.enumerate() {
            if scanned >= MAX_SCAN {
                more = true;
                break;
            }
            let Ok(item) = item else { continue };
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if is_system_name(&name) || (!show_hidden && is_hidden_name(&name)) {
                continue;
            }
            let Ok(kind) = item.file_type() else { continue };
            let child = dir.join(&name);
            if kind.is_symlink() {
                // Links (and Windows junctions) are listed only when they
                // lead to a folder inside the roots.
                match self.roots.prepare(&child, false) {
                    Ok(target) if target.is_dir() => {}
                    _ => continue,
                }
            } else if !kind.is_dir() {
                continue;
            }
            if !show_hidden && has_hidden_attribute(&item) {
                continue;
            }
            if entries.len() >= MAX_ENTRIES {
                more = true;
                continue;
            }
            let has_history = history.contains(&child);
            if let Some(e) = entry(&child, false, has_history, None) {
                entries.push(e);
            }
        }
        entries.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(ServerMsg::Folder {
            path: path_string(&dir).unwrap_or_default(),
            name: folder_name(&dir),
            parent,
            root: path_string(&root).unwrap_or_default(),
            breadcrumbs,
            entries,
            truncated: more,
        })
    }

    /// Folders agents worked in, for `has_agent_history`.
    fn history_index(&self, sessions: &[SessionInfo]) -> HistoryIndex {
        let paths = self
            .seen(sessions)
            .into_iter()
            .map(|(p, _)| path_key(&p))
            .collect();
        let claude = self
            .sources
            .claude_projects
            .as_deref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().to_str().map(fold_case))
                    .collect()
            })
            .unwrap_or_default();
        HistoryIndex { paths, claude }
    }

    /// Answer to `FolderPath`: where `name` inside `parent` would be.
    pub fn folder_path(&self, parent: &str, name: &str) -> Result<ServerMsg, Error> {
        let name = validate_folder_name(name)?;
        let parent = self.roots.prepare(Path::new(parent), false)?;
        let candidate = parent.join(&name);
        let (path, exists) = match self.roots.prepare(&candidate, false) {
            Ok(path) => (path, true),
            Err(err) if err.code() == ErrorCode::NotFound => {
                (self.roots.resolved(&candidate), false)
            }
            Err(err) => return Err(err),
        };
        let Some(path) = path_string(&path) else {
            return Err(Error::api(
                ErrorCode::InvalidRequest,
                "the folder name is not valid text",
            ));
        };
        Ok(ServerMsg::FolderPath { path, exists })
    }
}

struct HistoryIndex {
    /// [`path_key`]s of folders agents worked in.
    paths: HashSet<String>,
    /// Claude Code's project folder names (case-folded).
    claude: HashSet<String>,
}

impl HistoryIndex {
    fn contains(&self, path: &Path) -> bool {
        self.paths.contains(&path_key(path))
            || self.claude.contains(&fold_case(
                &vca_adapters::claude::transcript::encode_project_dir(path),
            ))
    }
}

fn outside(path: &str) -> Error {
    Error::api(
        ErrorCode::Forbidden,
        format!("{path} is outside the allowed workspace roots"),
    )
}

/// The path as text; `None` when it is not valid Unicode.
fn path_string(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

/// The last component, or the whole path for a drive or file system root.
fn folder_name(path: &Path) -> String {
    match path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

/// A comparison key: case-insensitive where the file system usually is.
fn path_key(path: &Path) -> String {
    let text = without_verbatim_prefix(path.to_owned())
        .to_string_lossy()
        .into_owned();
    let text = text.trim_end_matches(['/', '\\']).to_owned();
    fold_case(&text)
}

fn fold_case(text: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        text.to_lowercase()
    } else {
        text.to_owned()
    }
}

fn entry(
    path: &Path,
    full_name: bool,
    has_agent_history: bool,
    last_used: Option<UnixMillis>,
) -> Option<FolderEntry> {
    Some(FolderEntry {
        name: if full_name {
            path_string(path)?
        } else {
            folder_name(path)
        },
        path: path_string(path)?,
        is_git_repo: path.join(".git").exists(),
        has_agent_history,
        last_used,
    })
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') || NOISE_FOLDERS.contains(&name)
}

fn is_system_name(name: &str) -> bool {
    SYSTEM_FOLDERS.contains(&name.to_lowercase().as_str())
}

#[cfg(windows)]
fn has_hidden_attribute(item: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    item.metadata()
        .is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0)
}

#[cfg(not(windows))]
fn has_hidden_attribute(_item: &std::fs::DirEntry) -> bool {
    false
}

/// Checks a new folder name: one plain path component that every common
/// file system accepts.
fn validate_folder_name(name: &str) -> Result<String, Error> {
    let invalid = |why: &str| Err(Error::api(ErrorCode::InvalidRequest, why.to_owned()));
    let name = name.trim();
    if name.is_empty() {
        return invalid("the folder name is empty");
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return invalid("the folder name is too long");
    }
    if name == "." || name == ".." {
        return invalid("the folder name cannot be . or ..");
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return invalid("the folder name cannot contain slashes or control characters");
    }
    if cfg!(windows) {
        if name
            .chars()
            .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return invalid("the folder name cannot contain < > : \" | ? *");
        }
        if name.ends_with('.') {
            return invalid("the folder name cannot end with a dot");
        }
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            return invalid("the folder name is reserved on Windows");
        }
    }
    let mut comps = Path::new(name).components();
    if !matches!(
        (comps.next(), comps.next()),
        (Some(Component::Normal(_)), None)
    ) {
        return invalid("the folder name must be a single folder");
    }
    Ok(name.to_owned())
}

fn mtime_millis(path: &Path) -> Option<UnixMillis> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    millis(modified)
}

fn millis(t: SystemTime) -> Option<UnixMillis> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

/// Claude Code: one folder per project, named after the encoded path; the
/// newest transcript's recorded `cwd` is the project folder.
fn claude_projects(projects: &Path) -> Vec<Seen> {
    let Ok(read) = std::fs::read_dir(projects) else {
        return Vec::new();
    };
    let mut found: Vec<(PathBuf, PathBuf, UnixMillis)> = Vec::new();
    for project in read.filter_map(|e| e.ok()) {
        let dir = project.path();
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        let newest = files
            .filter_map(|e| e.ok())
            .filter(|f| {
                f.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".jsonl") && !n.starts_with("agent-"))
            })
            .filter_map(|f| {
                let at = f.metadata().ok()?.modified().ok().and_then(millis)?;
                Some((f.path(), at))
            })
            .max_by_key(|(_, at)| *at);
        if let Some((file, at)) = newest {
            found.push((dir, file, at));
        }
    }
    found.sort_by_key(|(_, _, at)| std::cmp::Reverse(*at));
    found
        .into_iter()
        .take(MAX_TRANSCRIPT_PROJECTS)
        .filter_map(|(dir, file, at)| {
            let cwd = transcript_cwd(&file).or_else(|| {
                let name = dir.file_name()?.to_str()?;
                Some(vca_adapters::claude::transcript::decode_project_dir(name))
            })?;
            Some((cwd, at))
        })
        .collect()
}

/// The first `cwd` recorded in the head of a Claude Code transcript.
fn transcript_cwd(file: &Path) -> Option<PathBuf> {
    use std::io::{BufRead, BufReader, Read};
    let reader = BufReader::new(std::fs::File::open(file).ok()?.take(64 * 1024));
    let mut meta = vca_adapters::claude::transcript::TranscriptMeta::default();
    for line in reader.lines().take(50) {
        let Ok(line) = line else { break };
        meta.observe_line(&line);
        if meta.cwd.is_some() {
            break;
        }
    }
    meta.cwd
}

/// Codex: `sessions/YYYY/MM/DD/rollout-*.jsonl`, newest folders first.
fn codex_projects(sessions: &Path) -> Vec<Seen> {
    let mut files = Vec::new();
    collect_rollouts(sessions, 0, &mut files);
    files.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
    let mut out: Vec<Seen> = Vec::new();
    let mut keys = HashSet::new();
    for (file, at) in files {
        if out.len() >= MAX_TRANSCRIPT_PROJECTS {
            break;
        }
        let Some(cwd) = vca_adapters::codex::rollout::read_meta(&file)
            .ok()
            .and_then(|m| m.cwd)
        else {
            continue;
        };
        if keys.insert(path_key(&cwd)) {
            out.push((cwd, at));
        }
    }
    out
}

fn collect_rollouts(dir: &Path, depth: usize, out: &mut Vec<Seen>) {
    if depth > 4 || out.len() >= MAX_ROLLOUTS {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = read.filter_map(|e| e.ok()).collect();
    // Date folders sort by name; newest first.
    items.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    for item in items {
        if out.len() >= MAX_ROLLOUTS {
            return;
        }
        let path = item.path();
        let Ok(kind) = item.file_type() else { continue };
        if kind.is_dir() {
            collect_rollouts(&path, depth + 1, out);
        } else if item
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            if let Some(at) = mtime_millis(&path) {
                out.push((path, at));
            }
        }
    }
}

#[cfg(test)]
mod tests;
