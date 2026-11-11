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

/// Units of at most this many characters are checked for repetition loops.
const MAX_UNIT_CHARS: usize = 4;
/// Consecutive repetitions of a short unit that make a loop.
const MIN_REPEATS: usize = 4;
/// A single ASCII letter repeated without separators needs this many
/// repetitions, so the DNS record type "AAAA" survives.
const MIN_ASCII_LETTER_RUN: usize = 5;
/// A unit containing a digit needs this many repetitions, so version
/// strings and addresses ("1.1.1.1", "1.1.1.1.1") survive. Digits repeated
/// without separators ("10000") never form a loop.
const MIN_DIGIT_REPEATS: usize = 8;
/// Runs of one punctuation mark up to this length are kept ("......").
const MAX_PUNCT_RUN: usize = 6;
/// Longer punctuation runs are shortened to this many marks.
const PUNCT_RUN_KEPT: usize = 3;
/// Separator characters allowed between two repetitions of a unit.
const MAX_SEPARATOR_CHARS: usize = 2;
/// A transcript is dropped when one loop covers at least this share of its
/// visible (non-whitespace) characters...
const DROP_SHARE: f32 = 0.8;
/// ...and repeats its unit at least this many times.
const DROP_MIN_REPEATS: usize = 6;

/// Collapses repetition loops that speech engines (Whisper in particular)
/// emit on hesitations, silence or noise, such as "음음음음음…",
/// "O.O.O.O.O.…", "아. 아. 아. 아." or "thank you thank you thank you …".
/// Always safe to run: real speech does not repeat a syllable four times in
/// a row or a phrase three times.
///
/// - A unit of one to four characters (letters, digits, punctuation, mixed:
///   "O.", "ㅋ", "아.", "네") repeated four or more times in a row, with up to
///   two separator characters (spaces, `,`, `.`, `、`, …) between the
///   repetitions, is kept once. Exceptions keep real text: digits need eight
///   repetitions and never loop without separators ("1.1.1.1", "10000"), a
///   single ASCII letter needs five ("AAAA"), runs of one punctuation mark
///   are kept up to six ("......") and longer ones shortened to three, and
///   tokens that look like URLs, e-mail addresses or paths are left alone.
/// - A word or a phrase of up to six words repeated three or more times in a
///   row is kept once.
/// - A transcript that is at least 80% one loop of six or more repetitions
///   is a hallucination and becomes empty.
pub fn collapse_repeats(text: &str) -> String {
    match collapse_units(text) {
        Some(collapsed) => collapse_phrases(&collapsed),
        None => String::new(),
    }
}

/// Whether `text` is a repetition-loop hallucination that
/// [`collapse_repeats`] turns into an empty transcript.
pub fn is_repetition_hallucination(text: &str) -> bool {
    !text.trim().is_empty() && collapse_units(text).is_none()
}

// ---------------------------------------------------------------------------
// Known hallucination phrases
// ---------------------------------------------------------------------------

/// Stock phrases Whisper invents on silence or noise ("감사합니다." (thank you),
/// "Thank you.", "ご視聴ありがとうございました"). Shared with the UI
/// (`apps/ui/src/speech/hallucinations.ts`) and the reference STT server.
const PHRASES_JSON: &str = include_str!("../../../apps/ui/src/speech/hallucinationPhrases.json");

/// Longest free part ("*") of a phrase, in normalized characters.
const MAX_WILDCARD_CHARS: usize = 30;
/// A segment longer than this is real speech, whatever its text.
const MAX_HALLUCINATION_SECS: f32 = 6.0;
/// A segment is low-confidence below this (`1 - no_speech_prob` or `exp(avg_logprob)`).
const LOW_CONFIDENCE: f32 = 0.5;
/// A last segment after a pause is judged against this, laxer, limit.
const TRAILING_CONFIDENCE: f32 = 0.65;
/// A silence of at least this many milliseconds before a segment is a pause.
const PAUSE_MS: u64 = 1000;
/// A plain-text chunk shorter than this is too short to hold the phrase...
const SHORT_CHUNK_SECS: f32 = 1.2;
/// ...and one with a root-mean-square level (0..1) below this is quiet.
const QUIET_RMS: f32 = 0.012;

struct Phrase {
    prefix: String,
    /// `Some` when the phrase has a free part: the text after it.
    suffix: Option<String>,
}

fn normalize_phrase(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn phrases() -> &'static [Phrase] {
    static PHRASES: std::sync::OnceLock<Vec<Phrase>> = std::sync::OnceLock::new();
    PHRASES.get_or_init(|| {
        let value: serde_json::Value =
            serde_json::from_str(PHRASES_JSON).expect("hallucinationPhrases.json is valid");
        let mut out = Vec::new();
        for list in value["phrases"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
        {
            for entry in list.as_array().into_iter().flatten() {
                let Some(entry) = entry.as_str() else {
                    continue;
                };
                out.push(match entry.split_once('*') {
                    Some((head, tail)) => Phrase {
                        prefix: normalize_phrase(head),
                        suffix: Some(normalize_phrase(tail)),
                    },
                    None => Phrase {
                        prefix: normalize_phrase(entry),
                        suffix: None,
                    },
                });
            }
        }
        out
    })
}

/// Whether the whole of `text` is a known hallucination phrase, ignoring
/// case, punctuation and spacing ("감사합니다." and "Thank you!" are; a phrase
/// inside a longer sentence is not).
pub fn is_hallucination_phrase(text: &str) -> bool {
    let norm = normalize_phrase(text);
    if norm.is_empty() {
        return false;
    }
    phrases().iter().any(|p| match &p.suffix {
        None => norm == p.prefix,
        Some(suffix) => {
            let Some(rest) = norm.strip_prefix(p.prefix.as_str()) else {
                return false;
            };
            rest.strip_suffix(suffix.as_str())
                .is_some_and(|free| free.chars().count() <= MAX_WILDCARD_CHARS)
        }
    })
}

/// How long and how loud a plain-text chunk's audio is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioLevel {
    pub duration_secs: f32,
    /// Root-mean-square level of the samples, 0.0 to 1.0.
    pub rms: f32,
}

/// For engines that return plain text only: empties `text` when the whole
/// chunk is one known hallucination phrase and its audio is short or quiet.
/// Without audio information the text is kept.
pub fn drop_lone_hallucination(text: &str, audio: Option<AudioLevel>) -> String {
    match audio {
        Some(a)
            if is_hallucination_phrase(text)
                && (a.duration_secs < SHORT_CHUNK_SECS || a.rms < QUIET_RMS) =>
        {
            String::new()
        }
        _ => text.to_owned(),
    }
}

/// Removes segments that are a known hallucination phrase on their own and
/// that the engine itself doubts: low confidence, and not longer than a few
/// seconds. A last segment after a pause is judged slightly laxer. Segments
/// without a confidence are kept, and so is any phrase inside a longer
/// segment. Rebuilds the transcript text when something was removed; returns
/// how many segments were removed.
pub fn strip_hallucinated_segments(transcript: &mut crate::stt::Transcript) -> usize {
    let segments = &transcript.segments;
    let last = segments.len().saturating_sub(1);
    let mut keep = Vec::with_capacity(segments.len());
    let mut prev_end: Option<u64> = None;
    for (i, seg) in segments.iter().enumerate() {
        let after_pause =
            matches!((prev_end, seg.start_ms), (Some(e), Some(s)) if s >= e + PAUSE_MS);
        prev_end = seg.end_ms.or(prev_end);
        let secs = match (seg.start_ms, seg.end_ms) {
            (Some(s), Some(e)) if e >= s => Some((e - s) as f32 / 1000.0),
            _ => None,
        };
        let limit = if i == last && after_pause {
            TRAILING_CONFIDENCE
        } else {
            LOW_CONFIDENCE
        };
        let doubtful = seg.confidence.is_some_and(|c| c < limit)
            && secs.is_none_or(|d| d <= MAX_HALLUCINATION_SECS);
        keep.push(!(doubtful && is_hallucination_phrase(&seg.text)));
    }
    let removed = keep.iter().filter(|k| !**k).count();
    if removed > 0 {
        let mut flags = keep.iter();
        transcript
            .segments
            .retain(|_| *flags.next().unwrap_or(&true));
        transcript.text =
            crate::stt::google::join_segments(transcript.segments.iter().map(|s| s.text.as_str()));
    }
    removed
}

/// One repetition loop found at a position.
struct Loop {
    /// Characters of the unit.
    unit: usize,
    /// End (exclusive, in chars) of the last repetition.
    end: usize,
    reps: usize,
    /// Copies of the unit kept in the output.
    keep: usize,
}

/// Separators allowed between repetitions of a unit.
fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ',' | '.' | '、' | '。' | '，' | '!' | '?' | '…' | '·' | '-' | '~' | ';' | ':'
        )
}

/// Characters of URL-, e-mail- or path-like tokens, which are never
/// collapsed.
fn protected_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        let token = chars[start..i].iter().collect::<String>().to_lowercase();
        if token.contains("://") || token.starts_with("www.") || token.contains(['@', '/', '\\']) {
            mask[start..i].fill(true);
        }
    }
    mask
}

/// The repetitions a loop of `unit` needs, or `None` when it never loops.
fn min_repeats(unit: &[char], used_separator: bool) -> Option<usize> {
    if unit.iter().any(|c| c.is_numeric()) {
        if !used_separator && unit.iter().all(|c| c.is_numeric()) {
            return None;
        }
        return Some(MIN_DIGIT_REPEATS);
    }
    if !used_separator && unit.len() == 1 && unit[0].is_ascii_alphabetic() {
        return Some(MIN_ASCII_LETTER_RUN);
    }
    Some(MIN_REPEATS)
}

/// The shortest repetition loop starting at `i`, if any.
fn loop_at(chars: &[char], folded: &[char], protected: &[bool], i: usize) -> Option<Loop> {
    let n = chars.len();
    if protected[i] || chars[i].is_whitespace() {
        return None;
    }
    // Runs of one punctuation mark are kept up to `MAX_PUNCT_RUN`.
    if !chars[i].is_alphanumeric() {
        let mut end = i + 1;
        while end < n && chars[end] == chars[i] && !protected[end] {
            end += 1;
        }
        let reps = end - i;
        return (reps > MAX_PUNCT_RUN).then_some(Loop {
            unit: 1,
            end,
            reps,
            keep: PUNCT_RUN_KEPT,
        });
    }
    for len in 1..=MAX_UNIT_CHARS {
        if i + len > n {
            break;
        }
        let unit = &folded[i..i + len];
        if unit.iter().any(|c| c.is_whitespace()) || protected[i..i + len].contains(&true) {
            break;
        }
        let matches = |pos: usize| {
            pos + len <= n
                && &folded[pos..pos + len] == unit
                && !protected[pos..pos + len].contains(&true)
        };
        let (mut reps, mut end, mut used_separator) = (1, i + len, false);
        loop {
            if matches(end) {
                reps += 1;
                end += len;
                continue;
            }
            let next = (1..=MAX_SEPARATOR_CHARS)
                .take_while(|s| end + s <= n && is_separator(chars[end + s - 1]))
                .find(|s| matches(end + s));
            match next {
                Some(s) => {
                    reps += 1;
                    end += s + len;
                    used_separator = true;
                }
                None => break,
            }
        }
        if min_repeats(unit, used_separator).is_some_and(|need| reps >= need) {
            return Some(Loop {
                unit: len,
                end,
                reps,
                keep: 1,
            });
        }
    }
    None
}

/// Collapses short-unit loops; `None` when the whole text is one loop (see
/// [`DROP_SHARE`]).
fn collapse_units(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let folded: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let protected = protected_mask(&chars);
    let visible = |range: &[char]| range.iter().filter(|c| !c.is_whitespace()).count();
    let total = visible(&chars);
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match loop_at(&chars, &folded, &protected, i) {
            Some(l) => {
                if l.reps >= DROP_MIN_REPEATS
                    && visible(&chars[i..l.end]) as f32 >= DROP_SHARE * total as f32
                {
                    return None;
                }
                for _ in 0..l.keep {
                    out.extend(&chars[i..i + l.unit]);
                }
                i = l.end;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    Some(out)
}

fn collapse_phrases(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 3 {
        return text.to_owned();
    }
    let key = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let keys: Vec<String> = words.iter().map(|w| key(w)).collect();
    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    let mut changed = false;
    let mut i = 0;
    'outer: while i < words.len() {
        for n in 1..=6 {
            if i + 3 * n > words.len() || keys[i..i + n].iter().all(String::is_empty) {
                continue;
            }
            let mut reps = 1;
            while i + (reps + 1) * n <= words.len()
                && keys[i + reps * n..i + (reps + 1) * n] == keys[i..i + n]
            {
                reps += 1;
            }
            if reps >= 3 {
                kept.extend(&words[i..i + n]);
                i += reps * n;
                changed = true;
                continue 'outer;
            }
        }
        kept.push(words[i]);
        i += 1;
    }
    if changed {
        kept.join(" ")
    } else {
        text.to_owned()
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
    #[test]
    fn collapses_engine_repetition_loops() {
        assert_eq!(collapse_repeats("음음음음음음음음 테스트"), "음 테스트");
        assert_eq!(
            collapse_repeats("음 음 음 음 음 테스트 해줘"),
            "음 테스트 해줘"
        );
        assert_eq!(
            collapse_repeats("thank you thank you thank you thank you."),
            "thank you"
        );
        assert_eq!(
            collapse_repeats("감사합니다 감사합니다 감사합니다 감사합니다"),
            "감사합니다"
        );
        // Ordinary text is untouched.
        for t in [
            "PNPM 테스트 실행해 줘",
            "a a b",
            "ㅋㅋㅋ 좋아",
            "really really good",
            "1000 users",
        ] {
            assert_eq!(collapse_repeats(t), t);
        }
    }

    #[test]
    fn drops_a_transcript_that_is_one_loop() {
        let loop_120 = "O.".repeat(120);
        assert_eq!(collapse_repeats(&loop_120), "");
        assert_eq!(collapse_repeats(&format!("{loop_120}O")), "");
        assert_eq!(collapse_repeats(&"O. ".repeat(50)), "");
        assert_eq!(collapse_repeats(&"아. ".repeat(30)), "");
        assert_eq!(collapse_repeats(&"네 ".repeat(10)), "");
        assert_eq!(collapse_repeats(&"ㅋ".repeat(40)), "");
        assert_eq!(collapse_repeats(&".".repeat(40)), "");
        assert_eq!(collapse_repeats(&"you ".repeat(12)), "");
        // A short real prefix does not save a long loop (>= 80%).
        assert_eq!(collapse_repeats(&format!("네 {}", "O.".repeat(60))), "");
        assert!(is_repetition_hallucination(&loop_120));
        assert!(!is_repetition_hallucination("테스트 실행해 줘"));
        assert!(!is_repetition_hallucination(""));
    }

    #[test]
    fn collapses_short_unit_loops_inside_text() {
        assert_eq!(collapse_repeats("O.O.O.O.O 테스트"), "O 테스트");
        assert_eq!(collapse_repeats("O.O.O.O.O. 테스트"), "O. 테스트");
        assert_eq!(
            collapse_repeats("아. 아. 아. 아. 로그인 고쳐줘"),
            "아. 로그인 고쳐줘"
        );
        assert_eq!(collapse_repeats("네, 네, 네, 네, 보내줘"), "네, 보내줘");
        assert_eq!(collapse_repeats("ㅋㅋㅋㅋㅋㅋ 좋아"), "ㅋ 좋아");
        assert_eq!(collapse_repeats("hahahaha that works"), "ha that works");
        assert_eq!(collapse_repeats("음..음..음..음.. 테스트"), "음.. 테스트");
        assert_eq!(collapse_repeats("Wait.......... what"), "Wait... what");
        assert_eq!(collapse_repeats("aaaaa ok"), "a ok");
        // A loop in a long real sentence is collapsed, not dropped.
        assert_eq!(
            collapse_repeats("로그인 화면의 버그를 고쳐줘 O.O.O.O.O.O"),
            "로그인 화면의 버그를 고쳐줘 O"
        );
        // Case-insensitive.
        assert_eq!(collapse_repeats("O.o.O.o 끝"), "O 끝");
    }

    #[test]
    fn keeps_legitimate_text() {
        for t in [
            "ㅋㅋ",
            "ㅋㅋㅋ",
            "1.2.3",
            "1.1.1.1",
            "DNS 서버를 1.1.1.1로 바꿔 줘",
            "version 1.1.1.1.1 released",
            "1.0.0.0 and 10000 and 2.2.2.2",
            "Wait......",
            "음...... 그래",
            "a.b.c.d",
            "foo.bar.baz.qux()",
            "AAAA 레코드 추가해 줘",
            "add an AAAA record",
            "https://aaaa.example.com/a/a/a/a",
            "www.ooooo.com",
            "send it to bbbbb@example.com",
            "src/a/a/a/a/mod.rs",
            "Mississippi",
            "1111 and 0000",
            "x = a - b - c",
            "네 보내줘",
            "네 네",
        ] {
            assert_eq!(collapse_repeats(t), t, "{t:?}");
        }
    }

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

    fn seg(text: &str, start: u64, end: u64, confidence: Option<f32>) -> crate::stt::Segment {
        crate::stt::Segment {
            text: text.to_owned(),
            start_ms: Some(start),
            end_ms: Some(end),
            confidence,
            language: None,
        }
    }

    fn transcript(segments: Vec<crate::stt::Segment>) -> crate::stt::Transcript {
        let text = crate::stt::google::join_segments(segments.iter().map(|s| s.text.as_str()));
        crate::stt::Transcript {
            text,
            language: None,
            segments,
        }
    }

    #[test]
    fn shared_phrase_cases() {
        #[derive(serde::Deserialize)]
        struct Case {
            text: String,
            phrase: bool,
        }
        let value: serde_json::Value = serde_json::from_str(PHRASES_JSON).unwrap();
        let cases: Vec<Case> = serde_json::from_value(value["cases"].clone()).unwrap();
        assert!(cases.len() > 10);
        for c in cases {
            assert_eq!(is_hallucination_phrase(&c.text), c.phrase, "{:?}", c.text);
        }
    }

    #[test]
    fn lone_chunk_needs_short_or_quiet_audio() {
        let quiet = Some(AudioLevel {
            duration_secs: 4.0,
            rms: 0.004,
        });
        let short = Some(AudioLevel {
            duration_secs: 0.8,
            rms: 0.05,
        });
        let loud = Some(AudioLevel {
            duration_secs: 4.0,
            rms: 0.05,
        });
        assert_eq!(drop_lone_hallucination("감사합니다.", quiet), "");
        assert_eq!(drop_lone_hallucination("감사합니다.", short), "");
        assert_eq!(drop_lone_hallucination("Thank you.", quiet), "");
        // Loud, long audio: the user may really have said it. No audio info: keep.
        assert_eq!(drop_lone_hallucination("감사합니다.", loud), "감사합니다.");
        assert_eq!(drop_lone_hallucination("감사합니다.", None), "감사합니다.");
        // Never a phrase inside a longer sentence.
        let s = "도와주셔서 감사합니다 다음 작업 진행해";
        assert_eq!(drop_lone_hallucination(s, quiet), s);
    }

    #[test]
    fn drops_low_confidence_phrase_segments() {
        let mut t = transcript(vec![
            seg("로그인 버그 고쳐줘", 0, 3000, Some(0.95)),
            seg("시청해 주셔서 감사합니다", 8000, 9500, Some(0.4)),
        ]);
        assert_eq!(strip_hallucinated_segments(&mut t), 1);
        assert_eq!(t.text, "로그인 버그 고쳐줘");
        assert_eq!(t.segments.len(), 1);

        // A pause before the last segment makes the limit laxer.
        let mut t = transcript(vec![
            seg("run the tests", 0, 2000, Some(0.95)),
            seg("Thank you.", 5000, 6000, Some(0.6)),
        ]);
        assert_eq!(strip_hallucinated_segments(&mut t), 1);
        assert_eq!(t.text, "run the tests");

        // A lone "Thank you." with a high no_speech_prob (confidence 1 - p).
        let mut t = transcript(vec![seg("Thank you.", 0, 1000, Some(0.1))]);
        assert_eq!(strip_hallucinated_segments(&mut t), 1);
        assert_eq!(t.text, "");
    }

    #[test]
    fn keeps_real_speech_segments() {
        // Good confidence: the user really said it.
        let mut t = transcript(vec![
            seg("로그인 버그 고쳐줘", 0, 3000, Some(0.95)),
            seg("감사합니다", 3100, 4000, Some(0.9)),
        ]);
        assert_eq!(strip_hallucinated_segments(&mut t), 0);
        assert_eq!(t.segments.len(), 2);
        assert!(t.text.ends_with("감사합니다"));
        // Mid-confidence without a pause is kept.
        let mut t = transcript(vec![
            seg("run the tests", 0, 2000, Some(0.95)),
            seg("Thank you.", 2100, 3000, Some(0.6)),
        ]);
        assert_eq!(strip_hallucinated_segments(&mut t), 0);
        // Phrase inside a longer segment, even with low confidence.
        let mut t = transcript(vec![seg(
            "도와주셔서 감사합니다 다음 작업 진행해",
            0,
            4000,
            Some(0.2),
        )]);
        assert_eq!(strip_hallucinated_segments(&mut t), 0);
        // No confidence at all: keep.
        let mut t = transcript(vec![seg("감사합니다", 0, 1000, None)]);
        assert_eq!(strip_hallucinated_segments(&mut t), 0);
        // A long segment is speech.
        let mut t = transcript(vec![seg("감사합니다", 0, 12000, Some(0.2))]);
        assert_eq!(strip_hallucinated_segments(&mut t), 0);
    }
}
