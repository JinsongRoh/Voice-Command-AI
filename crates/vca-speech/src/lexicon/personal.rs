//! Layer 3: the personal lexicon (user terms and learned corrections).
//!
//! Learning works on pairs of (shown transcript, sent transcript). The two
//! texts are diffed word by word; each replaced span of at most a few words
//! is an *observation* that spoken form X should be written as term Y. After
//! `threshold` consistent observations (and more than for any competing term
//! of the same spoken form) the mapping is promoted into the lexicon.
//!
//! When the user reverts a correction (the shown text contained a term and
//! the user wrote back the spoken form), a *rejection* is counted instead;
//! after `threshold` rejections that mapping is suppressed.

use serde::{Deserialize, Serialize};

use super::{Layer, Lexicon, TermEntry, Variant};
use crate::text::{CharClass, Token, normalize, tokenize};

/// Maximum words on the spoken side of a learnable edit.
const MAX_SPOKEN_WORDS: usize = 4;
/// Maximum words on the written side of a learnable edit.
const MAX_TERM_WORDS: usize = 3;
/// Inputs longer than this (tokens) are not diffed.
const MAX_DIFF_TOKENS: usize = 1500;

/// One observed mapping from an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedMapping {
    /// Normalized spoken form.
    pub variant: String,
    /// Term as the user wrote it.
    pub term: String,
    /// Consistent observations so far.
    pub count: u32,
    /// True when this observation promoted the mapping into the lexicon (or,
    /// for rejections, suppressed it).
    pub promoted: bool,
    /// True when the user reverted a correction rather than making one.
    pub rejection: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Observation {
    variant: String,
    term: String,
    count: u32,
    rejection: bool,
}

/// User-defined terms plus learning state. Serializable so the server can
/// persist it per user (JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonalLexicon {
    lexicon: Lexicon,
    /// Observations needed before a mapping is promoted.
    pub threshold: u32,
    #[serde(default)]
    observations: Vec<Observation>,
    /// Suppressed (spoken form, term) mappings.
    #[serde(default)]
    suppressed: Vec<(String, String)>,
}

impl Default for PersonalLexicon {
    fn default() -> Self {
        Self::with_threshold(2)
    }
}

impl PersonalLexicon {
    pub fn with_threshold(threshold: u32) -> Self {
        Self {
            lexicon: Lexicon::new(Layer::Personal),
            threshold: threshold.max(1),
            observations: Vec::new(),
            suppressed: Vec::new(),
        }
    }

    pub fn lexicon(&self) -> &Lexicon {
        &self.lexicon
    }

    /// Mappings the user rejected often enough to stop applying.
    pub fn suppressions(&self) -> &[(String, String)] {
        &self.suppressed
    }

    /// Adds or merges a user-defined term.
    pub fn add_term(&mut self, entry: TermEntry) {
        self.lexicon.add(TermEntry {
            match_term: entry.match_term.or(Some(true)),
            ..entry
        });
    }

    /// Removes a term and everything learned about it.
    pub fn remove_term(&mut self, term: &str) -> bool {
        self.observations.retain(|o| o.term != term);
        self.lexicon.remove(term)
    }

    /// Serializes to JSON.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Restores from JSON produced by [`Self::to_json`].
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        let mut p: Self = serde_json::from_str(s)?;
        // Rebuild the term index skipped during deserialization.
        let entries = p.lexicon.entries().to_vec();
        p.lexicon = Lexicon::from_entries(Layer::Personal, entries);
        Ok(p)
    }

    /// Learns from an edit without knowledge of other layers.
    pub fn learn(&mut self, original: &str, edited: &str) -> Vec<LearnedMapping> {
        self.learn_with(original, edited, |_| None)
    }

    /// Learns from an edit. `canonical_for` maps a spoken form to the term
    /// the engine would currently produce, which is used to recognize
    /// reverted corrections.
    pub fn learn_with(
        &mut self,
        original: &str,
        edited: &str,
        canonical_for: impl Fn(&str) -> Option<String>,
    ) -> Vec<LearnedMapping> {
        let mut out = Vec::new();
        for (spoken, written) in replaced_spans(original, edited) {
            let variant = normalize(&spoken);
            if variant.is_empty()
                || written.is_empty()
                || variant == normalize(&written) && spoken == written
            {
                continue;
            }
            // Reverting a correction: the shown text held a term that the
            // engine produces from what the user typed back.
            let reverted = canonical_for(&written).is_some_and(|t| t == spoken);
            let mapping = if reverted {
                self.observe(normalize(&written), spoken.clone(), true)
            } else {
                self.observe(variant, written.clone(), false)
            };
            out.push(mapping);
        }
        out
    }

    fn observe(&mut self, variant: String, term: String, rejection: bool) -> LearnedMapping {
        let idx = match self
            .observations
            .iter()
            .position(|o| o.variant == variant && o.term == term && o.rejection == rejection)
        {
            Some(i) => i,
            None => {
                self.observations.push(Observation {
                    variant: variant.clone(),
                    term: term.clone(),
                    count: 0,
                    rejection,
                });
                self.observations.len() - 1
            }
        };
        self.observations[idx].count += 1;
        let count = self.observations[idx].count;
        let competing = self
            .observations
            .iter()
            .filter(|o| o.variant == variant && o.term != term && !o.rejection && !rejection)
            .map(|o| o.count)
            .max()
            .unwrap_or(0);
        let already = if rejection {
            self.suppressed
                .iter()
                .any(|(v, t)| v == &variant && t == &term)
        } else {
            self.lexicon
                .get(&term)
                .is_some_and(|e| e.variants.iter().any(|v| normalize(&v.text) == variant))
        };
        let promoted = count >= self.threshold && count > competing && !already;
        if promoted {
            if rejection {
                let pair = (variant.clone(), term.clone());
                if !self.suppressed.contains(&pair) {
                    self.suppressed.push(pair);
                }
                // A rejected personal mapping is removed outright.
                if let Some(entry) = self.lexicon.get(&term).cloned() {
                    let variants: Vec<Variant> = entry
                        .variants
                        .into_iter()
                        .filter(|v| normalize(&v.text) != variant)
                        .collect();
                    self.lexicon.remove(&term);
                    if !variants.is_empty() {
                        self.lexicon.add(TermEntry {
                            variants,
                            ..TermEntry::new(term.clone())
                        });
                    }
                }
            } else {
                self.suppressed
                    .retain(|(v, t)| !(v == &variant && t == &term));
                self.lexicon.add(TermEntry {
                    variants: vec![Variant::new(variant.clone(), None)],
                    category: Some("learned".into()),
                    ..TermEntry::new(term.clone())
                });
            }
        }
        LearnedMapping {
            variant,
            term,
            count,
            promoted,
            rejection,
        }
    }
}

/// Word-level diff of two texts. Returns (original span, edited span) pairs
/// for every small replacement.
pub fn replaced_spans(original: &str, edited: &str) -> Vec<(String, String)> {
    let a = tokenize(original);
    let b = tokenize(edited);
    if a.len() > MAX_DIFF_TOKENS || b.len() > MAX_DIFF_TOKENS {
        return Vec::new();
    }
    let ops = lcs_pairs(&a, &b);
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    for (mi, mj) in ops.into_iter().chain(std::iter::once((a.len(), b.len()))) {
        let del = &a[i..mi];
        let ins = &b[j..mj];
        if let Some(pair) = learnable(original, edited, del, ins) {
            out.push(pair);
        }
        i = mi + 1;
        j = mj + 1;
    }
    out
}

fn learnable(
    original: &str,
    edited: &str,
    del: &[Token],
    ins: &[Token],
) -> Option<(String, String)> {
    if del.is_empty() || ins.is_empty() {
        return None;
    }
    let lexical = |t: &[Token]| t.iter().filter(|t| t.class.is_lexical()).count();
    let (dw, iw) = (lexical(del), lexical(ins));
    if dw == 0 || iw == 0 || dw > MAX_SPOKEN_WORDS || iw > MAX_TERM_WORDS {
        return None;
    }
    let spoken = &original[del[0].start..del[del.len() - 1].end()];
    let written = &edited[ins[0].start..ins[ins.len() - 1].end()];
    let (spoken, written) = trim_cjk_affixes(spoken, written);
    if spoken.is_empty() || written.is_empty() || spoken == written {
        return None;
    }
    Some((spoken.to_owned(), written.to_owned()))
}

/// Strips common leading/trailing characters when they are CJK or Hangul
/// (particles such as `를` or `を` that attach to the replaced word).
fn trim_cjk_affixes<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    let (mut a, mut b) = (a, b);
    while let (Some(x), Some(y)) = (a.chars().next_back(), b.chars().next_back()) {
        if x == y
            && CharClass::of(x).is_boundary_free()
            && a.len() > x.len_utf8()
            && b.len() > y.len_utf8()
        {
            a = &a[..a.len() - x.len_utf8()];
            b = &b[..b.len() - y.len_utf8()];
        } else {
            break;
        }
    }
    while let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) {
        if x == y
            && CharClass::of(x).is_boundary_free()
            && a.len() > x.len_utf8()
            && b.len() > y.len_utf8()
        {
            a = &a[x.len_utf8()..];
            b = &b[y.len_utf8()..];
        } else {
            break;
        }
    }
    (a.trim(), b.trim())
}

/// Indices of matching tokens along a longest common subsequence.
fn lcs_pairs(a: &[Token], b: &[Token]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[at(i, j)] = if a[i].text == b[j].text {
                dp[at(i + 1, j + 1)] + 1
            } else {
                dp[at(i + 1, j)].max(dp[at(i, j + 1)])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i].text == b[j].text {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[at(i + 1, j)] >= dp[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_finds_replacements() {
        let spans = replaced_spans(
            "please fix the use effect hook in cube control",
            "please fix the useEffect hook in kubectl",
        );
        assert_eq!(
            spans,
            vec![
                ("use effect".to_owned(), "useEffect".to_owned()),
                ("cube control".to_owned(), "kubectl".to_owned())
            ]
        );
    }

    #[test]
    fn diff_trims_attached_particles() {
        let spans = replaced_spans("유즈 이펙트를 고쳐줘", "useEffect를 고쳐줘");
        assert_eq!(
            spans,
            vec![("유즈 이펙트".to_owned(), "useEffect".to_owned())]
        );
        let spans = replaced_spans("ユーズエフェクトを直して", "useEffectを直して");
        assert_eq!(
            spans,
            vec![("ユーズエフェクト".to_owned(), "useEffect".to_owned())]
        );
    }

    #[test]
    fn large_rewrites_are_not_learned() {
        let spans = replaced_spans(
            "make it faster",
            "rewrite the whole caching layer using a different approach",
        );
        assert!(spans.is_empty(), "{spans:?}");
    }

    #[test]
    fn promotion_after_threshold() {
        let mut p = PersonalLexicon::with_threshold(2);
        let first = p.learn("open the vca speech crate", "open the vca-speech crate");
        assert_eq!(first.len(), 1);
        assert!(!first[0].promoted);
        assert!(p.lexicon().is_empty());
        let second = p.learn("check vca speech", "check vca-speech");
        assert!(second[0].promoted);
        let entry = p.lexicon().get("vca-speech").unwrap();
        assert_eq!(entry.variants[0].text, "vca speech");
    }

    #[test]
    fn inconsistent_corrections_need_a_majority() {
        let mut p = PersonalLexicon::with_threshold(2);
        p.learn("open cloud code", "open Claude Code");
        p.learn("open cloud code", "open Cloud Code");
        // One each: nothing promoted yet.
        assert!(p.lexicon().is_empty());
        let m = p.learn("open cloud code", "open Claude Code");
        assert!(m[0].promoted);
        assert!(p.lexicon().get("Claude Code").is_some());
        assert!(p.lexicon().get("Cloud Code").is_none());
        // Repeating the same edit does not promote twice.
        let again = p.learn("open cloud code", "open Claude Code");
        assert!(!again[0].promoted);
    }

    #[test]
    fn json_round_trip() {
        let mut p = PersonalLexicon::default();
        p.add_term(TermEntry::new("Kubrick").variant("cue brick", None));
        let json = p.to_json().unwrap();
        let back = PersonalLexicon::from_json(&json).unwrap();
        assert_eq!(back.lexicon().get("Kubrick").unwrap().variants.len(), 1);
    }
}
