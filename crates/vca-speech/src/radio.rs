//! Radio-style voice words: the wake word "보코 / Voco", the send word
//! "보내줘 오버 / Over / 送信、オーバー / Cambio" and the reply "로저 / Roger".
//!
//! Two helpers for the recognition groundwork of hands-free v2:
//!
//! - [`prompt_words`]: the words to put in the vocabulary prompt of
//!   prompt-style speech engines (Whisper, OpenAI-compatible APIs), so a
//!   short "보코" (Voco) is not heard as "보고" (report) or "버코". The same
//!   words are in the
//!   base lexicon (`lexicon/<lang>/voice.toml`) for phrase-boosting engines.
//! - [`speakable`]: rewrites the words for a text-to-speech voice of the
//!   language, so a Korean voice says "보코" and "로저" instead of spelling
//!   "Voco" or reading "Roger" letter by letter.
//!
//! Matching spoken wake and send words (with their common mishearings) is
//! done where hands-free runs, in the UI (`apps/ui/src/speech/wakeWord.ts`).

use std::borrow::Cow;

use crate::stt::base_language;

/// Prompt words per language (ISO 639-1), most important first. Korean
/// lists the vocative "보코야" too: a quick "보코" is often written "보컬"
/// (vocal), and showing the engine the "보코" spelling twice tips it towards
/// it. "보컬" itself is never a lexicon variant of "보코": it is a real word
/// that dictation must keep ("보컬 트레이닝 앱", vocal training app); the UI
/// accepts an isolated
/// "보컬" as the wake word instead (`apps/ui/src/speech/wakeWord.ts`).
const PROMPT_WORDS: &[(&str, &[&str])] = &[
    ("ko", &["보코", "보코야", "보내줘 오버", "오버", "로저"]),
    ("en", &["Voco", "Over", "Roger"]),
    ("ja", &["ボコ", "送信、オーバー", "オーバー", "ラジャー"]),
    ("es", &["Voco", "Cambio", "Roger"]),
];

/// Spoken forms of the Latin spellings, per voice language. Keys are lower
/// case; matching ignores case and needs a word boundary on both sides.
const SPOKEN_FORMS: &[(&str, &[(&str, &str)])] = &[
    (
        "ko",
        &[("voco", "보코"), ("roger", "로저"), ("over", "오버")],
    ),
    (
        "ja",
        &[
            ("voco", "ボコ"),
            ("roger", "ラジャー"),
            ("over", "オーバー"),
        ],
    ),
];

/// The radio words for the vocabulary prompt of `language` (BCP-47). Other
/// languages get the English words; an unknown language gets the words of
/// every supported language (Korean first), without duplicates.
pub fn prompt_words(language: Option<&str>) -> Vec<&'static str> {
    let words_of = |lang: &str| {
        PROMPT_WORDS
            .iter()
            .find(|(l, _)| *l == lang)
            .map(|(_, w)| *w)
    };
    match language.filter(|l| !l.trim().is_empty()) {
        Some(l) => {
            let base = base_language(l);
            words_of(&base)
                .or_else(|| words_of("en"))
                .unwrap_or_default()
                .to_vec()
        }
        None => {
            let mut out: Vec<&'static str> = Vec::new();
            for (_, words) in PROMPT_WORDS {
                for w in *words {
                    if !out.iter().any(|o| o.eq_ignore_ascii_case(w)) {
                        out.push(w);
                    }
                }
            }
            out
        }
    }
}

/// Rewrites "Voco", "Roger" and "Over" (any case, whole words) to how a
/// voice of `language` (BCP-47) should say them: "보코", "로저", "오버" for
/// Korean and "ボコ", "ラジャー", "オーバー" for Japanese. Other languages,
/// English included, are returned unchanged.
pub fn speakable<'a>(text: &'a str, language: Option<&str>) -> Cow<'a, str> {
    let Some(base) = language.map(base_language) else {
        return Cow::Borrowed(text);
    };
    let Some((_, forms)) = SPOKEN_FORMS.iter().find(|(l, _)| *l == base) else {
        return Cow::Borrowed(text);
    };
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut pos = 0;
    while pos < text.len() {
        let rest = &text[pos..];
        let at_start = text[..pos].chars().next_back().is_none_or(|c| !is_word(c));
        let hit = at_start
            .then(|| {
                forms.iter().find(|(latin, _)| {
                    rest.len() >= latin.len()
                        && rest.is_char_boundary(latin.len())
                        && rest[..latin.len()].eq_ignore_ascii_case(latin)
                        && rest[latin.len()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !is_word(c))
                })
            })
            .flatten();
        match hit {
            Some((latin, spoken)) => {
                out.push_str(spoken);
                pos += latin.len();
                changed = true;
            }
            None => {
                let c = rest.chars().next().unwrap_or_default();
                out.push(c);
                pos += c.len_utf8().max(1);
            }
        }
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_words_per_language() {
        assert_eq!(
            prompt_words(Some("ko-KR")),
            ["보코", "보코야", "보내줘 오버", "오버", "로저"]
        );
        assert_eq!(prompt_words(Some("en")), ["Voco", "Over", "Roger"]);
        assert_eq!(prompt_words(Some("ja")).first(), Some(&"ボコ"));
        assert!(prompt_words(Some("ja")).contains(&"送信、オーバー"));
        assert_eq!(prompt_words(Some("es-MX")), ["Voco", "Cambio", "Roger"]);
        // Other languages use the English words.
        assert_eq!(prompt_words(Some("de")), ["Voco", "Over", "Roger"]);
    }

    #[test]
    fn prompt_words_without_a_language_cover_all_and_stay_short() {
        let all = prompt_words(None);
        assert_eq!(all[0], "보코");
        for w in [
            "Voco",
            "Over",
            "Roger",
            "ボコ",
            "Cambio",
            "보내줘 오버",
            "보코야",
        ] {
            assert!(all.contains(&w), "{w}");
        }
        // "Voco" and "Roger" are listed once.
        assert_eq!(all.iter().filter(|w| **w == "Voco").count(), 1);
        assert!(all.join(", ").chars().count() <= 100);
        assert_eq!(prompt_words(Some("")), all);
    }

    #[test]
    fn korean_voices_say_the_radio_words_in_hangul() {
        let ko = Some("ko-KR");
        assert_eq!(speakable("Voco가 준비됐어요", ko), "보코가 준비됐어요");
        assert_eq!(speakable("VOCO, Roger.", ko), "보코, 로저.");
        assert_eq!(
            speakable("보내려면 Over라고 말하세요", ko),
            "보내려면 오버라고 말하세요"
        );
        assert_eq!(speakable("roger that", ko), "로저 that");
        // Whole words only.
        assert_eq!(speakable("overflow를 고쳐", ko), "overflow를 고쳐");
        assert_eq!(speakable("Vocoder 설정", ko), "Vocoder 설정");
        assert_eq!(speakable("my_over 변수", ko), "my_over 변수");
        assert_eq!(speakable("보코, 로저", ko), "보코, 로저");
        assert!(matches!(speakable("그대로", ko), Cow::Borrowed(_)));
    }

    #[test]
    fn japanese_voices_use_katakana_and_english_is_unchanged() {
        assert_eq!(speakable("Voco, Roger", Some("ja")), "ボコ, ラジャー");
        assert_eq!(
            speakable("Voco, Roger. Over", Some("en-US")),
            "Voco, Roger. Over"
        );
        assert_eq!(speakable("Voco, Roger", Some("es")), "Voco, Roger");
        assert_eq!(speakable("Voco, Roger", None), "Voco, Roger");
    }
}
