//! Application messages exchanged between VCA Server and paired clients.
//!
//! Messages travel inside the Noise channel as MessagePack-encoded
//! [`Envelope`]s. Requests carry a client-chosen `id`; the server echoes it
//! in the response. Server-initiated messages use `id = 0`.

use serde::{Deserialize, Serialize};

use crate::UnixMillis;
use crate::error::ErrorCode;
use crate::event::{Input, SessionEvent};
use crate::resource::ResourceSnapshot;
use crate::session::{
    CloudLaunchRequest, CloudProvider, LaunchRequest, SessionAction, SessionId, SessionInfo,
};

/// Bumped on incompatible protocol changes.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: u64,
    pub body: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        protocol_version: u32,
        app_version: String,
    },
    Ping {
        at: UnixMillis,
    },
    ListSessions,
    /// Start streaming events of a session. With `from_seq`, events after
    /// that sequence number are replayed first (when still buffered).
    Subscribe {
        session_id: SessionId,
        from_seq: Option<u64>,
    },
    Unsubscribe {
        session_id: SessionId,
    },
    SendInput {
        session_id: SessionId,
        input: Input,
    },
    Control {
        session_id: SessionId,
        action: SessionAction,
    },
    Launch {
        request: LaunchRequest,
    },
    /// Answered with [`ServerMsg::Launched`].
    LaunchCloud {
        request: CloudLaunchRequest,
    },
    GetResources,
    /// Move a queued job to a new position (0 = next).
    ReorderQueue {
        job_id: String,
        position: u32,
    },
    CancelQueued {
        job_id: String,
    },
    MarkRead {
        session_id: SessionId,
    },
    ListDevices,
    RevokeDevice {
        device_id: String,
    },
    /// Drop every remote connection and close the tunnel (SEC-09).
    KillSwitch,
    /// Cloud providers the server can launch and follow. Answered with
    /// [`ServerMsg::CloudProviders`].
    ListCloudProviders,
    /// Recognize one utterance with the server's speech pipeline (VOC-01).
    /// Answered with [`ServerMsg::Transcript`]. The transcript is only
    /// shown for review; nothing is sent to a session (VOC-03).
    Transcribe {
        /// Encoded audio (WebM/Opus, Ogg, M4A, WAV) or raw 16-bit PCM.
        #[serde(with = "crate::event::serde_bytes_compat")]
        audio: Vec<u8>,
        format: AudioEncoding,
        /// Sample rate; required for `pcm16`, informational otherwise.
        sample_rate: Option<u32>,
        /// BCP-47 primary language; `None` uses the server's setting.
        language: Option<String>,
        /// The selected session, for project vocabulary and intent routing.
        session_id: Option<SessionId>,
    },
}

/// Audio container of a [`ClientMsg::Transcribe`] request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioEncoding {
    Wav,
    /// Raw little-endian signed 16-bit mono PCM.
    Pcm16,
    Webm,
    Ogg,
    M4a,
}

/// A term the developer vocabulary engine replaced (VOC-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptCorrection {
    /// What the recognizer heard.
    pub from: String,
    /// The vocabulary term that replaced it (appears in the text).
    pub to: String,
    /// `base`, `project` or `personal`.
    pub layer: String,
}

/// How the server routed a transcript (VOC-06). The client confirms
/// control commands and ambiguous results before acting on them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptIntent {
    /// Text meant for the agent.
    Prompt,
    /// A command for VCA. `action` is the router's action name (`pause`,
    /// `stop`, `approve`, ...); `session_action` is set for lifecycle
    /// commands. `session_id: None` means the selected session.
    Control {
        action: String,
        session_action: Option<SessionAction>,
        session_id: Option<SessionId>,
        confidence: f32,
    },
    /// Could be a command or a prompt; ask the user.
    Ambiguous,
}

/// A cloud provider as the server offers it (CLD-07): the client hides
/// what `capabilities` does not allow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudProviderInfo {
    pub provider: CloudProvider,
    /// Account label from the server configuration.
    pub account: String,
    /// A credential is available for the account.
    pub configured: bool,
    pub create: bool,
    pub follow_up: bool,
    pub stop: bool,
    pub approve_plan: bool,
    /// Progress is limited to coarse status (no conversation).
    pub status_only: bool,
    /// Endpoint shapes could not be verified against public docs.
    pub unverified: bool,
}

/// A device paired with the server (SEC-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub paired_at: UnixMillis,
    pub last_seen: UnixMillis,
    /// True for the device that sent the request.
    pub current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Queued,
    Delivered,
    /// The agent visibly reacted to the input.
    Acknowledged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        protocol_version: u32,
        server_version: String,
        server_name: String,
    },
    Pong {
        at: UnixMillis,
    },
    Ok,
    Error {
        code: ErrorCode,
        message: String,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    SessionUpdated {
        session: SessionInfo,
    },
    SessionRemoved {
        session_id: SessionId,
    },
    Event {
        session_id: SessionId,
        event: SessionEvent,
    },
    /// Sent after a resubscribe when requested events are no longer buffered;
    /// the client should reload the session view.
    StreamGap {
        session_id: SessionId,
        first_available_seq: u64,
    },
    InputAck {
        session_id: SessionId,
        status: DeliveryStatus,
    },
    Launched {
        session: SessionInfo,
    },
    Resources {
        snapshot: ResourceSnapshot,
    },
    Devices {
        devices: Vec<PairedDevice>,
    },
    CloudProviders {
        providers: Vec<CloudProviderInfo>,
    },
    Transcript {
        /// Cleaned-up and vocabulary-corrected text, for review (VOC-03).
        text: String,
        /// The recognizer's text before corrections.
        raw_text: String,
        language: Option<String>,
        corrections: Vec<TranscriptCorrection>,
        intent: TranscriptIntent,
        /// Name of the engine that recognized the audio.
        engine: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ControlKey;

    #[test]
    fn json_shape_is_tagged() {
        let msg = Envelope {
            id: 7,
            body: ClientMsg::SendInput {
                session_id: "s1".into(),
                input: Input::Key {
                    key: ControlKey::Interrupt,
                },
            },
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["body"]["type"], "send_input");
        assert_eq!(json["body"]["input"]["type"], "key");
        assert_eq!(json["body"]["input"]["key"], "interrupt");
    }

    #[test]
    fn msgpack_roundtrip_with_bytes() {
        let msg = Envelope {
            id: 0,
            body: ServerMsg::Event {
                session_id: "s1".into(),
                event: SessionEvent::Output {
                    seq: 3,
                    at: 1,
                    data: b"hello\x1b[0m".to_vec(),
                },
            },
        };
        let bytes = rmp_serde::to_vec_named(&msg).unwrap();
        let back: Envelope<ServerMsg> = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn json_roundtrip_with_bytes() {
        let ev = SessionEvent::Output {
            seq: 1,
            at: 2,
            data: vec![0, 255, 10],
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: SessionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }
}
