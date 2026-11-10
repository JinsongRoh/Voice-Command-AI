//! Server-side speech recognition (VOC-01): audio from a client goes
//! through the configured STT engine, filler clean-up, the developer
//! vocabulary engine (base, project and personal layers), the optional LLM
//! context correction (layer `context`, final chunks only) and the intent
//! router. The result is only returned for review (VOC-03); nothing is sent
//! to a session from here.
//!
//! Engines and correction LLMs are profiles (see `config.rs`); each paired
//! device picks one of each (`SetSpeechPreferences`, stored in the data
//! directory), else the defaults apply.
//!
//! With a correction LLM selected, a rolling topic per dictation (keyed by
//! session, else by client) is refreshed every few final chunks; its domain
//! terms are added to the recognizer's prompt hints and to the correction
//! prompt, and its label is returned with every transcript. Every LLM failure
//! is fail-open: the vocabulary-corrected text is returned.
//!
//! The engine is off by default. API keys are read from environment
//! variables named in the configuration, never from the file itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::Semaphore;
use vca_core::error::ErrorCode;
use vca_core::{
    AudioEncoding, Error, PersonalTerm, SPEECH_LLM_NONE, ServerMsg, SessionId,
    TranscriptCorrection, TranscriptIntent,
};
use vca_speech::intent::SessionAlias;
use vca_speech::lexicon::{Layer, Lexicon, PersonalLexicon, ProjectLexicon, ProjectScanConfig};
use vca_speech::llm::{Applied, ContextCorrector, CorrectionInput};
use vca_speech::{
    AudioFormat, AudioInput, Intent, IntentContext, IntentRouter, SpeechError, SttEngine, Target,
    TranscribeOptions, VocabularyEngine, cleanup::FillerRemover,
};

use crate::config::{LlmProfileKind, SpeechConfig, SpeechEngineProfile};
use crate::speech_profiles::{DevicePref, EngineSlot, LlmSlot, Prefs, Secrets};
use crate::util::lock;

/// Project vocabularies are rescanned after this long.
const PROJECT_TTL: Duration = Duration::from_secs(600);
/// Characters of vocabulary hints sent to prompt-style engines.
const HINT_CHARS: usize = 600;
/// Phrase hints sent to phrase-boosting engines.
const HINT_PHRASES: usize = 200;
/// Characters of preceding dictation added to the prompt (Whisper reads
/// about 224 prompt tokens, and the end of the prompt matters most).
const CONTEXT_CHARS: usize = 300;
const PERSONAL_FILE: &str = "personal.json";
/// Topic state of an idle dictation is forgotten after this long.
const TOPIC_TTL: Duration = Duration::from_secs(30 * 60);
/// Dictations whose topic is kept at once (the oldest is dropped first).
const MAX_TOPIC_KEYS: usize = 256;
/// Characters of recent final text kept per dictation for topic detection.
const TOPIC_TEXT_CHARS: usize = 2000;
/// The topic is refreshed early once this much final text was added.
const TOPIC_GROWTH_CHARS: usize = 600;
/// The first topic is detected once this much text is known.
const MIN_TOPIC_CHARS: usize = 12;
/// Characters of topic terms placed before the vocabulary hints.
const TOPIC_HINT_CHARS: usize = 250;
/// Vocabulary hint terms sent to the correction prompt.
const LLM_VOCAB_TERMS: usize = 40;
/// Correction layer name of the LLM context pass.
const CONTEXT_LAYER: &str = "context";
/// Longest wait of an interim preview for a recognizer slot: a stale
/// preview fails fast instead of queuing behind slow requests.
const PREVIEW_PERMIT_WAIT: Duration = Duration::from_secs(10);
/// Longest wait of a final chunk for a recognizer slot.
const FINAL_PERMIT_WAIT: Duration = Duration::from_secs(30);

/// How long a request waits for a recognizer slot, by kind.
#[derive(Debug, Clone, Copy)]
struct PermitWait {
    preview: Duration,
    final_chunk: Duration,
}

impl Default for PermitWait {
    fn default() -> Self {
        Self {
            preview: PREVIEW_PERMIT_WAIT,
            final_chunk: FINAL_PERMIT_WAIT,
        }
    }
}

/// Vocabulary engines per project folder (`None`: no project), with the
/// time they were built and the [`Personal::generation`] they were built from.
type VocabCache = HashMap<Option<PathBuf>, (Instant, u64, Arc<VocabularyEngine>)>;

/// The personal vocabulary in use. A saved list replaces it; dictations
/// already running finish with the engine they hold, and cached engines of an
/// older generation are not used again.
struct Personal {
    generation: u64,
    lexicon: Option<PersonalLexicon>,
}

fn read_personal(path: &Path) -> anyhow::Result<Option<PersonalLexicon>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(
            PersonalLexicon::from_json(&text)
                .with_context(|| format!("reading {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// One recognition request.
#[derive(Debug, Clone)]
pub struct TranscribeRequest {
    pub audio: Vec<u8>,
    pub format: AudioEncoding,
    pub sample_rate: Option<u32>,
    pub language: Option<String>,
    /// Folder of the selected session, for the project vocabulary.
    pub project_dir: Option<PathBuf>,
    /// Preceding dictation text (see `ClientMsg::Transcribe::context`).
    pub context: Option<String>,
    /// A final chunk (see `ClientMsg::Transcribe::is_final`): only these get
    /// the LLM context correction and update the topic.
    pub is_final: bool,
    /// The dictation the topic belongs to (session, else client).
    pub topic_key: Option<String>,
    /// Paired device id whose engine and LLM preference applies; `None`
    /// for the owner's local clients ([`LOCAL_DEVICE`]).
    pub device: Option<String>,
}

fn not_configured(id: &str, credential: &str) -> Error {
    Error::api(
        ErrorCode::InvalidRequest,
        format!(
            "speech profile {id:?} is not configured: add the key on the PC with              `vca speech login {credential}`"
        ),
    )
}

/// Topic terms first (they describe the current dictation), then the
/// vocabulary hints, within [`HINT_CHARS`].
fn hints_with_topic(terms: &[String], vocab_hints: impl FnOnce(usize) -> String) -> String {
    let mut out = String::new();
    for term in terms {
        let extra = if out.is_empty() { 0 } else { 2 };
        if out.chars().count() + extra + term.chars().count() > TOPIC_HINT_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(term);
    }
    let used = out.chars().count();
    let room = HINT_CHARS.saturating_sub(if used == 0 { 0 } else { used + 2 });
    let vocab = vocab_hints(room);
    match (out.is_empty(), vocab.is_empty()) {
        (_, true) => out,
        (true, false) => vocab,
        (false, false) => format!("{out}, {vocab}"),
    }
}

/// The hands-free voice words (`vca_speech::radio::prompt_words`) first,
/// then the vocabulary hints without them, within `room` characters. The
/// voice words are short and must not be crowded out by project terms.
fn voice_then_vocab(
    voice: &[&str],
    room: usize,
    vocab_hints: impl FnOnce(usize) -> String,
) -> String {
    let mut out = String::new();
    for word in voice {
        let extra = if out.is_empty() { 0 } else { 2 };
        if out.chars().count() + extra + word.chars().count() > room {
            break;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(word);
    }
    let used = out.chars().count();
    let rest = vocab_hints(room.saturating_sub(if used == 0 { 0 } else { used + 2 }));
    for term in rest.split(", ").filter(|t| !t.is_empty()) {
        if voice.iter().any(|w| w.eq_ignore_ascii_case(term)) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(term);
    }
    out
}

/// `first` followed by the entries of `rest` not already present
/// (case-insensitive), at most `limit` in total.
fn merge_terms(first: &[String], rest: Vec<String>, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in first.iter().cloned().chain(rest) {
        if out.len() == limit {
            break;
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out
}

/// The last `n` characters of `s`.
fn tail(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    let start = s.char_indices().nth(count - n).map_or(s.len(), |(i, _)| i);
    &s[start..]
}

/// Vocabulary hints followed by the tail of the preceding dictation, which
/// Whisper-style engines treat as the text just before the audio.
fn prompt_with_context(hints: String, context: Option<&str>) -> Option<String> {
    let context: String = context
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let context = context.trim();
    let skip = context.chars().count().saturating_sub(CONTEXT_CHARS);
    let tail: String = context.chars().skip(skip).collect();
    match (hints.is_empty(), tail.is_empty()) {
        (true, true) => None,
        (false, true) => Some(hints),
        (true, false) => Some(tail),
        (false, false) => Some(format!("{hints}\n{tail}")),
    }
}

/// A reviewed-to-be transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptResult {
    pub text: String,
    pub raw_text: String,
    pub language: Option<String>,
    pub corrections: Vec<TranscriptCorrection>,
    pub intent: TranscriptIntent,
    pub engine: String,
    /// Topic of the dictation, when the LLM pass detected one.
    pub topic: Option<String>,
}

/// Rolling topic of one dictation.
struct TopicState {
    topic: Option<String>,
    terms: Vec<String>,
    /// Recent final text (at most [`TOPIC_TEXT_CHARS`]).
    recent: String,
    finals_since: u32,
    chars_since: usize,
    refreshing: bool,
    touched: Instant,
}

impl TopicState {
    fn new() -> Self {
        Self {
            topic: None,
            terms: Vec::new(),
            recent: String::new(),
            finals_since: 0,
            chars_since: 0,
            refreshing: false,
            touched: Instant::now(),
        }
    }
}

/// Rolling topics per dictation, and the bound on concurrent LLM requests
/// (shared by every LLM profile).
struct Topics {
    permits: Semaphore,
    every_n: u32,
    map: Mutex<HashMap<String, TopicState>>,
}

impl Topics {
    fn new(max_concurrent: usize, every_n: u32) -> Self {
        Self {
            permits: Semaphore::new(max_concurrent.max(1)),
            every_n: every_n.max(1),
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Drops expired dictations, and the oldest beyond [`MAX_TOPIC_KEYS`].
    fn prune(map: &mut HashMap<String, TopicState>) {
        map.retain(|_, s| s.touched.elapsed() < TOPIC_TTL);
        while map.len() > MAX_TOPIC_KEYS {
            let Some(oldest) = map
                .iter()
                .min_by_key(|(_, s)| s.touched)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            map.remove(&oldest);
        }
    }

    /// The current topic label and terms of a dictation.
    fn snapshot(&self, key: &str) -> (Option<String>, Vec<String>) {
        let mut map = lock(&self.map);
        Self::prune(&mut map);
        map.get(key)
            .map(|s| (s.topic.clone(), s.terms.clone()))
            .unwrap_or_default()
    }

    /// Validated fixes for a final chunk, or `None` (fail-open) on any
    /// error, rejection or timeout. Waiting for a permit counts toward the
    /// timeout.
    async fn correct(
        &self,
        corrector: &ContextCorrector,
        input: CorrectionInput<'_>,
        timeout: Duration,
    ) -> Option<Applied> {
        let attempt = async {
            let _permit = self.permits.acquire().await.ok()?;
            match corrector.correct(&input).await {
                Ok(applied) => Some(applied),
                Err(err) => {
                    tracing::debug!(%err, "context correction skipped");
                    None
                }
            }
        };
        match tokio::time::timeout(timeout, attempt).await {
            Ok(result) => result,
            Err(_) => {
                tracing::debug!("context correction timed out");
                None
            }
        }
    }

    /// Records a final chunk and starts a topic refresh when one is due.
    fn note_final(
        self: &Arc<Self>,
        key: &str,
        text: &str,
        context: Option<&str>,
        corrector: ContextCorrector,
        timeout: Duration,
    ) {
        let (previous, recent) = {
            let mut map = lock(&self.map);
            Self::prune(&mut map);
            let state = map.entry(key.to_owned()).or_insert_with(TopicState::new);
            state.touched = Instant::now();
            if state.recent.is_empty()
                && let Some(ctx) = context.map(str::trim).filter(|c| !c.is_empty())
            {
                state.recent = ctx.to_owned();
            }
            let text = text.trim();
            if !text.is_empty() {
                if !state.recent.is_empty() {
                    state.recent.push(' ');
                }
                state.recent.push_str(text);
                state.recent = tail(&state.recent, TOPIC_TEXT_CHARS).to_owned();
                state.finals_since += 1;
                state.chars_since += text.chars().count();
            }
            let due = if state.topic.is_none() {
                state.recent.chars().count() >= MIN_TOPIC_CHARS
            } else {
                state.finals_since >= self.every_n || state.chars_since >= TOPIC_GROWTH_CHARS
            };
            if !due || state.refreshing {
                return;
            }
            state.refreshing = true;
            state.finals_since = 0;
            state.chars_since = 0;
            (state.topic.clone(), state.recent.clone())
        };
        let topics = Arc::clone(self);
        let key = key.to_owned();
        tokio::spawn(async move {
            topics
                .refresh(key, previous, recent, corrector, timeout)
                .await
        });
    }

    async fn refresh(
        &self,
        key: String,
        previous: Option<String>,
        text: String,
        corrector: ContextCorrector,
        timeout: Duration,
    ) {
        let attempt = async {
            let _permit = self.permits.acquire().await.ok()?;
            match corrector.detect_topic(previous.as_deref(), &text).await {
                Ok(topic) => Some(topic),
                Err(err) => {
                    tracing::debug!(%err, "topic detection failed");
                    None
                }
            }
        };
        let detected = tokio::time::timeout(timeout, attempt).await.ok().flatten();
        let mut map = lock(&self.map);
        if let Some(state) = map.get_mut(&key) {
            state.refreshing = false;
            if let Some(t) = detected {
                state.topic = Some(t.topic);
                state.terms = t.terms;
                state.touched = Instant::now();
            }
        }
    }
}

/// The speech pipeline. Shared by all clients.
pub struct Speech {
    engines: Vec<EngineSlot>,
    llms: Vec<LlmSlot>,
    default_engine: Option<String>,
    default_llm: Option<String>,
    secrets: Secrets,
    prefs: Prefs,
    topics: Arc<Topics>,
    cfg: SpeechConfig,
    base: Lexicon,
    personal_path: PathBuf,
    personal: Mutex<Personal>,
    /// Held across a save, so two saves cannot overwrite each other's file
    /// and memory in a different order.
    personal_save: Mutex<()>,
    router: IntentRouter,
    fillers: FillerRemover,
    vocab: Mutex<VocabCache>,
    permits: Semaphore,
    permit_wait: PermitWait,
}

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

/// Device key of clients without a paired device (the owner's local CLI).
pub const LOCAL_DEVICE: &str = "local";

impl Speech {
    /// Builds the pipeline from the configured profiles; `None` when no
    /// engine is configured. Clients are created on first use, so profiles
    /// without a credential yet are listed as not configured instead of
    /// failing the start.
    pub fn from_config(cfg: &SpeechConfig, data_dir: &Path) -> anyhow::Result<Option<Self>> {
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("invalid [speech] configuration: {e}"))?;
        let engines: Vec<EngineSlot> = cfg
            .engine_profiles()
            .into_iter()
            .map(EngineSlot::new)
            .collect();
        if engines.is_empty() {
            return Ok(None);
        }
        Self::build(engines, cfg.clone(), data_dir).map(Some)
    }

    /// A pipeline around one injected `engine` (tests), as profile
    /// `default`. LLM profiles come from `cfg`.
    pub fn with_engine(
        engine: Arc<dyn SttEngine>,
        cfg: SpeechConfig,
        data_dir: &Path,
    ) -> anyhow::Result<Self> {
        let profile = SpeechEngineProfile {
            id: "default".into(),
            label: Some(engine.name().to_owned()),
            ..SpeechEngineProfile::default()
        };
        Self::with_engines(vec![(profile, engine)], cfg, data_dir)
    }

    /// A pipeline around injected engines with their profiles (tests).
    pub fn with_engines(
        engines: Vec<(SpeechEngineProfile, Arc<dyn SttEngine>)>,
        cfg: SpeechConfig,
        data_dir: &Path,
    ) -> anyhow::Result<Self> {
        let engines = engines
            .into_iter()
            .map(|(p, e)| EngineSlot::fixed(p, e))
            .collect();
        Self::build(engines, cfg, data_dir)
    }

    fn build(engines: Vec<EngineSlot>, cfg: SpeechConfig, data_dir: &Path) -> anyhow::Result<Self> {
        let languages = base_languages(&cfg);
        let filter: Option<Vec<&str>> = languages
            .as_ref()
            .map(|l| l.iter().map(String::as_str).collect());
        let base = match &cfg.lexicon_dir {
            Some(dir) => Lexicon::load_dir(dir, filter.as_deref())
                .with_context(|| format!("loading the lexicon in {}", dir.display()))?,
            None => Lexicon::builtin(filter.as_deref()).context("built-in lexicon")?,
        };
        let personal_path = data_dir.join("speech").join(PERSONAL_FILE);
        let personal = read_personal(&personal_path)?;
        let llm_timeout = Duration::from_millis(cfg.llm.timeout_ms.max(1));
        let llms: Vec<LlmSlot> = cfg
            .llm_profiles()
            .into_iter()
            .map(|p| LlmSlot::new(p, llm_timeout))
            .collect();
        let default_engine = cfg
            .default_engine_id()
            .filter(|id| engines.iter().any(|e| &e.profile.id == id))
            .or_else(|| engines.first().map(|e| e.profile.id.clone()));
        let default_llm = cfg
            .default_llm_id()
            .filter(|id| llms.iter().any(|l| &l.profile.id == id));
        Ok(Self {
            engines,
            llms,
            default_engine,
            default_llm,
            secrets: Secrets::in_data_dir(data_dir),
            prefs: Prefs::load(data_dir),
            topics: Arc::new(Topics::new(
                cfg.llm.max_concurrent,
                cfg.llm.topic_every_n_finals,
            )),
            permits: Semaphore::new(cfg.max_concurrent.max(1)),
            cfg,
            base,
            personal_path,
            personal: Mutex::new(Personal {
                generation: 0,
                lexicon: personal,
            }),
            personal_save: Mutex::new(()),
            router: IntentRouter::new(),
            fillers: FillerRemover::new(),
            vocab: Mutex::new(HashMap::new()),
            permit_wait: PermitWait::default(),
        })
    }

    /// The default engine profile id (for logs).
    pub fn engine_name(&self) -> &str {
        self.default_engine.as_deref().unwrap_or("none")
    }

    /// The detected topic of a dictation (see `TranscribeRequest::topic_key`).
    pub fn topic(&self, key: &str) -> Option<String> {
        self.topics.snapshot(key).0
    }

    /// Waits (bounded) until no background topic refresh is in flight, so a
    /// test never ends with a request still on its way to a mock server whose
    /// port another test may reuse.
    #[cfg(test)]
    async fn settle_topics(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while lock(&self.topics.map).values().any(|s| s.refreshing) {
            assert!(Instant::now() < deadline, "topic refresh never finished");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Replaces the `ant` token provider of every Anthropic profile (tests
    /// inject a fake one; nothing runs the real CLI then).
    pub fn set_token_provider(&self, provider: Arc<dyn vca_speech::llm::TokenProvider>) {
        for slot in &self.llms {
            if slot.profile.kind == LlmProfileKind::Anthropic {
                slot.set_token_provider(Arc::clone(&provider));
            }
        }
    }

    fn engine_slot(&self, device: &str) -> Option<&EngineSlot> {
        let pref = self.prefs.get(device).engine;
        pref.iter()
            .chain(self.default_engine.iter())
            .find_map(|id| self.engines.iter().find(|e| &e.profile.id == id))
            .or_else(|| self.engines.first())
    }

    fn llm_slot(&self, device: &str) -> Option<&LlmSlot> {
        let id = match self.prefs.get(device).llm {
            Some(id) if id == SPEECH_LLM_NONE => return None,
            Some(id) if self.llms.iter().any(|l| l.profile.id == id) => Some(id),
            _ => self.default_llm.clone(),
        }?;
        self.llms.iter().find(|l| l.profile.id == id)
    }

    /// The profiles and this device's choice (`ServerMsg::SpeechOptions`).
    pub fn options(&self, device: &str) -> ServerMsg {
        let engines = self
            .engines
            .iter()
            .map(|e| {
                e.info(
                    &self.secrets,
                    self.default_engine.as_deref() == Some(e.profile.id.as_str()),
                )
            })
            .collect();
        let llms = self
            .llms
            .iter()
            .map(|l| {
                l.info(
                    &self.secrets,
                    self.default_llm.as_deref() == Some(l.profile.id.as_str()),
                )
            })
            .collect();
        ServerMsg::SpeechOptions {
            engines,
            llms,
            current_engine: self.engine_slot(device).map(|e| e.profile.id.clone()),
            current_llm: self.llm_slot(device).map(|l| l.profile.id.clone()),
        }
    }

    /// Stores a device's choice. Unknown or unconfigured profiles are
    /// refused.
    pub fn set_preferences(
        &self,
        device: &str,
        engine: Option<String>,
        llm: Option<String>,
    ) -> Result<(), Error> {
        let engine = engine.filter(|e| !e.is_empty());
        let llm = llm.filter(|l| !l.is_empty());
        if let Some(id) = &engine {
            let slot = self
                .engines
                .iter()
                .find(|e| &e.profile.id == id)
                .ok_or_else(|| {
                    api_error(
                        ErrorCode::NotFound,
                        format!("no speech engine profile {id:?}"),
                    )
                })?;
            if !slot.configured(&self.secrets) {
                return Err(not_configured(id, slot.credential_name()));
            }
        }
        if let Some(id) = llm.as_ref().filter(|l| *l != SPEECH_LLM_NONE) {
            let slot = self
                .llms
                .iter()
                .find(|l| &l.profile.id == id)
                .ok_or_else(|| {
                    api_error(
                        ErrorCode::NotFound,
                        format!("no correction LLM profile {id:?}"),
                    )
                })?;
            if !slot.configured(&self.secrets) {
                return Err(not_configured(id, slot.credential_name()));
            }
        }
        self.prefs.set(device, DevicePref { engine, llm })
    }

    /// Drops the preferences of a revoked device. Blocking (a file write).
    pub(crate) fn forget_device(&self, device: &str) -> Result<(), Error> {
        self.prefs.forget(device)
    }

    /// The credential-store name for `vca speech login <profile>`: the
    /// profile's `credential` (or id), or a shared `credential` name.
    fn credential_name(&self, profile: &str) -> Result<String, Error> {
        let names = self
            .engines
            .iter()
            .map(|e| (e.profile.id.as_str(), e.credential_name()))
            .chain(
                self.llms
                    .iter()
                    .map(|l| (l.profile.id.as_str(), l.credential_name())),
            );
        let mut shared = None;
        for (id, name) in names {
            if id == profile {
                return Ok(name.to_owned());
            }
            if name == profile {
                shared = Some(name.to_owned());
            }
        }
        shared.ok_or_else(|| {
            api_error(
                ErrorCode::NotFound,
                format!(
                    "no speech profile {profile:?}; add it to [[speech.engines]] or [[speech.llms]]"
                ),
            )
        })
    }

    /// Stores a profile's API key (local channel only).
    pub fn set_credential(&self, profile: &str, secret: &str) -> Result<(), Error> {
        let name = self.credential_name(profile)?;
        self.secrets.put(&name, secret)?;
        tracing::info!(profile, "speech credential stored");
        Ok(())
    }

    /// Deletes a profile's stored API key. Returns whether one was stored.
    pub fn delete_credential(&self, profile: &str) -> Result<bool, Error> {
        let name = self.credential_name(profile)?;
        let removed = self.secrets.delete(&name)?;
        if removed {
            tracing::info!(profile, "speech credential deleted");
        }
        Ok(removed)
    }

    /// The commonly misheard words in use.
    pub fn personal_terms(&self) -> Vec<PersonalTerm> {
        lock(&self.personal)
            .lexicon
            .as_ref()
            .map(|p| crate::personal_terms::from_entries(p.lexicon().entries()))
            .unwrap_or_default()
    }

    /// Validates `terms` and replaces the list: the file first (atomically),
    /// then the vocabulary of the next dictation. Starts from the file, so a
    /// hand edit made since the last save or start is kept, and a file that
    /// no longer parses is left alone. Returns the stored list. Blocking (a
    /// file write).
    pub fn save_personal_terms(
        &self,
        terms: Vec<PersonalTerm>,
    ) -> Result<Vec<PersonalTerm>, Error> {
        let terms = crate::personal_terms::validate(terms)?;
        let _save = lock(&self.personal_save);
        let mut personal = read_personal(&self.personal_path)
            .map_err(|err| {
                tracing::warn!(%err, "cannot read the personal vocabulary");
                api_error(ErrorCode::Internal, "cannot read the stored word list")
            })?
            .unwrap_or_default();
        let entries = crate::personal_terms::to_entries(terms, personal.lexicon().entries());
        personal.set_terms(entries);
        let json = personal
            .to_json()
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
        crate::names::write_atomic(&self.personal_path, json.as_bytes()).map_err(|err| {
            tracing::warn!(%err, "cannot write {PERSONAL_FILE}");
            api_error(ErrorCode::Internal, "cannot save the word list")
        })?;
        let saved = crate::personal_terms::from_entries(personal.lexicon().entries());
        let mut current = lock(&self.personal);
        current.generation += 1;
        current.lexicon = Some(personal);
        Ok(saved)
    }

    /// Recognizes one utterance and routes it.
    pub async fn transcribe(
        &self,
        req: TranscribeRequest,
        ctx: &IntentContext,
    ) -> Result<TranscriptResult, Error> {
        if req.audio.is_empty() {
            return Err(api_error(ErrorCode::InvalidRequest, "no audio"));
        }
        if req.audio.len() > self.cfg.max_audio_bytes {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                format!(
                    "audio is {} bytes; the limit is {}",
                    req.audio.len(),
                    self.cfg.max_audio_bytes
                ),
            ));
        }
        let device = req.device.as_deref().unwrap_or(LOCAL_DEVICE);
        let slot = self
            .engine_slot(device)
            .ok_or_else(|| api_error(ErrorCode::Unsupported, "no speech engine is configured"))?;
        let engine = slot.engine(&self.secrets)?;
        let llm = self.llm_slot(device);
        let audio_ms = audio_millis(&req.audio, req.format, req.sample_rate);
        let audio = audio_input(req.audio, req.format, req.sample_rate)?;
        let is_final = req.is_final;
        let started = Instant::now();
        let vocab = self.vocabulary(req.project_dir.as_deref()).await;
        tracing::info!(
            is_final,
            audio_ms,
            elapsed_ms = elapsed_ms(started),
            "speech: vocabulary ready"
        );
        let language = req.language.clone().or_else(|| self.cfg.language.clone());
        let topic_key = req.topic_key.as_deref().filter(|_| llm.is_some());
        let (topic, topic_terms) = match topic_key {
            Some(key) => self.topics.snapshot(key),
            None => (None, Vec::new()),
        };
        let voice = vca_speech::radio::prompt_words(language.as_deref());
        let hints = hints_with_topic(&topic_terms, |room| {
            voice_then_vocab(&voice, room, |r| vocab.hint_prompt(r))
        });
        let options = TranscribeOptions {
            primary_language: language.clone(),
            secondary_languages: self.cfg.secondary_languages.clone(),
            prompt_hint: prompt_with_context(hints, req.context.as_deref()),
            phrase_hints: merge_terms(&topic_terms, vocab.hint_phrases(HINT_PHRASES), HINT_PHRASES),
        };

        // A bounded wait: when the speech server hangs (a dead connection
        // after a network change), later requests fail instead of queuing
        // for minutes. Previews give up sooner than finals.
        let wait = if is_final {
            self.permit_wait.final_chunk
        } else {
            self.permit_wait.preview
        };
        let available = self.permits.available_permits();
        let started = Instant::now();
        let permit = match tokio::time::timeout(wait, self.permits.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(api_error(ErrorCode::Unavailable, "speech is shutting down"));
            }
            Err(_) => {
                tracing::warn!(
                    is_final,
                    audio_ms,
                    waited_ms = elapsed_ms(started),
                    "speech: no recognizer slot became free; request dropped"
                );
                return Err(api_error(
                    ErrorCode::Unavailable,
                    format!(
                        "{SPEECH_BUSY}: the speech server is busy; no recognizer slot \
                         was free within {} s",
                        wait.as_secs()
                    ),
                ));
            }
        };
        tracing::info!(
            is_final,
            audio_ms,
            permits_available = available,
            elapsed_ms = elapsed_ms(started),
            "speech: recognizer slot acquired"
        );
        let started = Instant::now();
        let result = engine.transcribe(&audio, &options).await;
        // The slot covers the recognizer only; the LLM pass has its own.
        drop(permit);
        tracing::info!(
            is_final,
            audio_ms,
            ok = result.is_ok(),
            elapsed_ms = elapsed_ms(started),
            "speech: engine call finished"
        );
        let mut transcript = result.map_err(speech_error)?;
        // Drop low-confidence segments that are only a known Whisper
        // hallucination ("감사합니다." (thank you), "Thanks for watching!").
        vca_speech::cleanup::strip_hallucinated_segments(&mut transcript);
        let detected = transcript.language.clone().or(language);
        let raw_text = vca_speech::cleanup::collapse_repeats(transcript.text.trim());
        let cleaned = if self.cfg.remove_fillers {
            self.fillers.remove(&raw_text)
        } else {
            raw_text.clone()
        };
        let mut vctx = vocab.context().clone();
        vctx.language = detected.clone();
        let corrected = vocab.correct_with(&cleaned, &vctx);
        let mut corrections: Vec<TranscriptCorrection> = corrected
            .corrections
            .iter()
            .map(|c| TranscriptCorrection {
                from: c.original.clone(),
                to: c.replacement.clone(),
                layer: layer_name(c.layer).to_owned(),
            })
            .collect();
        let mut text = corrected.text;
        // Interim previews never reach the LLM (latency and cost).
        if req.is_final
            && let Some(llm) = llm
            && let Some(corrector) = llm.corrector(&self.secrets)
        {
            if !text.trim().is_empty() {
                // Terms already in the text come first: they are correct
                // as dictated and must not be "fixed".
                let known = vocab.known_terms_in(&text);
                let limit = LLM_VOCAB_TERMS + topic_terms.len() + known.len();
                let mut hints = known;
                hints.extend(vocab.hint_phrases(LLM_VOCAB_TERMS));
                let terms = merge_terms(&topic_terms, hints, limit);
                let input = CorrectionInput {
                    text: &text,
                    preceding: req.context.as_deref(),
                    topic: topic.as_deref(),
                    terms: &terms,
                    language: detected.as_deref(),
                };
                let started = Instant::now();
                let applied = self.topics.correct(&corrector, input, llm.timeout).await;
                tracing::info!(
                    is_final,
                    audio_ms,
                    applied = applied.is_some(),
                    elapsed_ms = elapsed_ms(started),
                    "speech: LLM correction finished"
                );
                if let Some(applied) = applied {
                    corrections.extend(applied.fixes.into_iter().map(|f| TranscriptCorrection {
                        from: f.from,
                        to: f.to,
                        layer: CONTEXT_LAYER.to_owned(),
                    }));
                    text = applied.text;
                }
            }
            if let Some(key) = topic_key {
                self.topics
                    .note_final(key, &text, req.context.as_deref(), corrector, llm.timeout);
            }
        }
        let intent = to_intent(self.router.route(&text, ctx));
        Ok(TranscriptResult {
            text,
            raw_text,
            language: detected,
            corrections,
            intent,
            engine: engine.name().to_owned(),
            topic,
        })
    }

    /// The vocabulary engine for a project folder, scanning it at most once
    /// per [`PROJECT_TTL`] and rebuilding it after a saved word list.
    async fn vocabulary(&self, project: Option<&Path>) -> Arc<VocabularyEngine> {
        let key = project
            .filter(|_| self.cfg.project_vocabulary)
            .map(Path::to_path_buf);
        let generation = lock(&self.personal).generation;
        if let Some((at, built_from, engine)) = lock(&self.vocab).get(&key)
            && at.elapsed() < PROJECT_TTL
            && *built_from == generation
        {
            return Arc::clone(engine);
        }
        // Read together: an engine must never claim a newer list than it has.
        let (generation, personal) = {
            let personal = lock(&self.personal);
            (personal.generation, personal.lexicon.clone())
        };
        let mut engine = VocabularyEngine::new(self.base.clone());
        if let Some(p) = personal {
            engine.set_personal(p);
        }
        if let Some(dir) = key.clone() {
            let scanned = tokio::task::spawn_blocking(move || {
                vca_speech::lexicon::project::extract(&dir, &ProjectScanConfig::default())
            })
            .await;
            match scanned {
                Ok(Ok(project)) => apply_project(&mut engine, project),
                Ok(Err(err)) => tracing::debug!(%err, "project vocabulary scan failed"),
                Err(err) => tracing::debug!(%err, "project vocabulary scan panicked"),
            }
        }
        let engine = Arc::new(engine);
        let mut cache = lock(&self.vocab);
        cache.retain(|_, (at, built_from, _)| {
            at.elapsed() < PROJECT_TTL && *built_from >= generation
        });
        cache.insert(key, (Instant::now(), generation, Arc::clone(&engine)));
        engine
    }
}

fn apply_project(engine: &mut VocabularyEngine, project: ProjectLexicon) {
    let mut ctx = engine.context().clone();
    ctx.topics = project.ecosystems.clone();
    engine.set_project(project);
    engine.set_context(ctx);
}

/// Language directories of the base lexicon to load: the configured
/// languages plus English (technical terms are mostly English). `None`
/// loads all when no language is configured.
fn base_languages(cfg: &SpeechConfig) -> Option<Vec<String>> {
    let primary = cfg.language.as_deref()?;
    let mut out = vec!["en".to_owned()];
    for l in std::iter::once(primary).chain(cfg.secondary_languages.iter().map(String::as_str)) {
        let base = vca_speech::stt::base_language(l);
        if !out.contains(&base) {
            out.push(base);
        }
    }
    Some(out)
}

fn audio_input(
    bytes: Vec<u8>,
    format: AudioEncoding,
    sample_rate: Option<u32>,
) -> Result<AudioInput, Error> {
    let format = match format {
        AudioEncoding::Wav => AudioFormat::Wav,
        AudioEncoding::Pcm16 => AudioFormat::Pcm16,
        AudioEncoding::Webm => AudioFormat::Webm,
        AudioEncoding::Ogg => AudioFormat::Ogg,
        AudioEncoding::M4a => AudioFormat::M4a,
    };
    let rate = match (format, sample_rate) {
        (AudioFormat::Pcm16, None | Some(0)) => {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                "pcm16 audio needs a sample rate",
            ));
        }
        (AudioFormat::Pcm16, _) if bytes.len() % 2 != 0 => {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                "pcm16 audio has an odd number of bytes",
            ));
        }
        (_, rate) => rate.unwrap_or(0),
    };
    Ok(AudioInput::from_encoded(bytes, format, rate))
}

/// Message prefix of a request that found no free recognizer slot in time.
const SPEECH_BUSY: &str = "speech_busy";

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Audio length in milliseconds for logs: exact for PCM16, from the header
/// for a plain 44-byte-header WAV, 0 when unknown (compressed formats).
fn audio_millis(bytes: &[u8], format: AudioEncoding, sample_rate: Option<u32>) -> u64 {
    let (rate, channels, bits, data) = match format {
        AudioEncoding::Pcm16 => (sample_rate.unwrap_or(0), 1u16, 16u16, bytes.len()),
        AudioEncoding::Wav if bytes.len() > 44 => {
            let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
            (rate, u16_at(22), u16_at(34), bytes.len() - 44)
        }
        _ => return 0,
    };
    let bytes_per_sec = u64::from(rate) * u64::from(channels) * u64::from(bits / 8);
    if bytes_per_sec == 0 {
        return 0;
    }
    (data as u64).saturating_mul(1000) / bytes_per_sec
}

/// Message prefix of a speech server that could not be reached (connection
/// refused or dropped, already retried once). The UI matches it to show a
/// friendly "try again" message instead of the transport error.
const SPEECH_UNREACHABLE: &str = "speech_unreachable";

fn speech_error(err: SpeechError) -> Error {
    if err.is_unreachable() {
        let detail = match err {
            // The request URL can carry an API key (Google `key=`), so drop it.
            SpeechError::Http(e) => e.without_url().to_string(),
            other => other.to_string(),
        };
        return api_error(
            ErrorCode::Unavailable,
            format!("{SPEECH_UNREACHABLE}: the speech server could not be reached ({detail})"),
        );
    }
    let code = match &err {
        SpeechError::Audio(_) => ErrorCode::InvalidRequest,
        SpeechError::Unsupported(_) => ErrorCode::Unsupported,
        _ => ErrorCode::Unavailable,
    };
    let message = match err {
        // The request URL can carry an API key (Google `key=`), so drop it.
        SpeechError::Http(e) => format!("http error: {}", e.without_url()),
        other => other.to_string(),
    };
    api_error(code, message)
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Base => "base",
        Layer::Project => "project",
        Layer::Personal => "personal",
    }
}

fn to_intent(intent: Intent) -> TranscriptIntent {
    match intent {
        Intent::Prompt { .. } => TranscriptIntent::Prompt,
        Intent::Ambiguous { .. } => TranscriptIntent::Ambiguous,
        Intent::Control {
            action,
            target,
            confidence,
        } => {
            let session_id = match target {
                None | Some(Target::Current) | Some(Target::Template(_)) => None,
                Some(Target::Session(id)) => Some(SessionId(id)),
                // A session was named but not found: let the user decide.
                Some(Target::SessionNumber(_)) | Some(Target::SessionName(_)) => {
                    return TranscriptIntent::Ambiguous;
                }
            };
            let name = serde_json::to_value(action)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            TranscriptIntent::Control {
                action: name,
                session_action: action.session_action(),
                session_id,
                confidence,
            }
        }
    }
}

/// Builds the intent context from the session list.
pub fn intent_context(
    sessions: &[vca_core::SessionInfo],
    selected: Option<&SessionId>,
) -> IntentContext {
    let aliases = sessions
        .iter()
        .map(|s| {
            let mut names = vec![s.display_name.clone()];
            if let vca_core::SessionOrigin::Local { project_dir, .. } = &s.origin
                && let Some(folder) = project_dir.file_name()
            {
                let folder = folder.to_string_lossy().into_owned();
                if !names.contains(&folder) {
                    names.push(folder);
                }
            }
            SessionAlias {
                id: s.id.0.clone(),
                number: None,
                names,
            }
        })
        .collect();
    let pending_decision = selected
        .and_then(|id| sessions.iter().find(|s| &s.id == id))
        .is_some_and(|s| matches!(s.state, vca_core::SessionState::AwaitingDecision));
    IntentContext {
        sessions: aliases,
        templates: Vec::new(),
        pending_decision,
    }
}

#[cfg(test)]
mod tests {
    use vca_speech::stt::MockSttEngine;
    use vca_speech::{ControlAction, Transcript};

    use super::*;

    fn speech(mock: MockSttEngine) -> (Speech, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SpeechConfig {
            language: Some("en".into()),
            ..SpeechConfig::default()
        };
        let s = Speech::with_engine(Arc::new(mock), cfg, dir.path()).unwrap();
        (s, dir)
    }

    fn wav() -> Vec<u8> {
        vca_speech::audio::encode_wav_pcm16(&[0i16; 1600], 16_000, 1).unwrap()
    }

    fn req(audio: Vec<u8>, format: AudioEncoding) -> TranscribeRequest {
        TranscribeRequest {
            audio,
            format,
            sample_rate: None,
            language: None,
            project_dir: None,
            context: None,
            is_final: false,
            topic_key: None,
            device: None,
        }
    }

    /// An engine whose requests never finish (a dead connection).
    struct HangingEngine;

    #[async_trait::async_trait]
    impl SttEngine for HangingEngine {
        fn name(&self) -> &str {
            "hanging"
        }

        async fn transcribe(
            &self,
            _audio: &AudioInput,
            _options: &TranscribeOptions,
        ) -> vca_speech::Result<Transcript> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn a_full_recognizer_fails_waiting_requests_fast() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SpeechConfig {
            language: Some("en".into()),
            max_concurrent: 1,
            ..SpeechConfig::default()
        };
        let mut s = Speech::with_engine(Arc::new(HangingEngine), cfg, dir.path()).unwrap();
        s.permit_wait = PermitWait {
            preview: Duration::from_millis(50),
            final_chunk: Duration::from_millis(200),
        };
        let s = Arc::new(s);
        let ctx = IntentContext::default();
        // The first request holds the only slot forever.
        let holder = {
            let s = Arc::clone(&s);
            let ctx = ctx.clone();
            tokio::spawn(async move { s.transcribe(req(wav(), AudioEncoding::Wav), &ctx).await })
        };
        while s.permits.available_permits() > 0 {
            tokio::task::yield_now().await;
        }
        let started = Instant::now();
        let err = s
            .transcribe(req(wav(), AudioEncoding::Wav), &ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(err.to_string().contains(SPEECH_BUSY), "{err}");
        let preview_wait = started.elapsed();
        assert!(
            preview_wait < Duration::from_millis(190),
            "{preview_wait:?}"
        );

        let mut fin = req(wav(), AudioEncoding::Wav);
        fin.is_final = true;
        let started = Instant::now();
        let err = s.transcribe(fin, &ctx).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(
            started.elapsed() >= Duration::from_millis(200),
            "finals wait longer"
        );
        holder.abort();
    }

    #[tokio::test]
    async fn the_slot_is_freed_after_the_engine_call() {
        let (s, _dir) = speech(MockSttEngine::always("hello"));
        let before = s.permits.available_permits();
        s.transcribe(req(wav(), AudioEncoding::Wav), &IntentContext::default())
            .await
            .unwrap();
        assert_eq!(s.permits.available_permits(), before);
    }

    #[test]
    fn audio_length_is_estimated_for_logs() {
        // 1600 samples at 16 kHz mono.
        assert_eq!(audio_millis(&wav(), AudioEncoding::Wav, None), 100);
        assert_eq!(
            audio_millis(&[0; 32_000], AudioEncoding::Pcm16, Some(16_000)),
            1000
        );
        assert_eq!(audio_millis(&[0; 10], AudioEncoding::Pcm16, None), 0);
        assert_eq!(audio_millis(&[0; 10], AudioEncoding::Webm, None), 0);
    }

    #[test]
    fn context_follows_the_hints_and_is_trimmed() {
        assert_eq!(prompt_with_context(String::new(), None), None);
        assert_eq!(prompt_with_context(String::new(), Some("  \n ")), None);
        assert_eq!(
            prompt_with_context("pnpm, useEffect".into(), Some("고쳐 줘\u{7}")).as_deref(),
            Some("pnpm, useEffect\n고쳐 줘")
        );
        let long = "가".repeat(CONTEXT_CHARS + 50);
        let p = prompt_with_context(String::new(), Some(&long)).unwrap();
        assert_eq!(p.chars().count(), CONTEXT_CHARS);
    }

    #[tokio::test]
    async fn corrects_vocabulary_and_removes_fillers() {
        let (s, _d) = speech(MockSttEngine::always(
            "um run p n p m test with kube control",
        ));
        let out = s
            .transcribe(req(wav(), AudioEncoding::Wav), &IntentContext::default())
            .await
            .unwrap();
        assert_eq!(out.raw_text, "um run p n p m test with kube control");
        assert!(out.text.contains("pnpm"), "{}", out.text);
        assert!(out.text.contains("kubectl"), "{}", out.text);
        assert!(!out.text.starts_with("um"), "{}", out.text);
        assert!(
            out.corrections
                .iter()
                .any(|c| c.to == "pnpm" && c.layer == "base")
        );
        assert_eq!(out.intent, TranscriptIntent::Prompt);
        assert_eq!(out.engine, "mock");
    }

    #[tokio::test]
    async fn routes_control_commands_to_named_sessions() {
        let (s, _d) = speech(MockSttEngine::always("pause the payments session"));
        let ctx = IntentContext {
            sessions: vec![SessionAlias {
                id: "s-7".into(),
                number: None,
                names: vec!["payments".into()],
            }],
            ..IntentContext::default()
        };
        let out = s
            .transcribe(req(wav(), AudioEncoding::Wav), &ctx)
            .await
            .unwrap();
        match out.intent {
            TranscriptIntent::Control {
                action,
                session_action,
                session_id,
                ..
            } => {
                assert_eq!(
                    action,
                    serde_json::to_value(ControlAction::PauseSession).unwrap()
                );
                assert_eq!(session_action, Some(vca_core::SessionAction::Pause));
                assert_eq!(session_id, Some(SessionId("s-7".into())));
            }
            other => panic!("expected a control intent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn uses_the_project_vocabulary_of_the_selected_session() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("package.json"),
            r#"{"name": "payments-api", "dependencies": {}}"#,
        )
        .unwrap();
        let mock = MockSttEngine::always("deploy payments api");
        let (s, _d) = speech(mock);
        let mut r = req(wav(), AudioEncoding::Wav);
        r.project_dir = Some(project.path().to_owned());
        let out = s.transcribe(r, &IntentContext::default()).await.unwrap();
        assert!(out.text.contains("payments-api"), "{}", out.text);
        assert!(out.corrections.iter().any(|c| c.layer == "project"));
    }

    #[tokio::test]
    async fn passes_language_and_hints_to_the_engine() {
        let mock = MockSttEngine::always("hello");
        mock.push(Transcript::plain("hello"));
        let mock = Arc::new(mock);
        let dir = tempfile::tempdir().unwrap();
        let cfg = SpeechConfig {
            language: Some("ko".into()),
            secondary_languages: vec!["en".into()],
            ..SpeechConfig::default()
        };
        let s = Speech::with_engine(mock.clone(), cfg, dir.path()).unwrap();
        s.transcribe(req(wav(), AudioEncoding::Wav), &IntentContext::default())
            .await
            .unwrap();
        let (_, opts) = mock.calls().pop().unwrap();
        assert_eq!(opts.primary_language.as_deref(), Some("ko"));
        assert_eq!(opts.secondary_languages, vec!["en".to_owned()]);
        assert!(opts.prompt_hint.is_some_and(|h| !h.is_empty()));
    }

    #[tokio::test]
    async fn rejects_bad_audio_before_calling_the_engine() {
        let mock = Arc::new(MockSttEngine::always("x"));
        let dir = tempfile::tempdir().unwrap();
        let cfg = SpeechConfig {
            max_audio_bytes: 10,
            ..SpeechConfig::default()
        };
        let s = Speech::with_engine(mock.clone(), cfg, dir.path()).unwrap();
        let ctx = IntentContext::default();
        let err = s
            .transcribe(req(vec![0; 11], AudioEncoding::Webm), &ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        let err = s
            .transcribe(req(vec![0; 4], AudioEncoding::Pcm16), &ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        let err = s
            .transcribe(req(Vec::new(), AudioEncoding::Wav), &ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        assert!(mock.calls().is_empty());
    }

    mod personal_terms {
        use super::*;

        fn term(term: &str, variants: &[&str]) -> PersonalTerm {
            PersonalTerm {
                term: term.into(),
                variants: variants.iter().map(|v| (*v).to_owned()).collect(),
            }
        }

        async fn dictate(s: &Speech) -> TranscriptResult {
            s.transcribe(req(wav(), AudioEncoding::Wav), &IntentContext::default())
                .await
                .unwrap()
        }

        #[tokio::test]
        async fn a_saved_list_applies_to_the_next_dictation_without_a_restart() {
            let (s, dir) = speech(MockSttEngine::always("deploy sky harbour now"));
            assert!(s.personal_terms().is_empty());
            assert_eq!(dictate(&s).await.text, "deploy sky harbour now");

            let saved = s
                .save_personal_terms(vec![term("  SkyHarbor", &["sky harbour"])])
                .unwrap();
            assert_eq!(saved, [term("SkyHarbor", &["sky harbour"])]);
            assert_eq!(s.personal_terms(), saved);
            let out = dictate(&s).await;
            assert_eq!(out.text, "deploy SkyHarbor now");
            assert!(
                out.corrections
                    .iter()
                    .any(|c| c.to == "SkyHarbor" && c.layer == "personal")
            );

            // Deleting takes effect the same way.
            s.save_personal_terms(Vec::new()).unwrap();
            assert_eq!(dictate(&s).await.text, "deploy sky harbour now");

            // And the file is what a restart reads.
            s.save_personal_terms(vec![term("SkyHarbor", &["sky harbour"])])
                .unwrap();
            let mock = MockSttEngine::always("deploy sky harbour now");
            let cfg = SpeechConfig {
                language: Some("en".into()),
                ..SpeechConfig::default()
            };
            let again = Speech::with_engine(Arc::new(mock), cfg, dir.path()).unwrap();
            assert_eq!(again.personal_terms(), s.personal_terms());
            assert_eq!(dictate(&again).await.text, "deploy SkyHarbor now");
        }

        #[tokio::test]
        async fn terms_reach_the_recognizer_hints() {
            let mock = Arc::new(MockSttEngine::always("hello"));
            let dir = tempfile::tempdir().unwrap();
            let cfg = SpeechConfig {
                language: Some("en".into()),
                ..SpeechConfig::default()
            };
            let s = Speech::with_engine(mock.clone(), cfg, dir.path()).unwrap();
            s.save_personal_terms(vec![term("Zentorix", &[])]).unwrap();
            dictate(&s).await;
            let (_, opts) = mock.calls().pop().unwrap();
            assert!(opts.prompt_hint.is_some_and(|h| h.contains("Zentorix")));
            assert!(opts.phrase_hints.iter().any(|p| p == "Zentorix"));
        }

        #[tokio::test]
        async fn a_cached_project_vocabulary_picks_up_the_list() {
            let project = tempfile::tempdir().unwrap();
            let (s, _d) = speech(MockSttEngine::always("deploy sky harbour now"));
            let ctx = IntentContext::default();
            let request = || TranscribeRequest {
                project_dir: Some(project.path().to_owned()),
                ..req(wav(), AudioEncoding::Wav)
            };
            let out = s.transcribe(request(), &ctx).await.unwrap();
            assert_eq!(out.text, "deploy sky harbour now");
            s.save_personal_terms(vec![term("SkyHarbor", &["sky harbour"])])
                .unwrap();
            let out = s.transcribe(request(), &ctx).await.unwrap();
            assert_eq!(out.text, "deploy SkyHarbor now");
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn saves_are_safe_under_concurrent_dictation() {
            let (s, _d) = speech(MockSttEngine::always("deploy sky harbour now"));
            let s = Arc::new(s);
            let mut workers = Vec::new();
            for _ in 0..8 {
                let s = Arc::clone(&s);
                workers.push(tokio::spawn(async move {
                    for _ in 0..20 {
                        let text = dictate(&s).await.text;
                        // Always one list or the other, never a broken text.
                        assert!(
                            text == "deploy sky harbour now"
                                || text == "deploy SkyHarbor now"
                                || text == "deploy Zentorix now",
                            "{text}"
                        );
                    }
                }));
            }
            let saver = Arc::clone(&s);
            let saves = tokio::task::spawn_blocking(move || {
                for i in 0..30 {
                    let word = if i % 2 == 0 { "SkyHarbor" } else { "Zentorix" };
                    saver
                        .save_personal_terms(vec![term(word, &["sky harbour"])])
                        .unwrap();
                }
            });
            for w in workers {
                w.await.unwrap();
            }
            saves.await.unwrap();
            // The last save (Zentorix) wins, for every later dictation.
            assert_eq!(dictate(&s).await.text, "deploy Zentorix now");
            let dir_file = s.personal_path.clone();
            let stored = PersonalLexicon::from_json(&std::fs::read_to_string(dir_file).unwrap());
            assert_eq!(stored.unwrap().lexicon().len(), 1);
        }

        #[test]
        fn a_refused_list_changes_nothing() {
            let (s, _d) = speech(MockSttEngine::always("x"));
            s.save_personal_terms(vec![term("SkyHarbor", &["sky harbour"])])
                .unwrap();
            let before = std::fs::read_to_string(&s.personal_path).unwrap();
            let err = s.save_personal_terms(vec![term("a\nb", &[])]).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRequest);
            assert_eq!(std::fs::read_to_string(&s.personal_path).unwrap(), before);
            assert_eq!(s.personal_terms().len(), 1);
        }

        #[test]
        fn saving_keeps_the_rest_of_the_file_and_hand_edits() {
            let (s, _d) = speech(MockSttEngine::always("x"));
            let mut file = PersonalLexicon::with_threshold(5);
            file.add_term(TermEntryForTest::tagged("Kept", "kept one"));
            std::fs::create_dir_all(s.personal_path.parent().unwrap()).unwrap();
            std::fs::write(&s.personal_path, file.to_json().unwrap()).unwrap();

            let saved = s
                .save_personal_terms(vec![term("Kept", &["kept one"]), term("New", &["new one"])])
                .unwrap();
            assert_eq!(saved.len(), 2);
            let stored =
                PersonalLexicon::from_json(&std::fs::read_to_string(&s.personal_path).unwrap())
                    .unwrap();
            assert_eq!(stored.threshold, 5);
            let kept = stored.lexicon().get("Kept").unwrap();
            assert_eq!(kept.category.as_deref(), Some("mine"));
            assert_eq!(kept.match_term, Some(true));
            assert_eq!(stored.lexicon().get("New").unwrap().match_term, None);
        }

        #[test]
        fn a_damaged_file_is_not_overwritten() {
            let (s, _d) = speech(MockSttEngine::always("x"));
            std::fs::create_dir_all(s.personal_path.parent().unwrap()).unwrap();
            std::fs::write(&s.personal_path, "{ not json").unwrap();
            let err = s.save_personal_terms(Vec::new()).unwrap_err();
            assert_eq!(err.code(), ErrorCode::Internal);
            assert_eq!(
                std::fs::read_to_string(&s.personal_path).unwrap(),
                "{ not json"
            );
        }

        struct TermEntryForTest;

        impl TermEntryForTest {
            fn tagged(term: &str, variant: &str) -> vca_speech::lexicon::TermEntry {
                vca_speech::lexicon::TermEntry::new(term)
                    .variant(variant, None)
                    .category("mine")
            }
        }
    }

    #[test]
    fn off_builds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Speech::from_config(&SpeechConfig::default(), dir.path())
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn google_without_credentials_is_listed_as_unconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = SpeechConfig {
            engine: crate::config::SpeechEngineKind::Google,
            ..SpeechConfig::default()
        };
        cfg.google.project_id = "p".into();
        cfg.google.api_key_env = Some("VCA_TEST_UNSET_SPEECH_KEY_81723".into());
        cfg.google.access_token_env = None;
        // Starting no longer fails: the key can be added later.
        let s = Speech::from_config(&cfg, dir.path()).unwrap().unwrap();
        let ServerMsg::SpeechOptions { engines, .. } = s.options(LOCAL_DEVICE) else {
            panic!()
        };
        assert_eq!(engines[0].id, "google");
        assert!(!engines[0].configured);
        let err = s
            .transcribe(req(wav(), AudioEncoding::Wav), &IntentContext::default())
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unauthorized);
        assert!(err.to_string().contains("vca speech login google"), "{err}");
        // `vca speech login google` makes it usable.
        s.set_credential("google", "AIza-test").unwrap();
        let ServerMsg::SpeechOptions { engines, .. } = s.options(LOCAL_DEVICE) else {
            panic!()
        };
        assert!(engines[0].configured);
        assert!(s.delete_credential("google").unwrap());
        assert!(s.set_credential("missing", "x").is_err());
    }

    #[test]
    fn invalid_profiles_fail_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SpeechConfig {
            default_engine: Some("nope".into()),
            engines: vec![SpeechEngineProfile {
                id: "a".into(),
                base_url: Some("http://127.0.0.1:1/v1".into()),
                ..SpeechEngineProfile::default()
            }],
            ..SpeechConfig::default()
        };
        assert!(Speech::from_config(&cfg, dir.path()).is_err());
    }

    mod profiles {
        use crate::config::{EngineProfileKind, LlmProfile};

        use super::*;

        fn profile(id: &str) -> SpeechEngineProfile {
            SpeechEngineProfile {
                id: id.into(),
                label: Some(id.to_uppercase()),
                base_url: Some("http://127.0.0.1:1/v1".into()),
                ..SpeechEngineProfile::default()
            }
        }

        fn two_engines(dir: &Path) -> (Speech, Arc<MockSttEngine>, Arc<MockSttEngine>) {
            let a = Arc::new(MockSttEngine::always("from engine a"));
            let b = Arc::new(MockSttEngine::always("from engine b"));
            let cfg = SpeechConfig {
                language: Some("en".into()),
                default_engine: Some("b".into()),
                default_llm: Some("none".into()),
                llms: vec![
                    LlmProfile {
                        id: "local-llm".into(),
                        base_url: Some("http://127.0.0.1:1/v1".into()),
                        ..LlmProfile::default()
                    },
                    LlmProfile {
                        id: "claude".into(),
                        kind: LlmProfileKind::Anthropic,
                        api_key_env: Some("VCA_TEST_UNSET_ANTHROPIC_KEY_5521".into()),
                        ..LlmProfile::default()
                    },
                ],
                ..SpeechConfig::default()
            };
            let s = Speech::with_engines(
                vec![
                    (profile("a"), a.clone() as Arc<dyn SttEngine>),
                    (profile("b"), b.clone() as Arc<dyn SttEngine>),
                ],
                cfg,
                dir,
            )
            .unwrap();
            (s, a, b)
        }

        fn device_req(device: Option<&str>) -> TranscribeRequest {
            TranscribeRequest {
                device: device.map(str::to_owned),
                ..req(wav(), AudioEncoding::Wav)
            }
        }

        #[tokio::test]
        async fn devices_pick_engines_and_llms_and_the_choice_persists() {
            let dir = tempfile::tempdir().unwrap();
            let (s, a, b) = two_engines(dir.path());
            let ctx = IntentContext::default();

            // Default engine for everyone.
            let out = s.transcribe(device_req(Some("dev_1")), &ctx).await.unwrap();
            assert_eq!(out.raw_text, "from engine b");
            let ServerMsg::SpeechOptions {
                engines,
                llms,
                current_engine,
                current_llm,
            } = s.options("dev_1")
            else {
                panic!()
            };
            assert_eq!(engines.len(), 2);
            assert!(
                engines
                    .iter()
                    .any(|e| e.id == "b" && e.default && e.label == "B")
            );
            assert_eq!(
                engines[0].kind,
                EngineProfileKind::OpenaiCompatible.as_str()
            );
            assert_eq!(llms.len(), 2);
            assert!(llms.iter().any(|l| l.id == "local-llm" && l.configured));
            assert!(llms.iter().any(|l| l.id == "claude" && !l.configured));
            assert_eq!(current_engine.as_deref(), Some("b"));
            assert_eq!(current_llm, None);

            // dev_1 picks engine a and the local LLM; others are unaffected.
            s.set_preferences("dev_1", Some("a".into()), Some("local-llm".into()))
                .unwrap();
            let out = s.transcribe(device_req(Some("dev_1")), &ctx).await.unwrap();
            assert_eq!(out.raw_text, "from engine a");
            let out = s.transcribe(device_req(Some("dev_2")), &ctx).await.unwrap();
            assert_eq!(out.raw_text, "from engine b");
            let out = s.transcribe(device_req(None), &ctx).await.unwrap();
            assert_eq!(out.raw_text, "from engine b");
            assert_eq!(a.calls().len(), 1);
            assert_eq!(b.calls().len(), 3);

            // Unknown and unconfigured profiles are refused.
            assert_eq!(
                s.set_preferences("dev_1", Some("zzz".into()), None)
                    .unwrap_err()
                    .code(),
                ErrorCode::NotFound
            );
            let err = s
                .set_preferences("dev_1", None, Some("claude".into()))
                .unwrap_err();
            assert!(err.to_string().contains("vca speech login claude"), "{err}");

            // The choice survives a restart.
            drop(s);
            let (s, _, _) = two_engines(dir.path());
            let ServerMsg::SpeechOptions {
                current_engine,
                current_llm,
                ..
            } = s.options("dev_1")
            else {
                panic!()
            };
            assert_eq!(current_engine.as_deref(), Some("a"));
            assert_eq!(current_llm.as_deref(), Some("local-llm"));

            // "none" turns the correction off; None returns to the defaults.
            s.set_preferences("dev_1", Some("a".into()), Some("none".into()))
                .unwrap();
            let ServerMsg::SpeechOptions { current_llm, .. } = s.options("dev_1") else {
                panic!()
            };
            assert_eq!(current_llm, None);
            s.set_preferences("dev_1", None, None).unwrap();
            let ServerMsg::SpeechOptions { current_engine, .. } = s.options("dev_1") else {
                panic!()
            };
            assert_eq!(current_engine.as_deref(), Some("b"));

            // A stored key configures the Anthropic profile.
            s.set_credential("claude", "sk-ant-test").unwrap();
            s.set_preferences("dev_1", None, Some("claude".into()))
                .unwrap();
        }

        struct FixedTokens;

        #[async_trait::async_trait]
        impl vca_speech::llm::TokenProvider for FixedTokens {
            async fn token(&self, _refresh: bool) -> vca_speech::Result<String> {
                Ok("account-token".into())
            }
        }

        #[tokio::test]
        async fn anthropic_account_profile_corrects_final_chunks() {
            use wiremock::matchers::{header, method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/messages"))
                .and(header("authorization", "Bearer account-token"))
                .and(header("anthropic-beta", "oauth-2025-04-20"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "content": [{"type": "text",
                        "text": "{\"fixes\":[{\"from\":\"sub mitt handler\",\"to\":\"submit handler\"}]}"}],
                    "stop_reason": "end_turn"
                })))
                .mount(&server)
                .await;
            let dir = tempfile::tempdir().unwrap();
            let cfg = SpeechConfig {
                language: Some("en".into()),
                default_llm: Some("claude".into()),
                llms: vec![LlmProfile {
                    id: "claude".into(),
                    kind: LlmProfileKind::Anthropic,
                    base_url: Some(server.uri()),
                    ant_profile: Some("default".into()),
                    ..LlmProfile::default()
                }],
                ..SpeechConfig::default()
            };
            let mock = Arc::new(MockSttEngine::always(
                "the payment form fails in the sub mitt handler when the user clicks twice",
            ));
            let s = Speech::with_engine(mock, cfg, dir.path()).unwrap();
            s.set_token_provider(Arc::new(FixedTokens));
            let out = s
                .transcribe(
                    TranscribeRequest {
                        is_final: true,
                        ..req(wav(), AudioEncoding::Wav)
                    },
                    &IntentContext::default(),
                )
                .await
                .unwrap();
            assert!(out.text.contains("submit handler"), "{}", out.text);
            assert!(out.corrections.iter().any(|c| c.layer == "context"));
        }
    }

    #[test]
    fn intent_context_lists_names_and_folders() {
        let info = vca_core::SessionInfo {
            id: SessionId("a".into()),
            origin: vca_core::SessionOrigin::Local {
                agent: vca_core::AgentKind::ClaudeCode,
                project_dir: PathBuf::from("/code/payments"),
                pid: None,
            },
            display_name: "claude (payments)".into(),
            state: vca_core::SessionState::AwaitingDecision,
            control: vca_core::ControlTier::ManagedPty,
            profile: None,
            created_at: 0,
            last_activity: 0,
            unread: 0,
            preview: None,
            auto_name: None,
            custom_name: false,
            shell: None,
            pending_decision: None,
            subagents: 0,
            conductor: false,
            terminal_prompt: None,
            question: None,
            progress: None,
            problem: None,
        };
        let ctx = intent_context(std::slice::from_ref(&info), Some(&info.id));
        assert_eq!(ctx.sessions[0].names, vec!["claude (payments)", "payments"]);
        assert!(ctx.pending_decision);
        assert!(!intent_context(&[info], None).pending_decision);
    }

    #[test]
    fn topic_terms_come_before_vocabulary_hints() {
        let terms = vec!["Helm".to_owned(), "kubectl".to_owned()];
        let mut room = 0;
        let hints = hints_with_topic(&terms, |r| {
            room = r;
            "pnpm, useEffect".to_owned()
        });
        assert_eq!(hints, "Helm, kubectl, pnpm, useEffect");
        assert_eq!(room, HINT_CHARS - "Helm, kubectl, ".chars().count());
        assert_eq!(hints_with_topic(&[], |_| String::new()), "");
        assert_eq!(hints_with_topic(&terms, |_| String::new()), "Helm, kubectl");
        let merged = merge_terms(&terms, vec!["KUBECTL".into(), "pnpm".into()], 3);
        assert_eq!(merged, vec!["Helm", "kubectl", "pnpm"]);
        assert_eq!(tail("가나다라", 2), "다라");
    }

    #[test]
    fn voice_words_come_before_vocabulary_hints_without_duplicates() {
        let voice = ["보코", "오버", "Roger"];
        let mut room = 0;
        let hints = voice_then_vocab(&voice, 100, |r| {
            room = r;
            "pnpm, roger, 보코, useEffect".to_owned()
        });
        assert_eq!(hints, "보코, 오버, Roger, pnpm, useEffect");
        assert_eq!(room, 100 - "보코, 오버, Roger, ".chars().count());
        // Within the room; the voice words never exceed it.
        assert_eq!(voice_then_vocab(&voice, 6, |_| String::new()), "보코, 오버");
        assert_eq!(voice_then_vocab(&[], 10, |_| "pnpm".to_owned()), "pnpm");
        let all = voice_then_vocab(&vca_speech::radio::prompt_words(None), HINT_CHARS, |r| {
            "x".repeat(r)
        });
        assert!(all.chars().count() <= HINT_CHARS, "{}", all.chars().count());
    }

    mod llm {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use super::*;

        const HEARD: &str =
            "the payment form fails in the sub mitt handler when the user clicks twice quickly";

        fn chat(content: &str) -> ResponseTemplate {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": content}}]
            }))
        }

        fn with_llm(
            mock: Arc<MockSttEngine>,
            server: &MockServer,
            timeout_ms: u64,
        ) -> (Speech, tempfile::TempDir) {
            let dir = tempfile::tempdir().unwrap();
            let mut cfg = SpeechConfig {
                language: Some("en".into()),
                ..SpeechConfig::default()
            };
            cfg.llm.enabled = true;
            cfg.llm.base_url = format!("{}/v1", server.uri());
            cfg.llm.api_key_env = None;
            cfg.llm.timeout_ms = timeout_ms;
            cfg.llm.topic_every_n_finals = 2;
            let s = Speech::with_engine(mock, cfg, dir.path()).unwrap();
            (s, dir)
        }

        fn chunk(is_final: bool, key: &str) -> TranscribeRequest {
            TranscribeRequest {
                is_final,
                topic_key: Some(key.into()),
                context: Some("결제 폼 버그를 보고 있어요.".into()),
                ..req(wav(), AudioEncoding::Wav)
            }
        }

        async fn mount_correction(server: &MockServer, response: ResponseTemplate) {
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .and(body_string_contains(
                    "You correct speech-recognition errors",
                ))
                .respond_with(response)
                .mount(server)
                .await;
        }

        async fn mount_topic(server: &MockServer, response: ResponseTemplate) {
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .and(body_string_contains("You identify the subject"))
                .respond_with(response)
                .mount(server)
                .await;
        }

        #[tokio::test]
        async fn interim_requests_never_call_the_llm() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(chat(r#"{"fixes":[]}"#))
                .expect(0)
                .mount(&server)
                .await;
            let (s, _d) = with_llm(Arc::new(MockSttEngine::always(HEARD)), &server, 4000);
            for _ in 0..3 {
                let out = s
                    .transcribe(chunk(false, "session:a"), &IntentContext::default())
                    .await
                    .unwrap();
                assert!(out.corrections.iter().all(|c| c.layer != "context"));
                assert_eq!(out.topic, None);
            }
            assert_eq!(s.topic("session:a"), None);
            server.verify().await;
        }

        #[tokio::test]
        async fn final_chunks_get_context_corrections() {
            let server = MockServer::start().await;
            mount_correction(
                &server,
                chat(r#"{"fixes":[{"from":"sub mitt handler","to":"submit handler"}]}"#),
            )
            .await;
            mount_topic(&server, chat(r#"{"topic":"frontend forms","terms":[]}"#)).await;
            let (s, _d) = with_llm(Arc::new(MockSttEngine::always(HEARD)), &server, 4000);
            let ctx = IntentContext::default();
            let interim = s.transcribe(chunk(false, "session:a"), &ctx).await.unwrap();
            let out = s.transcribe(chunk(true, "session:a"), &ctx).await.unwrap();
            assert_eq!(out.raw_text, HEARD);
            assert_eq!(
                out.text,
                interim.text.replace("sub mitt handler", "submit handler")
            );
            assert!(out.corrections.iter().any(|c| c.from == "sub mitt handler"
                && c.to == "submit handler"
                && c.layer == "context"));
            s.settle_topics().await;
        }

        async fn assert_fails_open(server: &MockServer, timeout_ms: u64) {
            let (s, _d) = with_llm(Arc::new(MockSttEngine::always(HEARD)), server, timeout_ms);
            let ctx = IntentContext::default();
            let interim = s.transcribe(chunk(false, "k"), &ctx).await.unwrap();
            let started = Instant::now();
            let out = s.transcribe(chunk(true, "k"), &ctx).await.unwrap();
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(out.text, interim.text);
            assert_eq!(out.corrections, interim.corrections);
            s.settle_topics().await;
        }

        #[tokio::test]
        async fn llm_failures_fail_open() {
            // Server error.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;
            assert_fails_open(&server, 4000).await;
            // Bad JSON, an invalid `from`, and a too-large edit.
            for content in [
                "not json",
                r#"{"fixes":[{"from":"submit handler","to":"x"}]}"#,
                r#"{"fixes":[{"from":"the payment form fails in the sub mitt handler","to":"checkout breaks"}]}"#,
            ] {
                let server = MockServer::start().await;
                Mock::given(method("POST"))
                    .respond_with(chat(content))
                    .mount(&server)
                    .await;
                assert_fails_open(&server, 4000).await;
            }
            // Timeout.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(chat(r#"{"fixes":[]}"#).set_delay(Duration::from_secs(10)))
                .mount(&server)
                .await;
            assert_fails_open(&server, 200).await;
        }

        #[tokio::test]
        async fn topic_terms_reach_the_stt_prompt_and_the_reply() {
            let server = MockServer::start().await;
            mount_correction(&server, chat(r#"{"fixes":[]}"#)).await;
            mount_topic(
                &server,
                chat(r#"{"topic":"Kubernetes deploy","terms":["Helm","ArgoCD"]}"#),
            )
            .await;
            let mock = Arc::new(MockSttEngine::always(HEARD));
            let (s, _d) = with_llm(Arc::clone(&mock), &server, 4000);
            let ctx = IntentContext::default();
            let first = s.transcribe(chunk(true, "session:k"), &ctx).await.unwrap();
            assert_eq!(first.topic, None);
            // Topic detection runs in the background after the first final.
            let deadline = Instant::now() + Duration::from_secs(10);
            while s.topic("session:k").is_none() {
                assert!(Instant::now() < deadline, "no topic detected");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let next = s.transcribe(chunk(false, "session:k"), &ctx).await.unwrap();
            assert_eq!(next.topic.as_deref(), Some("Kubernetes deploy"));
            let (_, opts) = mock.calls().pop().unwrap();
            let prompt = opts.prompt_hint.unwrap();
            assert!(prompt.starts_with("Helm, ArgoCD, "), "{prompt}");
            assert!(prompt.ends_with("결제 폼 버그를 보고 있어요."), "{prompt}");
            assert_eq!(&opts.phrase_hints[..2], ["Helm", "ArgoCD"]);
            // Other dictations keep their own (empty) topic.
            let other = s.transcribe(chunk(false, "session:z"), &ctx).await.unwrap();
            assert_eq!(other.topic, None);
            // The topic and its terms reach the correction prompt too.
            s.transcribe(chunk(true, "session:k"), &ctx).await.unwrap();
            let requests = server.received_requests().await.unwrap();
            let last = requests
                .iter()
                .rev()
                .find(|r| String::from_utf8_lossy(&r.body).contains("You correct"))
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&last.body).unwrap();
            let user: serde_json::Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(user["topic"], "Kubernetes deploy");
            assert_eq!(user["domain_terms"][0], "Helm");
            s.settle_topics().await;
        }

        #[tokio::test]
        async fn without_llm_nothing_changes() {
            let (s, _d) = speech(MockSttEngine::always(HEARD));
            let out = s
                .transcribe(chunk(true, "session:a"), &IntentContext::default())
                .await
                .unwrap();
            assert_eq!(out.topic, None);
            assert!(out.corrections.iter().all(|c| c.layer != "context"));
            assert_eq!(s.topic("session:a"), None);
        }
    }

    /// A `send()` error from a port nothing listens on.
    async fn refused() -> reqwest::Error {
        let addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        reqwest::Client::new()
            .get(format!("http://{addr}/?key=secret"))
            .send()
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn unreachable_speech_server_gets_a_stable_marker() {
        let err = speech_error(SpeechError::Http(refused().await));
        assert_eq!(err.code(), ErrorCode::Unavailable);
        let msg = err.to_string();
        assert!(msg.contains("speech_unreachable: "), "{msg}");
        assert!(!msg.contains("secret"), "the URL must be dropped: {msg}");
    }

    #[test]
    fn answered_errors_keep_their_message() {
        let err = speech_error(SpeechError::Api {
            status: 500,
            message: "boom".into(),
        });
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(!err.to_string().contains("speech_unreachable"));
    }
}
