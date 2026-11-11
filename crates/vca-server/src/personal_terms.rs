//! The rules of the "commonly misheard words" list (see
//! `vca_core::personal_terms`): what a saved list may contain, and how it
//! maps onto the `TermEntry` records of `personal.json`.

use std::collections::HashMap;

use vca_core::error::ErrorCode;
use vca_core::{
    Error, MAX_PERSONAL_TERM_CHARS, MAX_PERSONAL_TERMS, MAX_PERSONAL_VARIANTS, PersonalTerm,
};
use vca_speech::lexicon::{TermEntry, Variant};
use vca_speech::text::normalize;

fn invalid(message: String) -> Error {
    Error::api(ErrorCode::InvalidRequest, message)
}

/// `text` trimmed, or why it cannot be stored.
fn clean(text: &str, what: &str) -> Result<String, Error> {
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid(format!("{what} is empty")));
    }
    if text.chars().any(char::is_control) {
        return Err(invalid(format!("{what} has control characters")));
    }
    if text.chars().count() > MAX_PERSONAL_TERM_CHARS {
        return Err(invalid(format!(
            "{what} is longer than {MAX_PERSONAL_TERM_CHARS} characters"
        )));
    }
    Ok(text.to_owned())
}

/// Checks a list sent by a client: bounded count and lengths, no control
/// characters, no duplicates. A misheard form of one character is refused
/// (it would rewrite that letter everywhere), and so is one that points at
/// two terms. A form that only differs in case from its term is allowed; it
/// fixes the capitals.
pub(crate) fn validate(terms: Vec<PersonalTerm>) -> Result<Vec<PersonalTerm>, Error> {
    if terms.len() > MAX_PERSONAL_TERMS {
        return Err(invalid(format!(
            "the list holds at most {MAX_PERSONAL_TERMS} words"
        )));
    }
    let mut seen_terms: HashMap<String, ()> = HashMap::new();
    let mut seen_forms: HashMap<String, ()> = HashMap::new();
    let mut out = Vec::with_capacity(terms.len());
    for entry in terms {
        let term = clean(&entry.term, "a word")?;
        if seen_terms.insert(normalize(&term), ()).is_some() {
            return Err(invalid(format!("{term:?} is in the list twice")));
        }
        if entry.variants.len() > MAX_PERSONAL_VARIANTS {
            return Err(invalid(format!(
                "a word holds at most {MAX_PERSONAL_VARIANTS} misheard forms"
            )));
        }
        let mut variants = Vec::with_capacity(entry.variants.len());
        for variant in entry.variants {
            let variant = clean(&variant, "a misheard form")?;
            let key = normalize(&variant);
            if key.chars().count() < 2 {
                return Err(invalid(format!(
                    "the misheard form {variant:?} is too short"
                )));
            }
            if variant == term {
                return Err(invalid(format!(
                    "the misheard form {variant:?} is the word itself"
                )));
            }
            if seen_forms.insert(key, ()).is_some() {
                return Err(invalid(format!(
                    "the misheard form {variant:?} is in the list twice"
                )));
            }
            variants.push(variant);
        }
        out.push(PersonalTerm { term, variants });
    }
    Ok(out)
}

/// What the screen shows of the stored entries.
pub(crate) fn from_entries(entries: &[TermEntry]) -> Vec<PersonalTerm> {
    entries
        .iter()
        .map(|e| PersonalTerm {
            term: e.term.clone(),
            variants: e.variants.iter().map(|v| v.text.clone()).collect(),
        })
        .collect()
}

/// The entries of a saved list. The screen edits spellings only, so what a
/// stored entry says beyond them (category, tags, weight, the language of a
/// form) stays. New entries leave `match_term` unset: only distinctive
/// spellings then rewrite their own casing, and "Go" never changes "go".
pub(crate) fn to_entries(terms: Vec<PersonalTerm>, current: &[TermEntry]) -> Vec<TermEntry> {
    terms
        .into_iter()
        .map(|t| {
            let known = current.iter().find(|e| e.term == t.term);
            let mut entry = known.cloned().unwrap_or_else(|| TermEntry::new(&t.term));
            entry.variants = t
                .variants
                .into_iter()
                .map(|text| {
                    known
                        .and_then(|e| e.variants.iter().find(|v| v.text == text))
                        .cloned()
                        .unwrap_or_else(|| Variant::new(text, None))
                })
                .collect();
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(term: &str, variants: &[&str]) -> PersonalTerm {
        PersonalTerm {
            term: term.into(),
            variants: variants.iter().map(|v| (*v).to_owned()).collect(),
        }
    }

    fn refused(terms: Vec<PersonalTerm>) -> String {
        let err = validate(terms).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        err.to_string()
    }

    #[test]
    fn trims_and_keeps_good_lists() {
        let out = validate(vec![
            term("  SkyHarbor ", &[" sky harbor", "스카이 하버"]),
            term("GitHub", &["github"]),
            term("Hint", &[]),
        ])
        .unwrap();
        assert_eq!(out[0], term("SkyHarbor", &["sky harbor", "스카이 하버"]));
        assert_eq!(out[1], term("GitHub", &["github"]));
        assert_eq!(out[2], term("Hint", &[]));
        assert!(validate(Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn refuses_empty_control_and_long_text() {
        assert!(refused(vec![term(" ", &[])]).contains("empty"));
        assert!(refused(vec![term("a", &["  "])]).contains("empty"));
        assert!(refused(vec![term("a\u{7}b", &[])]).contains("control"));
        assert!(refused(vec![term("ab", &["c\nd"])]).contains("control"));
        let long = "가".repeat(MAX_PERSONAL_TERM_CHARS + 1);
        assert!(refused(vec![term(&long, &[])]).contains("longer"));
        assert!(refused(vec![term("ab", &[&long])]).contains("longer"));
        let longest = "가".repeat(MAX_PERSONAL_TERM_CHARS);
        assert!(validate(vec![term(&longest, &[])]).is_ok());
    }

    #[test]
    fn refuses_too_many() {
        let many: Vec<PersonalTerm> = (0..=MAX_PERSONAL_TERMS)
            .map(|i| term(&format!("t{i}"), &[]))
            .collect();
        assert!(refused(many).contains("at most"));
        let forms: Vec<String> = (0..=MAX_PERSONAL_VARIANTS)
            .map(|i| format!("v{i}"))
            .collect();
        let forms: Vec<&str> = forms.iter().map(String::as_str).collect();
        assert!(refused(vec![term("ab", &forms)]).contains("at most"));
    }

    #[test]
    fn refuses_duplicates() {
        assert!(refused(vec![term("GitHub", &[]), term(" github", &[])]).contains("twice"));
        assert!(refused(vec![term("ab", &["cd", "CD"])]).contains("twice"));
        assert!(refused(vec![term("ab", &["cd"]), term("ef", &["cd"])]).contains("twice"));
        assert!(refused(vec![term("GitHub", &["GitHub"])]).contains("itself"));
    }

    #[test]
    fn refuses_one_letter_forms() {
        assert!(refused(vec![term("Alpha", &["a"])]).contains("too short"));
        assert!(refused(vec![term("Alpha", &["가"])]).contains("too short"));
    }

    #[test]
    fn saving_keeps_what_the_screen_does_not_show() {
        let current = vec![
            TermEntry {
                tags: vec!["infra".into()],
                ..TermEntry::new("SkyHarbor")
                    .variant("스카이 하버", Some("ko"))
                    .variant("sky harbour", None)
                    .category("product")
                    .weight(3.0)
            },
            TermEntry::new("Gone"),
        ];
        let out = to_entries(
            vec![
                term("SkyHarbor", &["스카이 하버", "sky harbor"]),
                term("Fresh", &["fresh one"]),
            ],
            &current,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].category.as_deref(), Some("product"));
        assert_eq!(out[0].tags, ["infra"]);
        assert_eq!(out[0].weight, 3.0);
        assert_eq!(
            out[0].variants,
            [
                Variant::new("스카이 하버", Some("ko")),
                Variant::new("sky harbor", None)
            ]
        );
        assert_eq!(out[1].match_term, None);
        assert_eq!(out[1].variants, [Variant::new("fresh one", None)]);
        assert_eq!(
            from_entries(&out)[0],
            term("SkyHarbor", &["스카이 하버", "sky harbor"])
        );
    }
}
