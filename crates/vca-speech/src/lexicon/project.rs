//! Layer 2: terms extracted from the active project directory.
//!
//! Sources, in order of weight:
//! - manifests: `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`
//!   (package names, workspace members, dependencies, npm scripts);
//! - git branch names (`.git/HEAD`, `.git/refs/heads`, `packed-refs`);
//! - top-level directory names and file stems;
//! - frequent compound identifiers in source files (bounded scan).
//!
//! Every term gets its spoken form as a variant when it is a compound
//! identifier (`useAuthToken` → "use auth token").

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Layer, Lexicon, TermEntry, Variant};
use crate::error::Result;
use crate::text::{split_identifier, spoken_form};

/// Limits of the project scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectScanConfig {
    /// Maximum source files read for identifiers.
    pub max_files: usize,
    /// Files larger than this are skipped.
    pub max_file_bytes: u64,
    /// Stop reading sources after this many bytes in total.
    pub max_total_bytes: u64,
    /// Maximum directory depth for the source scan.
    pub max_depth: usize,
    /// Number of identifiers kept.
    pub max_identifiers: usize,
    /// Minimum occurrences for an identifier to be kept.
    pub min_identifier_count: u32,
    /// Maximum number of git branches kept.
    pub max_branches: usize,
}

impl Default for ProjectScanConfig {
    fn default() -> Self {
        Self {
            max_files: 2_000,
            max_file_bytes: 256 * 1024,
            max_total_bytes: 16 * 1024 * 1024,
            max_depth: 8,
            max_identifiers: 300,
            min_identifier_count: 3,
            max_branches: 50,
        }
    }
}

/// Result of a project scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectLexicon {
    pub lexicon: Lexicon,
    /// Detected ecosystems (`rust`, `javascript`, `python`, `go`), used as
    /// ranking context for base-lexicon terms.
    pub ecosystems: Vec<String>,
    /// Current git branch, if any.
    pub current_branch: Option<String>,
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    ".idea",
    ".vscode",
    "coverage",
    "bin",
    "obj",
    "Pods",
    ".gradle",
];

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "kts", "swift", "c",
    "h", "cc", "cpp", "hpp", "cs", "rb", "php", "scala", "dart", "vue", "svelte", "lua", "ex",
    "exs", "zig", "sql", "sh",
];

/// Scans a project directory. Blocking; see [`extract_async`].
pub fn extract(root: &Path, config: &ProjectScanConfig) -> Result<ProjectLexicon> {
    let mut b = Builder::default();
    let mut ecosystems = Vec::new();

    if let Some(src) = read_small(&root.join("Cargo.toml")) {
        ecosystems.push("rust".to_owned());
        b.cargo(&src);
    }
    if let Some(src) = read_small(&root.join("package.json")) {
        ecosystems.push("javascript".to_owned());
        b.package_json(&src, &mut ecosystems);
    }
    if let Some(src) = read_small(&root.join("pyproject.toml")) {
        ecosystems.push("python".to_owned());
        b.pyproject(&src);
    } else if root.join("requirements.txt").is_file() || root.join("setup.py").is_file() {
        ecosystems.push("python".to_owned());
    }
    if let Some(src) = read_small(&root.join("go.mod")) {
        ecosystems.push("go".to_owned());
        b.go_mod(&src);
    }
    if root.join("Dockerfile").is_file()
        || root.join("compose.yaml").is_file()
        || root.join("docker-compose.yml").is_file()
    {
        ecosystems.push("docker".to_owned());
    }
    if root.join(".git").exists() {
        ecosystems.push("git".to_owned());
    }

    let (current_branch, branches) = git_branches(root, config.max_branches);
    for br in &branches {
        b.add(br, 2.0, "branch");
    }

    // Top-level entries.
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = e.path().is_dir();
            if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let stem = if is_dir {
                name
            } else {
                Path::new(&name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(name)
            };
            b.add(&stem, 1.5, if is_dir { "directory" } else { "file" });
        }
    }
    if let Some(dir_name) = root.file_name() {
        b.add(&dir_name.to_string_lossy(), 2.5, "project");
    }

    // Identifiers.
    let idents = scan_identifiers(root, config);
    let max_count = idents.first().map(|(_, c)| *c).unwrap_or(1).max(1) as f32;
    for (ident, count) in idents {
        let weight = 1.0 + (count as f32).ln() / max_count.ln().max(1.0);
        b.add(&ident, weight, "identifier");
    }

    ecosystems.dedup();
    Ok(ProjectLexicon {
        lexicon: b.finish(),
        ecosystems,
        current_branch,
    })
}

/// Async wrapper around [`extract`] that runs on the blocking pool.
pub async fn extract_async(root: PathBuf, config: ProjectScanConfig) -> Result<ProjectLexicon> {
    tokio::task::spawn_blocking(move || extract(&root, &config))
        .await
        .map_err(|e| crate::SpeechError::Engine(e.to_string()))?
}

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[derive(Default)]
struct Builder {
    entries: HashMap<String, TermEntry>,
}

impl Builder {
    fn add(&mut self, raw: &str, weight: f32, category: &str) {
        let term = raw.trim().trim_matches(|c: char| c == '"' || c == '\'');
        if term.chars().count() < 2 || term.len() > 80 || term.chars().all(|c| c.is_ascii_digit()) {
            return;
        }
        let entry = self
            .entries
            .entry(term.to_owned())
            .or_insert_with(|| TermEntry {
                category: Some(category.to_owned()),
                weight: 0.0,
                // Project names are distinctive in context: also fix casing.
                match_term: Some(term.chars().count() >= 4 && split_identifier(term).len() >= 2),
                ..TermEntry::new(term)
            });
        entry.weight = entry.weight.max(weight);
        // Spoken variants: for scoped npm packages and paths, the last
        // segment is what people say ("react query" for @tanstack/react-query).
        let last = term.rsplit('/').next().unwrap_or(term);
        for form in [spoken_form(term), spoken_form(last)].into_iter().flatten() {
            if form.chars().count() >= 5 && !entry.variants.iter().any(|v| v.text == form) {
                entry.variants.push(Variant::new(form, None));
            }
        }
    }

    fn cargo(&mut self, src: &str) {
        let Ok(doc) = src.parse::<toml::Table>() else {
            return;
        };
        if let Some(name) = doc
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        {
            self.add(name, 3.0, "package");
        }
        let ws = doc.get("workspace");
        if let Some(members) = ws.and_then(|w| w.get("members")).and_then(|m| m.as_array()) {
            for m in members.iter().filter_map(|m| m.as_str()) {
                if let Some(last) = m.rsplit('/').next().filter(|s| !s.contains('*')) {
                    self.add(last, 2.5, "package");
                }
            }
        }
        let tables = [
            doc.get("dependencies"),
            doc.get("dev-dependencies"),
            doc.get("build-dependencies"),
            ws.and_then(|w| w.get("dependencies")),
        ];
        for t in tables.into_iter().flatten() {
            if let Some(t) = t.as_table() {
                for k in t.keys() {
                    self.add(k, 1.8, "dependency");
                }
            }
        }
    }

    fn package_json(&mut self, src: &str, ecosystems: &mut Vec<String>) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(src) else {
            return;
        };
        if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
            self.add(name, 3.0, "package");
        }
        for key in ["dependencies", "devDependencies", "peerDependencies"] {
            if let Some(deps) = v.get(key).and_then(|d| d.as_object()) {
                for dep in deps.keys() {
                    if dep == "typescript" {
                        ecosystems.push("typescript".into());
                    }
                    if dep == "react" || dep == "next" {
                        ecosystems.push("react".into());
                    }
                    if dep == "vue" {
                        ecosystems.push("vue".into());
                    }
                    self.add(dep, 1.8, "dependency");
                }
            }
        }
        if let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) {
            for s in scripts.keys() {
                self.add(s, 1.5, "script");
            }
        }
    }

    fn pyproject(&mut self, src: &str) {
        let Ok(doc) = src.parse::<toml::Table>() else {
            return;
        };
        let project = doc.get("project");
        if let Some(name) = project.and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
            self.add(name, 3.0, "package");
        }
        if let Some(deps) = project
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_array())
        {
            for d in deps.iter().filter_map(|d| d.as_str()) {
                self.add(pep508_name(d), 1.8, "dependency");
            }
        }
        let poetry = doc.get("tool").and_then(|t| t.get("poetry"));
        if let Some(name) = poetry.and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
            self.add(name, 3.0, "package");
        }
        for key in ["dependencies", "dev-dependencies"] {
            if let Some(t) = poetry.and_then(|p| p.get(key)).and_then(|d| d.as_table()) {
                for k in t.keys().filter(|k| *k != "python") {
                    self.add(k, 1.8, "dependency");
                }
            }
        }
    }

    fn go_mod(&mut self, src: &str) {
        let mut in_require = false;
        for line in src.lines() {
            let line = line.split("//").next().unwrap_or("").trim();
            if let Some(module) = line.strip_prefix("module ") {
                if let Some(last) = module.trim().rsplit('/').next() {
                    self.add(last, 3.0, "package");
                }
                continue;
            }
            let dep = if line.starts_with("require (") {
                in_require = true;
                continue;
            } else if in_require && line == ")" {
                in_require = false;
                continue;
            } else if in_require {
                line
            } else if let Some(rest) = line.strip_prefix("require ") {
                rest
            } else {
                continue;
            };
            if let Some(path) = dep.split_whitespace().next() {
                let mut segs = path.rsplit('/');
                let mut last = segs.next().unwrap_or(path);
                // `github.com/foo/bar/v2` → `bar`
                if last.len() <= 3
                    && last.starts_with('v')
                    && last[1..].chars().all(|c| c.is_ascii_digit())
                {
                    last = segs.next().unwrap_or(last);
                }
                self.add(last, 1.8, "dependency");
            }
        }
    }

    fn finish(self) -> Lexicon {
        let mut entries: Vec<TermEntry> = self.entries.into_values().collect();
        entries.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(a.term.cmp(&b.term)));
        Lexicon::from_entries(Layer::Project, entries)
    }
}

/// The distribution name of a PEP 508 requirement (`requests>=2; python_version>"3"` → `requests`).
fn pep508_name(req: &str) -> &str {
    let end = req
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(req.len());
    &req[..end]
}

/// Resolves the git directory (handles worktrees where `.git` is a file).
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let content = std::fs::read_to_string(&dot).ok()?;
    let path = content.trim().strip_prefix("gitdir:")?.trim();
    let p = Path::new(path);
    Some(if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    })
}

/// Returns the current branch and all local branch names.
fn git_branches(root: &Path, max: usize) -> (Option<String>, Vec<String>) {
    let Some(git) = git_dir(root) else {
        return (None, Vec::new());
    };
    let current = std::fs::read_to_string(git.join("HEAD"))
        .ok()
        .and_then(|h| h.trim().strip_prefix("ref: refs/heads/").map(str::to_owned));
    let mut branches: Vec<String> = current.iter().cloned().collect();
    // Worktrees keep refs in the common dir.
    let common = std::fs::read_to_string(git.join("commondir"))
        .ok()
        .map(|c| git.join(c.trim()))
        .unwrap_or_else(|| git.clone());
    let heads = common.join("refs").join("heads");
    let mut stack = vec![(heads.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() && depth < 4 {
                stack.push((p, depth + 1));
            } else if let Ok(rel) = p.strip_prefix(&heads) {
                let name = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                branches.push(name);
            }
        }
    }
    if let Ok(packed) = std::fs::read_to_string(common.join("packed-refs")) {
        for line in packed.lines() {
            if let Some(name) = line
                .split_whitespace()
                .nth(1)
                .and_then(|r| r.strip_prefix("refs/heads/"))
            {
                branches.push(name.to_owned());
            }
        }
    }
    branches.sort();
    branches.dedup();
    // Keep the current branch first.
    if let Some(c) = &current {
        branches.retain(|b| b != c);
        branches.insert(0, c.clone());
    }
    branches.truncate(max);
    (current, branches)
}

/// True for identifiers worth adding: compound (camelCase, snake_case,
/// kebab) and not too short.
fn is_interesting_identifier(id: &str) -> bool {
    let len = id.chars().count();
    if !(5..=48).contains(&len) {
        return false;
    }
    // ALL_CAPS constants are rarely spoken.
    if !id.chars().any(|c| c.is_lowercase()) {
        return false;
    }
    split_identifier(id).len() >= 2
}

/// Counts identifiers in source files and returns the most frequent ones.
fn scan_identifiers(root: &Path, config: &ProjectScanConfig) -> Vec<(String, u32)> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut files = 0usize;
    let mut total = 0u64;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    'walk: while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut items: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let path = e.path();
            if ft.is_dir() {
                if depth < config.max_depth
                    && !name.starts_with('.')
                    && !SKIP_DIRS.contains(&name.as_ref())
                {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let ext_ok = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| SOURCE_EXTENSIONS.contains(&x));
            if !ext_ok {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.len() > config.max_file_bytes {
                continue;
            }
            if files >= config.max_files || total + meta.len() > config.max_total_bytes {
                break 'walk;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            files += 1;
            total += bytes.len() as u64;
            let text = String::from_utf8_lossy(&bytes);
            for ident in identifiers(&text) {
                if is_interesting_identifier(ident) {
                    *counts.entry(ident.to_owned()).or_insert(0) += 1;
                }
            }
        }
    }
    let mut v: Vec<(String, u32)> = counts
        .into_iter()
        .filter(|(_, c)| *c >= config.min_identifier_count)
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(config.max_identifiers);
    v
}

/// Cheap identifier tokenizer: ASCII `[A-Za-z_][A-Za-z0-9_]*`.
fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            let c = bytes[i];
            if c.is_ascii_alphabetic() || c == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                return Some(&text[start..i]);
            } else if c.is_ascii_digit() {
                // Skip numeric literals such as 0x1f or 10u32 entirely.
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn extracts_from_manifests_git_and_sources() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            r#"
[package]
name = "order-service"
[dependencies]
tokio = "1"
serde_json = "1"
[workspace]
members = ["crates/billing-core"]
"#,
        );
        write(
            root,
            "web/package.json",
            r#"{"name": "ignored-because-not-top-level"}"#,
        );
        write(
            root,
            "package.json",
            r#"{"name":"@acme/dashboard","dependencies":{"react":"18","@tanstack/react-query":"5"},"scripts":{"dev":"vite","test:e2e":"x"}}"#,
        );
        write(
            root,
            "pyproject.toml",
            "[project]\nname = \"ml-pipeline\"\ndependencies = [\"numpy>=1.26\", \"scikit-learn; python_version>'3.9'\"]\n",
        );
        write(
            root,
            "go.mod",
            "module github.com/acme/gateway\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.0\n\tgithub.com/redis/go-redis/v9 v9.0.0 // indirect\n)\n",
        );
        write(root, ".git/HEAD", "ref: refs/heads/feature/login-flow\n");
        write(root, ".git/refs/heads/main", "abc\n");
        write(root, ".git/refs/heads/feature/login-flow", "def\n");
        write(
            root,
            ".git/packed-refs",
            "# pack-refs\nabc refs/heads/release-2024\n",
        );
        let src = "fn parseOrderLine() {}\nlet x = parseOrderLine(); parseOrderLine();\nlet total_amount = 1; total_amount + total_amount;\nlet ok = 1; ok; ok;\n";
        write(root, "src/lib.rs", src);
        write(
            root,
            "node_modules/junk/index.js",
            "parseJunkThing parseJunkThing parseJunkThing",
        );

        let p = extract(root, &ProjectScanConfig::default()).unwrap();
        let lex = &p.lexicon;
        for t in [
            "order-service",
            "billing-core",
            "tokio",
            "serde_json",
            "@acme/dashboard",
            "@tanstack/react-query",
            "test:e2e",
            "ml-pipeline",
            "numpy",
            "scikit-learn",
            "gateway",
            "gin",
            "go-redis",
            "feature/login-flow",
            "main",
            "release-2024",
            "parseOrderLine",
            "total_amount",
            "src",
        ] {
            assert!(lex.get(t).is_some(), "missing {t}");
        }
        assert!(lex.get("parseJunkThing").is_none());
        assert!(lex.get("ok").is_none());
        assert_eq!(p.current_branch.as_deref(), Some("feature/login-flow"));
        assert!(p.ecosystems.contains(&"rust".to_owned()));
        assert!(p.ecosystems.contains(&"react".to_owned()));
        let rq = lex.get("@tanstack/react-query").unwrap();
        assert!(rq.variants.iter().any(|v| v.text == "react query"));
        let pol = lex.get("parseOrderLine").unwrap();
        assert!(pol.variants.iter().any(|v| v.text == "parse order line"));
    }

    #[test]
    fn identifier_tokenizer_skips_numbers() {
        let ids: Vec<_> = identifiers("x1 = 0x1f + foo_bar(10u32)").collect();
        assert_eq!(ids, vec!["x1", "foo_bar"]);
        assert_eq!(pep508_name("requests[socks]>=2"), "requests");
    }
}
