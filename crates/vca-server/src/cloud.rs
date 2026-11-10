//! Cloud coding agents (`vca-cloud`) as session cards (CLD-02).
//!
//! Accounts are configured in `[[cloud.accounts]]` with non-secret settings
//! only. The secret of an account comes from the environment variable
//! named by `credential_env` or from the server credential store
//! ([`FileCredentialStore`], below `<data dir>/cloud/`, owner-only files),
//! which only the local control channel can write (`vca cloud login`).
//! Secrets are never logged, written to the configuration or returned over
//! any API.
//!
//! `LaunchCloud` creates the remote session through the provider's adapter,
//! inserts a card and starts an [`ActivityPoller`] task owned by the
//! session. Cloud sessions never touch the resource broker or any local
//! process.
//!
//! ```toml
//! [cloud]
//! poll_min_ms = 5000
//! poll_max_ms = 120000
//!
//! [[cloud.accounts]]
//! provider = "cursor_cloud"
//! account = "work"                    # lowercase label, default "default"
//! credential_env = "CURSOR_API_KEY"   # optional; else `vca cloud login`
//!
//! [[cloud.accounts]]
//! provider = "claude_managed_agents"
//! agent_id = "agent_..."
//! environment_id = "env_..."
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use vca_cloud::providers::{
    ClaudeManagedAdapter, ClaudeRoutineAdapter, CodexCloudAdapter, CopilotAdapter, CursorAdapter,
    CursorAuth, DevinAdapter, JulesAdapter,
};
use vca_cloud::{
    ActivityPoller, Backoff, CloudAgentAdapter, CloudCapabilities, CloudLaunch, CloudSessionId,
    Credential, CredentialKey, CredentialStore, CredentialStoreError, PollUpdate, provider_slug,
};
use vca_core::error::ErrorCode;
use vca_core::{
    CloudLaunchRequest, CloudProvider, CloudProviderInfo, Error, MessageRole, SessionEvent,
    SessionInfo, SessionState, now_millis,
};
use vca_net::KeyStore;

pub use vca_cloud::Secret;

use crate::api::Api;
use crate::control::{LocalRequest, LocalResponse};
use crate::sessions::{Session, SessionManager};
use crate::util::{error_text, truncate_chars};

/// Account label used when the configuration gives none.
pub const DEFAULT_ACCOUNT: &str = "default";
/// Longest account label.
const MAX_ACCOUNT_LEN: usize = 48;
/// Characters of the last message shown on a card.
const PREVIEW_CHARS: usize = 240;
/// Poll updates buffered between the poller and the session.
const POLL_QUEUE: usize = 16;

/// Every cloud provider, in display order.
pub const PROVIDERS: [CloudProvider; 7] = [
    CloudProvider::Jules,
    CloudProvider::CursorCloud,
    CloudProvider::Devin,
    CloudProvider::CopilotCloud,
    CloudProvider::ClaudeManagedAgents,
    CloudProvider::ClaudeCodeRoutines,
    CloudProvider::CodexCloud,
];

/// Parses a provider from its short name (`jules`, `cursor`, `devin`,
/// `copilot`, `claude-managed`, `claude-routines`, `codex`) or its
/// protocol name (`cursor_cloud`, ...).
pub fn parse_provider(name: &str) -> Option<CloudProvider> {
    let name = name.trim().to_ascii_lowercase();
    PROVIDERS.into_iter().find(|p| {
        provider_slug(*p) == name
            || serde_json::to_value(p)
                .ok()
                .is_some_and(|v| v.as_str() == Some(name.as_str()))
    })
}

/// Whether the provider works without a stored secret (the Codex cloud
/// CLI uses the user's local sign-in).
fn needs_credential(provider: CloudProvider) -> bool {
    provider != CloudProvider::CodexCloud
}

/// Checks an account label: lowercase ASCII letters, digits, `-` and `_`.
pub fn validate_account(account: &str) -> Result<(), Error> {
    let ok = !account.is_empty()
        && account.len() <= MAX_ACCOUNT_LEN
        && account
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(Error::api(
            ErrorCode::InvalidRequest,
            format!(
                "invalid account label {account:?}: use 1-{MAX_ACCOUNT_LEN} lowercase letters, \
                 digits, '-' or '_'"
            ),
        ))
    }
}

fn default_account() -> String {
    DEFAULT_ACCOUNT.into()
}

/// How the Cursor API key is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorAuthConfig {
    #[default]
    Basic,
    Bearer,
}

/// One provider account (`[[cloud.accounts]]`). Holds no secrets: unknown
/// keys such as `api_key` are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudAccountConfig {
    pub provider: CloudProvider,
    /// Label distinguishing several accounts of one provider.
    #[serde(default = "default_account")]
    pub account: String,
    /// Environment variable holding the API key or token. When unset or
    /// empty, the credential store is used.
    #[serde(default)]
    pub credential_env: Option<String>,
    /// API base URL override (enterprise proxies, tests).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Claude Managed Agents: agent to run.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Claude Managed Agents: pinned agent version.
    #[serde(default)]
    pub agent_version: Option<u64>,
    /// Claude Managed Agents: environment; Codex cloud: `--env` id.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Claude Managed Agents: workspace.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Claude Code routines: routine to fire.
    #[serde(default)]
    pub routine_id: Option<String>,
    /// Devin: organization.
    #[serde(default)]
    pub org_id: Option<String>,
    /// Cursor: how the API key is sent.
    #[serde(default)]
    pub cursor_auth: Option<CursorAuthConfig>,
    /// Codex cloud: path of the `codex` executable.
    #[serde(default)]
    pub codex_program: Option<PathBuf>,
}

impl CloudAccountConfig {
    pub fn new(provider: CloudProvider, account: impl Into<String>) -> Self {
        Self {
            provider,
            account: account.into(),
            credential_env: None,
            base_url: None,
            agent_id: None,
            agent_version: None,
            environment_id: None,
            workspace_id: None,
            routine_id: None,
            org_id: None,
            cursor_auth: None,
            codex_program: None,
        }
    }

    pub fn key(&self) -> CredentialKey {
        CredentialKey::new(self.provider, self.account.clone())
    }

    fn label(&self) -> String {
        format!("{}/{}", provider_slug(self.provider), self.account)
    }

    /// Builds the adapter from the non-secret settings.
    pub fn adapter(&self) -> Result<Arc<dyn CloudAgentAdapter>, Error> {
        let missing = |field: &str| {
            Error::api(
                ErrorCode::InvalidRequest,
                format!("cloud account {} needs `{field}`", self.label()),
            )
        };
        let base = self.base_url.as_deref();
        let adapter: Arc<dyn CloudAgentAdapter> = match self.provider {
            CloudProvider::Jules => Arc::new(match base {
                Some(url) => JulesAdapter::with_base_url(url),
                None => JulesAdapter::new(),
            }),
            CloudProvider::CursorCloud => {
                let adapter = match base {
                    Some(url) => CursorAdapter::with_base_url(url),
                    None => CursorAdapter::new(),
                };
                Arc::new(
                    adapter.with_auth(match self.cursor_auth.unwrap_or_default() {
                        CursorAuthConfig::Basic => CursorAuth::Basic,
                        CursorAuthConfig::Bearer => CursorAuth::Bearer,
                    }),
                )
            }
            CloudProvider::Devin => {
                let org = self.org_id.clone().ok_or_else(|| missing("org_id"))?;
                Arc::new(match base {
                    Some(url) => DevinAdapter::with_base_url(url, org),
                    None => DevinAdapter::new(org),
                })
            }
            CloudProvider::CopilotCloud => Arc::new(match base {
                Some(url) => CopilotAdapter::with_base_url(url),
                None => CopilotAdapter::new(),
            }),
            CloudProvider::ClaudeManagedAgents => {
                let agent = self.agent_id.clone().ok_or_else(|| missing("agent_id"))?;
                let env = self
                    .environment_id
                    .clone()
                    .ok_or_else(|| missing("environment_id"))?;
                let mut adapter = ClaudeManagedAdapter::new(agent, env);
                if let Some(url) = base {
                    adapter = adapter.with_base_url(url);
                }
                if let Some(v) = self.agent_version {
                    adapter = adapter.with_agent_version(v);
                }
                if let Some(w) = &self.workspace_id {
                    adapter = adapter.with_workspace(w.clone());
                }
                Arc::new(adapter)
            }
            CloudProvider::ClaudeCodeRoutines => {
                let routine = self
                    .routine_id
                    .clone()
                    .ok_or_else(|| missing("routine_id"))?;
                let mut adapter = ClaudeRoutineAdapter::new(routine);
                if let Some(url) = base {
                    adapter = adapter.with_base_url(url);
                }
                Arc::new(adapter)
            }
            CloudProvider::CodexCloud => {
                let mut adapter = CodexCloudAdapter::new();
                if let Some(p) = &self.codex_program {
                    adapter = adapter.with_program(p.clone());
                }
                if let Some(env) = &self.environment_id {
                    adapter = adapter.with_environment(env.clone());
                }
                Arc::new(adapter)
            }
        };
        Ok(adapter)
    }
}

fn default_poll_min_ms() -> u64 {
    5_000
}

fn default_poll_max_ms() -> u64 {
    120_000
}

/// The `[cloud]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CloudConfig {
    /// Delay between activity polls while a session is active.
    #[serde(default = "default_poll_min_ms")]
    pub poll_min_ms: u64,
    /// Upper bound of the poll delay while a session is quiet.
    #[serde(default = "default_poll_max_ms")]
    pub poll_max_ms: u64,
    pub accounts: Vec<CloudAccountConfig>,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            poll_min_ms: default_poll_min_ms(),
            poll_max_ms: default_poll_max_ms(),
            accounts: Vec::new(),
        }
    }
}

impl CloudConfig {
    fn backoff(&self) -> Backoff {
        let min = Duration::from_millis(self.poll_min_ms.max(10));
        Backoff {
            min,
            max: Duration::from_millis(self.poll_max_ms).max(min),
            factor: 2.0,
        }
    }
}

/// The server credential store: one owner-only file per account below
/// `<data dir>/cloud/` (see [`vca_net::FileKeyStore`]: mode `0600` in a
/// `0700` directory on Unix; on Windows the files inherit the owner-only
/// ACL of the per-user data directory).
pub struct FileCredentialStore {
    keys: vca_net::FileKeyStore,
}

impl std::fmt::Debug for FileCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileCredentialStore")
            .field("dir", &self.keys.dir())
            .finish_non_exhaustive()
    }
}

impl FileCredentialStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            keys: vca_net::FileKeyStore::new(dir),
        }
    }

    /// The store below a data directory.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join("cloud"))
    }

    /// File name stem of a key: `<provider slug>_<account>`. Slugs contain
    /// no `_`, so the first one separates the parts.
    fn name(key: &CredentialKey) -> Result<String, CredentialStoreError> {
        validate_account(&key.account)
            .map_err(|_| CredentialStoreError::Unavailable("invalid account label".into()))?;
        Ok(format!("{}_{}", provider_slug(key.provider), key.account))
    }

    fn parse_name(name: &str) -> Option<CredentialKey> {
        let (slug, account) = name.split_once('_')?;
        let provider = PROVIDERS.into_iter().find(|p| provider_slug(*p) == slug)?;
        validate_account(account).ok()?;
        Some(CredentialKey::new(provider, account))
    }
}

fn store_err(e: vca_net::NetError) -> CredentialStoreError {
    // Storage errors carry paths and I/O reasons, never key material.
    CredentialStoreError::Unavailable(e.to_string())
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn get(&self, key: &CredentialKey) -> Result<Option<Credential>, CredentialStoreError> {
        let Some(bytes) = self.keys.load(&Self::name(key)?).map_err(store_err)? else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| CredentialStoreError::Unavailable("corrupt credential file".into()))?;
        Ok(Some(Credential::new(text)))
    }

    async fn put(&self, key: &CredentialKey, cred: Credential) -> Result<(), CredentialStoreError> {
        let secret = cred
            .secret()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CredentialStoreError::Unavailable("empty credential".into()))?;
        self.keys
            .save(&Self::name(key)?, secret.expose().as_bytes())
            .map_err(store_err)
    }

    async fn delete(&self, key: &CredentialKey) -> Result<bool, CredentialStoreError> {
        let name = Self::name(key)?;
        let existed = self.keys.load(&name).map_err(store_err)?.is_some();
        self.keys.delete(&name).map_err(store_err)?;
        Ok(existed)
    }

    async fn keys(&self) -> Result<Vec<CredentialKey>, CredentialStoreError> {
        let entries = match std::fs::read_dir(self.keys.dir()) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(CredentialStoreError::Unavailable(e.to_string())),
        };
        let mut out: Vec<CredentialKey> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                Self::parse_name(name.strip_suffix(".key")?)
            })
            .collect();
        out.sort_by(|a, b| {
            (provider_slug(a.provider), &a.account).cmp(&(provider_slug(b.provider), &b.account))
        });
        Ok(out)
    }
}

/// Serde helper for secrets on the owner-only local channel.
pub(crate) mod secret_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use vca_cloud::Secret;

    pub fn serialize<S: Serializer>(secret: &Secret, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(secret.expose())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        String::deserialize(d).map(Secret::new)
    }
}

/// What a cloud session card needs to reach the remote session.
pub(crate) struct CloudHandle {
    adapter: Arc<dyn CloudAgentAdapter>,
    credential: Credential,
    remote_id: CloudSessionId,
    capabilities: CloudCapabilities,
}

impl CloudHandle {
    /// Sends a follow-up message, when the provider supports it.
    pub(crate) async fn send_text(&self, text: &str) -> Result<(), Error> {
        if !self.capabilities.follow_up {
            return Err(Error::api(
                ErrorCode::Unsupported,
                "this cloud provider does not accept follow-up messages",
            ));
        }
        if text.trim().is_empty() {
            return Err(Error::api(ErrorCode::InvalidRequest, "empty message"));
        }
        self.adapter
            .send_message(&self.credential, &self.remote_id, text)
            .await
            .map_err(Error::from)
    }

    /// Stops the remote session, when the provider supports it.
    pub(crate) async fn stop(&self) -> Result<(), Error> {
        if !self.capabilities.stop {
            return Err(Error::api(
                ErrorCode::Unsupported,
                "this cloud provider cannot stop sessions",
            ));
        }
        self.adapter
            .stop(&self.credential, &self.remote_id)
            .await
            .map_err(Error::from)
    }
}

struct Account {
    cfg: CloudAccountConfig,
    adapter: Arc<dyn CloudAgentAdapter>,
}

/// Configured cloud accounts and their credentials.
pub struct Cloud {
    accounts: Vec<Account>,
    store: Arc<dyn CredentialStore>,
    backoff: Backoff,
}

impl std::fmt::Debug for Cloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let labels: Vec<String> = self.accounts.iter().map(|a| a.cfg.label()).collect();
        f.debug_struct("Cloud")
            .field("accounts", &labels)
            .finish_non_exhaustive()
    }
}

fn cloud_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

fn store_failure(e: CredentialStoreError) -> Error {
    cloud_error(ErrorCode::Unavailable, e.to_string())
}

impl Cloud {
    /// Builds the accounts of `cfg`. Fails on invalid labels, duplicate
    /// accounts or missing provider settings.
    pub fn new(cfg: &CloudConfig, store: Arc<dyn CredentialStore>) -> Result<Self, Error> {
        let mut accounts: Vec<Account> = Vec::new();
        for acct in &cfg.accounts {
            validate_account(&acct.account)?;
            if accounts.iter().any(|a| a.cfg.key() == acct.key()) {
                return Err(cloud_error(
                    ErrorCode::InvalidRequest,
                    format!("cloud account {} is configured twice", acct.label()),
                ));
            }
            accounts.push(Account {
                adapter: acct.adapter()?,
                cfg: acct.clone(),
            });
        }
        Ok(Self {
            accounts,
            store,
            backoff: cfg.backoff(),
        })
    }

    /// Uses the file credential store below `data_dir`.
    pub fn open(cfg: &CloudConfig, data_dir: &Path) -> Result<Self, Error> {
        Self::new(cfg, Arc::new(FileCredentialStore::in_data_dir(data_dir)))
    }

    fn account(&self, provider: CloudProvider, account: &str) -> Result<&Account, Error> {
        self.accounts
            .iter()
            .find(|a| a.cfg.provider == provider && a.cfg.account == account)
            .ok_or_else(|| {
                cloud_error(
                    ErrorCode::NotFound,
                    format!(
                        "no cloud account {}/{account}; add it to [[cloud.accounts]] first",
                        provider_slug(provider)
                    ),
                )
            })
    }

    /// The credential of an account: the configured environment variable,
    /// else the store. `None` when neither has one.
    async fn credential(&self, cfg: &CloudAccountConfig) -> Result<Option<Credential>, Error> {
        if let Some(var) = cfg.credential_env.as_deref().filter(|v| !v.is_empty()) {
            if let Some(value) = std::env::var(var).ok().filter(|v| !v.trim().is_empty()) {
                return Ok(Some(Credential::new(value.trim())));
            }
        }
        let stored = self.store.get(&cfg.key()).await.map_err(store_failure)?;
        if stored.is_some() {
            return Ok(stored);
        }
        Ok((!needs_credential(cfg.provider)).then(Credential::none))
    }

    /// One entry per configured account (CLD-07). Never includes secrets.
    pub async fn providers(&self) -> Vec<CloudProviderInfo> {
        let mut out = Vec::with_capacity(self.accounts.len());
        for a in &self.accounts {
            let caps = a.adapter.capabilities();
            let configured = match self.credential(&a.cfg).await {
                Ok(c) => c.is_some(),
                Err(err) => {
                    tracing::warn!(account = %a.cfg.label(), "cannot read the credential store: {err}");
                    false
                }
            };
            out.push(CloudProviderInfo {
                provider: a.cfg.provider,
                account: a.cfg.account.clone(),
                configured,
                create: caps.create,
                follow_up: caps.follow_up,
                stop: caps.stop,
                approve_plan: caps.approve_plan,
                status_only: caps.progress_is_status_only,
                unverified: caps.unverified,
            });
        }
        out
    }

    /// Stores the secret of a configured account (local channel only).
    pub async fn set_credential(
        &self,
        provider: CloudProvider,
        account: &str,
        secret: Secret,
    ) -> Result<(), Error> {
        validate_account(account)?;
        let acct = self.account(provider, account)?;
        if secret.expose().trim().is_empty() {
            return Err(cloud_error(ErrorCode::InvalidRequest, "empty credential"));
        }
        let secret = Secret::new(secret.expose().trim());
        self.store
            .put(&acct.cfg.key(), Credential::new(secret))
            .await
            .map_err(store_failure)?;
        tracing::info!(account = %acct.cfg.label(), "cloud credential stored");
        Ok(())
    }

    /// Deletes a stored secret. Returns whether one was stored.
    pub async fn delete_credential(
        &self,
        provider: CloudProvider,
        account: &str,
    ) -> Result<bool, Error> {
        validate_account(account)?;
        let removed = self
            .store
            .delete(&CredentialKey::new(provider, account))
            .await
            .map_err(store_failure)?;
        if removed {
            tracing::info!(account = %format!("{}/{account}", provider_slug(provider)), "cloud credential deleted");
        }
        Ok(removed)
    }

    /// Picks the account for a launch: the first configured account of the
    /// provider that has a credential.
    async fn launch_account(
        &self,
        provider: CloudProvider,
    ) -> Result<(&Account, Credential), Error> {
        let candidates: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| a.cfg.provider == provider)
            .collect();
        if candidates.is_empty() {
            return Err(cloud_error(
                ErrorCode::NotFound,
                format!(
                    "no {} account is configured (see [[cloud.accounts]])",
                    provider_slug(provider)
                ),
            ));
        }
        for a in &candidates {
            if let Some(cred) = self.credential(&a.cfg).await? {
                return Ok((a, cred));
            }
        }
        Err(cloud_error(
            ErrorCode::Unauthorized,
            format!(
                "no credential for {}; run `vca cloud login {}` or set its credential_env",
                candidates[0].cfg.label(),
                provider_slug(provider)
            ),
        ))
    }

    /// Creates a remote session and its card (CLD-03).
    pub async fn launch(
        &self,
        mgr: &SessionManager,
        req: &CloudLaunchRequest,
    ) -> Result<SessionInfo, Error> {
        if req.prompt.trim().is_empty() {
            return Err(cloud_error(
                ErrorCode::InvalidRequest,
                "the prompt is empty",
            ));
        }
        let (acct, credential) = self.launch_account(req.provider).await?;
        let capabilities = acct.adapter.capabilities();
        if !capabilities.create {
            return Err(cloud_error(
                ErrorCode::Unsupported,
                "this cloud provider cannot start sessions",
            ));
        }
        let display_name = req.display_name.clone().filter(|n| !n.trim().is_empty());
        let launch = CloudLaunch {
            prompt: req.prompt.clone(),
            repository: Some(req.repository.trim().to_owned()).filter(|r| !r.is_empty()),
            title: display_name.clone(),
            ..CloudLaunch::default()
        };
        let remote = acct
            .adapter
            .create_session(&credential, &launch)
            .await
            .map_err(Error::from)?;
        let id = remote.default_session_id();
        if mgr.get(&id).is_some() {
            return Err(cloud_error(
                ErrorCode::Conflict,
                format!("session {id} already exists"),
            ));
        }
        let mut info = remote.to_session_info(id, now_millis());
        if let Some(name) = display_name {
            info.display_name = name;
        }
        tracing::info!(account = %acct.cfg.label(), remote_id = %remote.id, "cloud session created");
        let handle = CloudHandle {
            adapter: Arc::clone(&acct.adapter),
            credential: credential.clone(),
            remote_id: remote.id.clone(),
            capabilities,
        };
        let session = mgr.insert_cloud(info, handle);
        if capabilities.progress {
            let poller =
                ActivityPoller::new(Arc::clone(&acct.adapter), credential, remote.id.clone(), 0)
                    .with_backoff(self.backoff);
            let (tx, rx) = mpsc::channel(POLL_QUEUE);
            session.add_task(tokio::spawn(poller.run(tx)));
            session.add_task(tokio::spawn(follow(
                mgr.clone(),
                Arc::downgrade(&session),
                rx,
            )));
        }
        Ok(session.info())
    }
}

/// Handles the cloud credential requests of the owner-only local channel.
pub(crate) async fn local_request(api: &Api, req: LocalRequest) -> LocalResponse {
    let id = req.id();
    let Some(cloud) = api.cloud() else {
        return LocalResponse::error(id, ErrorCode::Unsupported, "cloud sessions are off");
    };
    let fail = |err: Error| LocalResponse::error(id, err.code(), error_text(&err));
    match req {
        LocalRequest::CloudCredentialSet {
            provider,
            account,
            secret,
            ..
        } => match cloud.set_credential(provider, &account, secret).await {
            Ok(()) => LocalResponse::Done { id },
            Err(err) => fail(err),
        },
        LocalRequest::CloudCredentialDelete {
            provider, account, ..
        } => match cloud.delete_credential(provider, &account).await {
            Ok(true) => LocalResponse::Done { id },
            Ok(false) => LocalResponse::error(
                id,
                ErrorCode::NotFound,
                format!(
                    "no stored credential for {}/{account}",
                    provider_slug(provider)
                ),
            ),
            Err(err) => fail(err),
        },
        _ => LocalResponse::error(id, ErrorCode::InvalidRequest, "unexpected request"),
    }
}

/// Gives a provider event the sequence number of the session log.
fn with_seq(mut event: SessionEvent, new: u64) -> SessionEvent {
    match &mut event {
        SessionEvent::Output { seq, .. }
        | SessionEvent::Message { seq, .. }
        | SessionEvent::ToolCall { seq, .. }
        | SessionEvent::StateChanged { seq, .. }
        | SessionEvent::DecisionRequested { seq, .. }
        | SessionEvent::Notification { seq, .. } => *seq = new,
    }
    event
}

/// Applies one provider event to the card and the session log.
fn apply_event(mgr: &SessionManager, session: &Session, event: SessionEvent) {
    if let SessionEvent::StateChanged { state, .. } = event {
        mgr.set_state(session, state);
        return;
    }
    let at = event_at(&event);
    let (preview, unread) = match &event {
        SessionEvent::Message { role, text, .. } => (
            Some(truncate_chars(text.trim(), PREVIEW_CHARS, false)).filter(|p| !p.is_empty()),
            *role == MessageRole::Assistant,
        ),
        SessionEvent::Notification { .. } => (None, true),
        _ => (None, false),
    };
    session.push(|seq| with_seq(event, seq));
    if unread {
        mgr.add_unread(session);
    }
    mgr.update_info(session, |info| {
        info.last_activity = info.last_activity.max(at);
        if preview.is_some() {
            info.preview = preview;
        }
        true
    });
}

fn event_at(event: &SessionEvent) -> u64 {
    match event {
        SessionEvent::Output { at, .. }
        | SessionEvent::Message { at, .. }
        | SessionEvent::ToolCall { at, .. }
        | SessionEvent::StateChanged { at, .. }
        | SessionEvent::DecisionRequested { at, .. }
        | SessionEvent::Notification { at, .. } => *at,
    }
}

/// Moves poll updates into the session until polling ends or the session
/// is dropped.
async fn follow(mgr: SessionManager, session: Weak<Session>, mut rx: mpsc::Receiver<PollUpdate>) {
    while let Some(update) = rx.recv().await {
        let Some(s) = session.upgrade() else {
            return;
        };
        match update {
            PollUpdate::Events(events) => {
                for event in events {
                    apply_event(&mgr, &s, event);
                }
            }
            PollUpdate::TransientError(err) => {
                tracing::debug!(session = %s.id, "cloud poll failed, retrying: {err}");
            }
            PollUpdate::Finished(None) => {
                if matches!(s.info().state, SessionState::Exited { .. }) {
                    mgr.schedule_removal(&s);
                }
                return;
            }
            PollUpdate::Finished(Some(err)) => {
                tracing::warn!(session = %s.id, "cloud polling stopped: {err}");
                mgr.set_state(
                    &s,
                    SessionState::Error {
                        message: err.to_string(),
                    },
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_cloud::InMemoryCredentialStore;

    #[test]
    fn parses_provider_names() {
        assert_eq!(parse_provider("cursor"), Some(CloudProvider::CursorCloud));
        assert_eq!(
            parse_provider("cursor_cloud"),
            Some(CloudProvider::CursorCloud)
        );
        assert_eq!(
            parse_provider("Claude-Managed"),
            Some(CloudProvider::ClaudeManagedAgents)
        );
        assert_eq!(parse_provider("codex"), Some(CloudProvider::CodexCloud));
        assert_eq!(parse_provider("grok"), None);
    }

    #[test]
    fn account_labels() {
        assert!(validate_account("work").is_ok());
        assert!(validate_account("my_team-2").is_ok());
        for bad in ["", "Work", "a b", "../x", &"x".repeat(49)] {
            assert!(validate_account(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn config_rejects_secrets_and_unknown_keys() {
        let cfg: CloudConfig = toml::from_str(
            r#"
            [[accounts]]
            provider = "devin"
            org_id = "org_1"
            credential_env = "DEVIN_KEY"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.accounts[0].account, DEFAULT_ACCOUNT);
        assert_eq!(cfg.poll_min_ms, 5_000);
        let with_key = "[[accounts]]\nprovider = \"jules\"\napi_key = \"k\"";
        assert!(toml::from_str::<CloudConfig>(with_key).is_err());
    }

    #[test]
    fn adapters_need_their_settings() {
        let devin = CloudAccountConfig::new(CloudProvider::Devin, "default");
        assert!(devin.adapter().is_err());
        let managed = CloudAccountConfig {
            agent_id: Some("a".into()),
            ..CloudAccountConfig::new(CloudProvider::ClaudeManagedAgents, "default")
        };
        assert!(managed.adapter().is_err());
        for p in [
            CloudProvider::Jules,
            CloudProvider::CursorCloud,
            CloudProvider::CopilotCloud,
            CloudProvider::CodexCloud,
        ] {
            let a = CloudAccountConfig::new(p, "default").adapter().unwrap();
            assert_eq!(a.provider(), p);
        }
    }

    #[test]
    fn duplicate_accounts_are_rejected() {
        let acct = CloudAccountConfig::new(CloudProvider::Jules, "default");
        let cfg = CloudConfig {
            accounts: vec![acct.clone(), acct],
            ..CloudConfig::default()
        };
        assert!(Cloud::new(&cfg, Arc::new(InMemoryCredentialStore::new())).is_err());
    }

    #[tokio::test]
    async fn providers_report_configured_without_secrets() {
        let cfg = CloudConfig {
            accounts: vec![
                CloudAccountConfig::new(CloudProvider::Jules, "default"),
                CloudAccountConfig::new(CloudProvider::CodexCloud, "default"),
            ],
            ..CloudConfig::default()
        };
        let cloud = Cloud::new(&cfg, Arc::new(InMemoryCredentialStore::new())).unwrap();
        let list = cloud.providers().await;
        assert!(!list[0].configured);
        // Codex cloud uses the local CLI sign-in.
        assert!(list[1].configured);
        assert!(list[1].status_only);

        cloud
            .set_credential(CloudProvider::Jules, "default", Secret::new(" sk-top \n"))
            .await
            .unwrap();
        let list = cloud.providers().await;
        assert!(list[0].configured);
        assert!(!serde_json::to_string(&list).unwrap().contains("sk-top"));
        assert!(!format!("{cloud:?}").contains("sk-top"));

        // Unknown accounts cannot be logged in.
        let err = cloud
            .set_credential(CloudProvider::Devin, "default", Secret::new("x"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotFound);
        assert!(
            cloud
                .delete_credential(CloudProvider::Jules, "default")
                .await
                .unwrap()
        );
        assert!(
            !cloud
                .delete_credential(CloudProvider::Jules, "default")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn credential_env_is_read_before_the_store() {
        // `set_var` needs `unsafe`, which the workspace denies, so use
        // variables that are (or are surely not) set in every test run.
        let path_var = if std::env::var_os("PATH").is_some() {
            "PATH"
        } else {
            "Path"
        };
        let missing = format!("VCA_TEST_UNSET_{}", uuid::Uuid::new_v4().simple());
        let cfg = CloudConfig {
            accounts: vec![
                CloudAccountConfig {
                    credential_env: Some(path_var.into()),
                    ..CloudAccountConfig::new(CloudProvider::Jules, "env")
                },
                CloudAccountConfig {
                    credential_env: Some(missing),
                    ..CloudAccountConfig::new(CloudProvider::Jules, "store")
                },
            ],
            ..CloudConfig::default()
        };
        let cloud = Cloud::new(&cfg, Arc::new(InMemoryCredentialStore::new())).unwrap();
        let env_cred = cloud.credential(&cloud.accounts[0].cfg).await.unwrap();
        assert!(env_cred.is_some());
        let store_acct = &cloud.accounts[1].cfg;
        assert!(cloud.credential(store_acct).await.unwrap().is_none());
        cloud
            .set_credential(CloudProvider::Jules, "store", Secret::new("k"))
            .await
            .unwrap();
        assert_eq!(
            cloud.credential(store_acct).await.unwrap(),
            Some(Credential::new("k"))
        );
    }

    #[tokio::test]
    async fn file_store_roundtrip_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::in_data_dir(dir.path());
        let key = CredentialKey::new(CloudProvider::ClaudeManagedAgents, "work");
        assert!(store.get(&key).await.unwrap().is_none());
        assert!(store.keys().await.unwrap().is_empty());
        store.put(&key, Credential::new("sk-ant-1")).await.unwrap();
        assert_eq!(
            store.get(&key).await.unwrap(),
            Some(Credential::new("sk-ant-1"))
        );
        assert_eq!(store.keys().await.unwrap(), vec![key.clone()]);
        let file = dir.path().join("cloud").join("claude-managed_work.key");
        assert!(file.exists());
        // Stored encoded, not as plain text.
        assert!(!std::fs::read_to_string(&file).unwrap().contains("sk-ant-1"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
        assert!(store.delete(&key).await.unwrap());
        assert!(!store.delete(&key).await.unwrap());
        let bad = CredentialKey::new(CloudProvider::Jules, "../x");
        assert!(store.put(&bad, Credential::new("k")).await.is_err());
        assert!(store.put(&key, Credential::none()).await.is_err());
    }

    #[test]
    fn credential_request_is_local_only_and_redacted() {
        let req = LocalRequest::CloudCredentialSet {
            id: 4,
            provider: CloudProvider::Jules,
            account: "default".into(),
            secret: Secret::new("sk-hidden"),
        };
        assert!(!format!("{req:?}").contains("sk-hidden"));
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["op"], "cloud_credential_set");
        assert_eq!(v["provider"], "jules");
        let back: LocalRequest = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back, req);
        // The remote API (`ClientMsg`) has no way to express it.
        let mut as_api = v;
        as_api["type"] = as_api["op"].clone();
        assert!(serde_json::from_value::<vca_core::ClientMsg>(as_api).is_err());
    }

    #[test]
    fn events_are_resequenced() {
        let ev = SessionEvent::Message {
            seq: 99,
            at: 5,
            role: MessageRole::Assistant,
            text: "hi".into(),
        };
        assert_eq!(with_seq(ev, 3).seq(), 3);
    }
}
