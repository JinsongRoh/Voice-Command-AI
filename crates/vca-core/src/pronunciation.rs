//! The pronunciation dictionary of spoken alerts: how a voice should say
//! names written in Latin letters (session and project names, product
//! terms). Kept by the server of each PC (`pronunciations.json` in its data
//! directory), so the PC's TTS engine and every device's own voice use the
//! same entries. The matching rules are in `vca_speech::pronounce`.
//!
//! Viewing the list needs the view scope; adding, changing and deleting an
//! entry need the command scope.

use serde::{Deserialize, Serialize};

use crate::UnixMillis;

/// `Welcome::features` entry of servers that answer the pronunciation
/// messages.
pub const FEATURE_PRONUNCIATIONS: &str = "pronunciations";

/// Where an entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PronunciationSource {
    /// Shipped with VCA (common product terms).
    Default,
    /// Added or changed by the user.
    #[default]
    User,
    /// Suggested by the server for a new session or project name (LLM or
    /// rules); the user can change it.
    Auto,
}

/// One entry: `term` is said `spoken` by a voice of `language`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pronunciation {
    /// The written term (Latin letters, digits, a little punctuation).
    /// "SkyHarbor", "Sky Harbor" and "sky-harbor" are the same term.
    pub term: String,
    /// What the voice says instead.
    pub spoken: String,
    /// ISO 639-1 language of the voice (`ko`, `ja`, ...).
    pub language: String,
    pub source: PronunciationSource,
    /// 0 for shipped entries.
    #[serde(default)]
    pub updated_at: UnixMillis,
}
