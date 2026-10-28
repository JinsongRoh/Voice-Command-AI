//! Intent routing: control commands versus agent prompts (VOC-06, VOC-08).
//!
//! The router is language-agnostic; all words come from data tables
//! (`src/intent/lang/<lang>.toml`, embedded at build time, plus any extra
//! tables added at runtime). Tables of every language are active at once,
//! so mixed-language utterances ("세션 2 pause 해줘", "pausa la session 3")
//! work without language detection.
//!
//! Method: the utterance is normalized and scanned for known lexemes
//! (action phrases, session words, numbers, "this/current" words, template
//! words). Filler words are removed from what is left. A control command is
//! recognized when an action is present and the remaining words are empty or
//! name a known session or template; a long remainder means the user is
//! talking to the agent ("stop using var in the tests" is a prompt).
//! Borderline cases return [`Intent::Ambiguous`] so the UI can ask.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use strsim::jaro_winkler;

use crate::error::{Result, SpeechError};
use crate::text::{CharClass, NormalizedText, is_on_boundary, normalize, tokenize};

/// Control actions the router recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    SelectSession,
    PauseSession,
    ResumeSession,
    StopSession,
    RestartSession,
    ShowResources,
    Approve,
    Deny,
    ReadLastOutput,
    RunTemplate,
}

impl ControlAction {
    /// The protocol action for session lifecycle commands.
    pub fn session_action(self) -> Option<vca_core::SessionAction> {
        use vca_core::SessionAction as A;
        match self {
            Self::PauseSession => Some(A::Pause),
            Self::ResumeSession => Some(A::Resume),
            Self::StopSession => Some(A::Stop),
            Self::RestartSession => Some(A::Restart),
            _ => None,
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "select" => Self::SelectSession,
            "pause" => Self::PauseSession,
            "resume" => Self::ResumeSession,
            "stop" => Self::StopSession,
            "restart" => Self::RestartSession,
            "show_resources" => Self::ShowResources,
            "approve" => Self::Approve,
            "deny" => Self::Deny,
            "read_last_output" => Self::ReadLastOutput,
            "run_template" => Self::RunTemplate,
            _ => return None,
        })
    }
}

/// What a control command applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Target {
    /// "this one", "current": the selected session.
    Current,
    /// A session resolved against [`IntentContext::sessions`] (its id).
    Session(String),
    /// A card number that could not be resolved (no directory given).
    SessionNumber(u32),
    /// A session name that could not be resolved.
    SessionName(String),
    /// A command template name.
    Template(String),
}

/// Result of routing an utterance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Intent {
    /// A command for VCA itself. `target: None` means the selected session.
    Control {
        action: ControlAction,
        target: Option<Target>,
        confidence: f32,
    },
    /// Text to send to the agent.
    Prompt { text: String },
    /// The UI should ask the user; candidates are ordered best first.
    Ambiguous { candidates: Vec<Intent> },
}

/// A session the user may address by voice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionAlias {
    pub id: String,
    /// Card number shown in the UI.
    pub number: Option<u32>,
    /// Display name, aliases and folder name.
    #[serde(default)]
    pub names: Vec<String>,
}

/// Runtime context for routing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IntentContext {
    #[serde(default)]
    pub sessions: Vec<SessionAlias>,
    /// Names of the user's command templates.
    #[serde(default)]
    pub templates: Vec<String>,
    /// Whether a decision (permission request) is pending, which makes bare
    /// "yes"/"no" control commands.
    #[serde(default)]
    pub pending_decision: bool,
}

/// One language table (see `src/intent/lang/en.toml` for the format).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentLanguage {
    pub language: String,
    /// Action key (`pause`, `stop`, ...) → phrases.
    #[serde(default)]
    pub actions: HashMap<String, Vec<String>>,
    /// Words meaning "session" ("session", "セッション", "세션").
    #[serde(default)]
    pub session_words: Vec<String>,
    /// Words meaning "the current one".
    #[serde(default)]
    pub current_words: Vec<String>,
    /// Words meaning "template".
    #[serde(default)]
    pub template_words: Vec<String>,
    /// Number words → value ("three", "세 번째", "三号").
    #[serde(default)]
    pub numbers: HashMap<String, u32>,
    /// Words with no meaning for routing (politeness, articles, particles).
    #[serde(default)]
    pub fillers: Vec<String>,
}

const EMBEDDED: &[(&str, &str)] = &[
    ("en", include_str!("lang/en.toml")),
    ("ja", include_str!("lang/ja.toml")),
    ("ko", include_str!("lang/ko.toml")),
    ("zh", include_str!("lang/zh.toml")),
    ("es", include_str!("lang/es.toml")),
    ("de", include_str!("lang/de.toml")),
    ("fr", include_str!("lang/fr.toml")),
    ("pt", include_str!("lang/pt.toml")),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lexeme {
    Action(ControlAction),
    SessionWord,
    Current,
    TemplateWord,
    Number(u32),
}

/// The intent router. Build once, share freely.
#[derive(Debug, Clone)]
pub struct IntentRouter {
    /// (normalized phrase, lexeme), longest phrase first.
    lexemes: Vec<(String, Lexeme)>,
    fillers: HashSet<String>,
    /// Fillers that may attach to the end of words in boundary-free scripts
    /// (particles such as `를`, `を`), longest first.
    attached_fillers: Vec<String>,
}

impl Default for IntentRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct Scan {
    actions: Vec<ControlAction>,
    session_word: bool,
    current: bool,
    template_word: bool,
    numbers: Vec<u32>,
    residue: Vec<String>,
    /// For template commands: the utterance without the template phrase.
    template_rest: Option<String>,
    /// The normalized utterance.
    normalized: String,
}

impl IntentRouter {
    /// A router with all built-in language tables.
    pub fn new() -> Self {
        let tables = EMBEDDED
            .iter()
            .map(|(lang, src)| {
                toml::from_str::<IntentLanguage>(src)
                    .unwrap_or_else(|e| panic!("built-in intent table {lang} is invalid: {e}"))
            })
            .collect::<Vec<_>>();
        Self::with_languages(tables)
    }

    /// A router with the given tables only.
    pub fn with_languages(tables: Vec<IntentLanguage>) -> Self {
        let mut r = Self {
            lexemes: Vec::new(),
            fillers: HashSet::new(),
            attached_fillers: Vec::new(),
        };
        for t in tables {
            r.add_language(t);
        }
        r
    }

    /// Parses and adds a language table in TOML form.
    pub fn add_language_toml(&mut self, source: &str) -> Result<()> {
        let table: IntentLanguage = toml::from_str(source).map_err(|e| SpeechError::Parse {
            path: "intent table".into(),
            message: e.to_string(),
        })?;
        self.add_language(table);
        Ok(())
    }

    /// Adds a language table.
    pub fn add_language(&mut self, t: IntentLanguage) {
        let mut push = |phrase: &str, lx: Lexeme| {
            let n = normalize(phrase);
            if !n.is_empty() {
                self.lexemes.push((n, lx));
            }
        };
        for (key, phrases) in &t.actions {
            let Some(action) = ControlAction::from_key(key) else {
                tracing::warn!(language = %t.language, key, "unknown intent action key");
                continue;
            };
            for p in phrases {
                push(p, Lexeme::Action(action));
            }
        }
        for w in &t.session_words {
            push(w, Lexeme::SessionWord);
        }
        for w in &t.current_words {
            push(w, Lexeme::Current);
        }
        for w in &t.template_words {
            push(w, Lexeme::TemplateWord);
        }
        for (w, n) in &t.numbers {
            push(w, Lexeme::Number(*n));
        }
        for f in &t.fillers {
            let n = normalize(f);
            if n.is_empty() {
                continue;
            }
            if n.chars().all(|c| CharClass::of(c).is_boundary_free()) {
                self.attached_fillers.push(n.clone());
            }
            self.fillers.insert(n);
        }
        self.lexemes.sort_by(|a, b| {
            b.0.chars()
                .count()
                .cmp(&a.0.chars().count())
                .then(a.0.cmp(&b.0))
        });
        self.lexemes.dedup();
        self.attached_fillers
            .sort_by_key(|f| std::cmp::Reverse(f.chars().count()));
        self.attached_fillers.dedup();
    }

    fn scan(&self, text: &str) -> Scan {
        // Punctuation becomes space so "stop, please" scans like "stop please".
        let cleaned: String = text
            .chars()
            .map(|c| {
                if CharClass::of(c) == CharClass::Other && !matches!(c, '\'' | '-' | '’') {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let norm = NormalizedText::new(&cleaned).text;
        let mut scan = Scan::default();
        let mut consumed = vec![false; norm.len()];
        let mut pos = 0;
        while pos < norm.len() {
            let rest = &norm[pos..];
            let c = rest.chars().next().unwrap_or(' ');
            if c.is_ascii_digit() {
                let len = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                if let Ok(n) = rest[..len].parse::<u32>() {
                    scan.numbers.push(n);
                }
                consumed[pos..pos + len].fill(true);
                pos += len;
                continue;
            }
            let hit = self.lexemes.iter().find(|(p, _)| {
                rest.starts_with(p.as_str())
                    && is_on_boundary(&norm, pos, pos + p.len())
                    && self.hangul_tail_ok(&norm[pos + p.len()..], p)
            });
            match hit {
                Some((p, lx)) => {
                    match *lx {
                        Lexeme::Action(a) => {
                            if a == ControlAction::RunTemplate && scan.template_rest.is_none() {
                                let rest = format!("{} {}", &norm[..pos], &norm[pos + p.len()..]);
                                scan.template_rest = Some(self.trim_fillers(&rest));
                            }
                            if !scan.actions.contains(&a) {
                                scan.actions.push(a);
                            }
                        }
                        Lexeme::SessionWord => scan.session_word = true,
                        Lexeme::Current => scan.current = true,
                        Lexeme::TemplateWord => scan.template_word = true,
                        Lexeme::Number(n) => scan.numbers.push(n),
                    }
                    consumed[pos..pos + p.len()].fill(true);
                    pos += p.len();
                }
                None => pos += c.len_utf8(),
            }
        }
        // Residue: unconsumed text minus fillers.
        let residue_text: String = norm
            .char_indices()
            .map(|(i, ch)| if consumed[i] { ' ' } else { ch })
            .collect();
        let mut words: Vec<String> = Vec::new();
        for tok in tokenize(&residue_text) {
            if tok.class == CharClass::Other {
                continue;
            }
            let w = tok.text;
            if tok.class.is_boundary_free() && !self.fillers.contains(w) {
                let stripped = self.strip_attached_fillers(w);
                if !stripped.is_empty() {
                    words.push(stripped);
                }
            } else {
                words.push(w.to_owned());
            }
        }
        // Multi-word fillers ("por favor", "can you") first, then single ones.
        scan.residue = self
            .drop_multiword_fillers(words)
            .into_iter()
            .filter(|w| !self.fillers.contains(w))
            .collect();
        scan.normalized = norm;
        scan
    }

    /// A Hangul lexeme followed directly by more Hangul only counts when
    /// what follows is a particle or verb ending listed as a filler
    /// ("세션으로", "재시작해줘"), so "네" (yes) does not match inside
    /// "네트워크" (network).
    fn hangul_tail_ok(&self, after: &str, phrase: &str) -> bool {
        let last_is_hangul = phrase
            .chars()
            .next_back()
            .is_some_and(|c| CharClass::of(c) == CharClass::Hangul);
        if !last_is_hangul {
            return true;
        }
        let run_len = after
            .find(|c: char| CharClass::of(c) != CharClass::Hangul)
            .unwrap_or(after.len());
        run_len == 0 || self.strip_attached_fillers(&after[..run_len]).is_empty()
    }

    /// Removes filler words at both ends of a phrase.
    fn trim_fillers(&self, phrase: &str) -> String {
        let mut words: Vec<&str> = phrase.split_whitespace().collect();
        while words.first().is_some_and(|w| self.fillers.contains(*w)) {
            words.remove(0);
        }
        while words.last().is_some_and(|w| self.fillers.contains(*w)) {
            words.pop();
        }
        words.join(" ")
    }

    fn strip_attached_fillers(&self, word: &str) -> String {
        let mut w = word;
        loop {
            let before = w;
            for f in &self.attached_fillers {
                if w == f {
                    return String::new();
                }
                if let Some(rest) = w.strip_suffix(f.as_str()).filter(|r| !r.is_empty()) {
                    w = rest;
                    break;
                }
            }
            if w == before {
                return w.to_owned();
            }
        }
    }

    fn drop_multiword_fillers(&self, words: Vec<String>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        'outer: while i < words.len() {
            for n in (2..=3).rev() {
                if i + n <= words.len() && self.fillers.contains(&words[i..i + n].join(" ")) {
                    i += n;
                    continue 'outer;
                }
            }
            out.push(words[i].clone());
            i += 1;
        }
        out
    }

    /// Routes one utterance.
    pub fn route(&self, utterance: &str, ctx: &IntentContext) -> Intent {
        let text = utterance.trim();
        let prompt = || Intent::Prompt {
            text: text.to_owned(),
        };
        if text.is_empty() {
            return prompt();
        }
        let mut scan = self.scan(text);

        // Session or template named in the residue.
        let named_session = if scan.template_rest.is_some() {
            None
        } else {
            take_session_name(&mut scan.residue, ctx)
        };

        if scan.actions.is_empty() {
            // "session 3", "3번 세션", or just a known session name.
            let number_target = scan.numbers.first().map(|&n| resolve_number(n, ctx));
            if scan.residue.is_empty() {
                if scan.session_word && (number_target.is_some() || named_session.is_some()) {
                    return control(
                        ControlAction::SelectSession,
                        number_target.or(named_session),
                        0.9,
                    );
                }
                if let Some(t) = named_session
                    && !scan.session_word
                    && scan.numbers.is_empty()
                {
                    return control(ControlAction::SelectSession, Some(t), 0.8);
                }
            }
            return prompt();
        }

        if let Some(rest) = scan.template_rest.take() {
            return self.route_template(rest, ctx, prompt());
        }

        // "run the deploy template": a template word plus a known template
        // name anywhere in the utterance.
        if scan.template_word
            && let Some(t) = find_template_in(&scan.normalized, ctx)
        {
            return control(ControlAction::RunTemplate, Some(Target::Template(t)), 0.85);
        }

        if scan.actions.len() > 1 {
            // "stop and restart": ask which one was meant.
            let target = scan
                .numbers
                .first()
                .map(|&n| resolve_number(n, ctx))
                .or(named_session);
            let mut candidates: Vec<Intent> = scan
                .actions
                .iter()
                .map(|&a| control(a, target.clone(), 0.5))
                .collect();
            if !scan.residue.is_empty() {
                candidates.push(prompt());
            }
            return Intent::Ambiguous { candidates };
        }

        let action = scan.actions[0];
        let residue_len = scan.residue.len();

        // Session-addressed actions.
        let mut confidence: f32 = 0.95;
        let target = if let Some(&n) = scan.numbers.first() {
            let t = resolve_number(n, ctx);
            if matches!(t, Target::SessionNumber(_)) && !ctx.sessions.is_empty() {
                confidence = 0.7;
            }
            Some(t)
        } else if let Some(t) = named_session {
            confidence = 0.9;
            Some(t)
        } else if scan.current {
            Some(Target::Current)
        } else {
            None
        };

        if matches!(action, ControlAction::Approve | ControlAction::Deny) && !ctx.pending_decision {
            confidence = confidence.min(0.55);
        }

        let intent = match residue_len {
            0 => control(action, target, confidence),
            1 if target.is_none() => {
                // One unknown word: a session we do not know, or a prompt.
                let name = scan.residue[0].clone();
                let c = control(action, Some(Target::SessionName(name)), 0.5);
                if scan.session_word && ctx.sessions.is_empty() {
                    return control_with(c, 0.7);
                }
                return Intent::Ambiguous {
                    candidates: vec![c, prompt()],
                };
            }
            1 | 2 if scan.session_word => {
                return Intent::Ambiguous {
                    candidates: vec![control(action, target, 0.5), prompt()],
                };
            }
            _ => return prompt(),
        };
        if confidence < 0.6 {
            Intent::Ambiguous {
                candidates: vec![intent, prompt()],
            }
        } else {
            intent
        }
    }
}

impl IntentRouter {
    fn route_template(&self, name: String, ctx: &IntentContext, prompt: Intent) -> Intent {
        let action = ControlAction::RunTemplate;
        if name.is_empty() {
            return Intent::Ambiguous {
                candidates: vec![control(action, None, 0.4), prompt],
            };
        }
        if let Some(t) = resolve_template(&name, ctx) {
            return control(action, Some(Target::Template(t)), 0.95);
        }
        let words = name.split_whitespace().count();
        let guess = control(action, Some(Target::Template(name)), 0.6);
        if ctx.templates.is_empty() && words <= 4 {
            guess
        } else {
            Intent::Ambiguous {
                candidates: vec![guess, prompt],
            }
        }
    }
}

fn control(action: ControlAction, target: Option<Target>, confidence: f32) -> Intent {
    Intent::Control {
        action,
        target,
        confidence,
    }
}

fn control_with(intent: Intent, confidence: f32) -> Intent {
    match intent {
        Intent::Control { action, target, .. } => control(action, target, confidence),
        other => other,
    }
}

fn resolve_number(n: u32, ctx: &IntentContext) -> Target {
    ctx.sessions
        .iter()
        .find(|s| s.number == Some(n))
        .map(|s| Target::Session(s.id.clone()))
        .unwrap_or(Target::SessionNumber(n))
}

fn name_matches(spoken: &str, name: &str) -> bool {
    let a: String = normalize(spoken)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    let b: String = normalize(name)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || (a.chars().count() >= 4 && jaro_winkler(&a, &b) >= 0.92)
}

/// Finds the longest contiguous run of residue words naming a known
/// session, removes it and returns the target.
fn take_session_name(residue: &mut Vec<String>, ctx: &IntentContext) -> Option<Target> {
    if ctx.sessions.is_empty() || residue.is_empty() {
        return None;
    }
    for len in (1..=residue.len().min(4)).rev() {
        for start in 0..=residue.len() - len {
            let spoken = residue[start..start + len].join(" ");
            if let Some(s) = ctx
                .sessions
                .iter()
                .find(|s| s.names.iter().any(|n| name_matches(&spoken, n)))
            {
                residue.drain(start..start + len);
                return Some(Target::Session(s.id.clone()));
            }
        }
    }
    None
}

/// The longest known template whose name occurs in the normalized text.
fn find_template_in(text: &str, ctx: &IntentContext) -> Option<String> {
    ctx.templates
        .iter()
        .filter(|t| {
            let n = normalize(t);
            !n.is_empty()
                && text
                    .match_indices(n.as_str())
                    .any(|(i, m)| is_on_boundary(text, i, i + m.len()))
        })
        .max_by_key(|t| t.len())
        .cloned()
}

fn resolve_template(name: &str, ctx: &IntentContext) -> Option<String> {
    ctx.templates
        .iter()
        .find(|t| name_matches(name, t))
        .cloned()
}

#[cfg(test)]
mod tests;
