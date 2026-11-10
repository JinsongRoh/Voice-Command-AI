//! The single owner of one DNSPilot OAuth grant's tokens.
//!
//! DNSPilot rotates refresh tokens strictly: every refresh returns a new
//! one, the old one dies at once, and presenting an old one again is treated
//! as theft and revokes the whole grant (its tunnels are disabled and its
//! app clients revoked). [`TokenKeeper`] makes sure a refresh token is sent
//! at most once:
//!
//! * **one keeper per grant and process**: every console call of the process
//!   goes through the same keeper (an async mutex), so concurrent callers
//!   wait for one refresh and share its result (single flight);
//! * **a cross-process lock** ([`TokenStore::try_lock`], for example a
//!   [`LockFile`] in the data directory) around read, refresh and persist,
//!   with a re-read of the stored tokens after taking it, in case another
//!   process rotated them;
//! * **a write-ahead marker** ([`TokenSet::refresh_in_flight`]) persisted
//!   before the refresh token is sent; the new tokens are persisted before
//!   the new access token is used. A crash in between leaves the marker, and
//!   a stored set with the marker is never refreshed again;
//! * **no retries**: after an ambiguous failure (timeout, dropped
//!   connection, server error) the refresh token is not resent; the keeper
//!   reports that the DNSPilot sign-in needs to be renewed;
//! * refreshes happen only when the access token is about to expire (or
//!   was rejected), never on start.
//!
//! Refreshes are logged at INFO with their outcome, never with token values.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::Mutex;

use super::oauth::{DeviceFlowClient, RefreshError, TokenSet, now_secs};
use crate::error::{ProviderError, Result};
use crate::secret::Secret;

/// Refresh the access token when it expires within this margin.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// How long a caller waits for another process's refresh.
const LOCK_WAIT: Duration = Duration::from_secs(60);
const LOCK_POLL: Duration = Duration::from_millis(50);
/// A lock file older than this was left by a crashed process: a refresh
/// holds the lock for one request (30 s timeout) plus two writes.
pub const LOCK_STALE_AFTER: Duration = Duration::from_secs(120);

/// What the person sees when the grant cannot be used safely any more.
pub const REAUTHORIZE_MESSAGE: &str =
    "DNSPilot sign-in needs to be renewed: connect DNSPilot again in the desktop app";

fn reauthorize() -> ProviderError {
    ProviderError::NotSignedIn(REAUTHORIZE_MESSAGE.into())
}

fn not_connected() -> ProviderError {
    ProviderError::NotSignedIn("connect DNSPilot first".into())
}

/// A held cross-process lock; dropping it releases the lock.
pub type LockGuard = Box<dyn Send>;

/// Durable storage of one grant's tokens.
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<TokenSet>>;
    /// Stores `tokens`, replacing the previous set. Must be durable once it
    /// returns `Ok` (a rotated refresh token that is lost ends the grant).
    fn save(&self, tokens: &TokenSet) -> Result<()>;
    /// Tries to take the cross-process lock around read, refresh and
    /// persist. `Ok(None)`: another process holds it. The default is a
    /// store private to this process, which needs no lock.
    fn try_lock(&self) -> Result<Option<LockGuard>> {
        Ok(Some(Box::new(())))
    }
}

/// Called with every new token set.
pub type OnSave = Arc<dyn Fn(&TokenSet) + Send + Sync>;

/// Tokens kept in memory only, with an optional callback for every new set
/// (the console client's original API; tests).
pub struct MemoryTokenStore {
    tokens: std::sync::Mutex<Option<TokenSet>>,
    on_save: Option<OnSave>,
}

impl MemoryTokenStore {
    pub fn new(tokens: Option<TokenSet>, on_save: Option<OnSave>) -> Self {
        Self {
            tokens: std::sync::Mutex::new(tokens),
            on_save,
        }
    }
}

impl TokenStore for MemoryTokenStore {
    fn load(&self) -> Result<Option<TokenSet>> {
        Ok(self
            .tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    fn save(&self, tokens: &TokenSet) -> Result<()> {
        *self.tokens.lock().unwrap_or_else(|e| e.into_inner()) = Some(tokens.clone());
        // The callback sees new token sets only, not the write-ahead marker.
        if tokens.refresh_in_flight.is_none() {
            if let Some(f) = &self.on_save {
                f(tokens);
            }
        }
        Ok(())
    }
}

/// An exclusive lock file (`create_new`), for processes that share a token
/// store. A file older than [`LOCK_STALE_AFTER`] is treated as left behind
/// by a crashed process and taken over. Needs no platform locking API.
#[derive(Debug, Clone)]
pub struct LockFile {
    path: PathBuf,
    stale_after: Duration,
}

struct LockFileGuard(PathBuf);

impl Drop for LockFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl LockFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            stale_after: LOCK_STALE_AFTER,
        }
    }

    /// Changes the stale age (tests).
    pub fn with_stale_after(mut self, stale_after: Duration) -> Self {
        self.stale_after = stale_after;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn create(&self) -> std::io::Result<LockFileGuard> {
        use std::io::Write;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)?;
        let guard = LockFileGuard(self.path.clone());
        // Diagnostics only: who holds it, since when.
        let _ = writeln!(f, "pid={} since={}", std::process::id(), now_secs());
        Ok(guard)
    }

    /// `Ok(None)` while another live holder has it.
    pub fn try_acquire(&self) -> Result<Option<LockGuard>> {
        match self.create() {
            Ok(g) => return Ok(Some(Box::new(g))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let age = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok());
        match age {
            Some(age) if age > self.stale_after => {
                tracing::warn!(
                    path = %self.path.display(),
                    age_secs = age.as_secs(),
                    "taking over a stale DNSPilot token lock"
                );
                match std::fs::remove_file(&self.path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                match self.create() {
                    Ok(g) => Ok(Some(Box::new(g))),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            _ => Ok(None),
        }
    }
}

struct Cache {
    loaded: bool,
    tokens: Option<TokenSet>,
    /// `tokens` holds a rotated set that is not stored yet.
    unsaved: bool,
}

/// The only place in a process that refreshes one grant's tokens. Share it
/// (`Arc`) between every console client of the grant.
pub struct TokenKeeper {
    oauth: DeviceFlowClient,
    store: Arc<dyn TokenStore>,
    cache: Mutex<Cache>,
    lock_wait: Duration,
}

impl std::fmt::Debug for TokenKeeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenKeeper")
            .field("console_url", &self.oauth.config().console_url)
            .finish_non_exhaustive()
    }
}

impl TokenKeeper {
    pub fn new(oauth: DeviceFlowClient, store: Arc<dyn TokenStore>) -> Self {
        Self {
            oauth,
            store,
            cache: Mutex::new(Cache {
                loaded: false,
                tokens: None,
                unsaved: false,
            }),
            lock_wait: LOCK_WAIT,
        }
    }

    /// Changes how long a refresh waits for another process (tests).
    pub fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    pub fn oauth(&self) -> &DeviceFlowClient {
        &self.oauth
    }

    /// Forgets the cached tokens: the next call reads the store again (after
    /// a new authorization was stored, or the tokens were deleted).
    pub async fn invalidate(&self) {
        let mut cache = self.cache.lock().await;
        if cache.unsaved {
            // Never drop the only copy of a rotated refresh token silently.
            tracing::warn!("discarding DNSPilot tokens that could not be stored");
        }
        *cache = Cache {
            loaded: false,
            tokens: None,
            unsaved: false,
        };
    }

    fn ensure_loaded(&self, cache: &mut Cache) -> Result<()> {
        if !cache.loaded {
            cache.tokens = self.store.load()?;
            cache.loaded = true;
        }
        Ok(())
    }

    /// What the store holds right now (no cache, no refresh).
    pub fn stored_tokens(&self) -> Option<TokenSet> {
        self.store.load().ok().flatten()
    }

    /// The current token set (without refreshing).
    pub async fn tokens(&self) -> Result<Option<TokenSet>> {
        let mut cache = self.cache.lock().await;
        self.ensure_loaded(&mut cache)?;
        Ok(cache.tokens.clone())
    }

    /// A usable access token. Refreshes only when it expires within
    /// [`REFRESH_MARGIN`], or when `rejected` (an access token the console
    /// answered 401 to) is still the current one. Concurrent callers wait
    /// for the same refresh.
    pub async fn access_token(&self, rejected: Option<&Secret>) -> Result<Secret> {
        let mut cache = self.cache.lock().await;
        self.ensure_loaded(&mut cache)?;
        if cache.unsaved {
            let Some(t) = cache.tokens.as_ref() else {
                return Err(not_connected());
            };
            self.store.save(t)?;
            cache.unsaved = false;
            tracing::info!("stored the rotated DNSPilot tokens on a later attempt");
        }
        let Some(current) = cache.tokens.as_ref() else {
            return Err(not_connected());
        };
        let needs = match rejected {
            Some(r) => r.expose() == current.access_token.expose(),
            None => current.expires_within(REFRESH_MARGIN),
        };
        if !needs {
            return Ok(current.access_token.clone());
        }
        if current.refresh_in_flight.is_some() {
            // Only a new authorization (maybe stored by another process)
            // makes the grant usable again.
            match self.store.load()? {
                Some(t) if t.refresh_in_flight.is_none() => {
                    let usable = !t.expires_within(REFRESH_MARGIN);
                    let access = t.access_token.clone();
                    cache.tokens = Some(t);
                    if usable {
                        return Ok(access);
                    }
                }
                _ => return Err(reauthorize()),
            }
        }
        self.refresh_locked(&mut cache, rejected).await
    }

    async fn acquire_cross_process(&self) -> Result<LockGuard> {
        let deadline = tokio::time::Instant::now() + self.lock_wait;
        loop {
            if let Some(g) = self.store.try_lock()? {
                return Ok(g);
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!("DNSPilot token refresh skipped: another process holds the lock");
                return Err(ProviderError::Timeout(
                    "another VCA process is renewing the DNSPilot sign-in; try again".into(),
                ));
            }
            tokio::time::sleep(LOCK_POLL).await;
        }
    }

    async fn refresh_locked(&self, cache: &mut Cache, rejected: Option<&Secret>) -> Result<Secret> {
        let _lock = self.acquire_cross_process().await?;
        // Re-read under the lock: another process may have rotated the
        // tokens (its refresh token is then the only valid one).
        let Some(stored) = self.store.load()? else {
            cache.tokens = None;
            return Err(not_connected());
        };
        let rotated_elsewhere = cache
            .tokens
            .as_ref()
            .is_none_or(|c| c.refresh_token.expose() != stored.refresh_token.expose());
        cache.tokens = Some(stored.clone());
        if stored.refresh_in_flight.is_some() {
            tracing::warn!(
                outcome = "needs_reauthorization",
                "DNSPilot token refresh refused: an earlier refresh did not record its result"
            );
            return Err(reauthorize());
        }
        if rotated_elsewhere {
            let rejected_again =
                rejected.is_some_and(|r| r.expose() == stored.access_token.expose());
            if !rejected_again && !stored.expires_within(REFRESH_MARGIN) {
                tracing::info!(
                    outcome = "adopted",
                    "DNSPilot tokens were renewed by another process"
                );
                return Ok(stored.access_token.clone());
            }
        }

        // Write-ahead: if this process dies before the new tokens are
        // stored, the marker keeps the (maybe rotated) token from being sent
        // again.
        let mut marked = stored.clone();
        marked.refresh_in_flight = Some(now_secs());
        self.store.save(&marked)?;
        cache.tokens = Some(marked);

        tracing::info!(at = now_secs(), "DNSPilot token refresh: sending");
        match self.oauth.refresh_once(&stored).await {
            Ok(fresh) => {
                let saved = self.store.save(&fresh);
                cache.tokens = Some(fresh.clone());
                if let Err(e) = saved {
                    // Keep the only copy in memory and retry the write on the
                    // next call; do not use the new token before it is stored.
                    cache.unsaved = true;
                    tracing::warn!(
                        outcome = "rotated_unsaved",
                        "DNSPilot token refresh succeeded but storing the new tokens failed: {e}"
                    );
                    return Err(e);
                }
                tracing::info!(
                    outcome = "rotated",
                    expires_at = fresh.expires_at,
                    "DNSPilot token refresh: done"
                );
                Ok(fresh.access_token)
            }
            Err(e) if e.token_still_valid() => {
                // The old refresh token was not consumed: clear the marker.
                tracing::info!(
                    outcome = "not_consumed",
                    "DNSPilot token refresh failed: {e}"
                );
                let restore = self.store.save(&stored);
                cache.tokens = Some(stored);
                restore?;
                Err(oauth_to_provider(e))
            }
            Err(RefreshError::InvalidGrant) => {
                tracing::warn!(
                    outcome = "invalid_grant",
                    "DNSPilot refused the refresh token; the grant is no longer valid"
                );
                Err(reauthorize())
            }
            Err(e) => {
                // Ambiguous: never resend. Look once more in case another
                // holder stored a newer set; otherwise ask for a new sign-in.
                tracing::warn!(outcome = "unknown", "DNSPilot token refresh: {e}");
                if let Ok(Some(again)) = self.store.load() {
                    if again.refresh_in_flight.is_none()
                        && again.refresh_token.expose() != stored.refresh_token.expose()
                        && !again.expires_within(REFRESH_MARGIN)
                    {
                        let access = again.access_token.clone();
                        cache.tokens = Some(again);
                        return Ok(access);
                    }
                }
                Err(reauthorize())
            }
        }
    }
}

fn oauth_to_provider(e: RefreshError) -> ProviderError {
    match e.into_oauth() {
        super::oauth::OAuthError::Provider(p) => p,
        other => ProviderError::Unauthorized(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_file_is_exclusive_and_stale_files_are_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let lock = LockFile::new(dir.path().join("x.lock"));
        let held = lock.try_acquire().unwrap().expect("free");
        assert!(lock.try_acquire().unwrap().is_none(), "held");
        drop(held);
        assert!(!lock.path().exists());
        let _again = lock.try_acquire().unwrap().expect("released");

        // A crashed holder's file: taken over once it is stale.
        std::fs::write(dir.path().join("y.lock"), b"pid=1").unwrap();
        let fresh = LockFile::new(dir.path().join("y.lock"));
        assert!(fresh.try_acquire().unwrap().is_none());
        let stale = fresh.with_stale_after(Duration::ZERO);
        std::thread::sleep(Duration::from_millis(20));
        assert!(stale.try_acquire().unwrap().is_some());
    }
}
