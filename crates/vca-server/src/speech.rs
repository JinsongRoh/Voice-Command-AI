//! Server-side speech recognition (VOC-01): audio from a client goes
//! through the configured STT engine, filler clean-up, the developer
//! vocabulary engine (base, project and personal layers) and the intent
//! router. The result is only returned for review (VOC-03); nothing is sent
//! to a session from here.
//!
//! The engine is off by default. API keys are read from environment
//! variables named in the configuration, never from the file itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tokio::sync::Semaphore;
use vca_core::error::ErrorCode;
use vca_core::{AudioEncoding, Error, SessionId, TranscriptCorrection, TranscriptIntent};
use vca_speech::intent::SessionAlias;
use vca_speech::lexicon::{Layer, Lexicon, PersonalLexicon, ProjectLexicon, ProjectScanConfig};
use vca_speech::stt::{
    GoogleAuth, GoogleSttConfig, GoogleSttEngine, OpenAiSttConfig, OpenAiSttEngine,
};
use vca_speech::{
    AudioFormat, AudioInput, Intent, IntentContext, IntentRouter, SpeechError, SttEngine, Target,
    TranscribeOptions, VocabularyEngine, cleanup::FillerRemover,
};

use crate::config::{SpeechConfig, SpeechEngineKind};
use crate::util::lock;

/// Project vocabularies are rescanned after this long.
const PROJECT_TTL: Duration = Duration::from_secs(600);
/// Characters of vocabulary hints sent to prompt-style engines.
const HINT_CHARS: usize = 600;
/// Phrase hints sent to phrase-boosting engines.
const HINT_PHRASES: usize = 200;
const PERSONAL_FILE: &str = "personal.json";

/// Vocabulary engines per project folder (`None`: no project), with the
/// time they were built.
type VocabCache = HashMap<Option<PathBuf>, (Instant, Arc<VocabularyEngine>)>;

/// One recognition request.
#[derive(Debug, Clone)]
pub struct TranscribeRequest {
    pub audio: Vec<u8>,
    pub format: AudioEncoding,
    pub sample_rate: Option<u32>,
    pub language: Option<String>,
    /// Folder of the selected session, for the project vocabulary.
    pub project_dir: Option<PathBuf>,
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
}

/// The speech pipeline. Shared by all clients.
pub struct Speech {
    engine: Arc<dyn SttEngine>,
    cfg: SpeechConfig,
    base: Lexicon,
    personal: Option<PersonalLexicon>,
    router: IntentRouter,
    fillers: FillerRemover,
    vocab: Mutex<VocabCache>,
    permits: Semaphore,
}

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

/// Reads the secret named by `var`, if configured and set.
fn env_secret(var: Option<&str>) -> Option<String> {
    let var = var.filter(|v| !v.is_empty())?;
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}

impl Speech {
    /// Builds the configured engine; `None` when speech is off.
    pub fn from_config(cfg: &SpeechConfig, data_dir: &Path) -> anyhow::Result<Option<Self>> {
        let engine: Arc<dyn SttEngine> = match cfg.engine {
            SpeechEngineKind::Off => return Ok(None),
            SpeechEngineKind::Openai => {
                let o = &cfg.openai;
                Arc::new(OpenAiSttEngine::new(OpenAiSttConfig {
                    base_url: o.base_url.clone(),
                    api_key: env_secret(o.api_key_env.as_deref()),
                    model: o.model.clone(),
                    timeout: Duration::from_secs(o.timeout_secs.max(1)),
                    ..OpenAiSttConfig::default()
                })?)
            }
            SpeechEngineKind::Google => {
                let g = &cfg.google;
                let auth = if let Some(key) = env_secret(g.api_key_env.as_deref()) {
                    GoogleAuth::ApiKey { key }
                } else if let Some(token) = env_secret(g.access_token_env.as_deref()) {
                    GoogleAuth::Bearer { token }
                } else {
                    bail!(
                        "speech engine google needs an API key or access token: set the \
                         variable named by speech.google.api_key_env or access_token_env"
                    );
                };
                let mut gc = GoogleSttConfig::new(g.project_id.clone(), auth);
                gc.location = g.location.clone();
                gc.model = g.model.clone();
                if let Some(url) = &g.base_url {
                    gc.base_url = url.clone();
                }
                Arc::new(GoogleSttEngine::new(gc)?)
            }
        };
        Self::with_engine(engine, cfg.clone(), data_dir).map(Some)
    }

    /// A pipeline around `engine` (tests inject a mock here).
    pub fn with_engine(
        engine: Arc<dyn SttEngine>,
        cfg: SpeechConfig,
        data_dir: &Path,
    ) -> anyhow::Result<Self> {
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
        let personal = match std::fs::read_to_string(&personal_path) {
            Ok(text) => Some(
                PersonalLexicon::from_json(&text)
                    .with_context(|| format!("reading {}", personal_path.display()))?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", personal_path.display()));
            }
        };
        Ok(Self {
            engine,
            permits: Semaphore::new(cfg.max_concurrent.max(1)),
            cfg,
            base,
            personal,
            router: IntentRouter::new(),
            fillers: FillerRemover::new(),
            vocab: Mutex::new(HashMap::new()),
        })
    }

    pub fn engine_name(&self) -> &str {
        self.engine.name()
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
        let audio = audio_input(req.audio, req.format, req.sample_rate)?;
        let vocab = self.vocabulary(req.project_dir.as_deref()).await;
        let language = req.language.clone().or_else(|| self.cfg.language.clone());
        let options = TranscribeOptions {
            primary_language: language.clone(),
            secondary_languages: self.cfg.secondary_languages.clone(),
            prompt_hint: Some(vocab.hint_prompt(HINT_CHARS)).filter(|h| !h.is_empty()),
            phrase_hints: vocab.hint_phrases(HINT_PHRASES),
        };

        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| api_error(ErrorCode::Unavailable, "speech is shutting down"))?;
        let transcript = self
            .engine
            .transcribe(&audio, &options)
            .await
            .map_err(speech_error)?;
        let detected = transcript.language.clone().or(language);
        let raw_text = transcript.text.trim().to_owned();
        let cleaned = if self.cfg.remove_fillers {
            self.fillers.remove(&raw_text)
        } else {
            raw_text.clone()
        };
        let mut vctx = vocab.context().clone();
        vctx.language = detected.clone();
        let corrected = vocab.correct_with(&cleaned, &vctx);
        let corrections = corrected
            .corrections
            .iter()
            .map(|c| TranscriptCorrection {
                from: c.original.clone(),
                to: c.replacement.clone(),
                layer: layer_name(c.layer).to_owned(),
            })
            .collect();
        let intent = to_intent(self.router.route(&corrected.text, ctx));
        Ok(TranscriptResult {
            text: corrected.text,
            raw_text,
            language: detected,
            corrections,
            intent,
            engine: self.engine.name().to_owned(),
        })
    }

    /// The vocabulary engine for a project folder, scanning it at most once
    /// per [`PROJECT_TTL`].
    async fn vocabulary(&self, project: Option<&Path>) -> Arc<VocabularyEngine> {
        let key = project
            .filter(|_| self.cfg.project_vocabulary)
            .map(Path::to_path_buf);
        if let Some((at, engine)) = lock(&self.vocab).get(&key)
            && at.elapsed() < PROJECT_TTL
        {
            return Arc::clone(engine);
        }
        let mut engine = VocabularyEngine::new(self.base.clone());
        if let Some(p) = &self.personal {
            engine.set_personal(p.clone());
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
        cache.retain(|_, (at, _)| at.elapsed() < PROJECT_TTL);
        cache.insert(key, (Instant::now(), Arc::clone(&engine)));
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

fn speech_error(err: SpeechError) -> Error {
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
        }
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

    #[test]
    fn off_builds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Speech::from_config(&SpeechConfig::default(), dir.path())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn google_without_credentials_is_a_configuration_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = SpeechConfig {
            engine: SpeechEngineKind::Google,
            ..SpeechConfig::default()
        };
        cfg.google.project_id = "p".into();
        cfg.google.api_key_env = Some("VCA_TEST_UNSET_SPEECH_KEY_81723".into());
        cfg.google.access_token_env = None;
        assert!(Speech::from_config(&cfg, dir.path()).is_err());
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
        };
        let ctx = intent_context(std::slice::from_ref(&info), Some(&info.id));
        assert_eq!(ctx.sessions[0].names, vec!["claude (payments)", "payments"]);
        assert!(ctx.pending_decision);
        assert!(!intent_context(&[info], None).pending_decision);
    }
}
