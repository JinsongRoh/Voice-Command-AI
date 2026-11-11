//! Cloud coding agent adapters using official provider APIs (requirements
//! §3.10, architecture §5.1).
//!
//! Every provider implements [`CloudAgentAdapter`]. Each adapter reports
//! [`CloudCapabilities`] so that the UI hides what a provider cannot do
//! (CLD-07). [`ActivityPoller`] turns a provider's activity feed into
//! `vca-core` [`SessionEvent`](vca_core::SessionEvent)s with stable
//! sequence numbers, and [`CloudSession::to_session_info`] builds the
//! session card (origin `Cloud`, control tier `Cloud`).
//!
//! Only official, documented APIs are used. The HTTP adapters use
//! `reqwest` with rustls and work without the user's machine (direct mode,
//! CLD-06); the Codex cloud adapter drives the `codex` CLI and therefore
//! runs only on the machine.
//!
//! Credentials are wrapped in [`Secret`], whose `Debug` output is redacted.

pub mod adapter;
pub mod credential;
pub mod error;
pub mod http;
pub mod poll;
pub mod providers;
pub mod repo;
pub mod types;

pub use adapter::CloudAgentAdapter;
pub use credential::{
    Credential, CredentialKey, CredentialStore, CredentialStoreError, InMemoryCredentialStore,
    Secret,
};
pub use error::{CloudError, Result};
pub use poll::{ActivityPoller, Backoff, PollUpdate, PollerState};
pub use types::{
    Activity, ActivityKind, ActivityPage, CloudCapabilities, CloudLaunch, CloudSession,
    CloudSessionId, Cursor, provider_name, provider_slug,
};
