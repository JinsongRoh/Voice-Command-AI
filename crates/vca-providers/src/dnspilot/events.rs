//! Agent restart handling for the local event stream (design consequence C10).
//!
//! Every local API event carries the Agent's `boot_id`, and every SSE
//! connection starts with `hello` (the `Last-Event-ID` resume point is
//! valid) or `reset` (a different Agent run; start from scratch). On a
//! reset, VCA resynchronizes its tunnel state from `/v1/status` and
//! `/v1/tunnels`. As a fallback for Agents that predate `boot_id`, an event
//! ID lower than the last one seen is also treated as a restart.

use super::contract::{EventPayload, events};
use super::sse::SseEvent;

/// A local API event after restart detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEvent {
    pub id: Option<String>,
    pub kind: String,
    pub payload: EventPayload,
}

/// What the caller should do with an incoming SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerOutcome {
    /// A regular event to process.
    Event(LocalEvent),
    /// The stream (re)started on the same Agent run; nothing was missed.
    Resumed,
    /// The Agent restarted or the resume point is lost: reload state from
    /// `/v1/status` and `/v1/tunnels`, then continue with later events.
    Resync { reason: ResyncReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncReason {
    /// The Agent sent `reset`.
    ResetEvent,
    /// The `boot_id` changed.
    BootIdChanged,
    /// Fallback heuristic: the event ID went down.
    EventIdWentDown,
}

/// Tracks `boot_id` and the last event ID across reconnects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventTracker {
    boot_id: Option<String>,
    last_event_id: Option<String>,
}

impl EventTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Value for the `Last-Event-ID` header on reconnect.
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    pub fn boot_id(&self) -> Option<&str> {
        self.boot_id.as_deref()
    }

    /// Records the `boot_id` from `/v1/status` after a resync.
    pub fn set_boot_id(&mut self, boot_id: Option<String>) {
        if boot_id.is_some() && boot_id != self.boot_id {
            self.last_event_id = None;
        }
        self.boot_id = boot_id;
    }

    pub fn on_event(&mut self, ev: &SseEvent) -> TrackerOutcome {
        let payload: EventPayload = serde_json::from_str(&ev.data).unwrap_or_default();
        let incoming_boot = payload.boot_id.clone();
        let outcome = match ev.event.as_str() {
            events::RESET => {
                self.boot_id = incoming_boot;
                self.last_event_id = None;
                TrackerOutcome::Resync {
                    reason: ResyncReason::ResetEvent,
                }
            }
            events::HELLO => {
                // `hello` promises the resume point is valid; a boot ID that
                // differs from ours still means we missed a restart.
                let changed =
                    matches!((&self.boot_id, &incoming_boot), (Some(a), Some(b)) if a != b);
                if incoming_boot.is_some() {
                    self.boot_id = incoming_boot;
                }
                if changed {
                    self.last_event_id = None;
                    TrackerOutcome::Resync {
                        reason: ResyncReason::BootIdChanged,
                    }
                } else {
                    TrackerOutcome::Resumed
                }
            }
            _ => {
                if let (Some(ours), Some(theirs)) = (&self.boot_id, &incoming_boot)
                    && ours != theirs
                {
                    self.boot_id = incoming_boot;
                    self.last_event_id = ev.id.clone();
                    return TrackerOutcome::Resync {
                        reason: ResyncReason::BootIdChanged,
                    };
                }
                if incoming_boot.is_none()
                    && id_went_down(self.last_event_id.as_deref(), ev.id.as_deref())
                {
                    self.last_event_id = ev.id.clone();
                    return TrackerOutcome::Resync {
                        reason: ResyncReason::EventIdWentDown,
                    };
                }
                if self.boot_id.is_none() {
                    self.boot_id = incoming_boot;
                }
                TrackerOutcome::Event(LocalEvent {
                    id: ev.id.clone(),
                    kind: ev.event.clone(),
                    payload,
                })
            }
        };
        if ev.id.is_some() {
            self.last_event_id = ev.id.clone();
        }
        outcome
    }
}

fn id_went_down(last: Option<&str>, new: Option<&str>) -> bool {
    match (
        last.and_then(|s| s.parse::<u64>().ok()),
        new.and_then(|s| s.parse::<u64>().ok()),
    ) {
        (Some(a), Some(b)) => b < a,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str, id: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            id: id.map(str::to_owned),
            event: event.into(),
            data: data.into(),
        }
    }

    #[test]
    fn hello_then_events_then_reset() {
        let mut t = EventTracker::new();
        assert_eq!(
            t.on_event(&ev("hello", None, r#"{"boot_id":"b1"}"#)),
            TrackerOutcome::Resumed
        );
        let out = t.on_event(&ev(
            "tunnel.updated",
            Some("5"),
            r#"{"boot_id":"b1","tunnel_id":"t1"}"#,
        ));
        match out {
            TrackerOutcome::Event(e) => assert_eq!(e.payload.tunnel_id.as_deref(), Some("t1")),
            other => panic!("{other:?}"),
        }
        assert_eq!(t.last_event_id(), Some("5"));
        assert_eq!(
            t.on_event(&ev("reset", None, r#"{"boot_id":"b2"}"#)),
            TrackerOutcome::Resync {
                reason: ResyncReason::ResetEvent
            }
        );
        assert_eq!(t.last_event_id(), None);
        assert_eq!(t.boot_id(), Some("b2"));
    }

    #[test]
    fn boot_id_change_mid_stream() {
        let mut t = EventTracker::new();
        t.on_event(&ev("hello", None, r#"{"boot_id":"b1"}"#));
        assert_eq!(
            t.on_event(&ev("x", Some("1"), r#"{"boot_id":"b9"}"#)),
            TrackerOutcome::Resync {
                reason: ResyncReason::BootIdChanged
            }
        );
        assert_eq!(t.boot_id(), Some("b9"));
    }

    #[test]
    fn hello_with_different_boot_id() {
        let mut t = EventTracker::new();
        t.set_boot_id(Some("b1".into()));
        assert!(matches!(
            t.on_event(&ev("hello", None, r#"{"boot_id":"b2"}"#)),
            TrackerOutcome::Resync { .. }
        ));
    }

    #[test]
    fn fallback_heuristic_without_boot_id() {
        let mut t = EventTracker::new();
        assert!(matches!(
            t.on_event(&ev("x", Some("10"), "{}")),
            TrackerOutcome::Event(_)
        ));
        assert_eq!(
            t.on_event(&ev("x", Some("1"), "{}")),
            TrackerOutcome::Resync {
                reason: ResyncReason::EventIdWentDown
            }
        );
        assert!(matches!(
            t.on_event(&ev("x", Some("2"), "{}")),
            TrackerOutcome::Event(_)
        ));
    }
}
