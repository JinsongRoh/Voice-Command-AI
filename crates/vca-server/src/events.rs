//! Per-session event log: server-assigned sequence numbers, a bounded
//! replay buffer and live fan-out to subscribers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use vca_core::{SessionEvent, UnixMillis};

use crate::util::lock;

const LIVE_CAPACITY: usize = 1024;

/// Replay buffer limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogLimits {
    pub max_events: usize,
    pub max_bytes: usize,
}

impl Default for LogLimits {
    fn default() -> Self {
        Self {
            max_events: 5000,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Events replayed for a subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// Set when events the subscriber asked for were already evicted: the
    /// first sequence number still available.
    pub gap: Option<u64>,
    pub events: Vec<SessionEvent>,
}

/// One session's ordered events. Sequence numbers start at 1 and increase
/// by one per event.
pub struct EventLog {
    events: VecDeque<(SessionEvent, usize)>,
    bytes: usize,
    next_seq: u64,
    limits: LogLimits,
    tx: broadcast::Sender<SessionEvent>,
}

fn event_size(ev: &SessionEvent) -> usize {
    64 + match ev {
        SessionEvent::Output { data, .. } => data.len(),
        SessionEvent::Message { text, .. } => text.len(),
        SessionEvent::ToolCall { name, summary, .. } => name.len() + summary.len(),
        SessionEvent::DecisionRequested { decision, .. } => {
            decision.prompt.len()
                + decision
                    .options
                    .iter()
                    .map(|o| o.id.len() + o.label.len())
                    .sum::<usize>()
        }
        SessionEvent::Notification { title, body, .. } => title.len() + body.len(),
        SessionEvent::StateChanged { .. } => 0,
    }
}

impl EventLog {
    pub fn new(limits: LogLimits) -> Self {
        let (tx, _) = broadcast::channel(LIVE_CAPACITY);
        Self {
            events: VecDeque::new(),
            bytes: 0,
            next_seq: 1,
            limits: LogLimits {
                max_events: limits.max_events.max(1),
                max_bytes: limits.max_bytes.max(1),
            },
            tx,
        }
    }

    /// Appends an event built from its sequence number and broadcasts it.
    pub fn push(&mut self, make: impl FnOnce(u64) -> SessionEvent) -> SessionEvent {
        let seq = self.next_seq;
        self.next_seq += 1;
        let ev = make(seq);
        debug_assert_eq!(ev.seq(), seq);
        let size = event_size(&ev);
        self.events.push_back((ev.clone(), size));
        self.bytes += size;
        // Keep at least the newest event, even when it alone is too large.
        while self.events.len() > 1
            && (self.events.len() > self.limits.max_events || self.bytes > self.limits.max_bytes)
        {
            if let Some((_, s)) = self.events.pop_front() {
                self.bytes -= s;
            }
        }
        let _ = self.tx.send(ev.clone());
        ev
    }

    /// Sequence number the next event will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Oldest buffered sequence number (`next_seq` when empty).
    pub fn first_available(&self) -> u64 {
        self.events.front().map_or(self.next_seq, |(e, _)| e.seq())
    }

    /// Buffered events with `seq > after`.
    pub fn replay_after(&self, after: u64) -> Replay {
        let first = self.first_available();
        let gap = (after + 1 < first).then_some(first);
        let events = self
            .events
            .iter()
            .filter(|(e, _)| e.seq() > after)
            .map(|(e, _)| e.clone())
            .collect();
        Replay { gap, events }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.tx.subscribe()
    }
}

/// Item of an [`EventStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamItem {
    Event(SessionEvent),
    /// The subscriber fell behind and events were evicted; resume at this seq.
    Gap(u64),
}

/// Live events of one session, recovering from the log when the live
/// channel lags.
pub struct EventStream {
    log: Arc<Mutex<EventLog>>,
    rx: broadcast::Receiver<SessionEvent>,
    next_seq: u64,
    pending: VecDeque<StreamItem>,
}

impl EventStream {
    /// Atomically replays events after `after` (`None`: no replay) and
    /// subscribes to everything that follows.
    pub fn open(log: &Arc<Mutex<EventLog>>, after: Option<u64>) -> (Replay, Self) {
        let guard = lock(log);
        let rx = guard.subscribe();
        let replay = match after {
            Some(a) => guard.replay_after(a),
            None => Replay {
                gap: None,
                events: Vec::new(),
            },
        };
        let next_seq = guard.next_seq();
        drop(guard);
        (
            replay,
            Self {
                log: Arc::clone(log),
                rx,
                next_seq,
                pending: VecDeque::new(),
            },
        )
    }

    fn resync(&mut self) {
        let guard = lock(&self.log);
        let replay = guard.replay_after(self.next_seq.saturating_sub(1));
        drop(guard);
        if let Some(first) = replay.gap {
            self.pending.push_back(StreamItem::Gap(first));
            self.next_seq = first;
        }
        for ev in replay.events {
            self.next_seq = ev.seq() + 1;
            self.pending.push_back(StreamItem::Event(ev));
        }
    }

    /// Next item; `None` when the log was dropped.
    pub async fn recv(&mut self) -> Option<StreamItem> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            match self.rx.recv().await {
                Ok(ev) if ev.seq() < self.next_seq => continue,
                Ok(ev) if ev.seq() == self.next_seq => {
                    self.next_seq += 1;
                    return Some(StreamItem::Event(ev));
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => self.resync(),
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Convenience constructors for events.
pub fn notification(seq: u64, at: UnixMillis, title: &str, body: &str) -> SessionEvent {
    SessionEvent::Notification {
        seq,
        at,
        title: title.to_owned(),
        body: body.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(seq: u64, n: usize) -> SessionEvent {
        SessionEvent::Output {
            seq,
            at: 0,
            data: vec![b'x'; n],
        }
    }

    #[test]
    fn assigns_sequential_seqs_and_evicts() {
        let mut log = EventLog::new(LogLimits {
            max_events: 3,
            max_bytes: 1 << 20,
        });
        for _ in 0..5 {
            log.push(|s| out(s, 1));
        }
        assert_eq!(log.first_available(), 3);
        assert_eq!(log.next_seq(), 6);
        let r = log.replay_after(0);
        assert_eq!(r.gap, Some(3));
        assert_eq!(
            r.events.iter().map(SessionEvent::seq).collect::<Vec<_>>(),
            [3, 4, 5]
        );
        let r = log.replay_after(2);
        assert_eq!(r.gap, None);
        assert_eq!(r.events.len(), 3);
        assert!(log.replay_after(5).events.is_empty());
    }

    #[test]
    fn evicts_by_bytes() {
        let mut log = EventLog::new(LogLimits {
            max_events: 100,
            // Each event counts 64 bytes of overhead plus its data, so two
            // fit and a third evicts the oldest.
            max_bytes: 400,
        });
        for _ in 0..4 {
            log.push(|s| out(s, 100));
        }
        assert_eq!(log.first_available(), 3);
    }

    #[tokio::test]
    async fn stream_replays_then_follows_and_reports_gaps() {
        let log = Arc::new(Mutex::new(EventLog::new(LogLimits {
            max_events: 4,
            max_bytes: 1 << 20,
        })));
        lock(&log).push(|s| out(s, 1));
        let (replay, mut stream) = EventStream::open(&log, Some(0));
        assert_eq!(replay.events.len(), 1);
        lock(&log).push(|s| out(s, 1));
        assert_eq!(stream.recv().await, Some(StreamItem::Event(out(2, 1))));

        // Overflow the live channel so the stream must resync from the log.
        for _ in 0..(LIVE_CAPACITY + 10) {
            lock(&log).push(|s| out(s, 1));
        }
        let first = lock(&log).first_available();
        assert_eq!(stream.recv().await, Some(StreamItem::Gap(first)));
        match stream.recv().await {
            Some(StreamItem::Event(e)) => assert_eq!(e.seq(), first),
            other => panic!("unexpected {other:?}"),
        }
    }
}
