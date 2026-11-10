//! Speech engine and correction LLM profiles: their credentials (local
//! store, `vca speech login`), per-device preferences, and the clients built
//! from them on demand (rebuilt when a credential changes).
//!
//! Secrets come from the environment variable a profile names, else from
//! the owner-only credential store below the data directory. They are never
//! read from `config.toml`, never sent to clients, and can only be written
//! over the local control channel. Each store has a second part for gateway
//! tokens (`<data>/speech/gateway`, `<data>/tts/gateway`): the bearer token
//! of a gateway or reverse proxy in front of an engine, under the same
//! credential name as the engine's key. Both are read on every use, so a
//! replaced key or token applies without restarting vcad.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vca_core::error::ErrorCode;
use vca_core::{Error, SpeechProfileInfo};
use vca_net::KeyStore;
use vca_speech::llm::{
    AntCliTokenProvider, AnthropicAuth, AnthropicClient, AnthropicConfig, ChatBackend, ChatClient,
    ChatConfig, ContextCorrector, TokenProvider,
};
use vca_speech::stt::{
    GoogleAuth, GoogleSttConfig, GoogleSttEngine, OpenAiSttConfig, OpenAiSttEngine,
};
use vca_speech::{EngineAuth, SttEngine};

use crate::config::{EngineProfileKind, LlmProfile, LlmProfileKind, SpeechEngineProfile};
use crate::util::lock;

const OPENAI_API: &str = "https://api.openai.com/v1";
const DEFAULT_STT_MODEL: &str = "whisper-1";
const DEFAULT_CHAT_MODEL: &str = "gpt-4o-mini";
const PREFS_FILE: &str = "preferences.json";

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

/// Reads the secret named by `var`, if configured and set.
pub(crate) fn env_secret(var: Option<&str>) -> Option<String> {
    let var = var.filter(|v| !v.is_empty())?;
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// One owner-only folder of secrets, one file per credential name.
struct SecretDir(vca_net::FileKeyStore);

impl SecretDir {
    fn get(&self, name: &str) -> Option<String> {
        match self.0.load(name) {
            Ok(Some(bytes)) => std::str::from_utf8(&bytes)
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            Ok(None) => None,
            Err(err) => {
                // Storage errors carry paths and reasons, never key material.
                tracing::warn!(%err, "cannot read an engine credential");
                None
            }
        }
    }

    fn put(&self, name: &str, secret: &str) -> Result<(), Error> {
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(api_error(ErrorCode::InvalidRequest, "empty credential"));
        }
        self.0
            .save(name, secret.as_bytes())
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))
    }

    fn delete(&self, name: &str) -> Result<bool, Error> {
        let existed = self.get(name).is_some();
        self.0
            .delete(name)
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
        Ok(existed)
    }
}

/// The local credential store of speech profiles (`<data>/speech/keys`
/// and `<data>/speech/gateway`) or of TTS engines (`<data>/tts/...`).
pub(crate) struct Secrets {
    keys: SecretDir,
    gateway: SecretDir,
}

impl Secrets {
    fn in_dir(dir: &Path) -> Self {
        Self {
            keys: SecretDir(vca_net::FileKeyStore::new(dir.join("keys"))),
            gateway: SecretDir(vca_net::FileKeyStore::new(dir.join("gateway"))),
        }
    }

    /// Speech engines and correction LLMs.
    pub(crate) fn in_data_dir(data_dir: &Path) -> Self {
        Self::in_dir(&data_dir.join("speech"))
    }

    /// TTS engines.
    pub(crate) fn tts_in_data_dir(data_dir: &Path) -> Self {
        Self::in_dir(&data_dir.join("tts"))
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        self.keys.get(name)
    }

    pub(crate) fn put(&self, name: &str, secret: &str) -> Result<(), Error> {
        self.keys.put(name, secret)
    }

    pub(crate) fn delete(&self, name: &str) -> Result<bool, Error> {
        self.keys.delete(name)
    }

    /// The stored gateway token of credential `name`.
    pub(crate) fn gateway(&self, name: &str) -> Option<String> {
        self.gateway.get(name)
    }

    pub(crate) fn put_gateway(&self, name: &str, token: &str) -> Result<(), Error> {
        self.gateway.put(name, token)
    }

    pub(crate) fn delete_gateway(&self, name: &str) -> Result<bool, Error> {
        self.gateway.delete(name)
    }
}

/// A key from the profile's environment variable, else the store.
fn lookup(env: Option<&str>, name: &str, secrets: &Secrets) -> Option<String> {
    env_secret(env).or_else(|| secrets.get(name))
}

/// A gateway token from the profile's environment variable, else the store.
pub(crate) fn lookup_gateway(env: Option<&str>, name: &str, secrets: &Secrets) -> Option<String> {
    env_secret(env).or_else(|| secrets.gateway(name))
}

/// Where requests carry the key, and the gateway token.
pub(crate) fn engine_auth(api_key_header: Option<&str>, gateway: Option<String>) -> EngineAuth {
    EngineAuth {
        key_header: nonempty(api_key_header).map(str::to_owned),
        gateway_token: gateway,
    }
}

/// The secrets a client is built with: the API key and the gateway token.
/// A client is rebuilt when either changes.
pub(crate) type SecretPair = (Option<String>, Option<String>);

/// A client that cannot be built; a key header conflict names the profile.
fn auth_error(what: &str, id: &str, err: vca_speech::SpeechError) -> Error {
    match err {
        vca_speech::SpeechError::Config(message) => api_error(
            ErrorCode::InvalidRequest,
            format!("{what} {id:?}: {message}"),
        ),
        other => api_error(ErrorCode::Internal, other.to_string()),
    }
}

fn missing_credential(what: &str, id: &str, name: &str) -> Error {
    api_error(
        ErrorCode::Unauthorized,
        format!(
            "{what} profile {id:?} has no credential: run `vca speech login {name}` on the PC \
             or set its api_key_env"
        ),
    )
}

type EngineCache = Option<(SecretPair, Arc<dyn SttEngine>)>;

/// One engine profile and its client.
pub(crate) struct EngineSlot {
    pub(crate) profile: SpeechEngineProfile,
    /// An injected engine (tests); used instead of building one.
    fixed: Option<Arc<dyn SttEngine>>,
    cache: Mutex<EngineCache>,
}

impl EngineSlot {
    pub(crate) fn new(profile: SpeechEngineProfile) -> Self {
        Self {
            profile,
            fixed: None,
            cache: Mutex::new(None),
        }
    }

    pub(crate) fn fixed(profile: SpeechEngineProfile, engine: Arc<dyn SttEngine>) -> Self {
        Self {
            profile,
            fixed: Some(engine),
            cache: Mutex::new(None),
        }
    }

    pub(crate) fn credential_name(&self) -> &str {
        self.profile
            .credential
            .as_deref()
            .unwrap_or(&self.profile.id)
    }

    fn key(&self, secrets: &Secrets) -> Option<String> {
        lookup(
            self.profile.api_key_env.as_deref(),
            self.credential_name(),
            secrets,
        )
    }

    fn gateway(&self, secrets: &Secrets) -> Option<String> {
        lookup_gateway(
            self.profile.gateway_token_env.as_deref(),
            self.credential_name(),
            secrets,
        )
    }

    /// A credential is available, or none is needed.
    pub(crate) fn configured(&self, secrets: &Secrets) -> bool {
        if self.fixed.is_some() {
            return true;
        }
        let p = &self.profile;
        match p.kind {
            EngineProfileKind::OpenaiCompatible => true,
            EngineProfileKind::Openai => self.key(secrets).is_some(),
            EngineProfileKind::Google => {
                nonempty(p.project_id.as_deref()).is_some()
                    && (self.key(secrets).is_some()
                        || env_secret(p.access_token_env.as_deref()).is_some())
            }
        }
    }

    pub(crate) fn info(&self, secrets: &Secrets, default: bool) -> SpeechProfileInfo {
        SpeechProfileInfo {
            id: self.profile.id.clone(),
            label: self
                .profile
                .label
                .clone()
                .unwrap_or_else(|| self.profile.id.clone()),
            kind: self.profile.kind.as_str().to_owned(),
            configured: self.configured(secrets),
            default,
        }
    }

    /// The engine, built (or rebuilt after a credential change) on demand.
    pub(crate) fn engine(&self, secrets: &Secrets) -> Result<Arc<dyn SttEngine>, Error> {
        if let Some(e) = &self.fixed {
            return Ok(Arc::clone(e));
        }
        let pair = (self.key(secrets), self.gateway(secrets));
        if let Some((k, engine)) = lock(&self.cache).as_ref()
            && *k == pair
        {
            return Ok(Arc::clone(engine));
        }
        let engine = self.build(pair.0.clone(), pair.1.clone())?;
        *lock(&self.cache) = Some((pair, Arc::clone(&engine)));
        Ok(engine)
    }

    fn build(
        &self,
        key: Option<String>,
        gateway: Option<String>,
    ) -> Result<Arc<dyn SttEngine>, Error> {
        let p = &self.profile;
        // Also for a profile saved before the rule.
        if key.is_some() || gateway.is_some() {
            if let Some(url) = nonempty(p.base_url.as_deref()) {
                vca_speech::auth::check_key_url(url, p.allow_plain_http)
                    .map_err(|e| api_error(ErrorCode::InvalidRequest, e.to_string()))?;
            }
        }
        let build_err = |e: vca_speech::SpeechError| api_error(ErrorCode::Internal, e.to_string());
        let model = nonempty(p.model.as_deref()).map(str::to_owned);
        Ok(match p.kind {
            EngineProfileKind::OpenaiCompatible | EngineProfileKind::Openai => {
                if p.kind == EngineProfileKind::Openai && key.is_none() {
                    return Err(missing_credential(
                        "speech engine",
                        &p.id,
                        self.credential_name(),
                    ));
                }
                let base_url = nonempty(p.base_url.as_deref()).unwrap_or(OPENAI_API);
                Arc::new(
                    OpenAiSttEngine::new(OpenAiSttConfig {
                        base_url: base_url.to_owned(),
                        api_key: key,
                        model: model.unwrap_or_else(|| DEFAULT_STT_MODEL.into()),
                        timeout: Duration::from_secs(p.timeout_secs.unwrap_or(60).max(1)),
                        auth: engine_auth(p.api_key_header.as_deref(), gateway),
                        ..OpenAiSttConfig::default()
                    })
                    .map_err(|e| auth_error("speech engine", &p.id, e))?,
                )
            }
            EngineProfileKind::Google => {
                let Some(project) = nonempty(p.project_id.as_deref()) else {
                    return Err(api_error(
                        ErrorCode::InvalidRequest,
                        format!("speech engine profile {:?} needs a project_id", p.id),
                    ));
                };
                let auth = if let Some(key) = key {
                    GoogleAuth::ApiKey { key }
                } else if let Some(token) = env_secret(p.access_token_env.as_deref()) {
                    GoogleAuth::Bearer { token }
                } else {
                    return Err(missing_credential(
                        "speech engine",
                        &p.id,
                        self.credential_name(),
                    ));
                };
                let mut gc = GoogleSttConfig::new(project.to_owned(), auth);
                if let Some(l) = nonempty(p.location.as_deref()) {
                    gc.location = l.to_owned();
                }
                if let Some(m) = model {
                    gc.model = m;
                }
                if let Some(url) = nonempty(p.base_url.as_deref()) {
                    gc.base_url = url.to_owned();
                }
                Arc::new(GoogleSttEngine::new(gc).map_err(build_err)?)
            }
        })
    }
}

/// One correction LLM profile and its client.
pub(crate) struct LlmSlot {
    pub(crate) profile: LlmProfile,
    pub(crate) timeout: Duration,
    /// `ant` token provider, created once so its token cache is shared.
    tokens: Mutex<Option<Arc<dyn TokenProvider>>>,
    cache: Mutex<Option<(SecretPair, ContextCorrector)>>,
}

impl LlmSlot {
    pub(crate) fn new(profile: LlmProfile, default_timeout: Duration) -> Self {
        let timeout = profile
            .timeout_ms
            .map(|ms| Duration::from_millis(ms.max(1)))
            .unwrap_or(default_timeout);
        Self {
            profile,
            timeout,
            tokens: Mutex::new(None),
            cache: Mutex::new(None),
        }
    }

    pub(crate) fn credential_name(&self) -> &str {
        self.profile
            .credential
            .as_deref()
            .unwrap_or(&self.profile.id)
    }

    fn key(&self, secrets: &Secrets) -> Option<String> {
        lookup(
            self.profile.api_key_env.as_deref(),
            self.credential_name(),
            secrets,
        )
    }

    fn gateway(&self, secrets: &Secrets) -> Option<String> {
        lookup_gateway(
            self.profile.gateway_token_env.as_deref(),
            self.credential_name(),
            secrets,
        )
    }

    fn ant_profile(&self) -> Option<&str> {
        nonempty(self.profile.ant_profile.as_deref())
    }

    /// Replaces the `ant` token provider (tests inject a fake one).
    pub(crate) fn set_token_provider(&self, provider: Arc<dyn TokenProvider>) {
        *lock(&self.tokens) = Some(provider);
        *lock(&self.cache) = None;
    }

    pub(crate) fn configured(&self, secrets: &Secrets) -> bool {
        match self.profile.kind {
            // Keyless self-hosted servers are fine; a named variable must be set.
            LlmProfileKind::OpenaiCompatible => {
                nonempty(self.profile.api_key_env.as_deref()).is_none()
                    || self.key(secrets).is_some()
            }
            LlmProfileKind::Anthropic => {
                self.key(secrets).is_some()
                    || self.ant_profile().is_some()
                    || lock(&self.tokens).is_some()
            }
        }
    }

    pub(crate) fn info(&self, secrets: &Secrets, default: bool) -> SpeechProfileInfo {
        SpeechProfileInfo {
            id: self.profile.id.clone(),
            label: self
                .profile
                .label
                .clone()
                .unwrap_or_else(|| self.profile.id.clone()),
            kind: self.profile.kind.as_str().to_owned(),
            configured: self.configured(secrets),
            default,
        }
    }

    /// The corrector, or `None` when the profile cannot authenticate (the
    /// pass is then skipped: fail-open).
    pub(crate) fn corrector(&self, secrets: &Secrets) -> Option<ContextCorrector> {
        let pair = (self.key(secrets), self.gateway(secrets));
        if let Some((k, c)) = lock(&self.cache).as_ref()
            && *k == pair
        {
            return Some(c.clone());
        }
        match self.build(pair.0.clone(), pair.1.clone()) {
            Ok(c) => {
                *lock(&self.cache) = Some((pair, c.clone()));
                Some(c)
            }
            Err(err) => {
                tracing::debug!(profile = %self.profile.id, "correction LLM unavailable: {err}");
                None
            }
        }
    }

    fn build(
        &self,
        key: Option<String>,
        gateway: Option<String>,
    ) -> Result<ContextCorrector, String> {
        let p = &self.profile;
        if key.is_some() || gateway.is_some() {
            if let Some(url) = nonempty(p.base_url.as_deref()) {
                vca_speech::auth::check_key_url(url, p.allow_plain_http)
                    .map_err(|e| e.to_string())?;
            }
        }
        let model = nonempty(p.model.as_deref()).map(str::to_owned);
        let backend: Arc<dyn ChatBackend> = match p.kind {
            LlmProfileKind::OpenaiCompatible => Arc::new(
                ChatClient::new(ChatConfig {
                    base_url: nonempty(p.base_url.as_deref())
                        .unwrap_or(OPENAI_API)
                        .to_owned(),
                    api_key: key,
                    model: model.unwrap_or_else(|| DEFAULT_CHAT_MODEL.into()),
                    timeout: self.timeout,
                    auth: engine_auth(p.api_key_header.as_deref(), gateway),
                })
                .map_err(|e| e.to_string())?,
            ),
            LlmProfileKind::Anthropic => {
                let auth = if let Some(key) = key {
                    AnthropicAuth::ApiKey(key)
                } else {
                    let mut tokens = lock(&self.tokens);
                    if tokens.is_none() && self.ant_profile().is_some() {
                        *tokens = Some(Arc::new(AntCliTokenProvider::new(
                            self.ant_profile().map(str::to_owned),
                        )));
                    }
                    match tokens.as_ref() {
                        Some(t) => AnthropicAuth::Token(Arc::clone(t)),
                        None => return Err("no API key and no ant_profile".into()),
                    }
                };
                let mut cfg = AnthropicConfig::new(auth, self.timeout);
                if let Some(url) = nonempty(p.base_url.as_deref()) {
                    cfg.base_url = url.to_owned();
                }
                if let Some(m) = model {
                    cfg.model = m;
                }
                Arc::new(AnthropicClient::new(cfg).map_err(|e| e.to_string())?)
            }
        };
        Ok(ContextCorrector::new(backend))
    }
}

/// A device's choice. `None` follows the server default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DevicePref {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) llm: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PrefsFile {
    #[serde(default)]
    devices: HashMap<String, DevicePref>,
}

/// Per-device preferences in `<data>/speech/preferences.json`.
pub(crate) struct Prefs {
    path: PathBuf,
    devices: Mutex<HashMap<String, DevicePref>>,
}

impl Prefs {
    pub(crate) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("speech").join(PREFS_FILE);
        let devices = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<PrefsFile>(&text) {
                Ok(f) => f.devices,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "ignoring unreadable speech preferences");
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };
        Self {
            path,
            devices: Mutex::new(devices),
        }
    }

    pub(crate) fn get(&self, device: &str) -> DevicePref {
        lock(&self.devices).get(device).cloned().unwrap_or_default()
    }

    /// Drops a device's preferences (a revoked device); nothing is written
    /// when it had none.
    pub(crate) fn forget(&self, device: &str) -> Result<(), Error> {
        if !lock(&self.devices).contains_key(device) {
            return Ok(());
        }
        self.set(device, DevicePref::default())
    }

    pub(crate) fn set(&self, device: &str, pref: DevicePref) -> Result<(), Error> {
        let mut devices = lock(&self.devices);
        if pref == DevicePref::default() {
            devices.remove(device);
        } else {
            devices.insert(device.to_owned(), pref);
        }
        let file = PrefsFile {
            devices: devices.clone(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|e| api_error(ErrorCode::Internal, e.to_string()))?;
        let io = |e: std::io::Error| {
            api_error(
                ErrorCode::Internal,
                format!("saving {}: {e}", self.path.display()),
            )
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revoked_device_loses_its_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = Prefs::load(dir.path());
        let pref = DevicePref {
            engine: Some("whisper".into()),
            llm: None,
        };
        prefs.set("dev_a", pref.clone()).unwrap();
        prefs.set("dev_b", pref.clone()).unwrap();
        prefs.forget("dev_a").unwrap();
        prefs.forget("never-paired").unwrap();
        let again = Prefs::load(dir.path());
        assert_eq!(again.get("dev_a"), DevicePref::default());
        assert_eq!(again.get("dev_b"), pref);
    }

    #[test]
    fn preferences_persist_per_device() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = Prefs::load(dir.path());
        assert_eq!(prefs.get("dev_a"), DevicePref::default());
        let pref = DevicePref {
            engine: Some("whisper".into()),
            llm: Some("none".into()),
        };
        prefs.set("dev_a", pref.clone()).unwrap();
        let again = Prefs::load(dir.path());
        assert_eq!(again.get("dev_a"), pref);
        assert_eq!(again.get("dev_b"), DevicePref::default());
        again.set("dev_a", DevicePref::default()).unwrap();
        assert_eq!(Prefs::load(dir.path()).get("dev_a"), DevicePref::default());
    }

    #[test]
    fn secrets_round_trip_and_never_appear_in_profile_info() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::in_data_dir(dir.path());
        let slot = EngineSlot::new(SpeechEngineProfile {
            id: "cloud".into(),
            kind: EngineProfileKind::Openai,
            ..SpeechEngineProfile::default()
        });
        assert!(!slot.configured(&secrets));
        assert!(slot.engine(&secrets).is_err());
        secrets.put("cloud", " sk-test \n").unwrap();
        assert_eq!(secrets.get("cloud").as_deref(), Some("sk-test"));
        assert!(slot.configured(&secrets));
        assert!(slot.engine(&secrets).is_ok());
        let info = format!("{:?}", slot.info(&secrets, true));
        assert!(!info.contains("sk-test"));
        assert!(secrets.delete("cloud").unwrap());
        assert!(!secrets.delete("cloud").unwrap());
        assert!(secrets.put("cloud", "  ").is_err());
    }

    #[test]
    fn anthropic_profiles_are_configured_by_key_or_ant_profile() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::in_data_dir(dir.path());
        let mut profile = LlmProfile {
            id: "claude".into(),
            kind: LlmProfileKind::Anthropic,
            ..LlmProfile::default()
        };
        let slot = LlmSlot::new(profile.clone(), Duration::from_secs(4));
        assert!(!slot.configured(&secrets));
        assert!(slot.corrector(&secrets).is_none());
        secrets.put("claude", "sk-ant").unwrap();
        assert!(slot.configured(&secrets));
        assert!(slot.corrector(&secrets).is_some());
        profile.id = "claude-account".into();
        profile.ant_profile = Some("default".into());
        let slot = LlmSlot::new(profile, Duration::from_secs(4));
        assert!(slot.configured(&secrets));
        // Building does not run the CLI; tokens are fetched per request.
        assert!(slot.corrector(&secrets).is_some());
    }

    /// A profile saved before the rule: the key does not go out, and the
    /// error says why.
    #[test]
    fn an_old_profile_cannot_send_its_key_over_plain_http() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::in_data_dir(dir.path());
        secrets.put("lan", "sk-1").unwrap();
        let profile = |url: &str, allow_plain_http| SpeechEngineProfile {
            id: "lan".into(),
            base_url: Some(url.into()),
            allow_plain_http,
            ..SpeechEngineProfile::default()
        };
        let refused = |url, flag| {
            let slot = EngineSlot::new(profile(url, flag));
            format!("{:?}", slot.engine(&secrets).err())
        };
        assert!(refused("http://192.168.0.10/v1", false).contains("Allow plain http"));
        assert!(refused("http://203.0.113.9/v1", true).contains("public"));
        assert_eq!(refused("http://192.168.0.10/v1", true), "None");
        assert_eq!(refused("http://100.64.0.1/v1", false), "None");
        // Without a key nothing secret travels.
        let empty = Secrets::in_data_dir(&dir.path().join("empty"));
        let slot = EngineSlot::new(profile("http://203.0.113.9/v1", false));
        assert!(slot.engine(&empty).is_ok());
    }

    #[test]
    fn gateway_tokens_are_stored_apart_and_rebuild_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::in_data_dir(dir.path());
        let profile = SpeechEngineProfile {
            id: "team".into(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            api_key_header: Some("X-VCA-Key".into()),
            ..SpeechEngineProfile::default()
        };
        let slot = EngineSlot::new(profile.clone());
        let first = slot.engine(&secrets).unwrap();
        secrets.put_gateway("team", "gw-token-1").unwrap();
        assert_eq!(secrets.gateway("team").as_deref(), Some("gw-token-1"));
        assert!(secrets.get("team").is_none(), "keys and tokens are apart");
        let second = slot.engine(&secrets).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "a new token rebuilds the client"
        );
        assert!(Arc::ptr_eq(&second, &slot.engine(&secrets).unwrap()));
        assert!(
            dir.path()
                .join("speech")
                .join("gateway")
                .join("team.key")
                .exists()
        );

        // Without a key header the stored token is a configuration error
        // that names the profile, never the token.
        let plain = EngineSlot::new(SpeechEngineProfile {
            api_key_header: None,
            ..profile
        });
        let err = format!("{:?}", plain.engine(&secrets).err().unwrap());
        assert!(
            err.contains("api_key_header") && err.contains("team"),
            "{err}"
        );
        assert!(!err.contains("gw-token-1"), "{err}");
        let llm = LlmSlot::new(
            LlmProfile {
                id: "team".into(),
                base_url: Some("http://127.0.0.1:9/v1".into()),
                ..LlmProfile::default()
            },
            Duration::from_secs(1),
        );
        assert!(llm.corrector(&secrets).is_none(), "fail-open on a conflict");

        assert!(secrets.delete_gateway("team").unwrap());
        assert!(!secrets.delete_gateway("team").unwrap());
        assert!(plain.engine(&secrets).is_ok());
        assert!(llm.corrector(&secrets).is_some());
    }
}
