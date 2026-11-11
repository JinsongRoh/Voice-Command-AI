//! Korean text clean-up before speech synthesis.
//!
//! Multilingual voices in Korean mode read plain Korean and common English
//! words well, but hashes ("e25f95a9"), file names ("questions.rs"), paths
//! ("dev/m0-foundation"), digits glued to letters, raw digit runs, markdown
//! and arrows come out as foreign-sounding babble: such an engine cannot
//! switch languages inside one request. [`korean_speakable`] rewrites those
//! spans into text a Korean voice can say. Plain Korean passes through as is,
//! and text without Hangul is never changed. Nothing here logs the text.
//!
//! The rules are a sequence of passes over the characters; each pass looks
//! only at its own input (as a regular-expression substitution would), so
//! the context checks before and after a span are plain index lookups.

use std::borrow::Cow;

const DIGITS: [&str; 10] = ["영", "일", "이", "삼", "사", "오", "육", "칠", "팔", "구"];
const SMALL_UNITS: [&str; 4] = ["", "십", "백", "천"];
const BIG_UNITS: [&str; 4] = ["", "만", "억", "조"];
/// Letter names, `a` to `z`.
const LETTERS: [&str; 26] = [
    "에이",
    "비",
    "씨",
    "디",
    "이",
    "에프",
    "지",
    "에이치",
    "아이",
    "제이",
    "케이",
    "엘",
    "엠",
    "엔",
    "오",
    "피",
    "큐",
    "알",
    "에스",
    "티",
    "유",
    "브이",
    "더블유",
    "엑스",
    "와이",
    "지",
];
/// Counters read with native numbers below 100 ("3개" -> "세 개").
const NATIVE_COUNTERS: &[&str] = &[
    "개", "명", "번", "대", "장", "권", "마리", "살", "시간", "가지", "곳", "줄", "건", "통", "잔",
    "벌", "켤레", "군데",
];
const NATIVE_ONES: [&str; 10] = [
    "", "한", "두", "세", "네", "다섯", "여섯", "일곱", "여덟", "아홉",
];
const NATIVE_TENS: [&str; 10] = [
    "", "열", "스물", "서른", "마흔", "쉰", "예순", "일흔", "여든", "아흔",
];
const NATIVE_TWENTY: &str = "스무";
/// File extensions read letter by letter after "점".
const EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "py", "md", "json", "exe", "msi", "toml", "sh", "ps1",
    "yml", "yaml", "txt", "log", "html", "css", "swift", "kt", "java", "go", "c", "cpp", "h",
    "apk", "ipa", "dmg", "zip", "tar", "gz", "csv", "xml", "pdf", "png", "jpg", "svg", "wav",
    "mp3", "lock", "env", "cfg", "ini", "plist",
];
/// Digit runs at least this long (without commas) are read digit by digit.
const SPELLED_RUN: usize = 8;
/// Longest digit run read as one number (below 10^16).
const MAX_NUMBER_DIGITS: usize = 16;
/// Characters of a hash that are spelled; the rest says nothing to a listener.
const HASH_SPELLED: usize = 6;
/// Shortest hexadecimal run treated as a hash.
const HASH_MIN: usize = 7;
/// Longest file extension.
const MAX_EXTENSION: usize = 5;

fn is_hangul(c: char) -> bool {
    ('가'..='힣').contains(&c)
}

fn at(c: &[char], i: usize) -> Option<char> {
    c.get(i).copied()
}

fn before(c: &[char], i: usize) -> Option<char> {
    i.checked_sub(1).and_then(|p| at(c, p))
}

fn alnum(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric())
}

fn digit(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_digit())
}

fn letter(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphabetic())
}

/// The end of the run starting at `i` whose characters satisfy `f`.
fn run_end(c: &[char], i: usize, f: impl Fn(char) -> bool) -> usize {
    let mut k = i;
    while k < c.len() && f(c[k]) {
        k += 1;
    }
    k
}

fn skip_ws(c: &[char], i: usize) -> usize {
    run_end(c, i, char::is_whitespace)
}

fn push_str(out: &mut Vec<char>, s: &str) {
    out.extend(s.chars());
}

fn parse(digits: &[char]) -> u64 {
    digits
        .iter()
        .filter_map(|c| c.to_digit(10))
        .fold(0, |n, d| n * 10 + u64::from(d))
}

/// Sino-Korean reading of a non-negative integer below 10^16.
fn sino(mut n: u64) -> String {
    if n == 0 {
        return DIGITS[0].to_owned();
    }
    let mut out = Vec::new();
    let mut group = 0;
    while n > 0 {
        let part = n % 10_000;
        n /= 10_000;
        if part > 0 {
            let mut words = String::new();
            for i in (0..4u32).rev() {
                let d = (part / 10u64.pow(i)) % 10;
                if d > 0 {
                    if d != 1 || i == 0 {
                        words.push_str(DIGITS[d as usize]);
                    }
                    words.push_str(SMALL_UNITS[i as usize]);
                }
            }
            if group == 1 && part == 1 {
                words.clear(); // "만" (10,000), not "일만" (one 10,000)
            }
            words.push_str(BIG_UNITS.get(group).copied().unwrap_or(""));
            out.push(words);
        }
        group += 1;
    }
    out.reverse();
    out.join(" ")
}

/// Native Korean number before a counter, 1..=99.
fn native(n: u64) -> Option<String> {
    match n {
        20 => Some(NATIVE_TWENTY.to_owned()),
        1..=99 => Some(NATIVE_TENS[(n / 10) as usize].to_owned() + NATIVE_ONES[(n % 10) as usize]),
        _ => None,
    }
}

fn spell_digits(s: &[char]) -> String {
    s.iter()
        .filter_map(|c| c.to_digit(10))
        .map(|d| DIGITS[d as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

fn spell_letters(s: &[char]) -> String {
    s.iter()
        .filter_map(|&c| {
            if let Some(d) = c.to_digit(10).filter(|_| c.is_ascii_digit()) {
                Some(DIGITS[d as usize])
            } else if c.is_ascii_alphabetic() {
                Some(LETTERS[usize::from(c.to_ascii_lowercase() as u8 - b'a')])
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One run of digits (commas allowed), given the characters right after it.
fn read_number(s: &[char], after: &[char]) -> String {
    let plain: Vec<char> = s.iter().copied().filter(|&c| c != ',').collect();
    if plain.is_empty() {
        return s.iter().collect();
    }
    let has_comma = plain.len() != s.len();
    // IDs, dates, phone numbers: digit by digit.
    if (plain.len() >= SPELLED_RUN && !has_comma)
        || (plain[0] == '0' && plain.len() > 1)
        || plain.len() > MAX_NUMBER_DIGITS
    {
        return spell_digits(&plain);
    }
    let n = parse(&plain);
    let after: String = after.iter().collect();
    if NATIVE_COUNTERS.iter().any(|c| after.starts_with(c)) {
        return native(n).unwrap_or_else(|| sino(n)) + " ";
    }
    sino(n)
}

/// Runs `f` at every position; `f` returns the end of a span it replaced
/// (after pushing the replacement), or `None` to copy one character.
fn pass(
    c: &[char],
    mut f: impl FnMut(&[char], usize, &mut Vec<char>) -> Option<usize>,
) -> Vec<char> {
    let mut out = Vec::with_capacity(c.len() + 8);
    let mut i = 0;
    while i < c.len() {
        match f(c, i, &mut out) {
            Some(end) if end > i => i = end,
            _ => {
                out.push(c[i]);
                i += 1;
            }
        }
    }
    out
}

/// List markers at line starts: "- ", "• ", "1. ", "2) ".
fn bullets(c: &[char]) -> Vec<char> {
    pass(c, |c, i, _| {
        if i > 0 && c[i - 1] != '\n' {
            return None;
        }
        let j = skip_ws(c, i);
        let mark_end = match at(c, j)? {
            '-' | '•' | '·' => j + 1,
            d if d.is_ascii_digit() => {
                let k = run_end(c, j, |c| c.is_ascii_digit());
                matches!(at(c, k), Some('.' | ')')).then_some(k + 1)?
            }
            _ => return None,
        };
        let end = skip_ws(c, mark_end);
        (end > mark_end).then_some(end)
    })
}

/// The length of an arrow starting at `i`.
fn arrow_len(c: &[char], i: usize) -> Option<usize> {
    match (at(c, i)?, at(c, i + 1)) {
        ('-' | '=', Some('>')) => Some(2),
        ('➡', Some('\u{FE0F}')) => Some(2),
        ('→' | '⇒' | '➜' | '➡' | '⟶', _) => Some(1),
        _ => None,
    }
}

/// Arrows, with the spaces around them, become a pause.
fn arrows(c: &[char]) -> Vec<char> {
    pass(c, |c, i, out| {
        let j = skip_ws(c, i);
        match arrow_len(c, j) {
            Some(len) => {
                push_str(out, ", ");
                Some(skip_ws(c, j + len))
            }
            None => {
                out.extend_from_slice(&c[i..j]);
                Some(j)
            }
        }
    })
}

/// Parentheses become pauses; markdown marks become spaces.
fn brackets_and_markdown(c: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(c.len() + 8);
    let mut in_mark = false;
    for &ch in c {
        if "*`#~|<>[]{}^".contains(ch) {
            if !in_mark {
                out.push(' ');
            }
            in_mark = true;
            continue;
        }
        in_mark = false;
        if matches!(ch, '(' | ')' | '（' | '）') {
            push_str(&mut out, ", ");
        } else {
            out.push(ch);
        }
    }
    out
}

/// " / " becomes a pause; spaces before a colon go.
fn slashes_and_colons(c: &[char]) -> Vec<char> {
    let slashes = pass(c, |c, i, out| {
        if !c[i].is_whitespace() {
            return None;
        }
        let j = skip_ws(c, i);
        if at(c, j) == Some('/') {
            let k = skip_ws(c, j + 1);
            if k > j + 1 {
                push_str(out, ", ");
                return Some(k);
            }
        }
        out.extend_from_slice(&c[i..j]);
        Some(j)
    });
    pass(&slashes, |c, i, out| {
        if !c[i].is_whitespace() {
            return None;
        }
        let j = skip_ws(c, i);
        if at(c, j) != Some(':') {
            out.extend_from_slice(&c[i..j]);
        }
        Some(j)
    })
}

/// A hexadecimal run of 7 or more with a digit and a letter: its first six
/// characters, spelled. ASCII edges only: "e25f95a9가" is a hash too.
fn hashes(c: &[char]) -> Vec<char> {
    pass(c, |c, i, out| {
        if alnum(before(c, i)) || !c[i].is_ascii_hexdigit() {
            return None;
        }
        let end = run_end(c, i, |c| c.is_ascii_hexdigit());
        let run = &c[i..end];
        let hash = run.len() >= HASH_MIN
            && !alnum(at(c, end))
            && run.iter().any(char::is_ascii_digit)
            && run.iter().any(char::is_ascii_alphabetic);
        if !hash {
            return None;
        }
        push_str(out, &spell_letters(&run[..HASH_SPELLED]));
        Some(end)
    })
}

/// "questions.rs" -> "questions 점 알 에스" for the known extensions.
fn file_names(c: &[char]) -> Vec<char> {
    pass(c, |c, i, out| {
        if alnum(before(c, i)) || !c[i].is_ascii_alphabetic() {
            return None;
        }
        let dot = run_end(c, i, |c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if at(c, dot) != Some('.') {
            return None;
        }
        let end = run_end(c, dot + 1, |c| c.is_ascii_alphanumeric());
        if !(1..=MAX_EXTENSION).contains(&(end - dot - 1)) {
            return None;
        }
        let ext = &c[dot + 1..end];
        let lower: String = ext.iter().map(char::to_ascii_lowercase).collect();
        if EXTENSIONS.contains(&lower.as_str()) {
            out.extend_from_slice(&c[i..dot]);
            push_str(out, " 점 ");
            push_str(out, &spell_letters(ext));
        } else {
            out.extend_from_slice(&c[i..end]);
        }
        Some(end)
    })
}

/// Word separators and joins become spaces: "dev/m0-foundation" ->
/// "dev m0 foundation", "QuestionForm" -> "Question Form" (not "iPhone"),
/// "m0" -> "m 0".
fn split_words(c: &[char]) -> Vec<char> {
    let seps: Vec<char> = (0..c.len())
        .map(|i| {
            let (p, n) = (before(c, i), at(c, i + 1));
            match c[i] {
                '/' | '\\' | '_' if alnum(p) && alnum(n) => ' ',
                _ => c[i],
            }
        })
        .collect();
    let c = &seps;
    let hyphens: Vec<char> = (0..c.len())
        .map(|i| {
            let (p, n) = (before(c, i), at(c, i + 1));
            if c[i] == '-' && alnum(p) && alnum(n) && (letter(p) || letter(n)) {
                ' '
            } else {
                c[i]
            }
        })
        .collect();
    let c = &hyphens;
    let mut camel = Vec::with_capacity(c.len() + 8);
    for i in 0..c.len() {
        if i >= 2
            && c[i - 2].is_ascii_lowercase()
            && c[i - 1].is_ascii_lowercase()
            && c[i].is_ascii_uppercase()
        {
            camel.push(' ');
        }
        camel.push(c[i]);
    }
    let c = &camel;
    let mut out = Vec::with_capacity(c.len() + 8);
    for i in 0..c.len() {
        let (p, here) = (before(c, i), Some(c[i]));
        if (letter(p) && digit(here)) || (digit(p) && letter(here)) {
            out.push(' ');
        }
        out.push(c[i]);
    }
    out
}

/// "2.5" -> "이 점 오", "1.2.3" -> "일 점 이 점 삼".
fn decimals(c: &[char]) -> Vec<char> {
    pass(c, |c, i, out| {
        let p = before(c, i);
        if !c[i].is_ascii_digit() || digit(p) || p == Some('.') {
            return None;
        }
        let mut k = run_end(c, i, |c| c.is_ascii_digit());
        let mut groups = 0;
        while at(c, k) == Some('.') && digit(at(c, k + 1)) {
            k = run_end(c, k + 1, |c| c.is_ascii_digit());
            groups += 1;
        }
        if groups == 0 || digit(at(c, k)) || at(c, k) == Some('.') {
            return None;
        }
        let parts: Vec<&[char]> = c[i..k].split(|&c| c == '.').collect();
        let read = if let [whole, fraction] = parts.as_slice() {
            let whole = if whole.len() < 9 {
                sino(parse(whole))
            } else {
                spell_digits(whole)
            };
            format!("{whole} 점 {}", spell_digits(fraction))
        } else {
            parts
                .iter()
                .map(|p| {
                    if p.len() < 5 {
                        sino(parse(p))
                    } else {
                        spell_digits(p)
                    }
                })
                .collect::<Vec<_>>()
                .join(" 점 ")
        };
        push_str(out, &read);
        Some(k)
    })
}

/// "3-5" -> "3 다시 5".
fn dashes(c: &[char]) -> Vec<char> {
    pass(c, |c, i, out| {
        if c[i] == '-' && digit(before(c, i)) && digit(at(c, i + 1)) {
            push_str(out, " 다시 ");
            return Some(i + 1);
        }
        None
    })
}

/// "98 %" -> "98 퍼센트". Runs before the decimals, which leave no digit
/// before the sign ("2.5%" -> "이 점 오 퍼센트").
fn percent(c: &[char]) -> Vec<char> {
    pass(c, |c, i, out| {
        if !digit(before(c, i)) {
            return None;
        }
        let j = skip_ws(c, i);
        if at(c, j) != Some('%') {
            return None;
        }
        push_str(out, " 퍼센트");
        Some(j + 1)
    })
}

/// The end of "1,234,567" style digits starting at `i`, if it is one.
fn grouped_end(c: &[char], i: usize, first_end: usize) -> Option<usize> {
    if first_end - i > 3 {
        return None;
    }
    let mut ends = Vec::new();
    let mut k = first_end;
    while at(c, k) == Some(',') && (1..=3).all(|d| digit(at(c, k + d))) {
        k += 4;
        ends.push(k);
    }
    let last = *ends.last()?;
    if !digit(at(c, last)) {
        return Some(last);
    }
    // One group fewer: what follows it is a comma.
    ends.len().checked_sub(2).map(|n| ends[n])
}

/// Digit runs read as Korean numbers.
fn numbers(c: &[char]) -> Vec<char> {
    pass(c, |c, i, out| {
        if !c[i].is_ascii_digit() {
            return None;
        }
        let first_end = run_end(c, i, |c| c.is_ascii_digit());
        let end = grouped_end(c, i, first_end).unwrap_or(first_end);
        let after = &c[end..(end + 3).min(c.len())];
        push_str(out, &read_number(&c[i..end], after));
        Some(end)
    })
}

/// Tidies the pauses: no repeated commas, none before a comma's spaces,
/// none at the start, after a sentence end or before one.
fn tidy_commas(c: &[char]) -> Vec<char> {
    let repeated = pass(c, |c, i, out| {
        let j = skip_ws(c, i);
        if at(c, j) != Some(',') {
            out.extend_from_slice(&c[i..j]);
            return Some(j);
        }
        let mut k = skip_ws(c, j + 1);
        let mut commas = 1;
        while at(c, k) == Some(',') {
            k = skip_ws(c, k + 1);
            commas += 1;
        }
        if commas < 2 {
            out.extend_from_slice(&c[i..j]);
            return Some(j);
        }
        push_str(out, ", ");
        Some(k)
    });
    let spaced = pass(&repeated, |c, i, out| {
        if !c[i].is_whitespace() {
            return None;
        }
        let j = skip_ws(c, i);
        if at(c, j) != Some(',') {
            out.extend_from_slice(&c[i..j]);
        }
        Some(j)
    });
    let after_end = pass(&spaced, |c, i, out| {
        if i == 0 && c[0] == ',' {
            return Some(skip_ws(c, 1));
        }
        if !matches!(c[i], '.' | '!' | '?') {
            return None;
        }
        let j = skip_ws(c, i + 1);
        if at(c, j) != Some(',') {
            return None;
        }
        // The sentence end and one space ("요?, Yes" -> "요? Yes", Korean polite ending).
        out.extend_from_slice(&c[i..j]);
        out.push(' ');
        Some(skip_ws(c, j + 1))
    });
    pass(&after_end, |c, i, out| {
        let j = skip_ws(c, i);
        if at(c, j) == Some(',') {
            let k = skip_ws(c, j + 1);
            if k == c.len() || matches!(c[k], '.' | '!' | '?') {
                return Some(k);
            }
        }
        out.extend_from_slice(&c[i..j]);
        Some(j)
    })
}

/// Text for a Korean voice: hashes, file names, paths, digits, markdown and
/// arrows rewritten so a multilingual voice reads them in Korean. Only text
/// with Hangul is changed, and the result is never empty (the input comes
/// back instead).
pub fn korean_speakable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_hangul) {
        return Cow::Borrowed(text);
    }
    let c: Vec<char> = text.chars().collect();
    let c = bullets(&c);
    let c = arrows(&c);
    let c = brackets_and_markdown(&c);
    let c = slashes_and_colons(&c);
    let c = hashes(&c);
    let c = file_names(&c);
    let c = split_words(&c);
    let c = percent(&c);
    let c = decimals(&c);
    let c = dashes(&c);
    let c = numbers(&c);
    let c = tidy_commas(&c);
    let mut out = String::with_capacity(c.len() * 3);
    let mut blank = false;
    for ch in c {
        if ch == ' ' || ch == '\t' {
            if !blank {
                out.push(' ');
            }
            blank = true;
        } else {
            out.push(ch);
            blank = false;
        }
    }
    let out = out.trim();
    if out.is_empty() || out == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(out.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(text: &str) -> String {
        korean_speakable(text).into_owned()
    }

    #[test]
    fn native_numbers_before_counters() {
        assert_eq!(k("파일 3개"), "파일 세 개");
        assert_eq!(k("21개"), "스물한 개");
        assert_eq!(k("20개"), "스무 개");
        assert_eq!(k("99명"), "아흔아홉 명");
        assert_eq!(k("1737개"), "천칠백삼십칠 개");
        assert_eq!(k("2켤레"), "두 켤레");
        assert_eq!(k("오후 3시"), "오후 삼시");
    }

    #[test]
    fn sino_numbers() {
        assert_eq!(k("10000 건너"), "만 건너");
        assert_eq!(k("10,000원"), "만원");
        assert_eq!(k("1,234,567원"), "백이십삼만 사천오백육십칠원");
        assert_eq!(k("100000000원"), "일 영 영 영 영 영 영 영 영원");
        assert_eq!(k("1,0000,000원"), "일,영 영 영 영,영 영 영원");
        assert_eq!(k("2026년 11월 8일"), "이천이십육년 십일월 팔일");
        assert_eq!(k("0원"), "영원");
        assert_eq!(sino(10_000), "만");
        assert_eq!(sino(110_000), "십일만");
        assert_eq!(sino(100_010_000), "일억 만");
        assert_eq!(sino(1_0000_0000_0000), "일조");
    }

    #[test]
    fn long_runs_and_leading_zeros_are_digit_by_digit() {
        assert_eq!(k("번호 12345678"), "번호 일 이 삼 사 오 육 칠 팔");
        assert_eq!(k("번호 1234567"), "번호 백이십삼만 사천오백육십칠");
        assert_eq!(k("코드 007"), "코드 영 영 칠");
        assert_eq!(
            k("전화 010-1234-5678"),
            "전화 영 일 영 다시 천이백삼십사 다시 오천육백칠십팔"
        );
    }

    #[test]
    fn decimals_and_versions() {
        assert_eq!(k("2.5초"), "이 점 오초");
        assert_eq!(k("0.25배"), "영 점 이 오배");
        assert_eq!(k("버전 1.2.3"), "버전 일 점 이 점 삼");
        assert_eq!(k("버전 1.2.3."), "버전 일.이.삼.");
    }

    #[test]
    fn percent() {
        assert_eq!(k("성공률 98%"), "성공률 구십팔 퍼센트");
        assert_eq!(k("성공률 98 %"), "성공률 구십팔 퍼센트");
        assert_eq!(k("확률 2.5%"), "확률 이 점 오 퍼센트");
    }

    #[test]
    fn hashes_spell_six_characters() {
        assert_eq!(k("SHA 값 e25f95a9가"), "SHA 값 이 이 오 에프 구 오가");
        assert_eq!(k("커밋 b897fc8"), "커밋 비 팔 구 칠 에프 씨");
        // Not hashes: no letter, no digit, too short, inside a word.
        assert_eq!(k("커밋 deadbeef"), "커밋 deadbeef");
        assert_eq!(k("커밋 abc123"), "커밋 abc 백이십삼");
        assert_eq!(k("커밋 xe25f95a9"), "커밋 xe 이십오 f 구십오 a 구");
    }

    #[test]
    fn file_extensions_are_spelled() {
        assert_eq!(k("questions.rs와"), "questions 점 알 에스와");
        assert_eq!(k("setup.exe 서명"), "setup 점 이 엑스 이 서명");
        assert_eq!(k("tts_server.py는"), "tts server 점 피 와이는");
        assert_eq!(k("QuestionForm.tsx를"), "Question Form 점 티 에스 엑스를");
        // Unknown extensions stay.
        assert_eq!(k("example.com 접속"), "example.com 접속");
    }

    #[test]
    fn camel_case_splits_but_not_iphone() {
        assert_eq!(k("QuestionForm 수정"), "Question Form 수정");
        assert_eq!(k("iPhone 설치"), "iPhone 설치");
        assert_eq!(k("GitHub 확인"), "Git Hub 확인");
        assert_eq!(
            k("브랜치 dev/m0-foundation에"),
            "브랜치 dev m 영 foundation에"
        );
    }

    #[test]
    fn arrows_markdown_and_bullets_go() {
        assert_eq!(k("빌드 -> 설치 → 완료"), "빌드, 설치, 완료");
        assert_eq!(k("**결과 요약**: 끝"), "결과 요약: 끝");
        assert_eq!(k("`cargo` 실행 ## 제목"), "cargo 실행 제목");
        assert_eq!(
            k("- 첫째 항목\n- 둘째 항목 21개"),
            "첫째 항목\n둘째 항목 스물한 개"
        );
        assert_eq!(k("1. 첫째\n2) 둘째"), "첫째\n둘째");
        assert_eq!(k("• 하나"), "하나");
    }

    #[test]
    fn slashes_become_pauses() {
        assert_eq!(k("예 / 아니오"), "예, 아니오");
        assert_eq!(
            k("허용하시겠어요? (Yes / No / Always allow)"),
            "허용하시겠어요? Yes, No, Always allow"
        );
        assert_eq!(k("(참고) 완료 ()."), "참고, 완료.");
    }

    #[test]
    fn text_without_hangul_is_unchanged() {
        for text in [
            "Hello world",
            "build 12 -> e25f95a9",
            "questions.rs",
            "",
            "  ",
        ] {
            assert!(matches!(korean_speakable(text), Cow::Borrowed(t) if t == text));
        }
        assert!(matches!(korean_speakable("안녕하세요."), Cow::Borrowed(_)));
    }

    #[test]
    fn never_empty() {
        assert_eq!(k("가"), "가");
        // Only marks around a Hangul-free remainder still keep some text.
        assert!(!k("** 가 **").is_empty());
    }

    #[test]
    fn reference_samples() {
        let cases = [
            (
                "빌드 서버에서 setup.exe 서명이 끝났습니다. SHA 값 e25f95a9가 일치하고 Defender 검사도 clean입니다.",
                "빌드 서버에서 setup 점 이 엑스 이 서명이 끝났습니다. SHA 값 이 이 오 에프 구 오가 일치하고 Defender 검사도 clean입니다.",
            ),
            (
                "questions.rs와 QuestionForm.tsx를 고쳤고, cargo test 1702개와 vitest 1737개가 모두 pass했습니다.",
                "questions 점 알 에스와 Question Form 점 티 에스 엑스를 고쳤고, cargo test 천칠백이 개와 vitest 천칠백삼십칠 개가 모두 pass했습니다.",
            ),
            (
                "Supertonic 엔진에 language 필드를 넘기도록 바꿨습니다. tts_server.py는 그대로 두고 vcad만 다시 빌드합니다.",
                "Supertonic 엔진에 language 필드를 넘기도록 바꿨습니다. tts server 점 피 와이는 그대로 두고 vcad만 다시 빌드합니다.",
            ),
            (
                "브랜치 dev/m0-foundation에 커밋하지 않은 변경이 있습니다. git status로 확인한 뒤 PR을 만들까요?",
                "브랜치 dev m 영 foundation에 커밋하지 않은 변경이 있습니다. git status로 확인한 뒤 PR을 만들까요?",
            ),
            (
                "**결과 요약**: build 12 (20261108-1642) → iPhone, Android tablet, Mac 모두 설치 완료 ✅",
                "결과 요약: build 십이, 이 영 이 육 일 일 영 팔 다시 천육백사십이, iPhone, Android tablet, Mac 모두 설치 완료 ✅",
            ),
            (
                "에이전트가 Bash로 npm run build를 실행하려고 합니다. 허용하시겠어요? (Yes / No / Always allow)",
                // The reference drops the space after "?" ("요?Yes").
                "에이전트가 Bash로 npm run build를 실행하려고 합니다. 허용하시겠어요? Yes, No, Always allow",
            ),
            (
                "파일 3개, 테스트 25개, 2.5초, 버전 1.2.3, 성공률 98%, 10,000원, 2026년 11월 8일 오후 3시",
                "파일 세 개, 테스트 스물다섯 개, 이 점 오초, 버전 일 점 이 점 삼, 성공률 구십팔 퍼센트, 만원, 이천이십육년 십일월 팔일 오후 삼시",
            ),
            ("Hello world", "Hello world"),
            (
                "- 첫째 항목\n- 둘째 항목 21개",
                "첫째 항목\n둘째 항목 스물한 개",
            ),
        ];
        for (text, want) in cases {
            assert_eq!(k(text), want, "{text}");
        }
    }
}
