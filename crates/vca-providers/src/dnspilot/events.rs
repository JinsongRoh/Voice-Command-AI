//! Resuming the Agent's event stream across reconnects and Agent restarts
//! (<https://dns.dnspilot.kr/docs/en/agent-local-api>, "Events").
//!
//! Each event's data is `{id, boot_id, kind, at, data}`. Event ids increase
//! by one and restart at 1 whenever the Agent restarts; `boot_id` changes on
//! every restart. A client resumes by sending the last id it saw and its
//! boot id (`Last-Event-ID`, `Last-Boot-ID`). Each connection starts with one
//! control event without an id: `hello {boot_id, last_id}` when resumed, or
//! `reset {boot_id, last_id, reason}` when the id belongs to another boot
//! (`boot_changed`) or is ahead of the Agent (`id_ahead`). After a reset,
//! events start again from the beginning of this boot, and the client must
//! re-read `GET /v1/status` and `GET /v1/tunnels`.

use super::contract::{AgentEventBody, event_kinds};
use super::sse::SseEvent;

/// One Agent event after resume bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEvent {
    pub id: Option<u64>,
    /// For example `tunnel.up` or `conn.deny` (see [`event_kinds`]).
    pub kind: String,
    pub body: AgentEventBody,
}

/// What the caller should do with an incoming SSE event.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackerOutcome {
    /// A regular event to process.
    Event(Box<AgentEvent>),
    /// `hello`: the stream resumed; nothing was missed.
    Resumed,
    /// Reload state from `GET /v1/status` and `GET /v1/tunnels`, then
    /// continue with later events.
    Resync { reason: ResyncReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResyncReason {
    /// The Agent sent `reset` with this reason (`boot_changed`,
    /// `id_ahead`, or another value).
    Reset(String),
    /// A regular event arrived with a different `boot_id` than ours.
    BootIdChanged,
}

/// Tracks the resume point (`boot_id`, last event id).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventTracker {
    boot_id: Option<String>,
    last_id: Option<u64>,
}

impl EventTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `(Last-Event-ID, Last-Boot-ID)` pair for the next connection.
    pub fn resume_point(&self) -> Option<(u64, &str)> {
        Some((self.last_id?, self.boot_id.as_deref()?))
    }

    pub fn boot_id(&self) -> Option<&str> {
        self.boot_id.as_deref()
    }

    pub fn last_id(&self) -> Option<u64> {
        self.last_id
    }

    /// Records the boot id read from `GET /v1/status` after a resync.
    pub fn set_boot_id(&mut self, boot_id: Option<String>) {
        if boot_id.is_some() && boot_id != self.boot_id {
            self.last_id = None;
        }
        self.boot_id = boot_id;
    }

    pub fn on_event(&mut self, ev: &SseEvent) -> TrackerOutcome {
        let body: AgentEventBody = serde_json::from_str(&ev.data).unwrap_or_default();
        // The kind is in the data; an SSE `event:` name, when present, is
        // accepted as well.
        let kind = body
            .kind
            .clone()
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| ev.event.clone());
        let id = body
            .id
            .or_else(|| ev.id.as_deref().and_then(|s| s.parse().ok()));
        match kind.as_str() {
            event_kinds::HELLO => {
                if body.boot_id.is_some() {
                    self.boot_id = body.boot_id;
                }
                if body.last_id.is_some() {
                    self.last_id = body.last_id;
                }
                TrackerOutcome::Resumed
            }
            event_kinds::RESET => {
                self.boot_id = body.boot_id.clone();
                self.last_id = None;
                TrackerOutcome::Resync {
                    reason: ResyncReason::Reset(body.reason.clone().unwrap_or_default()),
                }
            }
            _ => {
                let changed = matches!(
                    (&self.boot_id, &body.boot_id),
                    (Some(ours), Some(theirs)) if ours != theirs
                );
                if body.boot_id.is_some() {
                    self.boot_id = body.boot_id.clone();
                }
                if id.is_some() {
                    self.last_id = id;
                }
                if changed {
                    return TrackerOutcome::Resync {
                        reason: ResyncReason::BootIdChanged,
                    };
                }
                TrackerOutcome::Event(Box::new(AgentEvent { id, kind, body }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(data: &str) -> SseEvent {
        SseEvent {
            id: None,
            event: "message".into(),
            data: data.into(),
        }
    }

    #[test]
    fn hello_events_and_reset() {
        let mut t = EventTracker::new();
        assert_eq!(
            t.on_event(&ev(r#"{"kind":"hello","boot_id":"b1","last_id":41}"#)),
            TrackerOutcome::Resumed
        );
        assert_eq!(t.resume_point(), Some((41, "b1")));
        match t.on_event(&ev(
            r#"{"id":42,"boot_id":"b1","kind":"tunnel.up","at":"2026-10-30T00:00:00Z","data":{}}"#,
        )) {
            TrackerOutcome::Event(e) => {
                assert_eq!(e.kind, "tunnel.up");
                assert_eq!(e.id, Some(42));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(t.resume_point(), Some((42, "b1")));
        assert_eq!(
            t.on_event(&ev(
                r#"{"kind":"reset","boot_id":"b2","last_id":3,"reason":"boot_changed"}"#
            )),
            TrackerOutcome::Resync {
                reason: ResyncReason::Reset("boot_changed".into())
            }
        );
        assert_eq!(t.boot_id(), Some("b2"));
        assert_eq!(t.resume_point(), None);
    }

    #[test]
    fn sse_event_names_are_accepted_too() {
        let mut t = EventTracker::new();
        let hello = SseEvent {
            id: None,
            event: "hello".into(),
            data: r#"{"boot_id":"b1","last_id":0}"#.into(),
        };
        assert_eq!(t.on_event(&hello), TrackerOutcome::Resumed);
        let up = SseEvent {
            id: Some("1".into()),
            event: "tunnel.down".into(),
            data: r#"{"boot_id":"b1","data":{}}"#.into(),
        };
        assert!(matches!(t.on_event(&up), TrackerOutcome::Event(e) if e.kind == "tunnel.down"));
        assert_eq!(t.last_id(), Some(1));
    }

    #[test]
    fn boot_id_change_mid_stream() {
        let mut t = EventTracker::new();
        t.set_boot_id(Some("b1".into()));
        assert_eq!(
            t.on_event(&ev(r#"{"id":1,"boot_id":"b9","kind":"killed","data":{}}"#)),
            TrackerOutcome::Resync {
                reason: ResyncReason::BootIdChanged
            }
        );
        assert_eq!(t.resume_point(), Some((1, "b9")));
    }
}
