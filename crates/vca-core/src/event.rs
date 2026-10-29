use serde::{Deserialize, Serialize};

use crate::UnixMillis;
use crate::session::SessionState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub id: String,
    pub label: String,
}

/// A question the agent is waiting on, such as a permission prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub prompt: String,
    pub options: Vec<DecisionOption>,
}

/// Everything that happens in a session, in order. `seq` is assigned by the
/// server per session and increases by one per event, so that clients can
/// resume a stream after reconnecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Raw terminal output (for the raw view).
    Output {
        seq: u64,
        at: UnixMillis,
        #[serde(with = "serde_bytes_compat")]
        data: Vec<u8>,
    },
    /// Structured conversation message (for the chat view).
    Message {
        seq: u64,
        at: UnixMillis,
        role: MessageRole,
        text: String,
    },
    ToolCall {
        seq: u64,
        at: UnixMillis,
        name: String,
        summary: String,
    },
    StateChanged {
        seq: u64,
        at: UnixMillis,
        state: SessionState,
    },
    DecisionRequested {
        seq: u64,
        at: UnixMillis,
        decision: Decision,
    },
    Notification {
        seq: u64,
        at: UnixMillis,
        title: String,
        body: String,
    },
}

impl SessionEvent {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Output { seq, .. }
            | Self::Message { seq, .. }
            | Self::ToolCall { seq, .. }
            | Self::StateChanged { seq, .. }
            | Self::DecisionRequested { seq, .. }
            | Self::Notification { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKey {
    Enter,
    Escape,
    Interrupt,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Backspace,
}

/// Input delivered to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    /// Text typed into the session. With `submit`, Enter is pressed afterwards.
    Text {
        text: String,
        submit: bool,
    },
    Key {
        key: ControlKey,
    },
    /// Answer to a [`Decision`].
    Choose {
        decision_id: String,
        option_id: String,
    },
}

/// Serializes bytes as a byte string in MessagePack and as an array in JSON,
/// without pulling in an extra dependency.
pub(crate) mod serde_bytes_compat {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                Ok(v.to_vec())
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(v)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Vec<u8>, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = a.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(out)
            }
        }
        d.deserialize_byte_buf(V)
    }
}
