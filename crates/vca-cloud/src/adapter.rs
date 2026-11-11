//! The provider-neutral adapter trait (architecture §5.1).

use async_trait::async_trait;
use vca_core::CloudProvider;

use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::types::{
    ActivityPage, CloudCapabilities, CloudLaunch, CloudSession, CloudSessionId, Cursor,
};

/// A cloud coding agent reachable through an official API.
///
/// Methods a provider cannot offer return [`CloudError::Unsupported`], and
/// [`capabilities`](Self::capabilities) reports them as `false`, so callers
/// can hide them up front.
#[async_trait]
pub trait CloudAgentAdapter: Send + Sync {
    fn provider(&self) -> CloudProvider;

    fn capabilities(&self) -> CloudCapabilities;

    /// Lists sessions visible to the credential (most recent first where
    /// the provider sorts).
    async fn list_sessions(&self, cred: &Credential) -> Result<Vec<CloudSession>>;

    /// Fetches one session.
    async fn get_session(&self, cred: &Credential, id: &CloudSessionId) -> Result<CloudSession>;

    async fn create_session(&self, cred: &Credential, req: &CloudLaunch) -> Result<CloudSession>;

    async fn send_message(&self, cred: &Credential, id: &CloudSessionId, text: &str) -> Result<()>;

    /// Reads activity after `since` (see [`ActivityPage`] for the paging contract).
    async fn activity(
        &self,
        cred: &Credential,
        id: &CloudSessionId,
        since: Option<Cursor>,
    ) -> Result<ActivityPage>;

    async fn stop(&self, _cred: &Credential, _id: &CloudSessionId) -> Result<()> {
        Err(CloudError::Unsupported("stop"))
    }

    async fn approve_plan(&self, _cred: &Credential, _id: &CloudSessionId) -> Result<()> {
        Err(CloudError::Unsupported("approve_plan"))
    }
}
