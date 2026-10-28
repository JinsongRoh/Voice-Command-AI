//! Credential handling.
//!
//! Secrets are wrapped in [`Secret`], whose `Debug` and `Display` output is
//! redacted, so that credentials never reach logs by accident. Storage is
//! abstracted by [`CredentialStore`]; the server provides an OS keychain
//! implementation (CLD-05), and [`InMemoryCredentialStore`] serves tests and
//! ephemeral use.

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use tokio::sync::RwLock;
use vca_core::CloudProvider;

/// A secret string whose formatting output is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the secret value. Call this only where the value is sent to
    /// the provider; never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// A credential for one provider account.
///
/// Non-secret settings (organization ID, routine ID, agent ID, ...) are
/// passed to the adapter constructors instead, so that this type only ever
/// holds secret material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    secret: Option<Secret>,
}

impl Credential {
    /// A credential holding an API key or token.
    pub fn new(secret: impl Into<Secret>) -> Self {
        Self {
            secret: Some(secret.into()),
        }
    }

    /// No credential. Used by adapters that rely on a locally signed-in
    /// tool (Codex cloud CLI).
    pub fn none() -> Self {
        Self { secret: None }
    }

    pub fn secret(&self) -> Option<&Secret> {
        self.secret.as_ref()
    }

    /// Returns the secret or an authentication error when it is missing.
    pub(crate) fn require(&self) -> crate::Result<&str> {
        match &self.secret {
            Some(s) if !s.is_empty() => Ok(s.expose()),
            _ => Err(crate::CloudError::Auth("no credential configured".into())),
        }
    }
}

/// Identifies a stored credential: one provider, one named account.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CredentialKey {
    pub provider: CloudProvider,
    /// User-chosen account label, for example `"work"` or a routine name.
    pub account: String,
}

impl CredentialKey {
    pub fn new(provider: CloudProvider, account: impl Into<String>) -> Self {
        Self {
            provider,
            account: account.into(),
        }
    }
}

/// Errors from a credential store.
#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    #[error("credential store unavailable: {0}")]
    Unavailable(String),
}

/// Storage for credentials (OS keychain on the server, secure storage on the phone).
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, key: &CredentialKey) -> Result<Option<Credential>, CredentialStoreError>;
    async fn put(&self, key: &CredentialKey, cred: Credential) -> Result<(), CredentialStoreError>;
    /// Removes a credential. Returns whether one was stored.
    async fn delete(&self, key: &CredentialKey) -> Result<bool, CredentialStoreError>;
    /// Lists stored keys (never the secrets).
    async fn keys(&self) -> Result<Vec<CredentialKey>, CredentialStoreError>;
}

/// A credential store that lives only in memory.
#[derive(Default)]
pub struct InMemoryCredentialStore {
    inner: RwLock<HashMap<CredentialKey, Credential>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl fmt::Debug for InMemoryCredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InMemoryCredentialStore { .. }")
    }
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn get(&self, key: &CredentialKey) -> Result<Option<Credential>, CredentialStoreError> {
        Ok(self.inner.read().await.get(key).cloned())
    }

    async fn put(&self, key: &CredentialKey, cred: Credential) -> Result<(), CredentialStoreError> {
        self.inner.write().await.insert(key.clone(), cred);
        Ok(())
    }

    async fn delete(&self, key: &CredentialKey) -> Result<bool, CredentialStoreError> {
        Ok(self.inner.write().await.remove(key).is_some())
    }

    async fn keys(&self) -> Result<Vec<CredentialKey>, CredentialStoreError> {
        Ok(self.inner.read().await.keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_are_redacted() {
        let cred = Credential::new("cog_supersecret");
        let dbg = format!("{cred:?}");
        assert!(!dbg.contains("supersecret"), "{dbg}");
        assert_eq!(format!("{}", Secret::new("abc")), "<redacted>");
        assert_eq!(cred.secret().unwrap().expose(), "cog_supersecret");
    }

    #[test]
    fn require_rejects_missing_or_empty() {
        assert!(Credential::none().require().is_err());
        assert!(Credential::new("").require().is_err());
        assert_eq!(Credential::new("k").require().unwrap(), "k");
    }

    #[tokio::test]
    async fn in_memory_store_roundtrip() {
        let store = InMemoryCredentialStore::new();
        let key = CredentialKey::new(CloudProvider::Jules, "work");
        assert!(store.get(&key).await.unwrap().is_none());
        store.put(&key, Credential::new("k1")).await.unwrap();
        assert_eq!(store.get(&key).await.unwrap(), Some(Credential::new("k1")));
        assert_eq!(store.keys().await.unwrap(), vec![key.clone()]);
        assert!(store.delete(&key).await.unwrap());
        assert!(!store.delete(&key).await.unwrap());
        assert!(!format!("{store:?}").contains("k1"));
    }
}
