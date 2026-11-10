//! Quoting for PowerShell command text that VCA builds.

/// The characters PowerShell treats as a single quote: the ASCII apostrophe
/// and the typographic quotes U+2018, U+2019, U+201A and U+201B. Any of them
/// ends a single-quoted string, so all of them are doubled inside one.
pub const SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// `text` as a PowerShell single-quoted string literal. Inside such a
/// string nothing is expanded; the only special characters are the single
/// quotes ([`SINGLE_QUOTES`]), each escaped by doubling it.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        if SINGLE_QUOTES.contains(&c) {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Whether `c` should not appear in a title shown in, or passed through, a
/// terminal: control characters, line and paragraph separators, and the
/// invisible format characters (bidirectional overrides, zero-width
/// characters, the byte order mark) that can disguise what a title says,
/// and the other characters that render as nothing or as blank space
/// (Unicode default-ignorable code points, Hangul fillers, the Braille
/// blank), which can push the real text out of sight. The emoji and text
/// presentation selectors (U+FE0E, U+FE0F) stay: plain emoji need them.
pub fn is_unsafe_title_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{2800}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0D}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a quoted literal the way PowerShell's tokenizer does: returns
    /// the string value and whatever follows the closing quote.
    fn parse(literal: &str) -> (String, &str) {
        let mut chars = literal.char_indices().peekable();
        let (_, first) = chars.next().unwrap();
        assert!(SINGLE_QUOTES.contains(&first));
        let mut value = String::new();
        while let Some((i, c)) = chars.next() {
            if SINGLE_QUOTES.contains(&c) {
                match chars.peek() {
                    Some(&(_, n)) if SINGLE_QUOTES.contains(&n) => {
                        chars.next();
                        value.push(c);
                    }
                    _ => return (value, &literal[i + c.len_utf8()..]),
                }
            } else {
                value.push(c);
            }
        }
        panic!("unterminated literal {literal:?}");
    }

    #[test]
    fn plain_text_is_wrapped() {
        assert_eq!(quote("Payments"), "'Payments'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn every_single_quote_is_doubled() {
        assert_eq!(quote("it's"), "'it''s'");
        assert_eq!(quote("Bob\u{2019}s fix"), "'Bob\u{2019}\u{2019}s fix'");
        for q in SINGLE_QUOTES {
            let text = format!("a{q}; Remove-Item x; {q}b");
            let quoted = quote(&text);
            let (value, rest) = parse(&quoted);
            assert_eq!(value, text);
            assert_eq!(rest, "");
        }
    }

    #[test]
    fn mixed_quotes_cannot_break_out() {
        let text = "\u{2018}x\u{201B}'\u{201A}\u{2019}$(calc)\"`";
        let quoted = quote(text);
        let (value, rest) = parse(&quoted);
        assert_eq!(value, text);
        assert_eq!(rest, "");
    }

    #[test]
    fn invisible_and_control_characters_are_unsafe() {
        for c in [
            '\n', '\r', '\u{7}', '\u{202E}', '\u{200B}', '\u{2028}', '\u{FEFF}',
        ] {
            assert!(is_unsafe_title_char(c), "{c:?}");
        }
        for c in ['a', '\'', '\u{2019}', ' ', '\u{D55C}', '\u{FE0F}'] {
            assert!(!is_unsafe_title_char(c), "{c:?}");
        }
        // Blank-looking padding and invisible tags.
        for c in [
            '\u{3164}',
            '\u{FFA0}',
            '\u{115F}',
            '\u{2800}',
            '\u{034F}',
            '\u{FE00}',
            '\u{E0041}',
            '\u{E0100}',
        ] {
            assert!(is_unsafe_title_char(c), "{c:?}");
        }
    }
}
