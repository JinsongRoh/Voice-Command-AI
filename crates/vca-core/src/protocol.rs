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
use crate::session::{CloudLaunchRequest, LaunchRequest, SessionAction, SessionId, SessionInfo};

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
