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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub prompt: String,
    pub options: Vec<DecisionOption>,
    /// Inferred without agent hooks (an unanswered tool call and an idle
    /// agent): the agent probably waits for an answer in its own terminal.
    /// Such a decision has no options; it is answered at the PC or with the
    /// terminal key buttons.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inferred: bool,
    /// Questions the agent asked with a form (Claude Code's
    /// `AskUserQuestion` tool). Such a decision has no `options`; it is
    /// answered with [`Input::Answer`], one [`QuestionAnswer`] per question.
    /// Left out of the JSON when empty; empty from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
    /// The exact shell command the agent's hook reported for this decision.
    /// Only hook data: never parsed from `prompt`, which is display text. A
    /// spoken approval needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Decision {
    /// True for a question form ([`Self::questions`]).
    pub fn is_question(&self) -> bool {
        !self.questions.is_empty()
    }
}

/// One question of a question form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// Short tab label, for example "Pricing". May be empty.
    #[serde(default)]
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    /// Several options may be picked (check boxes).
    #[serde(default)]
    pub multi_select: bool,
    /// The form offers a free-text answer ("Type something.").
    #[serde(default)]
    pub free_text: bool,
}

/// One option of a [`Question`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// The answer to one [`Question`]: picked option indices (one for a
/// single-choice question) and/or free text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    #[serde(default)]
    pub options: Vec<u32>,
    #[serde(default)]
    pub text: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
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
    ///
    /// While a decision is open any text could answer the agent's prompt
    /// (a menu takes "1" or "y" without Enter), so the server refuses it
    /// unless `at_prompt` says a person typed it by hand: the composer, the
    /// terminal keys. Voice, automatic sends and other programs leave it
    /// unset. This stops accidents, not a hostile local program, which can
    /// set the flag or write to the agent's terminal itself.
    Text {
        text: String,
        submit: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        at_prompt: bool,
    },
    Key {
        key: ControlKey,
    },
    /// Answer to a [`Decision`].
    Choose {
        decision_id: String,
        option_id: String,
    },
    /// Answers to a question form ([`Decision::questions`]), one per
    /// question in order. The server types them into the agent's form.
    Answer {
        decision_id: String,
        answers: Vec<QuestionAnswer>,
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
