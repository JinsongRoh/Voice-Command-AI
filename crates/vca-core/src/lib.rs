//! Shared types and wire protocol for Voice Command AI.
//!
//! Every other crate depends on this one. Types here are serialized with
//! serde: JSON for local IPC and debugging, MessagePack on the wire between
//! the server and paired devices. Enums use an internal `type` tag in
//! `snake_case` so that the TypeScript UI can mirror them directly.

pub mod child_env;
pub mod conductor;
pub mod error;
pub mod event;
pub mod personal_terms;
pub mod powershell;
pub mod proclist;
pub mod pronunciation;
pub mod protocol;
pub mod resource;
pub mod restore;
pub mod session;
pub mod task;

pub use conductor::{
    CONDUCTOR_PROBLEM_HOOKS_MISSING, CONDUCTOR_PROBLEM_TURN_STALLED, ConductorAgent,
    ConductorAgentInfo, ConductorAllow, ConductorDecisionRule, ConductorEscalation,
    ConductorEscalationSource, ConductorLlm, ConductorRequest, ConductorRuleAnswer,
    ConductorSettings, ConductorState, ConductorStatus, ConductorTurn,
};
pub use error::{Error, Result};
pub use event::{
    ControlKey, Decision, DecisionOption, Input, MessageRole, Question, QuestionAnswer,
    QuestionOption, SessionEvent,
};
pub use personal_terms::{
    FEATURE_PERSONAL_TERMS, MAX_PERSONAL_TERM_CHARS, MAX_PERSONAL_TERMS, MAX_PERSONAL_VARIANTS,
    PersonalTerm,
};
pub use pronunciation::{FEATURE_PRONUNCIATIONS, Pronunciation, PronunciationSource};
pub use protocol::{
    ATTACH_CHUNK_BYTES, AttachFolderEntry, AudioEncoding, AuditEntry, ClientMsg, CloudProviderInfo,
    DangerLevel, DeliveryStatus, DeviceScope, Envelope, PROTOCOL_VERSION, PairedDevice, PushPhone,
    PushPlatform, PushToken, SPEECH_LLM_NONE, SUMMARY_MAX_CHARS, ServerMsg, SpeechProfileInfo,
    TTS_MAX_TEXT_CHARS, TranscriptCorrection, TranscriptIntent, TtsEngineInfo, TtsGender,
    TtsSpeaker,
};
pub use resource::{AdmissionVerdict, JobClass, QueueEntry, ResourceSnapshot, SessionUsage};
pub use restore::{
    ArrangeCell, ArrangeEdit, ArrangeItem, ArrangeItemStatus, ArrangeLayout, ArrangeMonitorPlan,
    ArrangeOverview, ArrangeResult, ArrangeSavedCell, ArrangeSavedMonitor, ArrangeSession,
    ArrangeWindow, ArrangeWindowKey, DEFAULT_RESTORE_DELAY_SECS, MAX_ARRANGE_COLS,
    MAX_ARRANGE_LAYOUT_CHARS, MAX_ARRANGE_LAYOUTS, MAX_ARRANGE_ROWS, MAX_RESTORE_DELAY_SECS,
    MAX_RESTORE_PROFILE_CHARS, MAX_RESTORE_PROMPT_CHARS, MAX_RESTORE_TITLE_CHARS, RestoreEdit,
    RestoreEntry, RestoreEntryDraft, RestoreEntryLive, RestoreEntrySource, RestoreItem,
    RestoreItemStatus, RestoreJob, RestoreMonitor, RestoreOverview, RestoreProfile, RestoreResume,
    RestoreRunRequest, WindowRect,
};
pub use session::{
    AgentKind, AgentQuestion, CloudLaunchRequest, CloudProvider, ControlTier, LaunchRequest,
    MAX_SESSION_NAME_CHARS, PendingDecisionInfo, SESSION_PROBLEM_HOOKS_MISSING, SessionAction,
    SessionId, SessionInfo, SessionOrigin, SessionState, ShellInfo, ShellRequest, TerminalPrompt,
    TurnActivity, TurnProgress, validate_session_name,
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
