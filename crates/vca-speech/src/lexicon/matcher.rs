//! Compiled lookup structures for the vocabulary engine: exact phrase
//! matching, fuzzy matching, ranking and conflict detection.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use strsim::{jaro_winkler, normalized_levenshtein};

use super::{CorrectionConfig, Layer, Lexicon, VocabContext};
use crate::stt::base_language;
use crate::text::{CharClass, NormalizedText, is_on_boundary, normalize, tokenize};

/// How a correction was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionKind {
    /// A listed spoken variant (or the term's own spelling) matched exactly,
    /// ignoring case and width.
    Exact,
    /// An approximate match above the configured similarity thresholds.
    Fuzzy,
}

/// One change applied by [`super::VocabularyEngine::correct`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    /// Byte range of the replaced text in the input transcript.
    pub start: usize,
    pub end: usize,
    /// Byte range of the replacement in the corrected text (for highlighting).
    pub corrected_start: usize,
    pub corrected_end: usize,
    /// The text that was replaced.
    pub original: String,
    /// The canonical term inserted.
    pub replacement: String,
    pub layer: Layer,
    pub kind: CorrectionKind,
    /// Match quality in `[0, 1]` (1 for exact matches).
    pub score: f32,
    /// Other terms that matched the same text but ranked lower.
    #[serde(default)]
    pub alternatives: Vec<String>,
}

/// Output of the correction pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionResult {
    pub text: String,
    pub corrections: Vec<Correction>,
}

/// Kind of a vocabulary conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// The same spoken form maps to different terms.
    Homophone,
    /// Two terms are spelled almost the same (`react_query` / `React Query`).
    NearDuplicate,
}

/// A conflict between terms, with the winner under the current context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub kind: ConflictKind,
    /// The shared spoken form (homophones) or the compact spelling.
    pub key: String,
    /// Competing terms and their layers, best first.
    pub candidates: Vec<(String, Layer)>,
    pub winner: String,
}

#[derive(Debug, Clone)]
struct IndexedTerm {
    term: String,
    layer: Layer,
    weight: f32,
    category: Option<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone)]
struct KeyCand {
    term: usize,
    language: Option<String>,
}

#[derive(Debug, Clone)]
struct Key {
    text: String,
    cands: Vec<KeyCand>,
}

#[derive(Debug, Clone)]
struct FuzzyKey {
    compact: String,
    len: usize,
    /// Number of words in the form the key was made from.
    words: usize,
    term: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CompiledIndex {
    terms: Vec<IndexedTerm>,
    keys: Vec<Key>,
    key_lookup: HashMap<String, usize>,
    by_first_char: HashMap<char, Vec<usize>>,
    fuzzy_by_len: HashMap<usize, Vec<FuzzyKey>>,
}

/// Lowercase alphanumerics only: `Node.js` → `nodejs`, `use effect` → `useeffect`.
fn compact(s: &str) -> String {
    normalize(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn is_latin_like(s: &str) -> bool {
    s.chars().all(|c| {
        matches!(
            CharClass::of(c),
            CharClass::Word | CharClass::Space | CharClass::Other
        )
    })
}

impl CompiledIndex {
    pub(crate) fn build<'a>(
        layers: impl IntoIterator<Item = &'a Lexicon>,
        suppressions: &[(String, String)],
    ) -> Self {
        let suppressed: HashSet<(String, &str)> = suppressions
            .iter()
            .map(|(k, t)| (normalize(k), t.as_str()))
            .collect();
        let mut idx = Self::default();
        let mut fuzzy_seen: HashSet<(String, usize)> = HashSet::new();
        for lex in layers {
            for entry in lex.entries() {
                let term_id = idx.terms.len();
                idx.terms.push(IndexedTerm {
                    term: entry.term.clone(),
                    layer: lex.layer,
                    weight: entry.weight,
                    category: entry.category.clone(),
                    tags: entry.tags.clone(),
                });
                let mut forms: Vec<(String, Option<String>)> = entry
                    .variants
                    .iter()
                    .map(|v| (normalize(&v.text), v.language.clone()))
                    .collect();
                if entry.matches_own_spelling() {
                    forms.push((normalize(&entry.term), None));
                }
                for (key, language) in forms {
                    if key.is_empty() || suppressed.contains(&(key.clone(), entry.term.as_str())) {
                        continue;
                    }
                    idx.add_key(
                        key.clone(),
                        KeyCand {
                            term: term_id,
                            language,
                        },
                    );
                    if is_latin_like(&key) {
                        let words = key.split_whitespace().count();
                        idx.add_fuzzy(&mut fuzzy_seen, compact(&key), words, term_id);
                    }
                }
                // Lowercase CLI-style names (`kubectl`) are safe fuzzy targets;
                // capitalized plain words (`React`) are not, because the
                // ordinary word ("react") would be rewritten.
                if is_latin_like(&entry.term) && !entry.term.chars().any(char::is_uppercase) {
                    idx.add_fuzzy(&mut fuzzy_seen, compact(&entry.term), 1, term_id);
                }
            }
        }
        idx
    }

    fn add_key(&mut self, text: String, cand: KeyCand) {
        let id = match self.key_lookup.get(&text) {
            Some(&id) => id,
            None => {
                let id = self.keys.len();
                if let Some(c) = text.chars().next() {
                    self.by_first_char.entry(c).or_default().push(id);
                }
                self.key_lookup.insert(text.clone(), id);
                self.keys.push(Key {
                    text,
                    cands: Vec::new(),
                });
                id
            }
        };
        let key = &mut self.keys[id];
        if !key.cands.iter().any(|c| c.term == cand.term) {
            key.cands.push(cand);
        }
    }

    fn add_fuzzy(
        &mut self,
        seen: &mut HashSet<(String, usize)>,
        compact: String,
        words: usize,
        term: usize,
    ) {
        let len = compact.chars().count();
        if len < 4 || !seen.insert((compact.clone(), term)) {
            return;
        }
        self.fuzzy_by_len.entry(len).or_default().push(FuzzyKey {
            compact,
            len,
            words,
            term,
        });
    }

    fn score(&self, term: usize, language: Option<&str>, ctx: &VocabContext) -> f32 {
        let t = &self.terms[term];
        let mut score = t.layer.priority() + t.weight;
        if ctx.topics.iter().any(|topic| {
            t.tags.iter().any(|tag| tag.eq_ignore_ascii_case(topic))
                || t.category
                    .as_deref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(topic))
        }) {
            score += 3.0;
        }
        if let (Some(vl), Some(cl)) = (language, ctx.language.as_deref())
            && base_language(vl) == base_language(cl)
        {
            score += 0.5;
        }
        score
    }

    /// Candidates of a key, best first, deduplicated by term spelling.
    fn ranked_cands(&self, key: usize, ctx: &VocabContext) -> Vec<(usize, f32)> {
        let mut scored: Vec<(usize, f32)> = self.keys[key]
            .cands
            .iter()
            .map(|c| (c.term, self.score(c.term, c.language.as_deref(), ctx)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut seen = HashSet::new();
        scored.retain(|(t, _)| seen.insert(self.terms[*t].term.clone()));
        scored
    }

    /// The best canonical term for a spoken form, ignoring context.
    pub(crate) fn canonical_for_variant(&self, spoken: &str) -> Option<String> {
        let key = *self.key_lookup.get(&normalize(spoken))?;
        self.ranked_cands(key, &VocabContext::default())
            .first()
            .map(|(t, _)| self.terms[*t].term.clone())
    }

    /// All distinct terms, most relevant first.
    pub(crate) fn ranked_terms<'a>(
        &'a self,
        ctx: &VocabContext,
    ) -> impl Iterator<Item = &'a str> + 'a {
        let mut order: Vec<(usize, f32)> = (0..self.terms.len())
            .map(|i| (i, self.score(i, None, ctx)))
            .collect();
        order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut seen = HashSet::new();
        order
            .into_iter()
            .filter(move |(i, _)| seen.insert(self.terms[*i].term.to_lowercase()))
            .map(move |(i, _)| self.terms[i].term.as_str())
    }

    pub(crate) fn correct(
        &self,
        input: &str,
        ctx: &VocabContext,
        config: &CorrectionConfig,
    ) -> CorrectionResult {
        let norm = NormalizedText::new(input);
        let text = norm.text.as_str();

        // 1. Exact phrase matches at every char position.
        let mut found: Vec<Match> = Vec::new();
        for (pos, c) in text.char_indices() {
            let Some(ids) = self.by_first_char.get(&c) else {
                continue;
            };
            for &id in ids {
                let key = &self.keys[id].text;
                if text[pos..].starts_with(key.as_str())
                    && is_on_boundary(text, pos, pos + key.len())
                {
                    let cands = self.ranked_cands(id, ctx);
                    if let Some(&(best, _)) = cands.first() {
                        found.push(Match {
                            start: pos,
                            end: pos + key.len(),
                            term: best,
                            kind: CorrectionKind::Exact,
                            score: 1.0,
                            alternatives: cands[1..].iter().map(|(t, _)| *t).collect(),
                        });
                    }
                }
            }
        }
        // Longest first, then earliest; keep non-overlapping.
        found.sort_by(|a, b| {
            (b.end - b.start)
                .cmp(&(a.end - a.start))
                .then(a.start.cmp(&b.start))
        });
        let mut accepted: Vec<Match> = Vec::new();
        for m in found {
            if !accepted.iter().any(|a| a.overlaps(&m)) {
                accepted.push(m);
            }
        }

        // 2. Fuzzy matches over word n-grams not covered by exact matches.
        if config.fuzzy {
            let fuzzy = self.fuzzy_matches(text, &accepted, ctx, config);
            for m in fuzzy {
                if !accepted.iter().any(|a| a.overlaps(&m)) {
                    accepted.push(m);
                }
            }
        }
        accepted.sort_by_key(|m| m.start);

        // 3. Apply to the original text.
        let mut out = String::with_capacity(input.len());
        let mut corrections = Vec::new();
        let mut cursor = 0;
        for m in accepted {
            let (start, end) = norm.original_range(m.start, m.end);
            if start < cursor || start >= end {
                continue;
            }
            let original = &input[start..end];
            let term = &self.terms[m.term];
            if original == term.term {
                continue;
            }
            out.push_str(&input[cursor..start]);
            let corrected_start = out.len();
            out.push_str(&term.term);
            corrections.push(Correction {
                start,
                end,
                corrected_start,
                corrected_end: out.len(),
                original: original.to_owned(),
                replacement: term.term.clone(),
                layer: term.layer,
                kind: m.kind,
                score: m.score,
                alternatives: m
                    .alternatives
                    .iter()
                    .map(|&t| self.terms[t].term.clone())
                    .collect(),
            });
            cursor = end;
        }
        out.push_str(&input[cursor..]);
        CorrectionResult {
            text: out,
            corrections,
        }
    }

    fn fuzzy_matches(
        &self,
        text: &str,
        exact: &[Match],
        ctx: &VocabContext,
        config: &CorrectionConfig,
    ) -> Vec<Match> {
        let tokens = tokenize(text);
        let mut out: Vec<Match> = Vec::new();
        for i in 0..tokens.len() {
            let mut compacted = String::new();
            for n in 0..config.max_fuzzy_words.max(1) {
                let Some(tok) = tokens.get(i + n) else { break };
                if tok.class != CharClass::Word
                    || exact
                        .iter()
                        .any(|m| tok.start < m.end && m.start < tok.end())
                {
                    break;
                }
                compacted.extend(tok.text.chars().filter(|c| c.is_alphanumeric()));
                let start = tokens[i].start;
                let end = tok.end();
                let len = compacted.chars().count();
                if len < config.min_fuzzy_len || compacted.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                // A single word that is itself a known spoken form or term
                // is left alone: it is a word we already know.
                if n == 0 && self.key_lookup.contains_key(&text[start..end]) {
                    break;
                }
                if let Some(m) = self.best_fuzzy(&compacted, len, n + 1, start, end, ctx, config) {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then((b.end - b.start).cmp(&(a.end - a.start)))
        });
        let mut accepted: Vec<Match> = Vec::new();
        for m in out {
            if !accepted.iter().any(|a| a.overlaps(&m)) {
                accepted.push(m);
            }
        }
        accepted
    }

    #[allow(clippy::too_many_arguments)]
    fn best_fuzzy(
        &self,
        compacted: &str,
        len: usize,
        words: usize,
        start: usize,
        end: usize,
        ctx: &VocabContext,
        config: &CorrectionConfig,
    ) -> Option<Match> {
        let lo = ((len as f64) * config.min_similarity).floor() as usize;
        let hi = ((len as f64) / config.min_similarity).ceil() as usize;
        let mut best: Option<(usize, f64, f32)> = None;
        for k in lo.max(config.min_fuzzy_len)..=hi {
            let Some(bucket) = self.fuzzy_by_len.get(&k) else {
                continue;
            };
            for fk in bucket {
                debug_assert_eq!(fk.len, k);
                // Several spoken words only match multi-word forms: "read me"
                // must not become `README` just because the letters agree.
                if words > 1 && fk.words < 2 {
                    continue;
                }
                let sim = normalized_levenshtein(compacted, &fk.compact);
                if sim < config.min_similarity {
                    continue;
                }
                let jw = jaro_winkler(compacted, &fk.compact);
                if jw < config.min_jaro_winkler {
                    continue;
                }
                let rank = self.score(fk.term, None, ctx);
                let better = match best {
                    None => true,
                    Some((_, bs, br)) => sim > bs || (sim == bs && rank > br),
                };
                if better {
                    best = Some((fk.term, sim, rank));
                }
            }
        }
        best.map(|(term, sim, _)| Match {
            start,
            end,
            term,
            kind: if sim >= 1.0 {
                CorrectionKind::Exact
            } else {
                CorrectionKind::Fuzzy
            },
            score: sim as f32,
            alternatives: Vec::new(),
        })
    }

    pub(crate) fn conflicts(&self, ctx: &VocabContext) -> Vec<Conflict> {
        let mut out = Vec::new();
        for (id, key) in self.keys.iter().enumerate() {
            let ranked = self.ranked_cands(id, ctx);
            if ranked.len() < 2 {
                continue;
            }
            out.push(Conflict {
                kind: ConflictKind::Homophone,
                key: key.text.clone(),
                winner: self.terms[ranked[0].0].term.clone(),
                candidates: ranked
                    .iter()
                    .map(|(t, _)| (self.terms[*t].term.clone(), self.terms[*t].layer))
                    .collect(),
            });
        }
        // Near-duplicates: compare project and personal terms with every
        // term of another layer (the base layer alone is curated).
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (i, a) in self.terms.iter().enumerate() {
            if a.layer == Layer::Base {
                continue;
            }
            let ca = compact(&a.term);
            if ca.chars().count() < 4 {
                continue;
            }
            for (j, b) in self.terms.iter().enumerate() {
                if i == j || a.layer == b.layer || a.term == b.term {
                    continue;
                }
                let cb = compact(&b.term);
                let near = ca == cb
                    || (ca.chars().count() >= 6 && normalized_levenshtein(&ca, &cb) >= 0.9);
                if !near {
                    continue;
                }
                let pair = if a.term < b.term {
                    (a.term.clone(), b.term.clone())
                } else {
                    (b.term.clone(), a.term.clone())
                };
                if !seen.insert(pair) {
                    continue;
                }
                let (sa, sb) = (self.score(i, None, ctx), self.score(j, None, ctx));
                let mut candidates = vec![(a.term.clone(), a.layer), (b.term.clone(), b.layer)];
                if sb > sa {
                    candidates.swap(0, 1);
                }
                out.push(Conflict {
                    kind: ConflictKind::NearDuplicate,
                    key: ca.clone(),
                    winner: candidates[0].0.clone(),
                    candidates,
                });
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
struct Match {
    start: usize,
    end: usize,
    term: usize,
    kind: CorrectionKind,
    score: f32,
    alternatives: Vec<usize>,
}

impl Match {
    fn overlaps(&self, other: &Match) -> bool {
        self.start < other.end && other.start < self.end
    }
}
