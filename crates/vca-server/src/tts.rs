//! Spoken alerts synthesized on this PC (`[tts]`): a client sends a short
//! alert title, the configured engine turns it into audio, and the audio
//! goes back to that client, which plays it.
//!
//! VCA ships no voices or model weights; every engine is the owner's own
//! server. Keys come from the environment variable a profile names, else
//! from the owner-only credential store `<data>/tts/keys` (`vca tts login`),
//! never from `config.toml`, and are never sent to clients. Engine URLs and
//! models are never sent to clients either. An engine behind an
//! authenticating gateway also has a gateway token (`gateway_token_env`, else
//! `<data>/tts/gateway`, `vca tts gateway-login`); keys and tokens are read on
//! every request, so replacing one needs no restart.
//!
//! Requests are capped at [`TTS_MAX_TEXT_CHARS`] characters, limited per
//! device and per server, and neither the text nor the audio is logged.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use vca_core::error::ErrorCode;
use vca_core::{Error, TTS_MAX_TEXT_CHARS, TtsEngineInfo};
use vca_speech::tts::{OpenAiTtsConfig, OpenAiTtsEngine};
use vca_speech::{SynthesisRequest, Synthesized, TtsEngine, TtsFormat};

use crate::config::{TtsConfig, TtsEngineKind, TtsEngineProfile};
use crate::speech_profiles::{SecretPair, Secrets, engine_auth, env_secret, lookup_gateway};
use crate::util::lock;

const DEFAULT_MODEL: &str = "tts-1";
const DEFAULT_VOICE: &str = "alloy";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Rate-limit window.
const WINDOW: Duration = Duration::from_secs(60);
/// Devices tracked by the rate limiter at once.
const MAX_LIMITER_KEYS: usize = 10_000;
/// Error text of a device over its limit.
pub const RATE_LIMITED: &str = "too many spoken alerts; try again in a minute";
/// Longest wait for a free synthesis slot.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(20);

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

fn base_language(tag: &str) -> &str {
    tag.split(['-', '_']).next().unwrap_or(tag)
}

/// Text for a Korean voice. The clean-up runs first: the Latin readings
/// would turn a hash or an extension into half-Hangul ("e25f95a9" ->
/// "이25에프95에이9", "questions.rs" -> "questions.알에스") that the clean-up
/// no longer recognizes. Numbers make the text longer; when that alone
/// pushes it over [`TTS_MAX_TEXT_CHARS`], the readings apply without it.
fn korean_text(text: &str) -> String {
    use vca_speech::ko_text::korean_speakable;
    use vca_speech::pronounce::korean_latin_readings;
    let cleaned = korean_latin_readings(&korean_speakable(text)).into_owned();
    if cleaned.chars().count() <= TTS_MAX_TEXT_CHARS {
        return cleaned;
    }
    let plain = korean_latin_readings(text);
    if plain.chars().count() <= TTS_MAX_TEXT_CHARS {
        plain.into_owned()
    } else {
        cleaned
    }
}

/// Sliding-window request counter per device.
struct Limiter {
    per_window: usize,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Limiter {
    fn new(per_minute: u32) -> Self {
        Self {
            per_window: per_minute.max(1) as usize,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one request; `false` when the device is over its limit.
    fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = lock(&self.hits);
        if hits.len() >= MAX_LIMITER_KEYS && !hits.contains_key(key) {
            hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < WINDOW));
            if hits.len() >= MAX_LIMITER_KEYS {
                return false;
            }
        }
        let q = hits.entry(key.to_owned()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            q.pop_front();
        }
        if q.len() >= self.per_window {
            return false;
        }
        q.push_back(now);
        true
    }
}

type EngineCache = Option<(SecretPair, Arc<dyn TtsEngine>)>;

/// One engine profile and its client.
struct Slot {
    profile: TtsEngineProfile,
    format: TtsFormat,
    /// An injected engine (tests); used instead of building one.
    fixed: Option<Arc<dyn TtsEngine>>,
    cache: Mutex<EngineCache>,
}

impl Slot {
    fn credential_name(&self) -> &str {
        self.profile
            .credential
            .as_deref()
            .unwrap_or(&self.profile.name)
    }

    /// The voice for `language`: exact tag, then base language, then the
    /// default voice.
    fn voice_for(&self, language: Option<&str>) -> Option<String> {
        let voices = &self.profile.voices;
        let found = language.and_then(|lang| {
            let lang = lang.trim();
            voices
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(lang))
                .or_else(|| {
                    let base = base_language(lang);
                    voices.iter().find(|(k, _)| k.eq_ignore_ascii_case(base))
                })
                .map(|(_, v)| v.clone())
        });
        found.or_else(|| nonempty(self.profile.voice.as_deref()).map(str::to_owned))
    }

    /// The voice to use: a requested speaker id (only one of this engine's
    /// speakers), else the language / default voice.
    fn resolve_voice(
        &self,
        requested: Option<&str>,
        language: Option<&str>,
    ) -> Result<Option<String>, Error> {
        match nonempty(requested) {
            Some(id) => self
                .profile
                .speakers
                .iter()
                .find(|s| s.id == id)
                .map(|s| Some(s.id.clone()))
                .ok_or_else(|| {
                    api_error(
                        ErrorCode::InvalidRequest,
                        "unknown voice for this TTS engine",
                    )
                }),
            None => Ok(self.voice_for(language)),
        }
    }

    fn info(&self, default: bool) -> TtsEngineInfo {
        TtsEngineInfo {
            name: self.profile.name.clone(),
            languages: self.profile.voices.keys().cloned().collect(),
            default,
            speakers: self.profile.speakers.clone(),
            disclosure: nonempty(self.profile.disclosure.as_deref()).map(str::to_owned),
        }
    }

    fn engine(
        &self,
        pair: SecretPair,
        max_audio_bytes: usize,
    ) -> Result<Arc<dyn TtsEngine>, Error> {
        if let Some(e) = &self.fixed {
            return Ok(Arc::clone(e));
        }
        if let Some((k, engine)) = lock(&self.cache).as_ref()
            && *k == pair
        {
            return Ok(Arc::clone(engine));
        }
        let p = &self.profile;
        // Also for a profile saved before the rule.
        if pair.0.is_some() || pair.1.is_some() {
            if let Some(url) = nonempty(p.base_url.as_deref()) {
                vca_speech::auth::check_key_url(url, p.allow_plain_http)
                    .map_err(|e| api_error(ErrorCode::InvalidRequest, e.to_string()))?;
            }
        }
        let engine: Arc<dyn TtsEngine> = match p.kind {
            TtsEngineKind::OpenaiCompatible => Arc::new(
                OpenAiTtsEngine::new(OpenAiTtsConfig {
                    base_url: nonempty(p.base_url.as_deref())
                        .unwrap_or_default()
                        .to_owned(),
                    api_key: pair.0.clone(),
                    model: nonempty(p.model.as_deref())
                        .unwrap_or(DEFAULT_MODEL)
                        .to_owned(),
                    voice: nonempty(p.voice.as_deref())
                        .unwrap_or(DEFAULT_VOICE)
                        .to_owned(),
                    format: self.format,
                    speed: Some(1.0),
                    timeout: Duration::from_secs(
                        p.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).max(1),
                    ),
                    max_audio_bytes,
                    auth: engine_auth(p.api_key_header.as_deref(), pair.1.clone()),
                })
                .map_err(|e| match e {
                    vca_speech::SpeechError::Config(message) => api_error(
                        ErrorCode::InvalidRequest,
                        format!("TTS engine {:?}: {message}", p.name),
                    ),
                    other => api_error(ErrorCode::Internal, other.to_string()),
                })?,
            ),
        };
        *lock(&self.cache) = Some((pair, Arc::clone(&engine)));
        Ok(engine)
    }
}

/// The TTS engines of this server. Shared by all clients.
pub struct Tts {
    slots: Vec<Slot>,
    default_engine: Option<String>,
    secrets: Secrets,
    limiter: Limiter,
    permits: Semaphore,
    max_audio_bytes: usize,
    /// The pronunciation dictionary applied before synthesis.
    pronunciations: OnceLock<Arc<crate::pronunciations::Pronunciations>>,
}

impl Tts {
    /// The configured engines; `None` when there are none (TTS off).
    pub fn from_config(cfg: &TtsConfig, data_dir: &Path) -> anyhow::Result<Option<Self>> {
        Self::build(cfg, data_dir, HashMap::new()).map(|t| (!t.slots.is_empty()).then_some(t))
    }

    /// The configured engines with some replaced by injected ones (tests).
    pub fn with_engines(
        cfg: &TtsConfig,
        data_dir: &Path,
        engines: HashMap<String, Arc<dyn TtsEngine>>,
    ) -> anyhow::Result<Self> {
        Self::build(cfg, data_dir, engines)
    }

    fn build(
        cfg: &TtsConfig,
        data_dir: &Path,
        mut fixed: HashMap<String, Arc<dyn TtsEngine>>,
    ) -> anyhow::Result<Self> {
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("invalid [tts] configuration: {e}"))?;
        let slots = cfg
            .engines
            .iter()
            .map(|p| Slot {
                format: p
                    .format
                    .as_deref()
                    .and_then(TtsFormat::parse)
                    .unwrap_or(TtsFormat::Mp3),
                fixed: fixed.remove(&p.name),
                profile: p.clone(),
                cache: Mutex::new(None),
            })
            .collect();
        Ok(Self {
            slots,
            default_engine: cfg.default_engine_name(),
            secrets: Secrets::tts_in_data_dir(data_dir),
            limiter: Limiter::new(cfg.requests_per_minute),
            permits: Semaphore::new(cfg.max_concurrent.max(1)),
            max_audio_bytes: cfg.max_audio_bytes.max(1),
            pronunciations: OnceLock::new(),
        })
    }

    /// Applies `dictionary` to every text before synthesis (names in Latin
    /// letters said the way the user taught).
    pub fn set_pronunciations(&self, dictionary: Arc<crate::pronunciations::Pronunciations>) {
        let _ = self.pronunciations.set(dictionary);
    }

    /// Engine names, languages and speakers (never URLs, models or keys).
    pub fn engines(&self) -> Vec<TtsEngineInfo> {
        self.slots
            .iter()
            .map(|s| s.info(self.default_engine.as_deref() == Some(s.profile.name.as_str())))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn slot(&self, name: &str) -> Result<&Slot, Error> {
        self.slots
            .iter()
            .find(|s| s.profile.name == name)
            .ok_or_else(|| api_error(ErrorCode::NotFound, format!("no TTS engine named {name:?}")))
    }

    /// The API key and the gateway token of `slot`, read on every request.
    fn secrets_of(&self, slot: &Slot) -> SecretPair {
        let key = env_secret(slot.profile.api_key_env.as_deref())
            .or_else(|| self.secrets.get(slot.credential_name()));
        let gateway = lookup_gateway(
            slot.profile.gateway_token_env.as_deref(),
            slot.credential_name(),
            &self.secrets,
        );
        (key, gateway)
    }

    /// Stores an engine's API key (local channel only).
    pub fn set_credential(&self, engine: &str, secret: &str) -> Result<(), Error> {
        let name = self.slot(engine)?.credential_name().to_owned();
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(api_error(ErrorCode::InvalidRequest, "empty credential"));
        }
        self.secrets.put(&name, secret)?;
        tracing::info!(engine, "TTS credential stored");
        Ok(())
    }

    /// Deletes an engine's stored API key. Returns whether one was stored.
    pub fn delete_credential(&self, engine: &str) -> Result<bool, Error> {
        let name = self.slot(engine)?.credential_name().to_owned();
        let existed = self.secrets.delete(&name)?;
        if existed {
            tracing::info!(engine, "TTS credential deleted");
        }
        Ok(existed)
    }

    /// Synthesizes `text` for `device` with `engine` (else the default) and
    /// `voice`, a speaker id of that engine (else its configured voice).
    pub async fn synthesize(
        &self,
        device: &str,
        text: &str,
        language: Option<&str>,
        engine: Option<&str>,
        voice: Option<&str>,
    ) -> Result<Synthesized, Error> {
        let text: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        // The pronunciation dictionary ("SkyHarbor" -> "스카이하버"), then
        // the radio words ("Voco" and "Roger" -> "보코" and "로저").
        let text = match self.pronunciations.get() {
            Some(p) => p.speakable(text.trim(), language),
            None => vca_speech::radio::speakable(text.trim(), language),
        };
        // Korean for a multilingual engine: hashes, file names, paths,
        // digits, markdown and arrows rewritten (they come out as babble in
        // another language), then known Latin words in Hangul, so fewer
        // letters can make it read the text in another language.
        let text = match language.map(|l| base_language(l.trim())) {
            Some(l) if l.eq_ignore_ascii_case("ko") => korean_text(&text),
            _ => text.into_owned(),
        };
        let text = text.as_str();
        if text.is_empty() {
            return Err(api_error(ErrorCode::InvalidRequest, "no text to speak"));
        }
        let chars = text.chars().count();
        if chars > TTS_MAX_TEXT_CHARS {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                format!("the text is {chars} characters; the limit is {TTS_MAX_TEXT_CHARS}"),
            ));
        }
        let name = match nonempty(engine) {
            Some(n) => n.to_owned(),
            None => self
                .default_engine
                .clone()
                .ok_or_else(|| api_error(ErrorCode::Unsupported, "no TTS engine is configured"))?,
        };
        let slot = self.slot(&name)?;
        let voice = slot.resolve_voice(voice, language)?;
        if !self.limiter.allow(device) {
            return Err(api_error(ErrorCode::Unavailable, RATE_LIMITED));
        }
        let engine = slot.engine(self.secrets_of(slot), self.max_audio_bytes)?;
        let request = SynthesisRequest {
            text: text.to_owned(),
            voice,
            speed: None,
            language: nonempty(language).map(str::to_owned),
        };
        let _permit = tokio::time::timeout(QUEUE_TIMEOUT, self.permits.acquire())
            .await
            .map_err(|_| api_error(ErrorCode::Unavailable, "the TTS engine is busy"))?
            .map_err(|_| api_error(ErrorCode::Unavailable, "TTS is shutting down"))?;
        match engine.synthesize(&request).await {
            Ok(out) => Ok(out),
            Err(err) => {
                // Only the engine name and the kind of failure: an engine's
                // error text may quote the input.
                let code = err.code();
                tracing::debug!(engine = %name, ?code, "speech synthesis failed");
                Err(api_error(
                    code,
                    format!("TTS engine {name:?} failed: {err}"),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use vca_core::{TtsGender, TtsSpeaker};

    struct Fake;

    #[async_trait]
    impl TtsEngine for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        async fn synthesize(&self, req: &SynthesisRequest) -> vca_speech::Result<Synthesized> {
            Ok(Synthesized {
                format: TtsFormat::Wav,
                audio: req.voice.clone().unwrap_or_default().into_bytes(),
            })
        }
    }

    fn cfg(per_minute: u32) -> TtsConfig {
        let mut voices = std::collections::BTreeMap::new();
        voices.insert("ko".to_owned(), "ko_voice".to_owned());
        voices.insert("en-GB".to_owned(), "gb_voice".to_owned());
        TtsConfig {
            requests_per_minute: per_minute,
            engines: vec![TtsEngineProfile {
                name: "local".into(),
                base_url: Some("http://127.0.0.1:9/v1".into()),
                voice: Some("plain".into()),
                voices,
                speakers: vec![
                    TtsSpeaker {
                        id: "F1".into(),
                        label: "Female 1".into(),
                        gender: TtsGender::Female,
                    },
                    TtsSpeaker {
                        id: "M1".into(),
                        label: "Male 1".into(),
                        gender: TtsGender::Male,
                    },
                ],
                disclosure: Some(" Machine-generated voice ".into()),
                ..TtsEngineProfile::default()
            }],
            ..TtsConfig::default()
        }
    }

    fn tts(per_minute: u32, dir: &Path) -> Tts {
        let mut fixed: HashMap<String, Arc<dyn TtsEngine>> = HashMap::new();
        fixed.insert("local".into(), Arc::new(Fake));
        Tts::with_engines(&cfg(per_minute), dir, fixed).unwrap()
    }

    #[tokio::test]
    async fn voices_follow_the_language() {
        let dir = tempfile::tempdir().unwrap();
        let t = tts(100, dir.path());
        let voice = |lang: Option<&'static str>| {
            let t = &t;
            async move {
                t.synthesize("d", "hi", lang, None, None)
                    .await
                    .unwrap()
                    .audio
            }
        };
        assert_eq!(voice(Some("ko-KR")).await, b"ko_voice");
        assert_eq!(voice(Some("en-gb")).await, b"gb_voice");
        assert_eq!(voice(Some("en-US")).await, b"plain");
        assert_eq!(voice(None).await, b"plain");
    }

    #[tokio::test]
    async fn a_requested_voice_must_be_one_of_the_speakers() {
        let dir = tempfile::tempdir().unwrap();
        let t = tts(100, dir.path());
        let with = |voice: Option<&'static str>| {
            let t = &t;
            async move {
                t.synthesize("d", "hi", Some("ko"), None, voice)
                    .await
                    .map(|o| o.audio)
            }
        };
        assert_eq!(with(Some("M1")).await.unwrap(), b"M1");
        assert_eq!(with(Some(" F1 ")).await.unwrap(), b"F1");
        // No voice, or a blank one: the language voice.
        assert_eq!(with(None).await.unwrap(), b"ko_voice");
        assert_eq!(with(Some("  ")).await.unwrap(), b"ko_voice");
        for bad in ["F2", "f1", "ko_voice", "plain"] {
            let err = with(Some(bad)).await.unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRequest, "{bad}");
        }
    }

    #[tokio::test]
    async fn an_engine_without_speakers_refuses_every_voice() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(100);
        c.engines[0].speakers.clear();
        let mut fixed: HashMap<String, Arc<dyn TtsEngine>> = HashMap::new();
        fixed.insert("local".into(), Arc::new(Fake));
        let t = Tts::with_engines(&c, dir.path(), fixed).unwrap();
        let err = t
            .synthesize("d", "hi", None, None, Some("F1"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        assert!(t.engines()[0].speakers.is_empty());
        assert_eq!(
            t.synthesize("d", "hi", None, None, None)
                .await
                .unwrap()
                .audio,
            b"plain"
        );
    }

    #[tokio::test]
    async fn text_is_capped_and_devices_are_limited_separately() {
        let dir = tempfile::tempdir().unwrap();
        let t = tts(2, dir.path());
        let long = "a".repeat(TTS_MAX_TEXT_CHARS + 1);
        let err = t
            .synthesize("a", &long, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        let ok = "a".repeat(TTS_MAX_TEXT_CHARS);
        assert!(t.synthesize("a", &ok, None, None, None).await.is_ok());
        assert_eq!(
            t.synthesize("a", " \n ", None, None, None)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
        assert!(t.synthesize("a", "two", None, None, None).await.is_ok());
        let err = t
            .synthesize("a", "three", None, None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(err.to_string().contains(RATE_LIMITED), "{err}");
        assert!(
            t.synthesize("b", "other device", None, None, None)
                .await
                .is_ok()
        );
        let err = t
            .synthesize("b", "x", None, Some("nope"), None)
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotFound);
    }

    #[test]
    fn listing_has_names_and_languages_only() {
        let dir = tempfile::tempdir().unwrap();
        let t = tts(5, dir.path());
        let list = t.engines();
        assert_eq!(
            list,
            vec![TtsEngineInfo {
                name: "local".into(),
                languages: vec!["en-GB".into(), "ko".into()],
                default: true,
                speakers: vec![
                    TtsSpeaker {
                        id: "F1".into(),
                        label: "Female 1".into(),
                        gender: TtsGender::Female,
                    },
                    TtsSpeaker {
                        id: "M1".into(),
                        label: "Male 1".into(),
                        gender: TtsGender::Male,
                    },
                ],
                disclosure: Some("Machine-generated voice".into()),
            }]
        );
        assert!(!format!("{list:?}").contains("127.0.0.1"));
        assert!(
            Tts::from_config(&TtsConfig::default(), dir.path())
                .unwrap()
                .is_none()
        );
    }

    /// Returns the text it was asked to say.
    struct Echo;

    #[async_trait]
    impl TtsEngine for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        async fn synthesize(&self, req: &SynthesisRequest) -> vca_speech::Result<Synthesized> {
            Ok(Synthesized {
                format: TtsFormat::Wav,
                audio: req.text.clone().into_bytes(),
            })
        }
    }

    #[tokio::test]
    async fn korean_voices_say_the_radio_words_in_hangul() {
        let dir = tempfile::tempdir().unwrap();
        let mut fixed: HashMap<String, Arc<dyn TtsEngine>> = HashMap::new();
        fixed.insert("local".into(), Arc::new(Echo));
        let t = Tts::with_engines(&cfg(100), dir.path(), fixed).unwrap();
        let said = |text: &'static str, lang: Option<&'static str>| {
            let t = &t;
            async move {
                let out = t.synthesize("d", text, lang, None, None).await.unwrap();
                String::from_utf8(out.audio).unwrap()
            }
        };
        assert_eq!(said(" Voco, Roger. ", Some("ko-KR")).await, "보코, 로저.");
        assert_eq!(said("VOCO가 Over", Some("ko")).await, "보코가 오버");
        assert_eq!(said("Voco, Roger.", Some("en-GB")).await, "Voco, Roger.");
        assert_eq!(said("Voco, Roger.", None).await, "Voco, Roger.");
    }

    #[tokio::test]
    async fn the_pronunciation_dictionary_applies_before_the_radio_words() {
        let dir = tempfile::tempdir().unwrap();
        let mut fixed: HashMap<String, Arc<dyn TtsEngine>> = HashMap::new();
        fixed.insert("local".into(), Arc::new(Echo));
        let t = Tts::with_engines(&cfg(100), dir.path(), fixed).unwrap();
        let p = crate::pronunciations::Pronunciations::open(None, None);
        p.set("SkyHarbor", "스카이하버", Some("ko")).unwrap();
        t.set_pronunciations(p.clone());
        let said = |text: &'static str, lang: Option<&'static str>| {
            let t = &t;
            async move {
                let out = t.synthesize("d", text, lang, None, None).await.unwrap();
                String::from_utf8(out.audio).unwrap()
            }
        };
        assert_eq!(
            said("Sky Harbor 끝, Roger", Some("ko-KR")).await,
            "스카이하버 끝, 로저"
        );
        assert_eq!(said("SkyHarbors", Some("ko")).await, "SkyHarbors");
        assert_eq!(said("SkyHarbor", Some("en")).await, "SkyHarbor");
        // Taught later: used at once.
        p.set("Zento", "젠토", None).unwrap();
        assert_eq!(said("zento", Some("ko")).await, "젠토");
    }

    /// Returns the language it was given and the text.
    struct Lang;

    #[async_trait]
    impl TtsEngine for Lang {
        fn name(&self) -> &str {
            "lang"
        }
        async fn synthesize(&self, req: &SynthesisRequest) -> vca_speech::Result<Synthesized> {
            let lang = req.language.clone().unwrap_or_else(|| "-".into());
            Ok(Synthesized {
                format: TtsFormat::Wav,
                audio: format!("{lang}|{}", req.text).into_bytes(),
            })
        }
    }

    #[tokio::test]
    async fn the_engine_gets_the_language_and_korean_gets_hangul_readings() {
        let dir = tempfile::tempdir().unwrap();
        let mut fixed: HashMap<String, Arc<dyn TtsEngine>> = HashMap::new();
        fixed.insert("local".into(), Arc::new(Lang));
        let t = Tts::with_engines(&cfg(100), dir.path(), fixed).unwrap();
        let said = |text: &'static str, lang: Option<&'static str>| {
            let t = &t;
            async move {
                let out = t.synthesize("d", text, lang, None, None).await.unwrap();
                String::from_utf8(out.audio).unwrap()
            }
        };
        assert_eq!(
            said("npm run build 실패, VCA", Some("ko-KR")).await,
            "ko-KR|엔피엠 run build 실패, 브이씨에이"
        );
        assert_eq!(said("PR 3", Some(" ko ")).await, "ko|피알 3");
        // Other languages keep their Latin words.
        assert_eq!(
            said("npm run build failed", Some("en-US")).await,
            "en-US|npm run build failed"
        );
        assert_eq!(said("VCA", None).await, "-|VCA");
    }

    #[tokio::test]
    async fn korean_text_is_cleaned_up_for_the_voice() {
        let dir = tempfile::tempdir().unwrap();
        let mut fixed: HashMap<String, Arc<dyn TtsEngine>> = HashMap::new();
        fixed.insert("local".into(), Arc::new(Lang));
        let t = Tts::with_engines(&cfg(100), dir.path(), fixed).unwrap();
        let said = |text: String, lang: Option<&'static str>| {
            let t = &t;
            async move {
                let out = t.synthesize("d", &text, lang, None, None).await.unwrap();
                String::from_utf8(out.audio).unwrap()
            }
        };
        // The clean-up runs before the Latin readings: the hash and the
        // extension are spelled whole, then "PR" and "ts" become Hangul.
        assert_eq!(
            said("PR **e25f95a9** -> main.ts 3개".into(), Some("ko-KR")).await,
            "ko-KR|피알 이 이 오 에프 구 오, main 점 티 에스 세 개"
        );
        assert_eq!(
            said("vitest 1737개 (98%)".into(), Some("ko")).await,
            "ko|vitest 천칠백삼십칠 개, 구십팔 퍼센트"
        );
        // Other languages and text without Hangul are left alone.
        assert_eq!(
            said("main.ts 3개 -> e25f95a9".into(), Some("en")).await,
            "en|main.ts 3개 -> e25f95a9"
        );
        assert_eq!(said("PR 3".into(), Some("ko")).await, "ko|피알 3");
        // Within the limit before the clean-up but not after: the readings
        // apply without it rather than failing.
        let digits = format!("{}개", "가 77".repeat(TTS_MAX_TEXT_CHARS / 4 - 1));
        assert!(digits.chars().count() <= TTS_MAX_TEXT_CHARS);
        assert!(
            vca_speech::ko_text::korean_speakable(&digits)
                .chars()
                .count()
                > TTS_MAX_TEXT_CHARS
        );
        assert_eq!(
            said(digits.clone(), Some("ko")).await,
            format!("ko|{digits}")
        );
    }

    #[test]
    fn credentials_are_stored_under_the_engine_name() {
        let dir = tempfile::tempdir().unwrap();
        let t = tts(5, dir.path());
        assert!(t.set_credential("missing", "k").is_err());
        assert!(t.set_credential("local", "  ").is_err());
        t.set_credential("local", " sk-1 \n").unwrap();
        assert_eq!(t.secrets_of(&t.slots[0]).0.as_deref(), Some("sk-1"));
        assert!(t.delete_credential("local").unwrap());
        assert!(!t.delete_credential("local").unwrap());
    }
}
