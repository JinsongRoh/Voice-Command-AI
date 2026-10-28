//! Shared types and wire protocol for Voice Command AI.
//!
//! Every other crate depends on this one. Types here are serialized with
//! serde: JSON for local IPC and debugging, MessagePack on the wire between
//! the server and paired devices. Enums use an internal `type` tag in
//! `snake_case` so that the TypeScript UI can mirror them directly.

pub mod error;
pub mod event;
pub mod protocol;
pub mod resource;
pub mod session;

pub use error::{Error, Result};
pub use event::{ControlKey, Decision, DecisionOption, Input, MessageRole, SessionEvent};
pub use protocol::{ClientMsg, Envelope, PROTOCOL_VERSION, ServerMsg};
pub use resource::{AdmissionVerdict, JobClass, QueueEntry, ResourceSnapshot, SessionUsage};
pub use session::{
    AgentKind, CloudProvider, ControlTier, LaunchRequest, SessionAction, SessionId, SessionInfo,
    SessionOrigin, SessionState,
};

/// Milliseconds since the Unix epoch, used for all timestamps in this crate.
pub type UnixMillis = u64;

/// Returns the current time as [`UnixMillis`].
pub fn now_millis() -> UnixMillis {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
