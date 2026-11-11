//! The pronunciation dictionary of spoken alerts, kept in
//! `data_dir/pronunciations.json` (see `vca_core::pronunciation`).
//!
//! - Shipped entries ([`DEFAULTS`]: common product terms) are in code; the
//!   user's entries win over them, and deleting a shipped entry keeps it off.
//! - The PC's TTS applies the dictionary in `Tts::synthesize`; devices get
//!   the list (`ListPronunciations`, pushed on every change) and apply it
//!   before their own voice.
//! - Session names with words in Latin letters get a Korean reading once
//!   ([`Pronunciations::start`]): from the correction LLM of `[speech]` when
//!   one is configured (a strict JSON-only prompt), else from rules
//!   ([`vca_speech::pronounce::suggest_reading`]). Suggestions are marked
//!   `auto`; the user can change or delete them, and a name is never asked
//!   about twice.
//! - A `pronunciations-import.json` in the data directory (the same format)
//!   is merged as the user's entries at start and renamed to `.done`.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use vca_core::error::ErrorCode;
use vca_core::{Error, Pronunciation, PronunciationSource, ServerMsg, UnixMillis, now_millis};
use vca_speech::llm::ChatBackend;
use vca_speech::pronounce::{
    self, Dictionary, LLM_SYSTEM_PROMPT, has_latin, llm_user_message, parse_llm_reading,
    suggest_reading, term_key, validate_spoken, validate_term,
};

use crate::sessions::{SessionManager, Update};
use crate::util::lock;

/// File name below the data directory.
pub const PRONUNCIATIONS_FILE: &str = "pronunciations.json";
/// Entries merged from this file at start (then renamed to `.done`).
pub const IMPORT_FILE: &str = "pronunciations-import.json";
const FILE_VERSION: u32 = 1;
/// Upper bound on stored entries.
const MAX_ENTRIES: usize = 2000;
/// Upper bound on remembered "already suggested" names.
const MAX_TRIED: usize = 5000;
/// Names waiting for a suggestion at most.
const MAX_QUEUE: usize = 200;
/// LLM suggestions per window, server-wide.
const LLM_PER_WINDOW: usize = 30;
const LLM_WINDOW: Duration = Duration::from_secs(600);
/// The language of suggestions.
const SUGGEST_LANGUAGE: &str = "ko";

/// Shipped entries: common product and technical terms, per language. The
/// user's own entries win.
pub const DEFAULTS: &[(&str, &str, &str)] = &[
    ("ko", "Claude Code", "클로드 코드"),
    ("ko", "Claude", "클로드"),
    ("ko", "Codex", "코덱스"),
    ("ko", "Gemini", "제미나이"),
    ("ko", "ChatGPT", "챗지피티"),
    ("ko", "OpenAI", "오픈에이아이"),
    ("ko", "Anthropic", "앤트로픽"),
    ("ko", "GitHub", "깃허브"),
    ("ko", "VS Code", "브이에스 코드"),
    ("ko", "PowerShell", "파워셸"),
    ("ko", "VCA", "브이씨에이"),
    ("ko", "Voco", "보코"),
    ("ko", "AI", "에이아이"),
    ("ko", "API", "에이피아이"),
    ("ko", "PDF", "피디에프"),
    ("ko", "MCP", "엠씨피"),
    ("ko", "CLI", "씨엘아이"),
    ("ko", "LLM", "엘엘엠"),
    ("ko", "TTS", "티티에스"),
    ("ko", "STT", "에스티티"),
    ("ko", "UI", "유아이"),
    ("ko", "URL", "유알엘"),
    ("ko", "PC", "피씨"),
    ("ko", "OPC UA", "오피씨 유에이"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEntry {
    term: String,
    spoken: String,
    language: String,
    #[serde(default)]
    source: PronunciationSource,
    #[serde(default)]
    updated_at: UnixMillis,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct FileData {
    #[serde(default = "file_version")]
    version: u32,
    #[serde(default)]
    entries: Vec<StoredEntry>,
    /// `language:key` of shipped entries the user deleted.
    #[serde(default)]
    removed: BTreeSet<String>,
    /// `language:key` of names a reading was already suggested for (or
    /// tried).
    #[serde(default)]
    tried: BTreeSet<String>,
}

fn file_version() -> u32 {
    FILE_VERSION
}

fn id(language: &str, key: &str) -> String {
    format!("{language}:{key}")
}

/// `ko-KR` -> `ko`; `None`, blank or odd tags -> `ko`.
fn language_of(tag: Option<&str>) -> Result<String, Error> {
    let tag = tag.map(str::trim).filter(|t| !t.is_empty());
    let Some(tag) = tag else {
        return Ok(SUGGEST_LANGUAGE.to_owned());
    };
    let base = vca_speech::stt::base_language(tag);
    if (2..=3).contains(&base.len()) && base.chars().all(|c| c.is_ascii_lowercase()) {
        Ok(base)
    } else {
        Err(Error::api(ErrorCode::InvalidRequest, "unknown language"))
    }
}

fn invalid(message: &str) -> Error {
    Error::api(ErrorCode::InvalidRequest, message)
}

enum Llm {
    Profile {
        slot: Box<crate::speech_profiles::LlmSlot>,
        secrets: crate::speech_profiles::Secrets,
    },
    Fixed(Arc<dyn ChatBackend>),
}

impl Llm {
    fn chat(&self) -> Option<(Arc<dyn ChatBackend>, Duration)> {
        match self {
            Self::Profile { slot, secrets } => {
                let corrector = slot.corrector(secrets)?;
                Some((corrector.chat(), slot.timeout.max(Duration::from_secs(8))))
            }
            Self::Fixed(chat) => Some((Arc::clone(chat), Duration::from_secs(10))),
        }
    }
}

/// What asking the LLM gave.
enum Asked {
    Reading(String),
    /// No LLM, or it failed or answered something unusable.
    Nothing,
    /// Over the server-wide limit: try again later.
    Limited,
}

/// The dictionary of this PC. Shared by the API, the TTS and the
/// suggestion worker.
pub struct Pronunciations {
    path: Option<PathBuf>,
    data: Mutex<FileData>,
    dictionaries: Mutex<HashMap<String, Arc<Dictionary>>>,
    notices: Option<broadcast::Sender<ServerMsg>>,
    llm: OnceLock<Llm>,
    llm_calls: Mutex<VecDeque<Instant>>,
    queue: Mutex<Option<mpsc::Sender<String>>>,
    queued: Mutex<BTreeSet<String>>,
}

impl Pronunciations {
    /// Loads `data_dir/pronunciations.json` (none: in memory only) and
    /// merges `pronunciations-import.json`. Changes are pushed on `notices`.
    pub fn open(
        data_dir: Option<&Path>,
        notices: Option<broadcast::Sender<ServerMsg>>,
    ) -> Arc<Self> {
        let path = data_dir.map(|d| d.join(PRONUNCIATIONS_FILE));
        let data = path.as_deref().map(load).unwrap_or_default();
        let this = Arc::new(Self {
            path,
            data: Mutex::new(data),
            dictionaries: Mutex::new(HashMap::new()),
            notices,
            llm: OnceLock::new(),
            llm_calls: Mutex::new(VecDeque::new()),
            queue: Mutex::new(None),
            queued: Mutex::new(BTreeSet::new()),
        });
        if let Some(dir) = data_dir {
            this.import(&dir.join(IMPORT_FILE));
        }
        this
    }

    /// Uses the default correction LLM of `[speech]` for suggestions.
    pub fn set_llm_from_config(&self, cfg: &crate::config::SpeechConfig, data_dir: &Path) {
        if cfg.default_llm.as_deref() == Some(vca_core::SPEECH_LLM_NONE) {
            return;
        }
        let timeout = Duration::from_millis(cfg.llm.timeout_ms.max(1));
        let profiles = cfg.llm_profiles();
        let chosen = cfg
            .default_llm_id()
            .and_then(|id| profiles.iter().find(|p| p.id == id).cloned())
            .or_else(|| profiles.first().cloned());
        if let Some(profile) = chosen {
            let _ = self.llm.set(Llm::Profile {
                slot: Box::new(crate::speech_profiles::LlmSlot::new(profile, timeout)),
                secrets: crate::speech_profiles::Secrets::in_data_dir(data_dir),
            });
        }
    }

    /// Uses `chat` for suggestions (tests use a fake).
    pub fn set_llm(&self, chat: Arc<dyn ChatBackend>) {
        let _ = self.llm.set(Llm::Fixed(chat));
    }

    fn import(&self, file: &Path) {
        if !file.exists() {
            return;
        }
        let imported = load(file);
        let mut count = 0;
        for e in imported.entries {
            match self.store(
                &e.term,
                &e.spoken,
                Some(&e.language),
                PronunciationSource::User,
            ) {
                Ok(_) => count += 1,
                Err(err) => tracing::warn!(%err, "skipped an imported pronunciation"),
            }
        }
        let done = file.with_extension("json.done");
        if let Err(err) = std::fs::rename(file, &done) {
            tracing::warn!(%err, "cannot rename {IMPORT_FILE}");
        }
        tracing::info!(count, "pronunciations imported");
    }

    /// Every entry of `language` (`None`: all), shipped ones included,
    /// sorted by language and term.
    pub fn list(&self, language: Option<&str>) -> Vec<Pronunciation> {
        let want = language
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(vca_speech::stt::base_language);
        let data = lock(&self.data);
        let mut out: Vec<Pronunciation> = Vec::new();
        let mut seen = BTreeSet::new();
        for e in &data.entries {
            seen.insert(id(&e.language, &term_key(&e.term)));
            out.push(Pronunciation {
                term: e.term.clone(),
                spoken: e.spoken.clone(),
                language: e.language.clone(),
                source: e.source,
                updated_at: e.updated_at,
            });
        }
        for (lang, term, spoken) in DEFAULTS {
            let key = id(lang, &term_key(term));
            if seen.contains(&key) || data.removed.contains(&key) {
                continue;
            }
            seen.insert(key);
            out.push(Pronunciation {
                term: (*term).to_owned(),
                spoken: (*spoken).to_owned(),
                language: (*lang).to_owned(),
                source: PronunciationSource::Default,
                updated_at: 0,
            });
        }
        drop(data);
        out.retain(|p| want.as_deref().is_none_or(|w| p.language == w));
        out.sort_by(|a, b| {
            a.language
                .cmp(&b.language)
                .then_with(|| a.term.to_lowercase().cmp(&b.term.to_lowercase()))
        });
        out
    }

    /// The answer and push of the pronunciation messages.
    pub fn list_msg(&self) -> ServerMsg {
        ServerMsg::Pronunciations {
            entries: self.list(None),
        }
    }

    fn changed(&self) {
        lock(&self.dictionaries).clear();
        if let Some(tx) = &self.notices {
            let _ = tx.send(self.list_msg());
        }
    }

    /// Adds or changes the user's entry for `term`.
    pub fn set(&self, term: &str, spoken: &str, language: Option<&str>) -> Result<(), Error> {
        self.store(term, spoken, language, PronunciationSource::User)?;
        self.changed();
        Ok(())
    }

    fn store(
        &self,
        term: &str,
        spoken: &str,
        language: Option<&str>,
        source: PronunciationSource,
    ) -> Result<bool, Error> {
        let term = validate_term(term).map_err(invalid)?;
        let spoken = validate_spoken(spoken).map_err(invalid)?;
        let language = language_of(language)?;
        let key = term_key(&term);
        let mut data = lock(&self.data);
        let mut next = data.clone();
        let existing = next
            .entries
            .iter()
            .position(|e| e.language == language && term_key(&e.term) == key);
        if source == PronunciationSource::Auto && existing.is_some() {
            // The user's word (or an earlier suggestion) stays.
            return Ok(false);
        }
        let entry = StoredEntry {
            term,
            spoken,
            language: language.clone(),
            source,
            updated_at: now_millis(),
        };
        match existing {
            Some(i) => next.entries[i] = entry,
            None => {
                if next.entries.len() >= MAX_ENTRIES {
                    return Err(Error::api(
                        ErrorCode::InvalidRequest,
                        "the pronunciation dictionary is full",
                    ));
                }
                next.entries.push(entry);
            }
        }
        next.removed.remove(&id(&language, &key));
        self.save(&next)?;
        *data = next;
        Ok(true)
    }

    /// Deletes the entry of `term`. Returns whether there was one.
    pub fn delete(&self, term: &str, language: Option<&str>) -> Result<bool, Error> {
        let language = language_of(language)?;
        let key = term_key(term);
        if key.is_empty() {
            return Err(invalid("a term needs a letter or a digit"));
        }
        let shipped = DEFAULTS
            .iter()
            .any(|(l, t, _)| *l == language && term_key(t) == key);
        let mut data = lock(&self.data);
        let mut next = data.clone();
        let before = next.entries.len();
        next.entries
            .retain(|e| !(e.language == language && term_key(&e.term) == key));
        let mut existed = next.entries.len() != before;
        if shipped && next.removed.insert(id(&language, &key)) {
            existed = true;
        }
        if !existed {
            return Ok(false);
        }
        // A deleted suggestion is not suggested again.
        next.tried.insert(id(&language, &key));
        prune_tried(&mut next.tried);
        self.save(&next)?;
        *data = next;
        drop(data);
        self.changed();
        Ok(true)
    }

    /// The dictionary of a voice language (BCP-47; `None`: Korean).
    pub fn dictionary(&self, language: Option<&str>) -> Arc<Dictionary> {
        let lang = language_of(language).unwrap_or_else(|_| String::new());
        if let Some(d) = lock(&self.dictionaries).get(&lang) {
            return Arc::clone(d);
        }
        let entries = self.list(Some(&lang));
        let dict = Arc::new(Dictionary::new(
            entries.iter().map(|e| (e.term.as_str(), e.spoken.as_str())),
        ));
        lock(&self.dictionaries).insert(lang, Arc::clone(&dict));
        dict
    }

    /// `text` as a voice of `language` should say it: the dictionary, then
    /// the radio words.
    pub fn speakable<'a>(&self, text: &'a str, language: Option<&str>) -> Cow<'a, str> {
        let Some(lang) = language else {
            return Cow::Borrowed(text);
        };
        let dict = self.dictionary(Some(lang));
        pronounce::speakable(text, Some(lang), &dict)
    }

    fn save(&self, data: &FileData) -> Result<(), Error> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = FileData {
            version: FILE_VERSION,
            ..data.clone()
        };
        let json = serde_json::to_vec_pretty(&file)
            .map_err(|e| Error::api(ErrorCode::Internal, e.to_string()))?;
        crate::names::write_atomic(path, &json).map_err(|e| {
            tracing::warn!(%e, "cannot write {PRONUNCIATIONS_FILE}");
            Error::api(
                ErrorCode::Internal,
                "cannot save the pronunciation dictionary",
            )
        })
    }

    // --- Suggestions for session names. ---

    /// The words of a name that need a reading: Latin letters, a valid
    /// term, not a file name or path, and nothing in the dictionary for
    /// them yet.
    pub fn words_needing_reading(&self, name: &str) -> Vec<String> {
        let dict = self.dictionary(Some(SUGGEST_LANGUAGE));
        let data = lock(&self.data);
        let mut out: Vec<String> = Vec::new();
        for word in name.split(|c: char| {
            c.is_whitespace() || matches!(c, '·' | '|' | ',' | '(' | ')' | '[' | ']' | ':' | ';')
        }) {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let letters = word.chars().filter(char::is_ascii_alphabetic).count();
            let digits = word.chars().filter(char::is_ascii_digit).count();
            if letters < 2
                || word.len() > 32
                || digits > letters
                || word.contains(['.', '/'])
                || validate_term(word).is_err()
            {
                continue;
            }
            let key = term_key(word);
            if data.tried.contains(&id(SUGGEST_LANGUAGE, &key))
                || !has_latin(&dict.apply(word))
                || out.iter().any(|w| term_key(w) == key)
            {
                continue;
            }
            out.push(word.to_owned());
        }
        out
    }

    /// Queues the words of `name` for a suggestion (after [`Self::start`]).
    pub fn observe_name(&self, name: &str) {
        let words = self.words_needing_reading(name);
        if words.is_empty() {
            return;
        }
        let Some(tx) = lock(&self.queue).clone() else {
            return;
        };
        let mut queued = lock(&self.queued);
        for word in words {
            if queued.len() >= MAX_QUEUE {
                break;
            }
            if queued.insert(term_key(&word)) && tx.try_send(word.clone()).is_err() {
                queued.remove(&term_key(&word));
            }
        }
    }

    fn llm_allowed(&self) -> bool {
        let now = Instant::now();
        let mut calls = lock(&self.llm_calls);
        while calls
            .front()
            .is_some_and(|t| now.duration_since(*t) >= LLM_WINDOW)
        {
            calls.pop_front();
        }
        if calls.len() >= LLM_PER_WINDOW {
            return false;
        }
        calls.push_back(now);
        true
    }

    async fn ask_llm(&self, term: &str) -> Asked {
        let Some((chat, timeout)) = self.llm.get().and_then(Llm::chat) else {
            return Asked::Nothing;
        };
        if !self.llm_allowed() {
            return Asked::Limited;
        }
        let user = llm_user_message(term);
        let call = chat.complete(LLM_SYSTEM_PROMPT, &user);
        match tokio::time::timeout(timeout, call).await {
            Ok(Ok(reply)) => parse_llm_reading(&reply).map_or(Asked::Nothing, Asked::Reading),
            Ok(Err(_)) | Err(_) => {
                // Never the name or the reply: only that it failed.
                tracing::debug!("pronunciation suggestion failed");
                Asked::Nothing
            }
        }
    }

    /// True when the suggestion language has an entry (stored or shipped)
    /// for `key`.
    fn has_entry(&self, key: &str) -> bool {
        self.list(Some(SUGGEST_LANGUAGE))
            .iter()
            .any(|e| term_key(&e.term) == key)
    }

    /// Suggests a Korean reading for `term` once: the LLM, else the rules.
    /// Returns the stored reading, if any.
    pub async fn suggest(&self, term: &str) -> Option<String> {
        let key = term_key(term);
        let tag = id(SUGGEST_LANGUAGE, &key);
        if key.is_empty() || lock(&self.data).tried.contains(&tag) || self.has_entry(&key) {
            return None;
        }
        let reading = match self.ask_llm(term).await {
            Asked::Reading(r) => Some(r),
            Asked::Nothing => suggest_reading(term),
            Asked::Limited => return None,
        };
        {
            let mut data = lock(&self.data);
            let mut next = data.clone();
            next.tried.insert(tag);
            prune_tried(&mut next.tried);
            if self.save(&next).is_ok() {
                *data = next;
            }
        }
        let reading = reading?;
        match self.store(
            term,
            &reading,
            Some(SUGGEST_LANGUAGE),
            PronunciationSource::Auto,
        ) {
            Ok(true) => {
                self.changed();
                Some(reading)
            }
            Ok(false) | Err(_) => None,
        }
    }

    /// Suggests readings for the names of `mgr`'s sessions, now and as
    /// they change.
    pub fn start(self: &Arc<Self>, mgr: &SessionManager) -> JoinHandle<()> {
        let (tx, mut rx) = mpsc::channel::<String>(MAX_QUEUE);
        *lock(&self.queue) = Some(tx);
        let mut updates = mgr.subscribe_updates();
        for s in mgr.list() {
            self.observe_session(&s);
        }
        let this = Arc::clone(self);
        let worker = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(word) = rx.recv().await {
                worker.suggest(&word).await;
                lock(&worker.queued).remove(&term_key(&word));
            }
        });
        tokio::spawn(async move {
            loop {
                match updates.recv().await {
                    Ok(Update::Changed(info)) => this.observe_session(&info),
                    Ok(Update::Removed(_)) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }

    fn observe_session(&self, info: &vca_core::SessionInfo) {
        self.observe_name(&info.display_name);
        if let Some(auto) = &info.auto_name
            && auto != &info.display_name
        {
            self.observe_name(auto);
        }
    }
}

fn prune_tried(tried: &mut BTreeSet<String>) {
    while tried.len() > MAX_TRIED {
        let first = tried.iter().next().cloned();
        match first {
            Some(k) => tried.remove(&k),
            None => break,
        };
    }
}

fn load(path: &Path) -> FileData {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return FileData::default(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "cannot read pronunciations");
            return FileData::default();
        }
    };
    match serde_json::from_slice::<FileData>(&bytes) {
        Ok(file) if file.version == FILE_VERSION => file,
        Ok(file) => {
            tracing::warn!(
                version = file.version,
                "unknown pronunciations file version"
            );
            FileData::default()
        }
        Err(err) => {
            let bad = path.with_extension("json.bad");
            tracing::warn!(%err, moved_to = %bad.display(), "a pronunciations file is damaged");
            let _ = std::fs::rename(path, bad);
            FileData::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake LLM answering with a fixed reply and counting calls.
    struct FakeLlm {
        reply: Result<String, ()>,
        calls: AtomicUsize,
        prompts: Mutex<Vec<(String, String)>>,
    }

    impl FakeLlm {
        fn new(reply: Result<&str, ()>) -> Arc<Self> {
            Arc::new(Self {
                reply: reply.map(str::to_owned),
                calls: AtomicUsize::new(0),
                prompts: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl ChatBackend for FakeLlm {
        async fn complete(&self, system: &str, user: &str) -> vca_speech::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            lock(&self.prompts).push((system.to_owned(), user.to_owned()));
            self.reply
                .clone()
                .map_err(|()| vca_speech::SpeechError::Decode("down".into()))
        }
    }

    fn spoken(p: &Pronunciations, term: &str) -> Option<String> {
        let key = term_key(term);
        p.list(Some("ko"))
            .into_iter()
            .find(|e| term_key(&e.term) == key)
            .map(|e| e.spoken)
    }

    #[test]
    fn user_entries_win_over_shipped_ones_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let p = Pronunciations::open(Some(dir.path()), None);
        assert_eq!(spoken(&p, "Codex").as_deref(), Some("코덱스"));
        assert_eq!(
            p.speakable("Claude Code 끝", Some("ko-KR")),
            "클로드 코드 끝"
        );
        p.set("codex", "코덱쓰", Some("ko-KR")).unwrap();
        p.set("Sky Harbor", "스카이하버", None).unwrap();
        assert_eq!(spoken(&p, "Codex").as_deref(), Some("코덱쓰"));
        assert_eq!(
            p.speakable("Codex와 SkyHarbor", Some("ko")),
            "코덱쓰와 스카이하버"
        );
        // English voices are untouched; other languages have their own list.
        assert_eq!(p.speakable("Codex", Some("en")), "Codex");
        p.set("SkyHarbor", "スカイハーバー", Some("ja")).unwrap();
        assert_eq!(p.speakable("SkyHarbor", Some("ja")), "スカイハーバー");
        assert_eq!(p.list(Some("ja")).len(), 1);

        let again = Pronunciations::open(Some(dir.path()), None);
        assert_eq!(spoken(&again, "Sky-Harbor").as_deref(), Some("스카이하버"));
        let entry = again
            .list(Some("ko"))
            .into_iter()
            .find(|e| e.term == "codex")
            .unwrap();
        assert_eq!(entry.source, PronunciationSource::User);
    }

    #[test]
    fn deleting_a_shipped_entry_keeps_it_off_until_taught_again() {
        let dir = tempfile::tempdir().unwrap();
        let p = Pronunciations::open(Some(dir.path()), None);
        assert!(p.delete("CODEX", None).unwrap());
        assert_eq!(spoken(&p, "Codex"), None);
        assert!(!p.delete("Codex", None).unwrap());
        assert_eq!(p.speakable("Codex", Some("ko")), "Codex");
        let again = Pronunciations::open(Some(dir.path()), None);
        assert_eq!(spoken(&again, "Codex"), None);
        again.set("Codex", "코덱스", None).unwrap();
        assert_eq!(spoken(&again, "Codex").as_deref(), Some("코덱스"));
        assert!(!p.delete("SkyHarbor", None).unwrap());
    }

    #[test]
    fn bad_entries_are_refused() {
        let p = Pronunciations::open(None, None);
        for (term, said, lang) in [
            ("", "가", None),
            ("스카이", "가", None),
            ("Sky", " ", None),
            ("Sky", "가", Some("korean-language")),
        ] {
            let err = p.set(term, said, lang).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRequest, "{term} {said}");
        }
    }

    #[test]
    fn changes_are_pushed_to_every_client() {
        let (tx, mut rx) = broadcast::channel(8);
        let p = Pronunciations::open(None, Some(tx));
        p.set("SkyHarbor", "스카이하버", None).unwrap();
        let ServerMsg::Pronunciations { entries } = rx.try_recv().unwrap() else {
            panic!("not a list");
        };
        assert!(entries.iter().any(|e| e.term == "SkyHarbor"));
        p.delete("SkyHarbor", None).unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn an_import_file_is_merged_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(IMPORT_FILE),
            r#"{"version":1,"entries":[
                {"term":"Zento","spoken":"젠토","language":"ko"},
                {"term":"스카이","spoken":"x","language":"ko"}
            ]}"#,
        )
        .unwrap();
        let p = Pronunciations::open(Some(dir.path()), None);
        assert_eq!(spoken(&p, "Zento").as_deref(), Some("젠토"));
        assert!(!dir.path().join(IMPORT_FILE).exists());
        assert!(dir.path().join("pronunciations-import.json.done").exists());
    }

    #[test]
    fn words_needing_a_reading() {
        let p = Pronunciations::open(None, None);
        p.set("Zento", "젠토", None).unwrap();
        assert_eq!(
            p.words_needing_reading("Zento SkyHarbor · claude (main.rs) 3번 x Codex 2024 Fluxor"),
            ["SkyHarbor", "Fluxor"]
        );
        assert!(p.words_needing_reading("세션 하나").is_empty());
    }

    #[tokio::test]
    async fn suggestions_use_the_llm_once_and_never_override_the_user() {
        let p = Pronunciations::open(None, None);
        let llm = FakeLlm::new(Ok("```json\n{\"reading\":\"스카이하버\"}\n```"));
        p.set_llm(llm.clone());
        assert_eq!(p.suggest("SkyHarbor").await.as_deref(), Some("스카이하버"));
        let entry = p
            .list(Some("ko"))
            .into_iter()
            .find(|e| e.term == "SkyHarbor")
            .unwrap();
        assert_eq!(entry.source, PronunciationSource::Auto);
        let (system, user) = lock(&llm.prompts)[0].clone();
        assert_eq!(system, LLM_SYSTEM_PROMPT);
        assert_eq!(user, "SkyHarbor");
        // Asked once only.
        assert_eq!(p.suggest("Sky Harbor").await, None);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
        // The user's word wins and a later suggestion does not replace it.
        p.set("Zento", "젠또", None).unwrap();
        assert_eq!(p.suggest("Zento").await, None);
        assert_eq!(spoken(&p, "Zento").as_deref(), Some("젠또"));
        // A user edit of a suggestion becomes the user's.
        p.set("SkyHarbor", "스카이 하버", None).unwrap();
        let entry = p
            .list(Some("ko"))
            .into_iter()
            .find(|e| term_key(&e.term) == "sky harbor")
            .unwrap();
        assert_eq!(entry.source, PronunciationSource::User);
        assert_eq!(entry.spoken, "스카이 하버");
    }

    #[tokio::test]
    async fn unusable_llm_answers_fall_back_to_the_rules() {
        let p = Pronunciations::open(None, None);
        p.set_llm(FakeLlm::new(Ok("It is read SkyHarbor.")));
        assert_eq!(
            p.suggest("AutoPlatform").await.as_deref(),
            Some("오토플랫폼")
        );
        // No rule either: nothing stored, and not asked again.
        assert_eq!(p.suggest("Zentorix").await, None);
        assert_eq!(spoken(&p, "Zentorix"), None);

        let q = Pronunciations::open(None, None);
        q.set_llm(FakeLlm::new(Err(())));
        assert_eq!(
            q.suggest("QRT Server").await.as_deref(),
            Some("큐알티 서버")
        );

        // Without an LLM: the rules.
        let r = Pronunciations::open(None, None);
        assert_eq!(
            r.suggest("DefenseCampus").await.as_deref(),
            Some("디펜스캠퍼스")
        );
    }

    #[tokio::test]
    async fn the_llm_limit_defers_suggestions() {
        let p = Pronunciations::open(None, None);
        let llm = FakeLlm::new(Ok(r#"{"reading":"가나"}"#));
        p.set_llm(llm.clone());
        for _ in 0..LLM_PER_WINDOW {
            assert!(p.llm_allowed());
        }
        assert_eq!(p.suggest("Zentorix").await, None);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
        // Not marked as tried: a later pass asks again.
        assert!(!p.words_needing_reading("Zentorix").is_empty());
    }

    #[test]
    fn deleted_suggestions_are_not_suggested_again() {
        let p = Pronunciations::open(None, None);
        p.set("SkyHarbor", "스카이하버", None).unwrap();
        p.delete("Sky-Harbor", None).unwrap();
        assert!(p.words_needing_reading("SkyHarbor").is_empty());
    }

    fn manager() -> SessionManager {
        SessionManager::new(
            crate::sessions::ManagerConfig {
                limits: crate::events::LogLimits::default(),
                exited_retention: Duration::from_secs(60),
                pause_scope: vca_broker::contain::Scope::ChildrenOnly,
                heuristics_interval: Duration::from_secs(1),
                control_endpoint: PathBuf::from("unused"),
                attach_dir: None,
                workspace_roots: crate::workspace::WorkspaceRoots::new(Vec::new()),
                session_names: None,
                allow_generic_launch: false,
            },
            None,
        )
    }

    async fn request(
        api: &crate::api::Api,
        client: &Arc<crate::api::Client>,
        rx: &mut mpsc::Receiver<vca_core::Envelope<ServerMsg>>,
        body: vca_core::ClientMsg,
    ) -> ServerMsg {
        api.handle(client, vca_core::Envelope { id: 7, body }).await;
        loop {
            let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if env.id == 7 {
                return env.body;
            }
        }
    }

    fn code(msg: &ServerMsg) -> Option<ErrorCode> {
        match msg {
            ServerMsg::Error { code, .. } => Some(*code),
            _ => None,
        }
    }

    #[tokio::test]
    async fn scopes_through_the_api() {
        use vca_core::ClientMsg;
        use vca_net::Scope;
        let api = crate::api::Api::new(manager(), "test-pc".into(), Duration::from_secs(60));
        let device = |scope| {
            Some(crate::api::DeviceIdentity {
                device_id: "dev_1".into(),
                scope,
            })
        };
        let set = ClientMsg::SetPronunciation {
            term: "SkyHarbor".into(),
            spoken: "스카이하버".into(),
            language: Some("ko-KR".into()),
        };
        let delete = ClientMsg::DeletePronunciation {
            term: "Sky Harbor".into(),
            language: None,
        };
        let list = ClientMsg::ListPronunciations {
            language: Some("ko".into()),
        };
        // Without the dictionary: unsupported, and not in the welcome.
        let (local, mut lrx) = api.connect("local");
        let reply = request(&api, &local, &mut lrx, list.clone()).await;
        assert_eq!(code(&reply), Some(ErrorCode::Unsupported));

        let dict = Pronunciations::open(None, Some(api.notices()));
        api.set_pronunciations(dict.clone());
        let welcome = request(
            &api,
            &local,
            &mut lrx,
            ClientMsg::Hello {
                protocol_version: vca_core::PROTOCOL_VERSION,
                app_version: "test".into(),
            },
        )
        .await;
        let ServerMsg::Welcome { features, .. } = welcome else {
            panic!("{welcome:?}");
        };
        assert!(features.contains(&vca_core::FEATURE_PRONUNCIATIONS.to_owned()));

        // View: lists, never changes.
        let (viewer, mut vrx) = api.connect_as("remote", device(Scope::View));
        let reply = request(&api, &viewer, &mut vrx, list.clone()).await;
        let ServerMsg::Pronunciations { entries } = reply else {
            panic!("{reply:?}");
        };
        assert!(entries.iter().all(|e| e.language == "ko"));
        assert!(entries.iter().any(|e| e.term == "Codex"));
        for msg in [set.clone(), delete.clone()] {
            let reply = request(&api, &viewer, &mut vrx, msg).await;
            assert_eq!(code(&reply), Some(ErrorCode::Forbidden), "{reply:?}");
        }
        assert!(dict.list(Some("ko")).iter().all(|e| e.term != "SkyHarbor"));

        // Command: teaches and deletes; every client gets the new list.
        let (commander, mut crx) = api.connect_as("remote", device(Scope::Command));
        let reply = request(&api, &commander, &mut crx, set).await;
        let ServerMsg::Pronunciations { entries } = reply else {
            panic!("{reply:?}");
        };
        let taught = entries.iter().find(|e| e.term == "SkyHarbor").unwrap();
        assert_eq!(taught.spoken, "스카이하버");
        assert_eq!(taught.language, "ko");
        assert_eq!(taught.source, PronunciationSource::User);
        let pushed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let env = vrx.recv().await.unwrap();
                if env.id == 0
                    && let ServerMsg::Pronunciations { entries } = env.body
                {
                    return entries;
                }
            }
        })
        .await
        .unwrap();
        assert!(pushed.iter().any(|e| e.term == "SkyHarbor"));
        let bad = ClientMsg::SetPronunciation {
            term: "스카이".into(),
            spoken: "x".into(),
            language: None,
        };
        let reply = request(&api, &commander, &mut crx, bad).await;
        assert_eq!(code(&reply), Some(ErrorCode::InvalidRequest));
        let reply = request(&api, &commander, &mut crx, delete.clone()).await;
        assert!(
            matches!(reply, ServerMsg::Pronunciations { .. }),
            "{reply:?}"
        );
        let reply = request(&api, &commander, &mut crx, delete).await;
        assert_eq!(code(&reply), Some(ErrorCode::NotFound));
    }

    #[tokio::test]
    async fn session_names_get_a_suggestion_in_the_background() {
        let p = Pronunciations::open(None, None);
        p.set_llm(FakeLlm::new(Ok(r#"{"reading":"젠토릭스"}"#)));
        let mgr = manager();
        let _worker = p.start(&mgr);
        p.observe_name("Zentorix AutoPlatform 정리");
        tokio::time::timeout(Duration::from_secs(5), async {
            while spoken(&p, "Zentorix").is_none() || spoken(&p, "AutoPlatform").is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(spoken(&p, "Zentorix").as_deref(), Some("젠토릭스"));
        // The fake LLM gives every name the same reading.
        assert_eq!(spoken(&p, "AutoPlatform").as_deref(), Some("젠토릭스"));
        assert!(p.words_needing_reading("Zentorix AutoPlatform").is_empty());
    }
}
