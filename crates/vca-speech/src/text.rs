//! Unicode-aware text helpers shared by the lexicon, intent router and
//! clean-up passes.
//!
//! Matching works on a *normalized* copy of the text (NFKC, lowercase,
//! whitespace collapsed) that keeps a mapping back to byte offsets in the
//! original string, so that replacements can be applied to the text the user
//! actually sees.

use unicode_normalization::UnicodeNormalization;

/// Coarse character classes used for tokenization and match boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CharClass {
    /// Whitespace.
    Space,
    /// Letters, digits and `_` of scripts that separate words with spaces
    /// (Latin, Cyrillic, Greek, ...).
    Word,
    /// Chinese characters (also used in Japanese kanji).
    Han,
    /// Japanese hiragana.
    Hiragana,
    /// Japanese katakana, including the prolonged sound mark.
    Katakana,
    /// Korean hangul.
    Hangul,
    /// Punctuation, symbols and everything else.
    Other,
}

impl CharClass {
    /// Classifies one character.
    pub fn of(c: char) -> Self {
        if c.is_whitespace() {
            return Self::Space;
        }
        let cp = c as u32;
        match cp {
            0x3040..=0x309F => Self::Hiragana,
            0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Self::Katakana,
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Self::Hangul,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF => Self::Han,
            _ if c.is_alphanumeric() || c == '_' => Self::Word,
            _ => Self::Other,
        }
    }

    /// True for scripts that do not reliably put spaces between words, so
    /// matches may touch other characters of the same script. Hangul is
    /// included because particles attach directly to the end of nouns (see
    /// [`is_on_boundary`] for the start-of-word rule).
    pub fn is_boundary_free(self) -> bool {
        matches!(
            self,
            Self::Han | Self::Hiragana | Self::Katakana | Self::Hangul
        )
    }

    /// True for any letter-like class (not space, not punctuation).
    pub fn is_lexical(self) -> bool {
        !matches!(self, Self::Space | Self::Other)
    }
}

/// Lowercases and NFKC-normalizes a string and collapses whitespace runs to
/// a single ASCII space, trimming both ends.
pub fn normalize(s: &str) -> String {
    NormalizedText::new(s).text
}

/// A normalized copy of a string with a map back to the original.
#[derive(Debug, Clone)]
pub struct NormalizedText {
    /// The normalized text.
    pub text: String,
    /// For every byte of `text` that starts a char, the byte range in the
    /// original string of the char that produced it. Indexed by char index.
    starts: Vec<(usize, usize)>,
    /// Byte offset in `text` of every char (same indexing as `starts`).
    char_offsets: Vec<usize>,
}

impl NormalizedText {
    /// Builds the normalized text. Normalization is applied per original
    /// character, which is exact for all practical transcript input.
    pub fn new(original: &str) -> Self {
        let mut text = String::with_capacity(original.len());
        let mut starts = Vec::with_capacity(original.len());
        let mut char_offsets = Vec::with_capacity(original.len());
        let mut pending_space: Option<(usize, usize)> = None;
        for (pos, c) in original.char_indices() {
            let range = (pos, pos + c.len_utf8());
            if c.is_whitespace() {
                if !text.is_empty() && pending_space.is_none() {
                    pending_space = Some(range);
                }
                continue;
            }
            if let Some(space) = pending_space.take() {
                char_offsets.push(text.len());
                starts.push(space);
                text.push(' ');
            }
            let mut buf = [0u8; 4];
            for n in c.encode_utf8(&mut buf).nfkc() {
                for l in n.to_lowercase() {
                    if l.is_whitespace() {
                        continue;
                    }
                    char_offsets.push(text.len());
                    starts.push(range);
                    text.push(l);
                }
            }
        }
        Self {
            text,
            starts,
            char_offsets,
        }
    }

    /// Converts a byte range in the normalized text into the corresponding
    /// byte range of the original string.
    pub fn original_range(&self, start: usize, end: usize) -> (usize, usize) {
        let first = self.char_offsets.partition_point(|&o| o < start);
        let last = self.char_offsets.partition_point(|&o| o < end);
        if first >= self.starts.len() || last == 0 {
            return (0, 0);
        }
        (self.starts[first].0, self.starts[last - 1].1)
    }
}

/// A token produced by [`tokenize`]: a maximal run of one character class,
/// or a single punctuation character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<'a> {
    pub text: &'a str,
    pub class: CharClass,
    /// Byte offset in the source string.
    pub start: usize,
}

impl Token<'_> {
    /// Byte offset just past the token.
    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

/// Splits text into script runs. Whitespace is dropped; punctuation becomes
/// one token per character; apostrophes and hyphens inside a word stay part
/// of the word ("don't", "pre-commit").
pub fn tokenize(s: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut chars = s.char_indices().peekable();
    let mut current: Option<(usize, CharClass)> = None;
    let mut prev_end = 0;
    while let Some((pos, c)) = chars.next() {
        let mut class = CharClass::of(c);
        // Keep joiners inside words: "don't", "pre-commit", "node.js".
        if matches!(c, '\'' | '-' | '.' | '’')
            && current.is_some_and(|(_, cl)| cl == CharClass::Word)
            && chars
                .peek()
                .is_some_and(|&(_, n)| CharClass::of(n) == CharClass::Word)
        {
            class = CharClass::Word;
        }
        match current {
            Some((_, cl)) if cl == class && class != CharClass::Other => {}
            Some((start, cl)) => {
                if cl != CharClass::Space {
                    tokens.push(Token {
                        text: &s[start..pos],
                        class: cl,
                        start,
                    });
                }
                current = Some((pos, class));
            }
            None => current = Some((pos, class)),
        }
        prev_end = pos + c.len_utf8();
    }
    if let Some((start, cl)) = current
        && cl != CharClass::Space
    {
        tokens.push(Token {
            text: &s[start..prev_end],
            class: cl,
            start,
        });
    }
    tokens
}

/// Returns true if a match of `[start, end)` inside `text` sits on token
/// boundaries: the characters on each side must not continue a word, except
/// for boundary-free scripts (CJK, Hangul) where a match may touch other
/// characters of the same script.
pub fn is_on_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let first = text[start..end].chars().next();
    let last = text[start..end].chars().next_back();
    let after = text[end..].chars().next();
    let ok = |outside: Option<char>, inside: Option<char>, leading: bool| match (outside, inside) {
        (None, _) | (_, None) => true,
        (Some(o), Some(i)) => {
            let (co, ci) = (CharClass::of(o), CharClass::of(i));
            if leading && co == CharClass::Hangul && ci == CharClass::Hangul {
                // Korean separates words with spaces and attaches particles
                // only at the end, so a match must start a word.
                false
            } else if ci.is_boundary_free() || co.is_boundary_free() {
                true
            } else {
                !(co == CharClass::Word && ci == CharClass::Word)
            }
        }
    };
    ok(before, first, true) && ok(after, last, false)
}

/// Splits an identifier into its words: `useEffect` → `["use", "effect"]`,
/// `HTTPServer` → `["http", "server"]`, `vca_speech` → `["vca", "speech"]`,
/// `react-query` → `["react", "query"]`. Words are lowercased.
pub fn split_identifier(ident: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in ident.split(|c: char| !c.is_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1).copied();
            let boundary = match prev {
                None => false,
                Some(p) => {
                    (c.is_uppercase() && p.is_lowercase())
                        || (c.is_uppercase()
                            && p.is_uppercase()
                            && next.is_some_and(|n| n.is_lowercase()))
                        || (c.is_ascii_digit() != p.is_ascii_digit())
                }
            };
            if boundary && !word.is_empty() {
                words.push(std::mem::take(&mut word).to_lowercase());
            }
            word.push(c);
        }
        if !word.is_empty() {
            words.push(word.to_lowercase());
        }
    }
    words
}

/// The spoken form of an identifier (its words joined with spaces), when it
/// differs from the identifier itself: `useEffect` → `use effect`.
pub fn spoken_form(ident: &str) -> Option<String> {
    let words = split_identifier(ident);
    if words.len() < 2 {
        return None;
    }
    Some(words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_maps_back_to_original() {
        let original = "Run  ＫＵＢＥ control\tnow";
        let n = NormalizedText::new(original);
        assert_eq!(n.text, "run kube control now");
        let start = n.text.find("kube control").unwrap();
        let (a, b) = n.original_range(start, start + "kube control".len());
        assert_eq!(&original[a..b], "ＫＵＢＥ control");
    }

    #[test]
    fn tokenize_splits_scripts() {
        let toks: Vec<_> = tokenize("useEffectを使って, don't stop 세션3으로")
            .into_iter()
            .map(|t| t.text)
            .collect();
        assert_eq!(
            toks,
            vec![
                "useEffect",
                "を",
                "使",
                "って",
                ",",
                "don't",
                "stop",
                "세션",
                "3",
                "으로"
            ]
        );
    }

    #[test]
    fn boundaries() {
        let t = "the kubectl tool";
        let s = t.find("kubectl").unwrap();
        assert!(is_on_boundary(t, s, s + 7));
        let t2 = "xkubectl";
        assert!(!is_on_boundary(t2, 1, 8));
        let t3 = "ユーズエフェクトを使う";
        assert!(is_on_boundary(t3, 0, "ユーズエフェクト".len()));
        // Korean: particles may follow, but a match must start a word.
        let t4 = "인터넷을 재시작";
        let s4 = t4.find("넷").unwrap();
        assert!(!is_on_boundary(t4, s4, s4 + "넷".len()));
        let s5 = t4.find("인터넷").unwrap();
        assert!(is_on_boundary(t4, s5, s5 + "인터넷".len()));
    }

    #[test]
    fn identifier_splitting() {
        assert_eq!(split_identifier("useEffect"), vec!["use", "effect"]);
        assert_eq!(split_identifier("HTTPServer"), vec!["http", "server"]);
        assert_eq!(split_identifier("vca_speech"), vec!["vca", "speech"]);
        assert_eq!(split_identifier("@tanstack/react-query").len(), 3);
        assert_eq!(split_identifier("utf8Decode"), vec!["utf", "8", "decode"]);
        assert_eq!(spoken_form("kubectl"), None);
    }
}
