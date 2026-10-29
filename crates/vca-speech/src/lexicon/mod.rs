//! The developer vocabulary engine (requirements §3.3.1).
//!
//! Three layers of terms feed two outputs:
//!
//! - **Layer 1, base** ([`Layer::Base`]): curated TOML files under
//!   `lexicon/<lang>/*.toml` (see `lexicon/README.md`), loaded with
//!   [`Lexicon::load_dir`].
//! - **Layer 2, project** ([`Layer::Project`]): extracted from the active
//!   project directory by [`project::extract`].
//! - **Layer 3, personal** ([`Layer::Personal`]): user terms plus mappings
//!   learned from transcript edits ([`personal::PersonalLexicon`]).
//!
//! [`VocabularyEngine::hint_prompt`] builds the vocabulary hint for STT
//! engines; [`VocabularyEngine::correct`] fixes a transcript after
//! recognition and reports every change for UI highlighting.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SpeechError};

mod matcher;
pub mod personal;
pub mod project;

pub use matcher::{Conflict, ConflictKind, Correction, CorrectionKind, CorrectionResult};
pub use personal::{LearnedMapping, PersonalLexicon};
pub use project::{ProjectLexicon, ProjectScanConfig};

use matcher::CompiledIndex;

/// Lexicon layer. Ordering follows priority: `Personal > Project > Base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Base,
    Project,
    Personal,
}

impl Layer {
    /// Ranking bonus of the layer when resolving conflicts.
    pub(crate) fn priority(self) -> f32 {
        match self {
            Self::Base => 0.0,
            Self::Project => 10.0,
            Self::Personal => 20.0,
        }
    }
}

/// A spoken or misrecognized form of a term.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Variant {
    pub text: String,
    /// Language the variant is spoken in (`None` = any).
    #[serde(default)]
    pub language: Option<String>,
}

impl Variant {
    pub fn new(text: impl Into<String>, language: Option<&str>) -> Self {
        Self {
            text: text.into(),
            language: language.map(str::to_owned),
        }
    }
}

/// One vocabulary term with its spoken variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TermEntry {
    /// Canonical spelling, e.g. `useEffect`.
    pub term: String,
    #[serde(default)]
    pub variants: Vec<Variant>,
    #[serde(default)]
    pub category: Option<String>,
    /// Ecosystem or topic tags used for context ranking (`rust`, `react`).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Relative importance; `1.0` is normal.
    #[serde(default = "default_weight")]
    pub weight: f32,
    /// Whether the term's own spelling, matched case-insensitively, is
    /// rewritten to the canonical casing (`github` → `GitHub`). `None`
    /// enables it only for distinctive spellings (inner capitals, digits or
    /// punctuation), so plain words such as `Go` never rewrite `go`.
    #[serde(default)]
    pub match_term: Option<bool>,
}

fn default_weight() -> f32 {
    1.0
}

impl TermEntry {
    pub fn new(term: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            variants: Vec::new(),
            category: None,
            tags: Vec::new(),
            weight: 1.0,
            match_term: None,
        }
    }

    /// Adds a variant (builder style).
    pub fn variant(mut self, text: impl Into<String>, language: Option<&str>) -> Self {
        self.variants.push(Variant::new(text, language));
        self
    }

    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// Whether the canonical spelling itself is used as a match key.
    pub fn matches_own_spelling(&self) -> bool {
        self.match_term
            .unwrap_or_else(|| is_distinctive(&self.term))
    }
}

/// True for spellings that are unlikely to be ordinary words: inner capitals
/// (`useEffect`, `GitHub`), digits (`k8s`) or inner punctuation (`Node.js`).
pub fn is_distinctive(term: &str) -> bool {
    let chars: Vec<char> = term.chars().collect();
    if chars.len() < 3 {
        return false;
    }
    let inner_upper =
        chars.iter().skip(1).any(|c| c.is_uppercase()) && chars.iter().any(|c| c.is_lowercase());
    let digit = chars.iter().any(|c| c.is_ascii_digit()) && chars.iter().any(|c| c.is_alphabetic());
    let punct = chars[1..chars.len() - 1]
        .iter()
        .any(|c| matches!(c, '.' | '-' | '_' | '/' | '+' | '#'));
    inner_upper || digit || punct
}

/// A set of terms belonging to one layer. Entries with the same canonical
/// term are merged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lexicon {
    pub layer: Layer,
    entries: Vec<TermEntry>,
    #[serde(skip)]
    by_term: HashMap<String, usize>,
}

impl PartialEq for Lexicon {
    fn eq(&self, other: &Self) -> bool {
        self.layer == other.layer && self.entries == other.entries
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LexiconFile {
    language: Option<String>,
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default, rename = "entry")]
    entries: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    term: String,
    #[serde(default)]
    variants: Vec<String>,
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    weight: Option<f32>,
    match_term: Option<bool>,
}

impl Lexicon {
    pub fn new(layer: Layer) -> Self {
        Self {
            layer,
            entries: Vec::new(),
            by_term: HashMap::new(),
        }
    }

    /// Builds a lexicon from entries.
    pub fn from_entries(layer: Layer, entries: impl IntoIterator<Item = TermEntry>) -> Self {
        let mut lex = Self::new(layer);
        for e in entries {
            lex.add(e);
        }
        lex
    }

    pub fn entries(&self) -> &[TermEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up an entry by its exact canonical term.
    pub fn get(&self, term: &str) -> Option<&TermEntry> {
        self.index_of(term).map(|i| &self.entries[i])
    }

    fn index_of(&self, term: &str) -> Option<usize> {
        if self.by_term.len() == self.entries.len() {
            self.by_term.get(term).copied()
        } else {
            self.entries.iter().position(|e| e.term == term)
        }
    }

    /// Adds an entry, merging variants, tags and the higher weight into an
    /// existing entry with the same term.
    pub fn add(&mut self, entry: TermEntry) {
        if self.by_term.len() != self.entries.len() {
            self.reindex();
        }
        if entry.term.trim().is_empty() {
            return;
        }
        match self.by_term.get(&entry.term) {
            Some(&i) => {
                let existing = &mut self.entries[i];
                for v in entry.variants {
                    if !existing.variants.contains(&v) {
                        existing.variants.push(v);
                    }
                }
                for t in entry.tags {
                    if !existing.tags.contains(&t) {
                        existing.tags.push(t);
                    }
                }
                existing.weight = existing.weight.max(entry.weight);
                if existing.category.is_none() {
                    existing.category = entry.category;
                }
                if entry.match_term.is_some() {
                    existing.match_term = entry.match_term;
                }
            }
            None => {
                self.by_term.insert(entry.term.clone(), self.entries.len());
                self.entries.push(entry);
            }
        }
    }

    /// Removes an entry by term. Returns true if it existed.
    pub fn remove(&mut self, term: &str) -> bool {
        match self.index_of(term) {
            Some(i) => {
                self.entries.remove(i);
                self.reindex();
                true
            }
            None => false,
        }
    }

    fn reindex(&mut self) {
        self.by_term = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.term.clone(), i))
            .collect();
    }

    /// Merges all entries of `other` into this lexicon.
    pub fn extend(&mut self, other: Lexicon) {
        for e in other.entries {
            self.add(e);
        }
    }

    /// Parses one lexicon TOML document. `default_language` is used when the
    /// file has no `language` key (normally the directory name).
    pub fn parse_toml(
        &mut self,
        source: &str,
        default_language: Option<&str>,
        origin: &str,
    ) -> Result<usize> {
        let file: LexiconFile = toml::from_str(source).map_err(|e| SpeechError::Parse {
            path: origin.to_owned(),
            message: e.to_string(),
        })?;
        let language = file.language.as_deref().or(default_language);
        let count = file.entries.len();
        for e in file.entries {
            let mut tags = file.tags.clone();
            for t in e.tags {
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
            self.add(TermEntry {
                variants: e
                    .variants
                    .into_iter()
                    .map(|v| Variant::new(v, language))
                    .collect(),
                category: e.category.or_else(|| file.category.clone()),
                tags,
                weight: e.weight.unwrap_or(1.0),
                match_term: e.match_term,
                term: e.term,
            });
        }
        Ok(count)
    }

    /// The base lexicon shipped with VCA (the repository's `lexicon/`
    /// directory, embedded at build time). With `languages`, only those
    /// language directories are used.
    pub fn builtin(languages: Option<&[&str]>) -> Result<Lexicon> {
        const FILES: &[(&str, &str, &str)] =
            include!(concat!(env!("OUT_DIR"), "/builtin_lexicon.rs"));
        let mut lex = Lexicon::new(Layer::Base);
        for (lang, name, source) in FILES {
            if let Some(filter) = languages
                && !filter.iter().any(|l| l.eq_ignore_ascii_case(lang))
            {
                continue;
            }
            lex.parse_toml(source, Some(lang), &format!("builtin:{lang}/{name}"))?;
        }
        Ok(lex)
    }

    /// Loads a base lexicon from `root/<lang>/*.toml`. With `languages`,
    /// only those language directories are read; English is always worth
    /// including because technical terms are mostly English.
    pub fn load_dir(root: &Path, languages: Option<&[&str]>) -> Result<Lexicon> {
        let mut lex = Lexicon::new(Layer::Base);
        let mut dirs: Vec<_> = std::fs::read_dir(root)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        dirs.sort_by_key(|e| e.file_name());
        for dir in dirs {
            let lang = dir.file_name().to_string_lossy().into_owned();
            if let Some(filter) = languages
                && !filter.iter().any(|l| l.eq_ignore_ascii_case(&lang))
            {
                continue;
            }
            let mut files: Vec<_> = std::fs::read_dir(dir.path())?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            files.sort();
            for f in files {
                let source = std::fs::read_to_string(&f)?;
                lex.parse_toml(&source, Some(&lang), &f.display().to_string())?;
            }
        }
        Ok(lex)
    }
}

/// Context used to rank terms for hints and to break ties between
/// homophones.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VocabContext {
    /// Language of the transcript (BCP-47), if known.
    pub language: Option<String>,
    /// Active ecosystems or topics (`rust`, `react`, `kubernetes`), usually
    /// from [`ProjectLexicon::ecosystems`]. Terms tagged with them rank higher.
    #[serde(default)]
    pub topics: Vec<String>,
}

/// Thresholds of the post-recognition correction pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionConfig {
    /// Enable fuzzy (approximate) matching in addition to exact variants.
    pub fuzzy: bool,
    /// Minimum normalized Levenshtein similarity for a fuzzy match.
    pub min_similarity: f64,
    /// Minimum Jaro-Winkler similarity for a fuzzy match.
    pub min_jaro_winkler: f64,
    /// Minimum length (chars, without spaces) of fuzzy candidates.
    pub min_fuzzy_len: usize,
    /// Maximum number of words combined into one fuzzy candidate.
    pub max_fuzzy_words: usize,
}

impl Default for CorrectionConfig {
    fn default() -> Self {
        Self {
            fuzzy: true,
            min_similarity: 0.87,
            min_jaro_winkler: 0.92,
            min_fuzzy_len: 5,
            max_fuzzy_words: 3,
        }
    }
}

/// The three-layer vocabulary engine.
#[derive(Debug, Clone)]
pub struct VocabularyEngine {
    base: Lexicon,
    project: Lexicon,
    personal: PersonalLexicon,
    context: VocabContext,
    config: CorrectionConfig,
    index: CompiledIndex,
}

impl Default for VocabularyEngine {
    fn default() -> Self {
        Self::new(Lexicon::new(Layer::Base))
    }
}

impl VocabularyEngine {
    /// Creates an engine with a base lexicon and empty project and personal
    /// layers.
    pub fn new(base: Lexicon) -> Self {
        let mut engine = Self {
            base: Lexicon {
                layer: Layer::Base,
                ..base
            },
            project: Lexicon::new(Layer::Project),
            personal: PersonalLexicon::default(),
            context: VocabContext::default(),
            config: CorrectionConfig::default(),
            index: CompiledIndex::default(),
        };
        engine.rebuild();
        engine
    }

    /// Loads the base lexicon from a directory (see [`Lexicon::load_dir`]).
    pub fn from_dir(root: &Path, languages: Option<&[&str]>) -> Result<Self> {
        Ok(Self::new(Lexicon::load_dir(root, languages)?))
    }

    pub fn base(&self) -> &Lexicon {
        &self.base
    }

    pub fn project(&self) -> &Lexicon {
        &self.project
    }

    pub fn personal(&self) -> &PersonalLexicon {
        &self.personal
    }

    pub fn context(&self) -> &VocabContext {
        &self.context
    }

    pub fn config(&self) -> &CorrectionConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: CorrectionConfig) {
        self.config = config;
    }

    /// Replaces the base layer.
    pub fn set_base(&mut self, base: Lexicon) {
        self.base = Lexicon {
            layer: Layer::Base,
            ..base
        };
        self.rebuild();
    }

    /// Replaces the project layer (for example when the active session
    /// changes) and adopts its ecosystems as context topics.
    pub fn set_project(&mut self, project: ProjectLexicon) {
        self.context.topics = project.ecosystems.clone();
        self.project = Lexicon {
            layer: Layer::Project,
            ..project.lexicon
        };
        self.rebuild();
    }

    /// Replaces the personal layer (for example after loading it from disk).
    pub fn set_personal(&mut self, personal: PersonalLexicon) {
        self.personal = personal;
        self.rebuild();
    }

    /// Sets the ranking context.
    pub fn set_context(&mut self, context: VocabContext) {
        self.context = context;
    }

    /// Adds a user-defined term to the personal layer.
    pub fn add_personal_term(&mut self, entry: TermEntry) {
        self.personal.add_term(entry);
        self.rebuild();
    }

    /// Learns from a user's edit of a transcript: `original` is the text the
    /// user was shown (after correction), `edited` what they sent. Returns
    /// the mappings observed; the index is rebuilt when one is promoted.
    pub fn learn_from_edit(&mut self, original: &str, edited: &str) -> Vec<LearnedMapping> {
        let known = |s: &str| -> Option<String> { self.index.canonical_for_variant(s) };
        let learned = self.personal.learn_with(original, edited, known);
        if learned.iter().any(|m| m.promoted) {
            self.rebuild();
        }
        learned
    }

    fn rebuild(&mut self) {
        self.index = CompiledIndex::build(
            [&self.base, &self.project, self.personal.lexicon()],
            self.personal.suppressions(),
        );
    }

    /// Builds a comma-separated vocabulary hint of at most `max_chars`
    /// characters, most relevant terms first.
    pub fn hint_prompt(&self, max_chars: usize) -> String {
        let mut out = String::new();
        for term in self.index.ranked_terms(&self.context) {
            let extra = if out.is_empty() { 0 } else { 2 };
            if out.chars().count() + extra + term.chars().count() > max_chars {
                continue;
            }
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(term);
        }
        out
    }

    /// The `limit` most relevant terms, for engines that take phrase lists.
    pub fn hint_phrases(&self, limit: usize) -> Vec<String> {
        self.index
            .ranked_terms(&self.context)
            .take(limit)
            .map(str::to_owned)
            .collect()
    }

    /// Corrects a transcript using the engine's context.
    pub fn correct(&self, transcript: &str) -> CorrectionResult {
        self.index.correct(transcript, &self.context, &self.config)
    }

    /// Corrects a transcript with an explicit context (for example the
    /// language detected by the STT engine).
    pub fn correct_with(&self, transcript: &str, context: &VocabContext) -> CorrectionResult {
        self.index.correct(transcript, context, &self.config)
    }

    /// Homophones and near-duplicate terms across layers, with the term that
    /// wins under the current context.
    pub fn conflicts(&self) -> Vec<Conflict> {
        self.index.conflicts(&self.context)
    }
}

#[cfg(test)]
mod tests;
