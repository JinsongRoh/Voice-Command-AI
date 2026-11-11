//! Strict validation of the fixes a chat model proposes.
//!
//! The model's answer is untrusted: a fix is applied only when every
//! proposed replacement is a whole-word substring of the text, the edits stay
//! small, and the text keeps its main script (no translation). Any problem
//! rejects the whole answer, and the caller keeps the text unchanged.

use serde::{Deserialize, Serialize};

use crate::text::CharClass;

/// One replacement proposed by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    /// Exact substring of the input text.
    pub from: String,
    /// Its replacement.
    pub to: String,
}

impl Fix {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

/// Bounds on what a correction may change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixLimits {
    /// Changed characters (the longer of `from` and `to`, per occurrence)
    /// over the characters of the text.
    pub max_edit_ratio: f32,
    /// Fixes per answer.
    pub max_fixes: usize,
}

impl Default for FixLimits {
    fn default() -> Self {
        Self {
            max_edit_ratio: 0.3,
            max_fixes: 10,
        }
    }
}

/// The text after applying validated fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub text: String,
    /// The fixes that changed something, in the order they were proposed.
    pub fixes: Vec<Fix>,
}

/// Why an answer was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("{0} fixes exceed the limit")]
    TooManyFixes(usize),
    #[error("a fix has an empty or blank side")]
    Empty,
    #[error("a fix contains control characters")]
    ControlChars,
    #[error("{0:?} does not occur in the text as a whole word")]
    NotFound(String),
    #[error("fixes overlap")]
    Overlap,
    #[error("the fixes change {changed} characters; the limit is {limit}")]
    TooLarge { changed: usize, limit: usize },
    #[error("the fixes change the language of the text")]
    LanguageChanged,
}

/// Validates `fixes` against `text` and applies them (every occurrence of
/// each `from`). Fixes whose `from` equals their `to` are ignored.
pub fn apply_fixes(text: &str, fixes: &[Fix], limits: FixLimits) -> Result<Applied, Rejection> {
    let fixes: Vec<&Fix> = fixes.iter().filter(|f| f.from != f.to).collect();
    if fixes.len() > limits.max_fixes {
        return Err(Rejection::TooManyFixes(fixes.len()));
    }
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    for (i, fix) in fixes.iter().enumerate() {
        if fix.from.trim().is_empty() || fix.to.trim().is_empty() {
            return Err(Rejection::Empty);
        }
        if fix.from.chars().chain(fix.to.chars()).any(char::is_control) {
            return Err(Rejection::ControlChars);
        }
        let before = spans.len();
        for (start, m) in text.match_indices(fix.from.as_str()) {
            let end = start + m.len();
            if on_word_boundary(text, start, end) {
                spans.push((start, end, i));
            }
        }
        if spans.len() == before {
            return Err(Rejection::NotFound(fix.from.clone()));
        }
    }
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err(Rejection::Overlap);
    }

    let total = text.chars().count();
    let limit = (total as f32 * limits.max_edit_ratio).floor() as usize;
    let changed: usize = spans
        .iter()
        .map(|&(_, _, i)| {
            fixes[i]
                .from
                .chars()
                .count()
                .max(fixes[i].to.chars().count())
        })
        .sum();
    if changed > limit {
        return Err(Rejection::TooLarge { changed, limit });
    }

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for &(start, end, i) in &spans {
        out.push_str(&text[cursor..start]);
        out.push_str(&fixes[i].to);
        cursor = end;
    }
    out.push_str(&text[cursor..]);

    if main_script(text) != main_script(&out) {
        return Err(Rejection::LanguageChanged);
    }
    Ok(Applied {
        text: out,
        fixes: fixes.into_iter().cloned().collect(),
    })
}

/// Words of spaced scripts must match whole: a match that starts or ends with
/// a letter must not touch another letter of a spaced script. CJK text has no
/// spaces, and Korean particles attach to nouns, so it is not restricted.
fn on_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let is_word = |c: Option<char>| c.is_some_and(|c| CharClass::of(c) == CharClass::Word);
    let first = text[start..end].chars().next();
    let last = text[start..end].chars().next_back();
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let glued_before = is_word(first) && is_word(before);
    let glued_after = is_word(last) && is_word(after);
    !glued_before && !glued_after
}

/// Script families for the "same language" check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Script {
    /// Latin, Cyrillic, Greek and other spaced scripts.
    Spaced,
    Hangul,
    /// Chinese characters and Japanese kana.
    Cjk,
}

/// The script with the most letters (digits excluded); `None` without letters.
fn main_script(text: &str) -> Option<Script> {
    let mut counts = [0usize; 3];
    for c in text.chars() {
        let idx = match CharClass::of(c) {
            CharClass::Word if !c.is_numeric() && c != '_' => 0,
            CharClass::Hangul => 1,
            CharClass::Han | CharClass::Hiragana | CharClass::Katakana => 2,
            _ => continue,
        };
        counts[idx] += 1;
    }
    let (idx, &n) = counts
        .iter()
        .enumerate()
        .max_by_key(|&(i, n)| (*n, std::cmp::Reverse(i)))?;
    if n == 0 {
        return None;
    }
    Some([Script::Spaced, Script::Hangul, Script::Cjk][idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, fixes: &[(&str, &str)]) -> Result<Applied, Rejection> {
        let fixes: Vec<Fix> = fixes.iter().map(|(f, t)| Fix::new(*f, *t)).collect();
        apply_fixes(text, &fixes, FixLimits::default())
    }

    #[test]
    fn applies_small_fixes_in_mixed_language_text() {
        let out = apply(
            "리액트 컴포넌트에서 use state 훅이 두 번 렌더링돼요 그리고 테스트도 실패해요",
            &[("use state", "useState")],
        )
        .unwrap();
        assert_eq!(
            out.text,
            "리액트 컴포넌트에서 useState 훅이 두 번 렌더링돼요 그리고 테스트도 실패해요"
        );
        assert_eq!(out.fixes, vec![Fix::new("use state", "useState")]);
    }

    #[test]
    fn replaces_every_whole_word_occurrence_only() {
        let out = apply(
            "run cube control apply first, then cube control get pods in the staging cluster and report",
            &[("cube control", "kubectl")],
        )
        .unwrap();
        assert_eq!(
            out.text,
            "run kubectl apply first, then kubectl get pods in the staging cluster and report"
        );
        // Inside a longer word is not a whole-word match.
        assert_eq!(
            apply("concatenate the arrays in the helper", &[("cat", "dog")]),
            Err(Rejection::NotFound("cat".into()))
        );
    }

    #[test]
    fn ignores_no_op_fixes_and_rejects_bad_ones() {
        let text = "deploy the payments service to staging today";
        assert_eq!(apply(text, &[("deploy", "deploy")]).unwrap().text, text);
        assert_eq!(
            apply(text, &[("deplay", "deploy")]),
            Err(Rejection::NotFound("deplay".into()))
        );
        assert_eq!(apply(text, &[("deploy", " ")]), Err(Rejection::Empty));
        assert_eq!(apply(text, &[("", "x")]), Err(Rejection::Empty));
        assert_eq!(
            apply(text, &[("deploy", "deploy\nnow")]),
            Err(Rejection::ControlChars)
        );
        assert_eq!(
            apply(text, &[("the payments", "a"), ("payments service", "b")]),
            Err(Rejection::Overlap)
        );
    }

    #[test]
    fn rejects_large_edits_and_too_many_fixes() {
        let text = "deploy the payments service to staging today";
        assert!(matches!(
            apply(text, &[("deploy the payments service", "ship billing")]),
            Err(Rejection::TooLarge { .. })
        ));
        let fixes: Vec<Fix> = (0..11)
            .map(|i| Fix::new(format!("w{i}"), format!("x{i}")))
            .collect();
        assert_eq!(
            apply_fixes("w0", &fixes, FixLimits::default()),
            Err(Rejection::TooManyFixes(11))
        );
    }

    #[test]
    fn rejects_translation() {
        // Korean translated into English flips the main script.
        let text = "로그인 버그 고쳐 줘";
        let limits = FixLimits {
            max_edit_ratio: 10.0,
            ..FixLimits::default()
        };
        assert_eq!(
            apply_fixes(
                text,
                &[Fix::new("로그인 버그 고쳐 줘", "fix the login bug")],
                limits
            ),
            Err(Rejection::LanguageChanged)
        );
    }

    #[test]
    fn main_script_counts_letters() {
        assert_eq!(main_script("123 !!"), None);
        assert_eq!(main_script("pnpm 설치해 줘 빨리"), Some(Script::Hangul));
        assert_eq!(main_script("テストを実行して"), Some(Script::Cjk));
        assert_eq!(main_script("run the tests"), Some(Script::Spaced));
    }
}
