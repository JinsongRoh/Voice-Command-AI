//! Replay buffers for resumable streams (dnspilot C5).
//!
//! The server keeps one [`ReplayBuffer`] per subscription (session). When a
//! client reconnects and subscribes with `from_seq` (the last sequence
//! number it has), [`ReplayBuffer::resume`] returns the missed events, or a
//! gap if some of them were evicted, in which case the server sends
//! `ServerMsg::StreamGap` and the client reloads the view.

use std::collections::VecDeque;

use vca_core::{ServerMsg, SessionEvent, SessionId};

/// An item with a sequence number and an approximate memory footprint.
pub trait Sequenced {
    fn seq(&self) -> u64;
    /// Approximate size in bytes, used for the byte budget.
    fn approx_size(&self) -> usize;
}

impl Sequenced for SessionEvent {
    fn seq(&self) -> u64 {
        SessionEvent::seq(self)
    }

    fn approx_size(&self) -> usize {
        64 + match self {
            SessionEvent::Output { data, .. } => data.len(),
            SessionEvent::Message { text, .. } => text.len(),
            SessionEvent::ToolCall { name, summary, .. } => name.len() + summary.len(),
            SessionEvent::StateChanged { .. } => 0,
            SessionEvent::DecisionRequested { decision, .. } => {
                decision.prompt.len()
                    + decision
                        .options
                        .iter()
                        .map(|o| o.id.len() + o.label.len())
                        .sum::<usize>()
            }
            SessionEvent::Notification { title, body, .. } => title.len() + body.len(),
        }
    }
}

/// Outcome of a resume request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeResult<T> {
    /// All events after `from_seq`, in order (possibly empty).
    Events(Vec<T>),
    /// Some requested events are gone; the oldest buffered one is
    /// `first_available_seq` (or the next sequence number if empty).
    Gap { first_available_seq: u64 },
}

/// Bounded buffer of the most recent events, by count and by bytes.
#[derive(Debug, Clone)]
pub struct ReplayBuffer<T> {
    items: VecDeque<(T, usize)>,
    bytes: usize,
    max_items: usize,
    max_bytes: usize,
    /// Sequence number of the last pushed item.
    last_seq: Option<u64>,
}

/// Errors from [`ReplayBuffer::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("sequence number {got} is not greater than the last one ({last})")]
pub struct OutOfOrder {
    pub last: u64,
    pub got: u64,
}

impl<T: Sequenced + Clone> ReplayBuffer<T> {
    /// Keeps at most `max_items` events and `max_bytes` bytes (at least one
    /// event is always kept).
    pub fn new(max_items: usize, max_bytes: usize) -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
            max_items: max_items.max(1),
            max_bytes,
            last_seq: None,
        }
    }

    /// Appends an event; sequence numbers must strictly increase.
    pub fn push(&mut self, item: T) -> Result<(), OutOfOrder> {
        let seq = item.seq();
        if let Some(last) = self.last_seq.filter(|&last| seq <= last) {
            return Err(OutOfOrder { last, got: seq });
        }
        let size = item.approx_size();
        self.items.push_back((item, size));
        self.bytes += size;
        self.last_seq = Some(seq);
        while self.items.len() > self.max_items
            || (self.bytes > self.max_bytes && self.items.len() > 1)
        {
            if let Some((_, s)) = self.items.pop_front() {
                self.bytes -= s;
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    pub fn first_seq(&self) -> Option<u64> {
        self.items.front().map(|(i, _)| i.seq())
    }

    /// Events with `seq > from_seq`, or a gap.
    ///
    /// A `from_seq` ahead of the newest event (e.g. the server restarted and
    /// sequence numbers began again) is also reported as a gap, so the
    /// client reloads instead of silently missing events.
    pub fn resume(&self, from_seq: u64) -> ResumeResult<T> {
        let next = self.last_seq.map_or(0, |s| s + 1);
        let Some(last) = self.last_seq else {
            return if from_seq == 0 {
                ResumeResult::Events(Vec::new())
            } else {
                ResumeResult::Gap {
                    first_available_seq: next,
                }
            };
        };
        if from_seq > last {
            return ResumeResult::Gap {
                first_available_seq: self.first_seq().unwrap_or(next),
            };
        }
        let first = self.first_seq().unwrap_or(next);
        // Events are contiguous only if the client has everything before `first`.
        if from_seq.saturating_add(1) < first {
            return ResumeResult::Gap {
                first_available_seq: first,
            };
        }
        ResumeResult::Events(
            self.items
                .iter()
                .filter(|(i, _)| i.seq() > from_seq)
                .map(|(i, _)| i.clone())
                .collect(),
        )
    }
}

impl ReplayBuffer<SessionEvent> {
    /// Builds the server messages answering a resubscribe: the missed
    /// events, or a single `StreamGap`.
    pub fn resume_messages(&self, session_id: &SessionId, from_seq: u64) -> Vec<ServerMsg> {
        match self.resume(from_seq) {
            ResumeResult::Events(evs) => evs
                .into_iter()
                .map(|event| ServerMsg::Event {
                    session_id: session_id.clone(),
                    event,
                })
                .collect(),
            ResumeResult::Gap {
                first_available_seq,
            } => vec![ServerMsg::StreamGap {
                session_id: session_id.clone(),
                first_available_seq,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, bytes: usize) -> SessionEvent {
        SessionEvent::Output {
            seq,
            at: 0,
            data: vec![b'x'; bytes],
        }
    }

    fn seqs(r: ResumeResult<SessionEvent>) -> Vec<u64> {
        match r {
            ResumeResult::Events(v) => v.iter().map(|e| e.seq()).collect(),
            ResumeResult::Gap { .. } => panic!("unexpected gap"),
        }
    }

    #[test]
    fn resume_returns_missed_events() {
        let mut b = ReplayBuffer::new(100, 1 << 20);
        for s in 1..=10 {
            b.push(ev(s, 10)).unwrap();
        }
        assert_eq!(seqs(b.resume(7)), vec![8, 9, 10]);
        assert_eq!(seqs(b.resume(10)), Vec::<u64>::new());
        assert_eq!(seqs(b.resume(0)), (1..=10).collect::<Vec<_>>());
    }

    #[test]
    fn eviction_by_count_produces_gap() {
        let mut b = ReplayBuffer::new(3, 1 << 20);
        for s in 1..=10 {
            b.push(ev(s, 10)).unwrap();
        }
        assert_eq!(b.len(), 3);
        assert_eq!(seqs(b.resume(7)), vec![8, 9, 10]);
        assert_eq!(
            b.resume(6),
            ResumeResult::Gap {
                first_available_seq: 8
            }
        );
    }

    #[test]
    fn eviction_by_bytes() {
        let mut b = ReplayBuffer::new(100, 3 * (64 + 100));
        for s in 1..=10 {
            b.push(ev(s, 100)).unwrap();
        }
        assert_eq!(b.len(), 3);
        assert!(b.bytes() <= 3 * 164);
        // One oversized event is still kept.
        b.push(ev(11, 10_000)).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b.first_seq(), Some(11));
    }

    #[test]
    fn rejects_out_of_order() {
        let mut b = ReplayBuffer::new(10, 1 << 20);
        b.push(ev(5, 1)).unwrap();
        assert_eq!(b.push(ev(5, 1)), Err(OutOfOrder { last: 5, got: 5 }));
        assert!(b.push(ev(4, 1)).is_err());
    }

    #[test]
    fn client_ahead_or_empty_buffer() {
        let mut b: ReplayBuffer<SessionEvent> = ReplayBuffer::new(10, 1 << 20);
        assert_eq!(b.resume(0), ResumeResult::Events(vec![]));
        assert_eq!(
            b.resume(5),
            ResumeResult::Gap {
                first_available_seq: 0
            }
        );
        b.push(ev(1, 1)).unwrap();
        assert_eq!(
            b.resume(50),
            ResumeResult::Gap {
                first_available_seq: 1
            }
        );
    }

    #[test]
    fn resume_messages_maps_to_protocol() {
        let mut b = ReplayBuffer::new(2, 1 << 20);
        for s in 1..=5 {
            b.push(ev(s, 1)).unwrap();
        }
        let sid: SessionId = "s1".into();
        let msgs = b.resume_messages(&sid, 1);
        assert_eq!(
            msgs,
            vec![ServerMsg::StreamGap {
                session_id: sid.clone(),
                first_available_seq: 4
            }]
        );
        let msgs = b.resume_messages(&sid, 4);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(&msgs[0], ServerMsg::Event { event, .. } if event.seq() == 5));
    }
}
