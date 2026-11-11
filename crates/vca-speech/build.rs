//! Embeds the repository's base lexicon (`lexicon/<lang>/*.toml`) so that
//! binaries work without a checkout. See `Lexicon::builtin`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let root = manifest.join("../../lexicon");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(dirs) = std::fs::read_dir(&root) {
        for dir in dirs.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
            println!("cargo:rerun-if-changed={}", dir.path().display());
            let lang = dir.file_name().to_string_lossy().into_owned();
            if let Ok(entries) = std::fs::read_dir(dir.path()) {
                for f in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
                    if f.extension().is_some_and(|x| x == "toml") {
                        files.push((lang.clone(), f));
                    }
                }
            }
        }
    }
    files.sort();

    let mut out = String::from("&[\n");
    for (lang, path) in &files {
        let path = canonical(path);
        writeln!(
            out,
            "    ({lang:?}, {name:?}, include_str!({path:?})),",
            name = path.file_name().unwrap_or_default().to_string_lossy(),
            path = path.display().to_string(),
        )
        .expect("write");
    }
    out.push_str("]\n");
    let dest =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("out dir")).join("builtin_lexicon.rs");
    std::fs::write(dest, out).expect("writing builtin_lexicon.rs");
}

fn canonical(path: &Path) -> PathBuf {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    // `include_str!` does not accept Windows verbatim paths.
    match p.to_str().and_then(|s| s.strip_prefix(r"\?\")) {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}
