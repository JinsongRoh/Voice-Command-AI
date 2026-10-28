//! Transcript clean-up (VOC-07).
//!
//! [`FillerRemover`] is a deterministic pass that removes hesitation sounds
//! ("um", "えーと", "음", "euh") using per-language tables. The
//! [`TranscriptCleaner`] trait is the hook for an optional LLM-based pass
//! (grammar, punctuation) implemented elsewhere.

use async_trait::async_trait;

use crate::error::Result;
use crate::text::{CharClass, NormalizedText, normalize};

/// A clean-up pass over a transcript.
#[async_trait]
pub trait TranscriptCleaner: Send + Sync {
    /// Returns the cleaned text. `language` is the transcript language when
    /// known (BCP-47).
    async fn clean(&self, text: &str, language: Option<&str>) -> Result<String>;
}

/// Hesitation sounds per language. Only sounds that are never real words in
/// the language are listed; ambiguous fillers ("like", "so", "あの") are left
/// for the optional LLM pass.
const FILLERS: &[(&str, &[&str])] = &[
    (
        "en",
        &[
            "um", "umm", "uh", "uhh", "uhm", "erm", "er", "hmm", "mm", "mhm", "uh-huh",
        ],
    ),
    (
        "ja",
        &[
            "えーと",
            "えっと",
            "えーっと",
            "えー",
            "あのー",
            "あのう",
            "うーん",
            "んー",
            "ええと",
        ],
    ),
    ("ko", &["음", "음음", "으음", "어어", "아아", "그러니까 음"]),
    ("zh", &["嗯", "呃", "额", "嗯嗯", "呃呃"]),
    ("es", &["eh", "ehm", "em", "mmm", "este eh"]),
    ("de", &["äh", "ähm", "öh", "öhm", "hm", "hmm"]),
    ("fr", &["euh", "heu", "hum", "bah euh"]),
    ("pt", &["ãh", "hum", "éé", "ahn"]),
];

/// Removes hesitation fillers. All languages are active at once because
/// transcripts mix languages.
#[derive(Debug, Clone)]
pub struct FillerRemover {
    /// Normalized fillers, longest first.
    fillers: Vec<String>,
}

impl Default for FillerRemover {
    fn default() -> Self {
        Self::new()
    }
}

impl FillerRemover {
    /// A remover with the built-in tables.
    pub fn new() -> Self {
        let mut r = Self {
            fillers: Vec::new(),
        };
        for (_, words) in FILLERS {
            r.add(words.iter().copied());
        }
        r
    }

    /// Adds custom fillers.
    pub fn add<'a>(&mut self, words: impl IntoIterator<Item = &'a str>) {
        for w in words {
            let n = normalize(w);
            if !n.is_empty() && !self.fillers.contains(&n) {
                self.fillers.push(n);
            }
        }
        self.fillers
            .sort_by_key(|f| std::cmp::Reverse(f.chars().count()));
    }

    /// Removes fillers and tidies the whitespace and punctuation they leave.
    pub fn remove(&self, text: &str) -> String {
        let norm = NormalizedText::new(text);
        let n = norm.text.as_str();
        let mut cut: Vec<(usize, usize)> = Vec::new();
        let mut pos = 0;
        while pos < n.len() {
            let rest = &n[pos..];
            let hit = self
                .fillers
                .iter()
                .find(|f| rest.starts_with(f.as_str()) && Self::whole(n, pos, pos + f.len()));
            match hit {
                Some(f) => {
                    let mut end = pos + f.len();
                    // Swallow trailing filler punctuation such as "um," or "えーと、".
                    while let Some(c) = n[end..].chars().next() {
                        if matches!(c, ',' | '、' | '，' | '…')
                            || (c == '.' && n[end..].starts_with("..."))
                        {
                            end += c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    cut.push(norm.original_range(pos, end));
                    pos = end;
                }
                None => pos += rest.chars().next().map_or(1, char::len_utf8),
            }
        }
        if cut.is_empty() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for (s, e) in cut {
            if s >= cursor {
                out.push_str(&text[cursor..s]);
                cursor = e;
            }
        }
        out.push_str(&text[cursor..]);
        tidy(&out)
    }

    /// A filler must be a whole word: in spaced scripts it needs non-word
    /// characters on both sides; in CJK it needs a non-letter or a script
    /// change on both sides, because fillers such as `嗯` never occur inside
    /// words but `えー` could start a longer word.
    fn whole(text: &str, start: usize, end: usize) -> bool {
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let first = text[start..end].chars().next();
        let last = text[start..end].chars().next_back();
        // Chinese hesitation characters (嗯, 呃) never occur inside words.
        if text[start..end]
            .chars()
            .all(|c| CharClass::of(c) == CharClass::Han)
        {
            return true;
        }
        let sep = |o: Option<char>, i: Option<char>| match (o, i) {
            (None, _) | (_, None) => true,
            (Some(o), Some(i)) => {
                let (co, ci) = (CharClass::of(o), CharClass::of(i));
                !co.is_lexical()
                    || (co != ci && !(ci == CharClass::Han && co == CharClass::Hiragana))
            }
        };
        sep(before, first) && sep(after, last)
    }
}

/// Collapses double spaces and spaces before punctuation, and trims leading
/// separators left behind by a removed filler.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == ' ' && (out.ends_with(' ') || out.is_empty()) {
            continue;
        }
        if matches!(c, ',' | '.' | '!' | '?' | '、' | '。') && out.ends_with(' ') {
            out.pop();
        }
        out.push(c);
    }
    let trimmed = out
        .trim()
        .trim_start_matches([',', '、', '，'])
        .trim_start();
    trimmed.to_owned()
}

#[async_trait]
impl TranscriptCleaner for FillerRemover {
    async fn clean(&self, text: &str, _language: Option<&str>) -> Result<String> {
        Ok(self.remove(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_fillers_in_many_languages() {
        let r = FillerRemover::new();
        assert_eq!(r.remove("Um, fix the uh login bug"), "fix the login bug");
        assert_eq!(r.remove("えーと、テストを実行して"), "テストを実行して");
        assert_eq!(r.remove("음 로그인 버그 고쳐줘"), "로그인 버그 고쳐줘");
        assert_eq!(r.remove("嗯，运行测试"), "运行测试");
        assert_eq!(r.remove("呃运行测试"), "运行测试");
        assert_eq!(r.remove("ähm, starte die Tests"), "starte die Tests");
        assert_eq!(r.remove("euh lance les tests"), "lance les tests");
        assert_eq!(r.remove("eh, ejecuta los tests"), "ejecuta los tests");
    }

    #[test]
    fn keeps_words_that_contain_fillers() {
        let r = FillerRemover::new();
        assert_eq!(
            r.remove("summarize the umbrella module"),
            "summarize the umbrella module"
        );
        assert_eq!(r.remove("check hmmer and erm"), "check hmmer and");
        assert_eq!(r.remove("음악 앱을 열어"), "음악 앱을 열어");
    }

    #[test]
    fn mixed_language_fillers() {
        let r = FillerRemover::new();
        assert_eq!(
            r.remove("えーと, run the uh cargo test 음"),
            "run the cargo test"
        );
    }

    #[tokio::test]
    async fn cleaner_trait() {
        let r = FillerRemover::new();
        assert_eq!(r.clean("uh hello", Some("en")).await.unwrap(), "hello");
    }
}
