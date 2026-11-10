//! Speech engine setup from the desktop app on this PC: reading, writing
//! and testing the speech-to-text engines (`[[speech.engines]]`), the
//! transcript-correction LLMs (`[[speech.llms]]`) and the TTS engines
//! (`[[tts.engines]]`) of `config.toml`.
//!
//! VCA ships no servers and no model weights: every engine is the owner's
//! own (a local OpenAI-compatible server or the owner's cloud account).
//! These requests exist only on the owner-only local control channel, and
//! [`handle_local`] refuses everything that is not that channel with
//! `forbidden`, so paired devices and the development WebSocket can never
//! read or change engine settings or keys. Phones still see the engines by
//! name through `ListSpeechOptions` / `ListTtsEngines`.
//!
//! * Keys go to the same owner-only credential stores as `vca speech login`
//!   (`<data>/speech/keys`) and `vca tts login` (`<data>/tts/keys`). They
//!   are never written to `config.toml`, never returned and never logged.
//!   Gateway tokens (for an engine behind a gateway or reverse proxy that
//!   authenticates with its own bearer token) are kept the same way, in
//!   `<data>/speech/gateway` and `<data>/tts/gateway`.
//! * `config.toml` is edited in place with `toml_edit`: comments, formatting
//!   and every unrelated section stay as they are. The whole new text is
//!   validated with the server's own configuration rules before it is
//!   written; the write is atomic (temporary file and rename) and the
//!   previous file is kept as `config.toml.bak`.
//! * Engines are loaded when vcad starts. A change is reported with
//!   `restart_required` until vcad runs with it (keys apply at once).
//! * A test sends one tiny real request to the owner's endpoint: a bundled
//!   one-second tone for speech-to-text, a one-word prompt for an LLM and a
//!   short fixed phrase for TTS. Neither the request nor the answer is
//!   logged or returned, only whether it worked, the latency and a size.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};
use vca_core::error::ErrorCode;
use vca_core::{Error, TtsSpeaker};

use crate::api::{Api, Client};
use crate::cloud::Secret;
use crate::config::{
    Config, EngineProfileKind, LlmProfile, LlmProfileKind, SpeechEngineProfile, TtsEngineKind,
    TtsEngineProfile, valid_profile_id,
};
use crate::control::{LocalRequest, LocalResponse};
use crate::speech_profiles::{EngineSlot, LlmSlot, Secrets, engine_auth, env_secret};
use crate::util::{error_text, lock, truncate_chars};

/// The transport name of the owner-only local control channel.
const LOCAL_TRANSPORT: &str = "local";
const FORBIDDEN: &str =
    "speech engines and their keys can only be set up in the desktop app on this PC";

const OPENAI_API: &str = "https://api.openai.com/v1";
const DEFAULT_STT_MODEL: &str = "whisper-1";
const DEFAULT_CHAT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_TTS_MODEL: &str = "tts-1";
const DEFAULT_TTS_VOICE: &str = "alloy";

/// Longest wait for one test request.
const TEST_TIMEOUT: Duration = Duration::from_secs(20);
/// The phrase a TTS test synthesizes.
const TEST_PHRASE: &str = "Voice Command AI test.";
const TEST_SYSTEM: &str = "You are a connection test. Reply with the single word: ok";
const TEST_USER: &str = "ok";
/// Largest audio accepted from a TTS test.
const TEST_MAX_AUDIO: usize = 2 * 1024 * 1024;
/// Characters of an engine's error text passed on.
const MAX_ERROR_CHARS: usize = 300;

const MAX_URL_CHARS: usize = 2048;
const MAX_LABEL_CHARS: usize = 64;
const MAX_NAME_CHARS: usize = 128;
const MAX_SECRET_CHARS: usize = 4096;
const MAX_SPEAKERS: usize = 64;

/// Which list an engine profile belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineType {
    /// Speech-to-text (`[[speech.engines]]`).
    Stt,
    /// Transcript-correction LLM (`[[speech.llms]]`).
    Llm,
    /// Text-to-speech (`[[tts.engines]]`).
    Tts,
}

impl EngineType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Llm => "llm",
            Self::Tts => "tts",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Self::Stt => "speech engine",
            Self::Llm => "correction LLM",
            Self::Tts => "TTS engine",
        }
    }

    /// `(section, array, id key)` in `config.toml`.
    fn location(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Stt => ("speech", "engines", "id"),
            Self::Llm => ("speech", "llms", "id"),
            Self::Tts => ("tts", "engines", "name"),
        }
    }

    /// `(section, key)` of the default.
    fn default_key(self) -> (&'static str, &'static str) {
        match self {
            Self::Stt => ("speech", "default_engine"),
            Self::Llm => ("speech", "default_llm"),
            Self::Tts => ("tts", "default_engine"),
        }
    }
}

/// One engine profile as the app edits it. Never carries a key: keys are
/// sent separately and only stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineForm {
    /// Profile id (`name` for TTS engines): `a-z`, `0-9`, `-`, `_`.
    pub id: String,
    /// Shown in the app (speech engines and LLMs only).
    pub label: Option<String>,
    /// `openai_compatible`, `openai`, `google` (speech-to-text);
    /// `openai_compatible`, `anthropic` (LLMs); `openai_compatible` (TTS).
    pub kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// TTS only: the default voice.
    pub voice: Option<String>,
    /// TTS only: voices a user may pick.
    pub speakers: Vec<TtsSpeaker>,
    /// TTS only: `mp3`, `opus`, `aac`, `flac` or `wav`.
    pub format: Option<String>,
    /// Name of the stored key; the id when unset.
    pub credential: Option<String>,
    /// Environment variable holding the key (wins over the stored key).
    pub api_key_env: Option<String>,
    /// Google speech-to-text only.
    pub project_id: Option<String>,
    /// Google speech-to-text only (`global` by default).
    pub location: Option<String>,
    /// The key may go over plain http to this private network address.
    /// Turning it on needs local verification.
    pub allow_plain_http: bool,
    /// OpenAI-compatible engines only: the header carrying the key, sent
    /// raw, instead of `Authorization: Bearer` (e.g. `X-VCA-Key`).
    pub api_key_header: Option<String>,
    /// OpenAI-compatible engines only: environment variable holding the
    /// gateway token (wins over the stored token).
    pub gateway_token_env: Option<String>,
}

/// What a save does with the stored gateway token.
#[derive(Debug, Default)]
pub enum GatewayChange {
    /// Leave it as it is.
    #[default]
    Keep,
    /// Store (or replace) it.
    Set(Secret),
    /// Delete it.
    Clear,
}

/// Where a profile's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    /// The credential store on this PC.
    Stored,
    /// The environment variable named by `api_key_env`.
    Env,
    None,
}

/// One configured profile (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineEntry {
    pub profile: EngineForm,
    pub key: KeySource,
    /// Where the gateway token comes from (`none`: requests go straight to
    /// the engine).
    pub gateway: KeySource,
    /// A key is available, or none is needed.
    pub ready: bool,
    /// The server default.
    pub default: bool,
    /// The single `[speech] engine` or `[speech.llm]` of older
    /// configurations: listed, but edited in `config.toml` only.
    pub read_only: bool,
}

/// The engine setup of this PC (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineSetup {
    /// The configuration file that is edited.
    pub config_path: String,
    pub stt: Vec<EngineEntry>,
    pub llm: Vec<EngineEntry>,
    pub tts: Vec<EngineEntry>,
    /// The file has engine changes vcad does not use yet: restart it.
    pub restart_required: bool,
}

/// The outcome of one test request. Never contains the request, the
/// answer or a key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineTestResult {
    pub ok: bool,
    pub latency_ms: u64,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    /// A short English summary (or the engine's error, keys removed).
    pub message: String,
    /// Speech-to-text and LLM tests: characters in the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars: Option<usize>,
    /// TTS tests: bytes of audio received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bytes: Option<usize>,
}

/// `Option<Secret>` on the wire: a string, or absent / empty for none.
pub(crate) mod opt_secret {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::cloud::Secret;

    pub fn serialize<S: Serializer>(secret: &Option<Secret>, s: S) -> Result<S::Ok, S::Error> {
        match secret {
            Some(secret) => s.serialize_some(secret.expose()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Secret>, D::Error> {
        Ok(Option::<String>::deserialize(d)?
            .filter(|s| !s.trim().is_empty())
            .map(Secret::new))
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::InvalidRequest, message)
}

fn internal(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::Internal, message)
}

/// The engine parts of a configuration, to tell whether vcad runs with the
/// file's engines.
#[derive(Debug, Clone, PartialEq)]
struct EngineSnapshot {
    stt: Vec<SpeechEngineProfile>,
    llm: Vec<LlmProfile>,
    default_stt: Option<String>,
    default_llm: Option<String>,
    tts: Vec<TtsEngineProfile>,
    default_tts: Option<String>,
}

impl EngineSnapshot {
    fn of(cfg: &Config) -> Self {
        Self {
            stt: cfg.speech.engine_profiles(),
            llm: cfg.speech.llm_profiles(),
            default_stt: cfg.speech.default_engine_id(),
            default_llm: cfg.speech.default_llm_id(),
            tts: cfg.tts.engines.clone(),
            default_tts: cfg.tts.default_engine_name(),
        }
    }
}

/// Engine setup of this PC: the configuration file, the credential stores
/// and the configuration vcad started with.
pub struct EngineAdmin {
    config_file: PathBuf,
    data_dir: PathBuf,
    started: EngineSnapshot,
    /// Serializes read-modify-write cycles of the file.
    write: Mutex<()>,
}

impl EngineAdmin {
    /// `started`: the configuration vcad runs with.
    pub fn new(config_file: PathBuf, data_dir: PathBuf, started: &Config) -> Self {
        Self {
            config_file,
            data_dir,
            started: EngineSnapshot::of(started),
            write: Mutex::new(()),
        }
    }

    fn read_text(&self) -> Result<String, Error> {
        match std::fs::read_to_string(&self.config_file) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(internal(format!(
                "cannot read {}: {e}",
                self.config_file.display()
            ))),
        }
    }

    fn parse(&self, text: &str) -> Result<Config, Error> {
        Config::from_toml_str(text).map_err(|e| {
            invalid(format!(
                "{} is not valid; fix it first: {}",
                self.config_file.display(),
                e.message()
            ))
        })
    }

    fn speech_secrets(&self) -> Secrets {
        Secrets::in_data_dir(&self.data_dir)
    }

    /// The credential store of `kind`.
    fn secrets(&self, kind: EngineType) -> Secrets {
        match kind {
            EngineType::Stt | EngineType::Llm => self.speech_secrets(),
            EngineType::Tts => Secrets::tts_in_data_dir(&self.data_dir),
        }
    }

    fn stored_key(&self, kind: EngineType, name: &str) -> Option<String> {
        self.secrets(kind).get(name)
    }

    fn store_key(&self, kind: EngineType, name: &str, secret: &Secret) -> Result<(), Error> {
        let value = check_secret("key", secret)?;
        self.secrets(kind).put(name, value)?;
        tracing::info!(kind = kind.as_str(), credential = name, "engine key stored");
        Ok(())
    }

    fn delete_key(&self, kind: EngineType, name: &str) -> Result<(), Error> {
        self.secrets(kind).delete(name)?;
        tracing::info!(
            kind = kind.as_str(),
            credential = name,
            "engine key deleted"
        );
        Ok(())
    }

    fn store_gateway(&self, kind: EngineType, name: &str, secret: &Secret) -> Result<(), Error> {
        let value = check_secret("gateway token", secret)?;
        self.secrets(kind).put_gateway(name, value)?;
        tracing::info!(
            kind = kind.as_str(),
            credential = name,
            "engine gateway token stored"
        );
        Ok(())
    }

    fn delete_gateway(&self, kind: EngineType, name: &str) -> Result<bool, Error> {
        let existed = self.secrets(kind).delete_gateway(name)?;
        if existed {
            tracing::info!(
                kind = kind.as_str(),
                credential = name,
                "engine gateway token deleted"
            );
        }
        Ok(existed)
    }

    fn key_source(&self, kind: EngineType, form: &EngineForm) -> KeySource {
        if env_secret(form.api_key_env.as_deref()).is_some() {
            KeySource::Env
        } else if self.stored_key(kind, credential_name(form)).is_some() {
            KeySource::Stored
        } else {
            KeySource::None
        }
    }

    fn gateway_source(&self, kind: EngineType, form: &EngineForm) -> KeySource {
        if env_secret(form.gateway_token_env.as_deref()).is_some() {
            KeySource::Env
        } else if self.secrets(kind).gateway(credential_name(form)).is_some() {
            KeySource::Stored
        } else {
            KeySource::None
        }
    }

    /// Does a key or token exist for `form`, stored or named in the
    /// environment?
    fn has_credentials(&self, kind: EngineType, form: &EngineForm) -> bool {
        self.test_key(kind, form).is_some() || self.test_gateway(kind, form).is_some()
    }

    /// Is this how the owner already saved these credentials: the same
    /// stored key, the same environment variables, the same URL?
    fn saved_with_credentials(&self, kind: EngineType, cfg: &Config, form: &EngineForm) -> bool {
        all_saved_forms(kind, cfg)
            .iter()
            .any(|saved| same_credentials(saved, form))
    }

    /// Does saving `form` need local verification? When credentials that
    /// already exist would go somewhere they were never saved for, or when it
    /// turns on plain http.
    pub fn save_moves_credentials(
        &self,
        kind: EngineType,
        form: &EngineForm,
    ) -> Result<bool, Error> {
        let form = normalize(kind, form.clone())?;
        let cfg = self.parse(&self.read_text()?)?;
        let flag_on = form.allow_plain_http
            && !all_saved_forms(kind, &cfg)
                .iter()
                .any(|saved| same_credentials(saved, &form) && saved.allow_plain_http);
        Ok(flag_on
            || (self.has_credentials(kind, &form)
                && !self.saved_with_credentials(kind, &cfg, &form)))
    }

    /// The key a test uses: the one sent with it, else the profile's.
    fn test_key(&self, kind: EngineType, form: &EngineForm) -> Option<String> {
        env_secret(form.api_key_env.as_deref())
            .or_else(|| self.stored_key(kind, credential_name(form)))
    }

    /// The gateway token a test uses: the one sent with it, else the
    /// profile's.
    fn test_gateway(&self, kind: EngineType, form: &EngineForm) -> Option<String> {
        env_secret(form.gateway_token_env.as_deref())
            .or_else(|| self.secrets(kind).gateway(credential_name(form)))
    }

    /// The current setup, read from the file.
    pub fn setup(&self) -> Result<EngineSetup, Error> {
        let text = self.read_text()?;
        let cfg = self.parse(&text)?;
        Ok(self.view(&cfg))
    }

    fn view(&self, cfg: &Config) -> EngineSetup {
        let secrets = self.speech_secrets();
        let speech = &cfg.speech;
        let legacy_stt = speech.engine_profiles().len() - speech.engines.len();
        let default_stt = speech.default_engine_id();
        let stt = speech
            .engine_profiles()
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let ready = EngineSlot::new(p.clone()).configured(&secrets);
                let form = stt_form(&p);
                EngineEntry {
                    key: self.key_source(EngineType::Stt, &form),
                    gateway: self.gateway_source(EngineType::Stt, &form),
                    ready,
                    default: default_stt.as_deref() == Some(p.id.as_str()),
                    read_only: i < legacy_stt,
                    profile: form,
                }
            })
            .collect();
        let legacy_llm = speech.llm_profiles().len() - speech.llms.len();
        let default_llm = speech.default_llm_id();
        let llm = speech
            .llm_profiles()
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let ready = LlmSlot::new(p.clone(), Duration::from_secs(1)).configured(&secrets);
                let form = llm_form(&p);
                EngineEntry {
                    key: self.key_source(EngineType::Llm, &form),
                    gateway: self.gateway_source(EngineType::Llm, &form),
                    ready,
                    default: default_llm.as_deref() == Some(p.id.as_str()),
                    read_only: i < legacy_llm,
                    profile: form,
                }
            })
            .collect();
        let default_tts = cfg.tts.default_engine_name();
        let tts = cfg
            .tts
            .engines
            .iter()
            .map(|p| {
                let form = tts_form(p);
                EngineEntry {
                    key: self.key_source(EngineType::Tts, &form),
                    gateway: self.gateway_source(EngineType::Tts, &form),
                    ready: true,
                    default: default_tts.as_deref() == Some(p.name.as_str()),
                    read_only: false,
                    profile: form,
                }
            })
            .collect();
        EngineSetup {
            config_path: self.config_file.display().to_string(),
            stt,
            llm,
            tts,
            restart_required: EngineSnapshot::of(cfg) != self.started,
        }
    }

    /// Applies `edit` to the file, validates the result with the server's
    /// own rules and writes it atomically (keeping a `.bak`). Returns the
    /// configuration now in the file.
    fn edit(
        &self,
        edit: impl FnOnce(&mut DocumentMut, &Config) -> Result<(), Error>,
    ) -> Result<Config, Error> {
        let _guard = lock(&self.write);
        let text = self.read_text()?;
        let before = self.parse(&text)?;
        let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
            invalid(format!(
                "cannot edit {}: {}",
                self.config_file.display(),
                e.message()
            ))
        })?;
        edit(&mut doc, &before)?;
        let new_text = doc.to_string();
        let after = Config::from_toml_str(&new_text)
            .map_err(|e| invalid(format!("the change is not valid: {}", e.message())))?;
        after.speech.validate().map_err(invalid)?;
        after.tts.validate().map_err(invalid)?;
        if new_text != text {
            write_atomic(&self.config_file, &new_text).map_err(|e| {
                internal(format!("cannot write {}: {e}", self.config_file.display()))
            })?;
            tracing::info!(path = %self.config_file.display(), "engine settings saved");
        }
        Ok(after)
    }

    /// Adds a profile, or changes the one with id `original` (which may be
    /// renamed). `secret`, when given, is stored under the profile's
    /// credential name after the file was written. A stored gateway token
    /// is kept.
    pub fn save(
        &self,
        kind: EngineType,
        original: Option<&str>,
        form: EngineForm,
        secret: Option<&Secret>,
        make_default: bool,
    ) -> Result<EngineSetup, Error> {
        self.save_with_gateway(
            kind,
            original,
            form,
            secret,
            make_default,
            GatewayChange::Keep,
        )
    }

    /// [`Self::save`], and `gateway` stores, replaces or deletes the
    /// profile's gateway token.
    pub fn save_with_gateway(
        &self,
        kind: EngineType,
        original: Option<&str>,
        form: EngineForm,
        secret: Option<&Secret>,
        make_default: bool,
        gateway: GatewayChange,
    ) -> Result<EngineSetup, Error> {
        let form = normalize(kind, form)?;
        // Checked before anything is written.
        if let Some(secret) = secret {
            check_secret("key", secret)?;
        }
        let has_gateway = match &gateway {
            GatewayChange::Set(token) => {
                check_secret("gateway token", token)?;
                true
            }
            GatewayChange::Clear => env_secret(form.gateway_token_env.as_deref()).is_some(),
            GatewayChange::Keep => self.test_gateway(kind, &form).is_some(),
        };
        if has_gateway {
            check_gateway_form(kind, &form)?;
        }
        check_plain_http_flag(&form)?;
        if secret.is_some() || has_gateway || self.test_key(kind, &form).is_some() {
            check_credentials_url(&form)?;
        }
        let original = original.map(str::trim).filter(|s| !s.is_empty());
        let cfg = self.edit(|doc, before| {
            let (section, array, id_key) = kind.location();
            if let Some(orig) = original
                && legacy_ids(kind, before).iter().any(|l| l == orig)
            {
                return Err(invalid(format!(
                    "{orig:?} comes from an older [speech] setting; edit it in config.toml"
                )));
            }
            let table = section_mut(doc, section)?;
            let list = array_mut(table, section, array)?;
            let index = match original {
                Some(orig) => Some(find(list, id_key, orig).ok_or_else(|| {
                    Error::api(
                        ErrorCode::NotFound,
                        format!("no {} {orig:?} in config.toml", kind.what()),
                    )
                })?),
                None => None,
            };
            let renamed_or_new = index.is_none() || original != Some(form.id.as_str());
            if renamed_or_new && find(list, id_key, &form.id).is_some() {
                return Err(invalid(format!(
                    "a {} named {:?} already exists",
                    kind.what(),
                    form.id
                )));
            }
            let row = match index {
                Some(i) => list.get_mut(i).ok_or_else(|| internal("lost the entry"))?,
                None => {
                    list.push(Table::new());
                    let last = list.len() - 1;
                    list.get_mut(last)
                        .ok_or_else(|| internal("lost the entry"))?
                }
            };
            write_row(kind, row, &form, original, before)?;
            let (dsec, dkey) = kind.default_key();
            let current_default = doc
                .get(dsec)
                .and_then(|t| t.get(dkey))
                .and_then(Item::as_str)
                .map(str::to_owned);
            if make_default {
                set_default(doc, kind, Some(&form.id))?;
            } else if let Some(orig) = original
                && current_default.as_deref() == Some(orig)
                && orig != form.id
            {
                set_default(doc, kind, Some(&form.id))?;
            }
            Ok(())
        })?;
        if let Some(secret) = secret {
            self.store_key(kind, credential_name(&form), secret)?;
        }
        match gateway {
            GatewayChange::Keep => {}
            GatewayChange::Set(token) => {
                self.store_gateway(kind, credential_name(&form), &token)?
            }
            GatewayChange::Clear => {
                self.delete_gateway(kind, credential_name(&form))?;
            }
        }
        Ok(self.view(&cfg))
    }

    /// Stores, replaces (`Some`) or deletes (`None`) the gateway token of
    /// the saved profile `id`. The profile must send its key in its own
    /// header (`api_key_header`) first. Applies at once: vcad reads the
    /// token on every request.
    pub fn set_gateway_token(
        &self,
        kind: EngineType,
        id: &str,
        token: Option<&Secret>,
    ) -> Result<EngineSetup, Error> {
        let id = id.trim();
        let text = self.read_text()?;
        let cfg = self.parse(&text)?;
        let form = saved_form(kind, &cfg, id).ok_or_else(|| {
            Error::api(
                ErrorCode::NotFound,
                format!("no {} {id:?} in config.toml", kind.what()),
            )
        })?;
        match token {
            Some(token) => {
                check_secret("gateway token", token)?;
                check_gateway_form(kind, &form)?;
                self.store_gateway(kind, credential_name(&form), token)?;
            }
            None => {
                if !self.delete_gateway(kind, credential_name(&form))? {
                    return Err(Error::api(
                        ErrorCode::NotFound,
                        format!("no stored gateway token for {} {id:?}", kind.what()),
                    ));
                }
            }
        }
        Ok(self.view(&cfg))
    }

    /// Removes a profile; with `delete_key`, also its stored key unless
    /// another profile shares it.
    pub fn remove(
        &self,
        kind: EngineType,
        id: &str,
        delete_key: bool,
    ) -> Result<EngineSetup, Error> {
        let id = id.trim();
        let mut credential = None;
        let cfg = self.edit(|doc, before| {
            if legacy_ids(kind, before).iter().any(|l| l == id) {
                return Err(invalid(format!(
                    "{id:?} comes from an older [speech] setting; remove it in config.toml"
                )));
            }
            let (section, array, id_key) = kind.location();
            let table = section_mut(doc, section)?;
            let list = array_mut(table, section, array)?;
            let index = find(list, id_key, id).ok_or_else(|| {
                Error::api(
                    ErrorCode::NotFound,
                    format!("no {} {id:?} in config.toml", kind.what()),
                )
            })?;
            let row = list.remove(index);
            credential = Some(
                row.get("credential")
                    .and_then(Item::as_str)
                    .unwrap_or(id)
                    .to_owned(),
            );
            if list.is_empty() {
                table.remove(array);
            }
            let (dsec, dkey) = kind.default_key();
            let is_default = doc
                .get(dsec)
                .and_then(|t| t.get(dkey))
                .and_then(Item::as_str)
                == Some(id);
            if is_default {
                // An LLM default falls back to no correction rather than to
                // another LLM by surprise; the others to the first engine.
                let fallback = (kind == EngineType::Llm).then_some(vca_core::SPEECH_LLM_NONE);
                set_default(doc, kind, fallback)?;
            }
            Ok(())
        })?;
        if delete_key && let Some(name) = credential {
            let shared = match kind {
                EngineType::Stt | EngineType::Llm => {
                    cfg.speech
                        .engine_profiles()
                        .iter()
                        .any(|p| p.credential.as_deref().unwrap_or(&p.id) == name)
                        || cfg
                            .speech
                            .llm_profiles()
                            .iter()
                            .any(|p| p.credential.as_deref().unwrap_or(&p.id) == name)
                }
                EngineType::Tts => cfg
                    .tts
                    .engines
                    .iter()
                    .any(|p| p.credential.as_deref().unwrap_or(&p.name) == name),
            };
            if !shared {
                self.delete_key(kind, &name)?;
                self.delete_gateway(kind, &name)?;
            }
        }
        Ok(self.view(&cfg))
    }

    /// Sets the default profile. `None`: the first profile (speech-to-text,
    /// TTS) or no correction (LLM).
    pub fn set_default_profile(
        &self,
        kind: EngineType,
        id: Option<&str>,
    ) -> Result<EngineSetup, Error> {
        let id = id.map(str::trim).filter(|s| !s.is_empty());
        if let Some(id) = id
            && !valid_profile_id(id)
        {
            return Err(invalid(format!("{id:?} is not a profile id")));
        }
        let cfg = self.edit(|doc, _| match (kind, id) {
            (EngineType::Llm, None) => set_default(doc, kind, Some(vca_core::SPEECH_LLM_NONE)),
            _ => set_default(doc, kind, id),
        })?;
        Ok(self.view(&cfg))
    }

    /// Tests a profile: the draft `form`, else the saved profile `id`. The
    /// key is `secret`, else the profile's (environment, then store); the
    /// gateway token likewise `gateway`, else the profile's.
    pub async fn test(
        &self,
        kind: EngineType,
        form: Option<EngineForm>,
        id: Option<&str>,
        secret: Option<&Secret>,
        gateway: Option<&Secret>,
    ) -> Result<EngineTestResult, Error> {
        let text = self.read_text()?;
        let cfg = self.parse(&text)?;
        let (form, draft) = match (form, id.map(str::trim).filter(|s| !s.is_empty())) {
            (Some(form), _) => (normalize(kind, form)?, true),
            (None, Some(id)) => (
                saved_form(kind, &cfg, id).ok_or_else(|| {
                    Error::api(
                        ErrorCode::NotFound,
                        format!("no {} {id:?} in config.toml", kind.what()),
                    )
                })?,
                false,
            ),
            (None, None) => return Err(invalid("name a profile to test")),
        };
        // A draft is whatever the caller says: stored keys only ever go to
        // the URL they were saved with.
        let stored_allowed = !draft || self.saved_with_credentials(kind, &cfg, &form);
        let key = secret
            .map(|s| s.expose().trim().to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| stored_allowed.then(|| self.test_key(kind, &form)).flatten());
        let gateway = gateway
            .map(|s| s.expose().trim().to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                stored_allowed
                    .then(|| self.test_gateway(kind, &form))
                    .flatten()
            });
        if key.is_some() || gateway.is_some() {
            check_credentials_url(&form)?;
        }
        let auth = engine_auth(form.api_key_header.as_deref(), gateway.clone());
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            TEST_TIMEOUT + Duration::from_secs(5),
            run_test(kind, &form, &cfg, key.clone(), auth),
        )
        .await
        .unwrap_or_else(|_| {
            Err(Error::api(
                ErrorCode::Unavailable,
                "the engine did not answer in time",
            ))
        });
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let result = match outcome {
            Ok(done) => EngineTestResult {
                ok: true,
                latency_ms,
                code: None,
                message: done.message,
                chars: done.chars,
                audio_bytes: done.audio_bytes,
            },
            Err(err) => EngineTestResult {
                ok: false,
                latency_ms,
                code: Some(err.code()),
                message: redact(
                    &redact(&error_text(&err), key.as_deref()),
                    gateway.as_deref(),
                ),
                chars: None,
                audio_bytes: None,
            },
        };
        // Only the outcome: never the request, the answer or the key.
        tracing::info!(
            kind = kind.as_str(),
            profile = %form.id,
            ok = result.ok,
            latency_ms,
            "engine test finished"
        );
        Ok(result)
    }
}

/// Ids of the profiles that come from the single `[speech] engine` /
/// `[speech.llm]` of older configurations.
fn legacy_ids(kind: EngineType, cfg: &Config) -> Vec<String> {
    match kind {
        EngineType::Stt => {
            let all = cfg.speech.engine_profiles();
            let n = all.len() - cfg.speech.engines.len();
            all.into_iter().take(n).map(|p| p.id).collect()
        }
        EngineType::Llm => {
            let all = cfg.speech.llm_profiles();
            let n = all.len() - cfg.speech.llms.len();
            all.into_iter().take(n).map(|p| p.id).collect()
        }
        EngineType::Tts => Vec::new(),
    }
}

fn credential_name(form: &EngineForm) -> &str {
    form.credential.as_deref().unwrap_or(&form.id)
}

fn stt_form(p: &SpeechEngineProfile) -> EngineForm {
    EngineForm {
        id: p.id.clone(),
        label: p.label.clone(),
        kind: p.kind.as_str().to_owned(),
        base_url: p.base_url.clone(),
        model: p.model.clone(),
        credential: p.credential.clone(),
        api_key_env: p.api_key_env.clone(),
        project_id: p.project_id.clone(),
        location: p.location.clone(),
        api_key_header: p.api_key_header.clone(),
        gateway_token_env: p.gateway_token_env.clone(),
        allow_plain_http: p.allow_plain_http,
        ..EngineForm::default()
    }
}

fn llm_form(p: &LlmProfile) -> EngineForm {
    EngineForm {
        id: p.id.clone(),
        label: p.label.clone(),
        kind: p.kind.as_str().to_owned(),
        base_url: p.base_url.clone(),
        model: p.model.clone(),
        credential: p.credential.clone(),
        api_key_env: p.api_key_env.clone(),
        api_key_header: p.api_key_header.clone(),
        gateway_token_env: p.gateway_token_env.clone(),
        allow_plain_http: p.allow_plain_http,
        ..EngineForm::default()
    }
}

fn tts_form(p: &TtsEngineProfile) -> EngineForm {
    EngineForm {
        id: p.name.clone(),
        kind: p.kind.as_str().to_owned(),
        base_url: p.base_url.clone(),
        model: p.model.clone(),
        voice: p.voice.clone(),
        speakers: p.speakers.clone(),
        format: p.format.clone(),
        credential: p.credential.clone(),
        api_key_env: p.api_key_env.clone(),
        api_key_header: p.api_key_header.clone(),
        gateway_token_env: p.gateway_token_env.clone(),
        allow_plain_http: p.allow_plain_http,
        ..EngineForm::default()
    }
}

fn saved_form(kind: EngineType, cfg: &Config, id: &str) -> Option<EngineForm> {
    match kind {
        EngineType::Stt => cfg
            .speech
            .engine_profiles()
            .iter()
            .find(|p| p.id == id)
            .map(stt_form),
        EngineType::Llm => cfg
            .speech
            .llm_profiles()
            .iter()
            .find(|p| p.id == id)
            .map(llm_form),
        EngineType::Tts => cfg.tts.engines.iter().find(|p| p.name == id).map(tts_form),
    }
}

/// Every saved profile of `kind`.
fn all_saved_forms(kind: EngineType, cfg: &Config) -> Vec<EngineForm> {
    match kind {
        EngineType::Stt => cfg.speech.engine_profiles().iter().map(stt_form).collect(),
        EngineType::Llm => cfg.speech.llm_profiles().iter().map(llm_form).collect(),
        EngineType::Tts => cfg.tts.engines.iter().map(tts_form).collect(),
    }
}

/// The same stored key, environment variables and URL. A new variable name
/// is another secret going to the URL, so it counts as a move.
fn same_credentials(saved: &EngineForm, form: &EngineForm) -> bool {
    credential_name(saved) == credential_name(form)
        && saved.base_url == form.base_url
        && saved.api_key_env == form.api_key_env
        && saved.gateway_token_env == form.gateway_token_env
}

/// See [`vca_speech::auth::check_key_url`].
fn check_credentials_url(form: &EngineForm) -> Result<(), Error> {
    match form.base_url.as_deref() {
        Some(url) => vca_speech::auth::check_key_url(url, form.allow_plain_http)
            .map_err(|e| invalid(e.to_string())),
        None => Ok(()),
    }
}

/// The flag only means something for plain http to a private address.
fn check_plain_http_flag(form: &EngineForm) -> Result<(), Error> {
    let private = form
        .base_url
        .as_deref()
        .is_some_and(vca_speech::auth::is_private_plain_http);
    if form.allow_plain_http && !private {
        return Err(invalid(
            "plain http can only be allowed for a private network address",
        ));
    }
    Ok(())
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// A secret from the app or the CLI: one non-empty line of bounded length.
fn check_secret<'a>(what: &str, secret: &'a Secret) -> Result<&'a str, Error> {
    let value = secret.expose().trim();
    if value.is_empty() {
        return Err(invalid(format!("the {what} is empty")));
    }
    if value.chars().count() > MAX_SECRET_CHARS || value.chars().any(char::is_control) {
        return Err(invalid(format!(
            "a {what} is one line of at most {MAX_SECRET_CHARS} characters"
        )));
    }
    Ok(value)
}

/// The engine speaks an OpenAI-compatible API (the only kind a key header
/// and a gateway token apply to).
fn openai_like(kind: EngineType, form_kind: &str) -> bool {
    match kind {
        EngineType::Stt => form_kind != "google",
        EngineType::Llm | EngineType::Tts => form_kind == "openai_compatible",
    }
}

/// A gateway token can be used with this profile: it sends its key in a
/// header of its own.
fn check_gateway_form(kind: EngineType, form: &EngineForm) -> Result<(), Error> {
    if !openai_like(kind, &form.kind) {
        return Err(invalid(format!(
            "{} {:?}: gateway tokens apply to OpenAI-compatible engines only",
            kind.what(),
            form.id
        )));
    }
    if form
        .api_key_header
        .as_deref()
        .is_none_or(vca_speech::auth::is_authorization)
    {
        return Err(invalid(format!(
            "{} {:?}: {}",
            kind.what(),
            form.id,
            vca_speech::auth::GATEWAY_NEEDS_KEY_HEADER
        )));
    }
    Ok(())
}

/// A short single-line text: a label, model, voice or project name.
fn check_text(field: &str, value: Option<&str>, max: usize) -> Result<(), Error> {
    if let Some(v) = value
        && (v.chars().count() > max || v.chars().any(char::is_control))
    {
        return Err(invalid(format!(
            "{field} must be one line of at most {max} characters"
        )));
    }
    Ok(())
}

/// An `http://` or `https://` API root with a host, without credentials,
/// query or fragment (keys never go into URLs).
pub fn check_base_url(url: &str) -> Result<(), String> {
    if url.chars().count() > MAX_URL_CHARS {
        return Err(format!(
            "the base URL is longer than {MAX_URL_CHARS} characters"
        ));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the base URL must not contain spaces".into());
    }
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return Err("the base URL must start with http:// or https://".into());
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.starts_with(':') {
        return Err("the base URL needs a host name, e.g. http://127.0.0.1:8000/v1".into());
    }
    if authority.contains('@') {
        return Err(
            "the base URL must not contain a user name or password; enter the key separately"
                .into(),
        );
    }
    if url.contains(['?', '#']) {
        return Err("the base URL must not contain ? or #".into());
    }
    Ok(())
}

/// An environment variable name.
fn check_env_name(name: &str) -> Result<(), Error> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok
        || name.len() > MAX_NAME_CHARS
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(invalid(format!(
            "{name:?} is not an environment variable name (A-Z, 0-9 and _)"
        )));
    }
    Ok(())
}

/// Trims and checks a form, and drops the fields its type does not have.
pub fn normalize(kind: EngineType, form: EngineForm) -> Result<EngineForm, Error> {
    let what = kind.what();
    let id = form.id.trim().to_owned();
    if !valid_profile_id(&id) {
        return Err(invalid(format!(
            "the {what} id {id:?} must be 1-64 characters of a-z, 0-9, - and _ (and not \"none\")"
        )));
    }
    let kind_text = form.kind.trim();
    let kind_text = if kind_text.is_empty() {
        "openai_compatible"
    } else {
        kind_text
    };
    let known = match kind {
        EngineType::Stt => {
            serde_json::from_value::<EngineProfileKind>(serde_json::json!(kind_text)).is_ok()
        }
        EngineType::Llm => {
            serde_json::from_value::<LlmProfileKind>(serde_json::json!(kind_text)).is_ok()
        }
        EngineType::Tts => {
            serde_json::from_value::<TtsEngineKind>(serde_json::json!(kind_text)).is_ok()
        }
    };
    if !known {
        return Err(invalid(format!("{kind_text:?} is not a known {what} kind")));
    }
    let mut out = EngineForm {
        id,
        label: trimmed(form.label),
        kind: kind_text.to_owned(),
        base_url: trimmed(form.base_url).map(|u| u.trim_end_matches('/').to_owned()),
        model: trimmed(form.model),
        voice: trimmed(form.voice),
        speakers: form.speakers,
        format: trimmed(form.format).map(|f| f.to_ascii_lowercase()),
        credential: trimmed(form.credential),
        api_key_env: trimmed(form.api_key_env),
        project_id: trimmed(form.project_id),
        location: trimmed(form.location),
        api_key_header: trimmed(form.api_key_header),
        gateway_token_env: trimmed(form.gateway_token_env),
        allow_plain_http: form.allow_plain_http,
    };
    check_text("the label", out.label.as_deref(), MAX_LABEL_CHARS)?;
    check_text("the model", out.model.as_deref(), MAX_NAME_CHARS)?;
    check_text("the voice", out.voice.as_deref(), MAX_NAME_CHARS)?;
    check_text("the project id", out.project_id.as_deref(), MAX_NAME_CHARS)?;
    check_text("the location", out.location.as_deref(), MAX_NAME_CHARS)?;
    if let Some(url) = out.base_url.as_deref() {
        check_base_url(url).map_err(invalid)?;
    }
    if let Some(c) = out.credential.as_deref()
        && !valid_profile_id(c)
    {
        return Err(invalid(format!(
            "the key name {c:?} must be 1-64 characters of a-z, 0-9, - and _"
        )));
    }
    if out.credential.as_deref() == Some(out.id.as_str()) {
        out.credential = None;
    }
    if let Some(env) = out.api_key_env.as_deref() {
        check_env_name(env)?;
    }
    if let Some(env) = out.gateway_token_env.as_deref() {
        check_env_name(env)?;
    }
    if let Some(h) = out.api_key_header.as_deref() {
        vca_speech::auth::check_key_header(h).map_err(invalid)?;
    }
    if (out.api_key_header.is_some() || out.gateway_token_env.is_some())
        && !openai_like(kind, &out.kind)
    {
        return Err(invalid(format!(
            "a key header and a gateway token apply to OpenAI-compatible {what}s only"
        )));
    }
    if out.gateway_token_env.is_some() {
        check_gateway_form(kind, &out)?;
    }
    let needs_url = out.kind == "openai_compatible";
    if needs_url && out.base_url.is_none() {
        return Err(invalid(format!(
            "a local or self-hosted {what} needs a base URL, e.g. http://127.0.0.1:8000/v1"
        )));
    }
    match kind {
        EngineType::Stt => {
            out.voice = None;
            out.speakers.clear();
            out.format = None;
            if out.kind == "google" {
                if out.project_id.is_none() {
                    return Err(invalid("a Google Cloud speech engine needs a project id"));
                }
            } else {
                out.project_id = None;
                out.location = None;
            }
        }
        EngineType::Llm => {
            out.voice = None;
            out.speakers.clear();
            out.format = None;
            out.project_id = None;
            out.location = None;
        }
        EngineType::Tts => {
            out.label = None;
            out.project_id = None;
            out.location = None;
            if let Some(f) = out.format.as_deref()
                && vca_speech::TtsFormat::parse(f).is_none()
            {
                return Err(invalid(format!(
                    "the audio format {f:?} is not one of mp3, opus, aac, flac, wav"
                )));
            }
            if out.speakers.len() > MAX_SPEAKERS {
                return Err(invalid(format!("at most {MAX_SPEAKERS} voices per engine")));
            }
            for sp in &mut out.speakers {
                sp.id = sp.id.trim().to_owned();
                sp.label = sp.label.trim().to_owned();
            }
        }
    }
    Ok(out)
}

/// The table `[name]`, created (implicit) when missing.
fn section_mut<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table, Error> {
    let root = doc.as_table_mut();
    if !root.contains_key(name) {
        let mut t = Table::new();
        t.set_implicit(true);
        root.insert(name, Item::Table(t));
    }
    root.get_mut(name)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| {
            invalid(format!(
                "[{name}] in config.toml is written in a form the app cannot edit; edit it by hand"
            ))
        })
}

/// The array of tables `[[section.name]]`, created when missing.
fn array_mut<'a>(
    table: &'a mut Table,
    section: &str,
    name: &str,
) -> Result<&'a mut ArrayOfTables, Error> {
    if !table.contains_key(name) {
        table.insert(name, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    table
        .get_mut(name)
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| {
            invalid(format!(
                "{section}.{name} in config.toml is written in a form the app cannot edit \
                 (use [[{section}.{name}]] tables); edit it by hand"
            ))
        })
}

fn find(list: &ArrayOfTables, id_key: &str, id: &str) -> Option<usize> {
    list.iter()
        .position(|t| t.get(id_key).and_then(Item::as_str) == Some(id))
}

/// Sets (or removes) a string key, keeping the comments around an existing
/// value and leaving an equal value untouched.
fn set_str(table: &mut Table, key: &str, value: Option<&str>) {
    let Some(value) = value else {
        table.remove(key);
        return;
    };
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            if old.as_str() == Some(value) {
                return;
            }
            let decor = old.decor().clone();
            *old = Value::from(value);
            *old.decor_mut() = decor;
        }
        _ => {
            table.insert(key, toml_edit::value(value));
        }
    }
}

/// Writes the form's fields into one `[[...]]` table. Keys the app does not
/// edit (timeouts, per-language voices, disclosure, ...) are kept.
fn write_row(
    kind: EngineType,
    row: &mut Table,
    form: &EngineForm,
    original: Option<&str>,
    before: &Config,
) -> Result<(), Error> {
    let (_, _, id_key) = kind.location();
    set_str(row, id_key, Some(&form.id));
    if kind != EngineType::Tts {
        set_str(row, "label", form.label.as_deref());
    }
    // An absent kind means openai_compatible: keep it absent.
    if row.contains_key("kind") || form.kind != "openai_compatible" {
        set_str(row, "kind", Some(&form.kind));
    }
    set_str(row, "base_url", form.base_url.as_deref());
    set_str(row, "model", form.model.as_deref());
    set_str(row, "api_key_env", form.api_key_env.as_deref());
    set_str(row, "credential", form.credential.as_deref());
    set_str(row, "api_key_header", form.api_key_header.as_deref());
    set_str(row, "gateway_token_env", form.gateway_token_env.as_deref());
    if form.allow_plain_http {
        row.insert("allow_plain_http", toml_edit::value(true));
    } else {
        row.remove("allow_plain_http");
    }
    match kind {
        EngineType::Stt => {
            set_str(row, "project_id", form.project_id.as_deref());
            set_str(row, "location", form.location.as_deref());
        }
        EngineType::Llm => {}
        EngineType::Tts => {
            set_str(row, "voice", form.voice.as_deref());
            set_str(row, "format", form.format.as_deref());
            let old = original
                .and_then(|o| before.tts.engines.iter().find(|e| e.name == o))
                .map(|e| e.speakers.clone())
                .unwrap_or_default();
            if old != form.speakers || (form.speakers.is_empty() && row.contains_key("speakers")) {
                if form.speakers.is_empty() {
                    row.remove("speakers");
                } else {
                    let mut list = ArrayOfTables::new();
                    for sp in &form.speakers {
                        list.push(speaker_table(sp)?);
                    }
                    row.insert("speakers", Item::ArrayOfTables(list));
                }
            }
        }
    }
    Ok(())
}

/// A speaker as a TOML table, with every field the speaker type has.
fn speaker_table(speaker: &TtsSpeaker) -> Result<Table, Error> {
    let text = toml::to_string(speaker).map_err(|e| internal(e.to_string()))?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| internal(e.to_string()))?;
    let mut table = doc.as_table().clone();
    table.set_position(None);
    table.set_implicit(false);
    table.decor_mut().clear();
    Ok(table)
}

/// Sets the default to `id`, or removes the key (`None`).
fn set_default(doc: &mut DocumentMut, kind: EngineType, id: Option<&str>) -> Result<(), Error> {
    let (section, key) = kind.default_key();
    if id.is_none() && !doc.contains_key(section) {
        return Ok(());
    }
    let table = section_mut(doc, section)?;
    if id.is_some() {
        table.set_implicit(false);
    }
    set_str(table, key, id);
    Ok(())
}

/// Writes `text` to `path` through a temporary file in the same folder,
/// keeping the previous file as `<name>.bak`.
pub(crate) fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("the configuration path has no file name"))?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let mut bak_name = name.to_os_string();
    bak_name.push(".bak");
    let bak = dir.join(bak_name);
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
            std::fs::copy(path, &bak)?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Removes the key from an engine's error text and shortens it.
fn redact(message: &str, key: Option<&str>) -> String {
    let mut text = message.to_owned();
    if let Some(key) = key.filter(|k| k.len() >= 4) {
        text = text.replace(key, "[redacted]");
    }
    truncate_chars(&text, MAX_ERROR_CHARS, false)
}

/// One second of a quiet 440 Hz tone (16 kHz mono), the speech-to-text
/// test input. Generated, so nothing recorded is ever sent.
pub fn test_tone() -> vca_speech::AudioInput {
    const RATE: u32 = 16_000;
    let samples: Vec<i16> = (0..RATE)
        .map(|i| {
            let t = f64::from(i) / f64::from(RATE);
            // 20 ms fade in and out against clicks.
            let fade = (t / 0.02).min((1.0 - t) / 0.02).clamp(0.0, 1.0);
            let s = (2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.2 * fade;
            (s * f64::from(i16::MAX)) as i16
        })
        .collect();
    vca_speech::AudioInput::from_pcm16(&samples, RATE)
}

struct TestDone {
    message: String,
    chars: Option<usize>,
    audio_bytes: Option<usize>,
}

fn speech_err(err: vca_speech::SpeechError) -> Error {
    Error::api(err.code(), err.to_string())
}

fn no_key(what: &str, form: &EngineForm) -> Error {
    Error::api(
        ErrorCode::Unauthorized,
        format!(
            "{what} {:?} has no key yet: enter it and save, or set its environment variable",
            form.id
        ),
    )
}

async fn run_test(
    kind: EngineType,
    form: &EngineForm,
    cfg: &Config,
    key: Option<String>,
    auth: vca_speech::EngineAuth,
) -> Result<TestDone, Error> {
    use vca_speech::llm::{
        AntCliTokenProvider, AnthropicAuth, AnthropicClient, AnthropicConfig, ChatBackend,
        ChatClient, ChatConfig,
    };
    use vca_speech::stt::{
        GoogleAuth, GoogleSttConfig, GoogleSttEngine, OpenAiSttConfig, OpenAiSttEngine,
    };
    use vca_speech::tts::{OpenAiTtsConfig, OpenAiTtsEngine};
    use vca_speech::{SttEngine, SynthesisRequest, TranscribeOptions, TtsEngine, TtsFormat};

    let model = form.model.clone();
    match kind {
        EngineType::Stt => {
            let engine: Box<dyn SttEngine> = match form.kind.as_str() {
                "google" => {
                    let auth = match key {
                        Some(key) => GoogleAuth::ApiKey { key },
                        None => {
                            let token = cfg
                                .speech
                                .engine_profiles()
                                .iter()
                                .find(|p| p.id == form.id)
                                .and_then(|p| env_secret(p.access_token_env.as_deref()));
                            match token {
                                Some(token) => GoogleAuth::Bearer { token },
                                None => return Err(no_key("the speech engine", form)),
                            }
                        }
                    };
                    let mut gc =
                        GoogleSttConfig::new(form.project_id.clone().unwrap_or_default(), auth);
                    if let Some(l) = &form.location {
                        gc.location = l.clone();
                    }
                    if let Some(m) = model {
                        gc.model = m;
                    }
                    if let Some(url) = &form.base_url {
                        gc.base_url = url.clone();
                    }
                    gc.timeout_secs = TEST_TIMEOUT.as_secs();
                    Box::new(GoogleSttEngine::new(gc).map_err(speech_err)?)
                }
                other => {
                    if other == "openai" && key.is_none() {
                        return Err(no_key("the speech engine", form));
                    }
                    Box::new(
                        OpenAiSttEngine::new(OpenAiSttConfig {
                            base_url: form.base_url.clone().unwrap_or_else(|| OPENAI_API.into()),
                            api_key: key,
                            model: model.unwrap_or_else(|| DEFAULT_STT_MODEL.into()),
                            timeout: TEST_TIMEOUT,
                            auth,
                            ..OpenAiSttConfig::default()
                        })
                        .map_err(speech_err)?,
                    )
                }
            };
            let transcript = engine
                .transcribe(&test_tone(), &TranscribeOptions::default())
                .await
                .map_err(speech_err)?;
            let chars = transcript.text.trim().chars().count();
            Ok(TestDone {
                message: format!("the speech engine answered ({chars} characters for a tone)"),
                chars: Some(chars),
                audio_bytes: None,
            })
        }
        EngineType::Llm => {
            let backend: Box<dyn ChatBackend> = match form.kind.as_str() {
                "anthropic" => {
                    let auth = match key {
                        Some(key) => AnthropicAuth::ApiKey(key),
                        None => {
                            let ant = cfg
                                .speech
                                .llm_profiles()
                                .iter()
                                .find(|p| p.id == form.id)
                                .and_then(|p| p.ant_profile.clone())
                                .filter(|p| !p.trim().is_empty());
                            match ant {
                                Some(profile) => AnthropicAuth::Token(Arc::new(
                                    AntCliTokenProvider::new(Some(profile)),
                                )),
                                None => return Err(no_key("the LLM", form)),
                            }
                        }
                    };
                    let mut ac = AnthropicConfig::new(auth, TEST_TIMEOUT);
                    if let Some(url) = &form.base_url {
                        ac.base_url = url.clone();
                    }
                    if let Some(m) = model {
                        ac.model = m;
                    }
                    ac.max_tokens = 16;
                    Box::new(AnthropicClient::new(ac).map_err(speech_err)?)
                }
                _ => Box::new(
                    ChatClient::new(ChatConfig {
                        base_url: form.base_url.clone().unwrap_or_else(|| OPENAI_API.into()),
                        api_key: key,
                        model: model.unwrap_or_else(|| DEFAULT_CHAT_MODEL.into()),
                        timeout: TEST_TIMEOUT,
                        auth,
                    })
                    .map_err(speech_err)?,
                ),
            };
            let reply = backend
                .complete(TEST_SYSTEM, TEST_USER)
                .await
                .map_err(speech_err)?;
            let chars = reply.trim().chars().count();
            Ok(TestDone {
                message: format!("the LLM answered ({chars} characters)"),
                chars: Some(chars),
                audio_bytes: None,
            })
        }
        EngineType::Tts => {
            let format = form
                .format
                .as_deref()
                .and_then(TtsFormat::parse)
                .unwrap_or(TtsFormat::Mp3);
            let voice = form
                .voice
                .clone()
                .or_else(|| form.speakers.first().map(|s| s.id.clone()))
                .unwrap_or_else(|| DEFAULT_TTS_VOICE.into());
            let engine = OpenAiTtsEngine::new(OpenAiTtsConfig {
                base_url: form.base_url.clone().unwrap_or_default(),
                api_key: key,
                model: model.unwrap_or_else(|| DEFAULT_TTS_MODEL.into()),
                voice,
                format,
                speed: None,
                timeout: TEST_TIMEOUT,
                max_audio_bytes: TEST_MAX_AUDIO,
                auth,
            })
            .map_err(speech_err)?;
            let out = engine
                .synthesize(&SynthesisRequest {
                    text: TEST_PHRASE.into(),
                    voice: None,
                    speed: None,
                    language: None,
                })
                .await
                .map_err(speech_err)?;
            if out.audio.is_empty() {
                return Err(Error::api(
                    ErrorCode::Unavailable,
                    "the TTS engine answered without audio",
                ));
            }
            let bytes = out.audio.len();
            Ok(TestDone {
                message: format!(
                    "the TTS engine answered ({bytes} bytes of {})",
                    out.format.as_str()
                ),
                chars: None,
                audio_bytes: Some(bytes),
            })
        }
    }
}

/// A file or store change, run off the async runtime.
type SetupJob = Box<dyn FnOnce(&EngineAdmin) -> Result<EngineSetup, Error> + Send>;

/// Handles the engine requests of the local control channel. Anything but
/// the owner's local channel (a paired device, the development WebSocket)
/// is refused with `forbidden`.
pub async fn handle_local(
    api: &Api,
    client: &Client,
    req: LocalRequest,
    caller: &crate::security::Caller,
) -> LocalResponse {
    let id = req.id();
    if client.device.is_some() || client.transport != LOCAL_TRANSPORT {
        tracing::warn!(
            transport = client.transport,
            "refused an engine setup request that did not come from this PC"
        );
        return LocalResponse::error(id, ErrorCode::Forbidden, FORBIDDEN);
    }
    let Some(admin) = api.engines().cloned() else {
        return LocalResponse::error(
            id,
            ErrorCode::Unsupported,
            "engine setup is not available on this server",
        );
    };
    let fail = |err: Error| LocalResponse::error(id, err.code(), error_text(&err));
    let blocking = |f: SetupJob| {
        let admin = Arc::clone(&admin);
        async move {
            match tokio::task::spawn_blocking(move || f(&admin)).await {
                Ok(Ok(setup)) => LocalResponse::Engines {
                    id,
                    setup: Box::new(setup),
                },
                Ok(Err(err)) => fail(err),
                Err(err) => LocalResponse::error(id, ErrorCode::Internal, err.to_string()),
            }
        }
    };
    match req {
        LocalRequest::EnginesGet { .. } => blocking(Box::new(|a| a.setup())).await,
        LocalRequest::EngineSave {
            kind,
            original_id,
            profile,
            secret,
            make_default,
            gateway_token,
            clear_gateway_token,
            step_up,
            ..
        } => {
            match admin.save_moves_credentials(kind, &profile) {
                Ok(false) => {}
                Ok(true) => {
                    use crate::security::{GateTarget, GatedAction};
                    let url = profile.base_url.clone().unwrap_or_default();
                    // The host first: the rest of a long URL may be cut.
                    let host = reqwest::Url::parse(&url)
                        .ok()
                        .and_then(|u| u.host_str().map(str::to_owned))
                        .unwrap_or_default();
                    let url = format!("{host} ({url})");
                    let target = GateTarget::new(GatedAction::EngineUrl, &profile).with_detail(url);
                    if let Err(refused) =
                        crate::local::gate(api, id, &target, step_up.as_ref(), caller).await
                    {
                        return refused;
                    }
                }
                Err(err) => return fail(err),
            }
            let gateway = match (gateway_token, clear_gateway_token) {
                (Some(token), _) => GatewayChange::Set(token),
                (None, true) => GatewayChange::Clear,
                (None, false) => GatewayChange::Keep,
            };
            blocking(Box::new(move |a| {
                a.save_with_gateway(
                    kind,
                    original_id.as_deref(),
                    profile,
                    secret.as_ref(),
                    make_default,
                    gateway,
                )
            }))
            .await
        }
        LocalRequest::EngineGatewayToken {
            kind,
            profile_id,
            token,
            ..
        } => {
            blocking(Box::new(move |a| {
                a.set_gateway_token(kind, &profile_id, token.as_ref())
            }))
            .await
        }
        LocalRequest::EngineRemove {
            kind,
            profile_id,
            delete_key,
            ..
        } => blocking(Box::new(move |a| a.remove(kind, &profile_id, delete_key))).await,
        LocalRequest::EngineSetDefault {
            kind, profile_id, ..
        } => {
            blocking(Box::new(move |a| {
                a.set_default_profile(kind, profile_id.as_deref())
            }))
            .await
        }
        LocalRequest::EngineTest {
            kind,
            profile,
            profile_id,
            secret,
            gateway_token,
            ..
        } => match admin
            .test(
                kind,
                profile,
                profile_id.as_deref(),
                secret.as_ref(),
                gateway_token.as_ref(),
            )
            .await
        {
            Ok(result) => LocalResponse::EngineTest { id, result },
            Err(err) => fail(err),
        },
        _ => LocalResponse::error(id, ErrorCode::InvalidRequest, "unexpected request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = r#"# My VCA settings
server_name = "studio" # keep this comment

[remote]
enabled = false

[speech]
language = "en"
# The local Whisper server
default_engine = "whisper"

[[speech.engines]]
id = "whisper"   # trailing comment
label = "Whisper"
base_url = "http://127.0.0.1:8000/v1"
timeout_secs = 30

[tts]
requests_per_minute = 5

[[tts.engines]]
name = "melo"
base_url = "http://127.0.0.1:8880/v1"
disclosure = "Machine-generated voice"
[tts.engines.voices]
ko = "KR"

[shell]
enabled = false
"#;

    fn admin(dir: &Path, text: &str) -> EngineAdmin {
        let file = dir.join("config.toml");
        std::fs::write(&file, text).unwrap();
        let cfg = Config::from_toml_str(text).unwrap();
        EngineAdmin::new(file, dir.join("data"), &cfg)
    }

    fn form(id: &str, url: &str) -> EngineForm {
        EngineForm {
            id: id.into(),
            kind: "openai_compatible".into(),
            base_url: Some(url.into()),
            ..EngineForm::default()
        }
    }

    #[test]
    fn urls_are_checked() {
        for ok in [
            "http://127.0.0.1:8000/v1",
            "https://api.openai.com/v1",
            "http://localhost:11434/v1",
            "http://[::1]:8000",
        ] {
            assert!(check_base_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "ftp://x/v1",
            "127.0.0.1:8000",
            "http://",
            "http:///v1",
            "http://user:pw@host/v1",
            "http://host/v1?key=1",
            "http://host/v 1",
            "javascript:alert(1)",
        ] {
            assert!(check_base_url(bad).is_err(), "{bad}");
        }
        assert!(check_base_url(&format!("http://h/{}", "a".repeat(MAX_URL_CHARS))).is_err());
    }

    #[test]
    fn forms_are_normalized_and_validated() {
        let f = normalize(
            EngineType::Stt,
            EngineForm {
                id: " local ".into(),
                kind: String::new(),
                base_url: Some(" http://127.0.0.1:8000/v1/ ".into()),
                credential: Some("local".into()),
                voice: Some("x".into()),
                ..EngineForm::default()
            },
        )
        .unwrap();
        assert_eq!(f.id, "local");
        assert_eq!(f.kind, "openai_compatible");
        assert_eq!(f.base_url.as_deref(), Some("http://127.0.0.1:8000/v1"));
        assert_eq!(f.credential, None, "the id is the default key name");
        assert_eq!(f.voice, None, "speech engines have no voice");

        let bad = |kind: EngineType, edit: fn(&mut EngineForm)| {
            let mut f = form("x", "http://127.0.0.1:1/v1");
            edit(&mut f);
            normalize(kind, f).unwrap_err().to_string()
        };
        assert!(bad(EngineType::Stt, |f| f.id = "Bad Id".into()).contains("id"));
        assert!(bad(EngineType::Stt, |f| f.id = "none".into()).contains("id"));
        assert!(bad(EngineType::Stt, |f| f.kind = "whisper.cpp".into()).contains("kind"));
        assert!(bad(EngineType::Tts, |f| f.kind = "openai".into()).contains("kind"));
        assert!(bad(EngineType::Llm, |f| f.kind = "google".into()).contains("kind"));
        assert!(bad(EngineType::Stt, |f| f.base_url = None).contains("base URL"));
        assert!(bad(EngineType::Stt, |f| f.base_url = Some("file:///x".into())).contains("http"));
        assert!(bad(EngineType::Llm, |f| f.label = Some("a\nb".into())).contains("label"));
        assert!(bad(EngineType::Llm, |f| f.label = Some("x".repeat(65))).contains("label"));
        assert!(bad(EngineType::Llm, |f| f.credential = Some("A B".into())).contains("key name"));
        assert!(
            bad(EngineType::Llm, |f| f.api_key_env = Some("1X".into())).contains("environment")
        );
        assert!(bad(EngineType::Tts, |f| f.format = Some("pcm".into())).contains("format"));
        assert!(
            bad(EngineType::Stt, |f| {
                f.kind = "google".into();
                f.base_url = None;
            })
            .contains("project")
        );
        // Cloud kinds have a default API root.
        let openai = EngineForm {
            id: "openai-stt".into(),
            kind: "openai".into(),
            ..EngineForm::default()
        };
        assert!(normalize(EngineType::Stt, openai).is_ok());
    }

    #[test]
    fn saving_keeps_comments_and_unrelated_sections_and_writes_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path(), ORIGINAL);
        let setup = a.setup().unwrap();
        assert!(!setup.restart_required);
        assert_eq!(setup.stt.len(), 1);
        assert!(setup.stt[0].default);
        assert_eq!(setup.tts[0].profile.id, "melo");

        let mut whisper = setup.stt[0].profile.clone();
        whisper.model = Some("Systran/faster-whisper-small".into());
        let setup = a
            .save(EngineType::Stt, Some("whisper"), whisper, None, false)
            .unwrap();
        assert!(setup.restart_required);
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        for kept in [
            "# My VCA settings",
            "server_name = \"studio\" # keep this comment",
            "# The local Whisper server",
            "id = \"whisper\"   # trailing comment",
            "timeout_secs = 30",
            "[shell]\nenabled = false",
            "disclosure = \"Machine-generated voice\"",
            "[tts.engines.voices]\nko = \"KR\"",
            "requests_per_minute = 5",
        ] {
            assert!(text.contains(kept), "lost {kept:?} in:\n{text}");
        }
        assert!(text.contains("model = \"Systran/faster-whisper-small\""));
        assert!(!text.contains("kind ="), "the default kind is not written");
        let bak = std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap();
        assert_eq!(bak, ORIGINAL);
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn add_rename_default_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path(), ORIGINAL);
        // A new LLM, made the default.
        let llm = EngineForm {
            label: Some("Ollama".into()),
            model: Some("qwen2.5:3b".into()),
            ..form("ollama", "http://127.0.0.1:11434/v1")
        };
        let setup = a.save(EngineType::Llm, None, llm, None, true).unwrap();
        assert_eq!(setup.llm.len(), 1);
        assert!(setup.llm[0].default);
        // The same id twice is refused.
        let err = a
            .save(
                EngineType::Llm,
                None,
                form("ollama", "http://127.0.0.1:1/v1"),
                None,
                false,
            )
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        // Renaming the default STT engine moves the default along.
        let setup = a
            .save(
                EngineType::Stt,
                Some("whisper"),
                form("fw", "http://127.0.0.1:8000/v1"),
                None,
                false,
            )
            .unwrap();
        assert_eq!(setup.stt[0].profile.id, "fw");
        assert!(setup.stt[0].default);
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(text.contains("default_engine = \"fw\""), "{text}");
        // A new TTS engine with voices.
        let tts = EngineForm {
            voice: Some("EN-US".into()),
            speakers: vec![TtsSpeaker {
                id: "EN-US".into(),
                label: "American English".into(),
                gender: vca_core::TtsGender::Female,
            }],
            ..form("supertonic", "http://127.0.0.1:8881/v1")
        };
        let setup = a.save(EngineType::Tts, None, tts, None, true).unwrap();
        assert_eq!(setup.tts.len(), 2);
        assert!(setup.tts[1].default);
        assert_eq!(setup.tts[1].profile.speakers.len(), 1);
        // Missing entries and legacy entries.
        let err = a
            .save(
                EngineType::Tts,
                Some("nope"),
                form("nope", "http://h/v1"),
                None,
                false,
            )
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotFound);
        // Removing the default LLM turns correction off.
        let setup = a.remove(EngineType::Llm, "ollama", true).unwrap();
        assert!(setup.llm.is_empty());
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(text.contains("default_llm = \"none\""), "{text}");
        // Removing the default TTS engine falls back to the first.
        let setup = a.remove(EngineType::Tts, "supertonic", false).unwrap();
        assert!(setup.tts[0].default);
        let cfg = Config::load(&dir.path().join("config.toml")).unwrap();
        cfg.speech.validate().unwrap();
        cfg.tts.validate().unwrap();
        assert_eq!(cfg.tts.default_engine, None);
        // Defaults can be set and cleared.
        let setup = a
            .set_default_profile(EngineType::Tts, Some("melo"))
            .unwrap();
        assert!(setup.tts[0].default);
        assert!(a.set_default_profile(EngineType::Tts, Some("x")).is_err());
    }

    #[test]
    fn a_missing_file_is_created_and_keys_are_stored_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cfg").join("config.toml");
        let a = EngineAdmin::new(file.clone(), dir.path().join("data"), &Config::default());
        assert!(a.setup().unwrap().stt.is_empty());
        let secret = Secret::new("sk-test-secret-123");
        let openai = EngineForm {
            id: "openai-stt".into(),
            kind: "openai".into(),
            model: Some("gpt-4o-transcribe".into()),
            ..EngineForm::default()
        };
        let setup = a
            .save(EngineType::Stt, None, openai, Some(&secret), true)
            .unwrap();
        assert_eq!(setup.stt[0].key, KeySource::Stored);
        assert!(setup.stt[0].ready);
        assert!(setup.restart_required);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("sk-test-secret-123"));
        assert!(text.contains("[[speech.engines]]"), "{text}");
        assert!(!format!("{setup:?}").contains("sk-test"));
        assert!(!serde_json::to_string(&setup).unwrap().contains("sk-test"));
        assert!(!dir.path().join("cfg").join("config.toml.bak").exists());
        // Removing with delete_key clears the stored key.
        let setup = a.remove(EngineType::Stt, "openai-stt", true).unwrap();
        assert!(setup.stt.is_empty());
        assert_eq!(
            Secrets::in_data_dir(&dir.path().join("data")).get("openai-stt"),
            None
        );
        // A TTS key goes to the TTS store.
        let tts = form("local-tts", "http://127.0.0.1:8880/v1");
        let setup = a
            .save(
                EngineType::Tts,
                None,
                tts,
                Some(&Secret::new("tts-key")),
                false,
            )
            .unwrap();
        assert_eq!(setup.tts[0].key, KeySource::Stored);
        let bad = Secret::new("two\nlines");
        assert!(
            a.save(
                EngineType::Tts,
                Some("local-tts"),
                form("local-tts", "http://127.0.0.1:8880/v1"),
                Some(&bad),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_changes_and_files_are_refused_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path(), ORIGINAL);
        // A legacy id cannot be edited here.
        let legacy = "[speech]\nengine = \"openai\"\n";
        std::fs::create_dir_all(dir.path().join("b")).unwrap();
        let b = admin(&dir.path().join("b"), legacy);
        let setup = b.setup().unwrap();
        assert!(setup.stt[0].read_only);
        assert!(
            b.save(
                EngineType::Stt,
                Some("openai"),
                form("openai", "http://h/v1"),
                None,
                false
            )
            .is_err()
        );
        assert!(b.remove(EngineType::Stt, "openai", false).is_err());
        // A broken file is never overwritten.
        std::fs::write(dir.path().join("config.toml"), "[speech\n").unwrap();
        let err = a
            .save(EngineType::Stt, None, form("x", "http://h/v1"), None, false)
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
            "[speech\n"
        );
    }

    #[test]
    fn errors_are_redacted_and_short() {
        let msg = redact("bad key sk-abcdef1234 for you", Some("sk-abcdef1234"));
        assert_eq!(msg, "bad key [redacted] for you");
        assert!(redact(&"x".repeat(1000), None).chars().count() <= MAX_ERROR_CHARS);
    }

    #[test]
    fn the_test_tone_is_one_second_of_audio() {
        let tone = test_tone();
        assert_eq!(tone.sample_rate, 16_000);
        assert_eq!(tone.bytes.len(), 32_000);
        let wav = tone.to_uploadable().unwrap();
        assert_eq!(wav.format, vca_speech::AudioFormat::Wav);
    }

    #[test]
    fn key_headers_and_gateway_variables_are_checked() {
        let team = |header: Option<&str>, env: Option<&str>| EngineForm {
            api_key_header: header.map(str::to_owned),
            gateway_token_env: env.map(str::to_owned),
            ..form("team", "http://127.0.0.1:8000/v1")
        };
        let ok = normalize(EngineType::Tts, team(Some(" X-VCA-Key "), Some("TEAM_GW"))).unwrap();
        assert_eq!(ok.api_key_header.as_deref(), Some("X-VCA-Key"));
        assert!(normalize(EngineType::Stt, team(Some("X-VCA-Key"), None)).is_ok());
        for bad in [
            team(Some("X VCA"), None),
            team(Some("Content-Type"), None),
            team(None, Some("TEAM_GW")),
            team(Some("authorization"), Some("TEAM_GW")),
            team(Some("X-VCA-Key"), Some("1BAD")),
        ] {
            assert!(normalize(EngineType::Llm, bad.clone()).is_err(), "{bad:?}");
        }
        let anthropic = EngineForm {
            kind: "anthropic".into(),
            ..team(Some("X-VCA-Key"), None)
        };
        let err = error_text(&normalize(EngineType::Llm, anthropic).unwrap_err());
        assert!(err.contains("OpenAI-compatible"), "{err}");
    }

    #[test]
    fn removing_a_profile_deletes_its_gateway_token() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path(), ORIGINAL);
        let team = EngineForm {
            api_key_header: Some("X-VCA-Key".into()),
            ..form("team", "http://127.0.0.1:8000/v1")
        };
        let setup = a
            .save_with_gateway(
                EngineType::Tts,
                None,
                team,
                None,
                false,
                GatewayChange::Set(Secret::new("gw-tts-token")),
            )
            .unwrap();
        let entry = setup.tts.iter().find(|e| e.profile.id == "team").unwrap();
        assert_eq!(entry.gateway, KeySource::Stored);
        let store = Secrets::tts_in_data_dir(&dir.path().join("data"));
        assert_eq!(store.gateway("team").as_deref(), Some("gw-tts-token"));
        a.remove(EngineType::Tts, "team", true).unwrap();
        assert!(store.gateway("team").is_none());
    }
}
