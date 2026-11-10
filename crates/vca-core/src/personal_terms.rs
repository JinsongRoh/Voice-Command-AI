//! The user's "commonly misheard words": the correct spelling of a term with
//! the forms the recognizer gets wrong. Kept by the server of each PC in the
//! personal layer of the speech vocabulary (`personal.json`) and edited from
//! Settings. The list is replaced as a whole.
//!
//! Viewing the list needs the view scope; saving it needs the admin scope and
//! is audited.

use serde::{Deserialize, Serialize};

/// `Welcome::features` entry of servers that answer
/// [`crate::ClientMsg::ListPersonalTerms`] and
/// [`crate::ClientMsg::SavePersonalTerms`]: server-side speech is on and this
/// `vcad` can change the list while it runs.
pub const FEATURE_PERSONAL_TERMS: &str = "personal_terms";

/// Most terms in the list.
pub const MAX_PERSONAL_TERMS: usize = 500;
/// Most misheard forms of one term.
pub const MAX_PERSONAL_VARIANTS: usize = 16;
/// Longest term or misheard form, in characters.
pub const MAX_PERSONAL_TERM_CHARS: usize = 64;

/// One word of the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalTerm {
    /// The correct spelling.
    pub term: String,
    /// What the recognizer writes instead. Empty: the term only reaches the
    /// recognizer's vocabulary hints.
    #[serde(default)]
    pub variants: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMsg, ServerMsg};

    #[test]
    fn wire_shape() {
        let list = serde_json::to_value(&ClientMsg::ListPersonalTerms).unwrap();
        assert_eq!(list, serde_json::json!({ "type": "list_personal_terms" }));
        let terms = vec![PersonalTerm {
            term: "SkyHarbor".into(),
            variants: vec!["sky harbour".into()],
        }];
        let json = serde_json::json!({
            "type": "save_personal_terms",
            "terms": [{ "term": "SkyHarbor", "variants": ["sky harbour"] }],
        });
        let save = ClientMsg::SavePersonalTerms {
            terms: terms.clone(),
        };
        assert_eq!(serde_json::to_value(&save).unwrap(), json);
        assert_eq!(serde_json::from_value::<ClientMsg>(json).unwrap(), save);
        assert_eq!(
            serde_json::to_value(&ServerMsg::PersonalTerms { terms }).unwrap(),
            serde_json::json!({
                "type": "personal_terms",
                "terms": [{ "term": "SkyHarbor", "variants": ["sky harbour"] }],
            })
        );
    }

    #[test]
    fn a_term_without_forms_may_omit_them() {
        let term: PersonalTerm = serde_json::from_str(r#"{"term":"SkyHarbor"}"#).unwrap();
        assert!(term.variants.is_empty());
    }
}
