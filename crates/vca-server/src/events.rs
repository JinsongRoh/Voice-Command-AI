//! Per-session event log: server-assigned sequence numbers, a bounded
//! replay buffer and live fan-out to subscribers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use vca_core::{SessionEvent, UnixMillis};

use crate::util::lock;

/// Live events a subscriber may fall behind by before it catches up from
/// the log ([`EventStream::recv`]). Every slot keeps its event alive until
/// all receivers saw it.
const LIVE_CAPACITY: usize = 128;
/// Most events a lagging stream copies from the log at once; an older rest
/// is reported as a gap (the client loads it as history when it wants).
const RESYNC_EVENTS: usize = MAX_PAGE_EVENTS;

/// Events in the first page a subscriber gets (and in a history page when
/// the client names no size). The rest is fetched on demand.
pub const PAGE_EVENTS: usize = 60;
/// Largest page a client may ask for.
pub const MAX_PAGE_EVENTS: usize = 500;

/// Most bytes of text or output one event keeps. A transcript line may be
/// megabytes (a tool's whole output); the log and every subscriber's live
/// buffer would hold each copy. Output keeps its end (the screen now),
/// text its beginning with a note.
pub const MAX_EVENT_BYTES: usize = 256 * 1024;
/// Appended to text cut at [`MAX_EVENT_BYTES`].
const TRUNCATED: &str = "\n… (truncated)";

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

impl Replay {
    /// Keeps only the newest `limit` events. When older ones are cut,
    /// `gap` names the first seq kept, so the subscriber knows its view
    /// starts there and can ask for the earlier ones with a history page.
    pub fn latest_page(mut self, limit: usize) -> Self {
        if self.events.len() > limit {
            let cut = self.events.len() - limit;
            self.events.drain(..cut);
            self.gap = self.events.first().map(SessionEvent::seq);
        }
        self
    }
}

/// One session's ordered events. Sequence numbers start at 1 and increase
/// by one per event.
pub struct EventLog {
    events: VecDeque<(SessionEvent, usize)>,
    bytes: usize,
    /// Events that are not terminal output (messages, tool calls, state
    /// changes ...) and their bytes: what output may not push out (see
    /// [`EventLog::evict_one`]).
    chat_events: usize,
    chat_bytes: usize,
    /// Highest seq removed from somewhere other than the front: a reader
    /// whose position is below it has missed output that is no longer here.
    lost_through: u64,
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
            decision.id.len()
                + decision.prompt.len()
                + decision.command.as_ref().map_or(0, String::len)
                + decision
                    .options
                    .iter()
                    .map(|o| o.id.len() + o.label.len())
                    .sum::<usize>()
                + decision
                    .questions
                    .iter()
                    .map(|q| {
                        16 + q.header.len()
                            + q.question.len()
                            + q.options
                                .iter()
                                .map(|o| 16 + o.label.len() + o.description.len())
                                .sum::<usize>()
                    })
                    .sum::<usize>()
        }
        SessionEvent::Notification { title, body, .. } => title.len() + body.len(),
        SessionEvent::StateChanged { .. } => 0,
    }
}

/// `text` cut to at most `max` bytes at a character boundary, with a note
/// when something was cut.
fn cap_text(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut end = max.saturating_sub(TRUNCATED.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(TRUNCATED);
}

/// The event with its text and output within [`MAX_EVENT_BYTES`] (see
/// there). Decisions stay whole: their size is the agent's hook payload.
fn cap_event(mut ev: SessionEvent, max: usize) -> SessionEvent {
    match &mut ev {
        SessionEvent::Output { data, .. } if data.len() > max => {
            data.drain(..data.len() - max);
            data.shrink_to_fit();
        }
        SessionEvent::Message { text, .. } => cap_text(text, max),
        SessionEvent::ToolCall { name, summary, .. } => {
            cap_text(name, max / 4);
            cap_text(summary, max);
        }
        SessionEvent::Notification { title, body, .. } => {
            cap_text(title, max / 4);
            cap_text(body, max);
        }
        _ => {}
    }
    ev
}

impl EventLog {
    pub fn new(limits: LogLimits) -> Self {
        let (tx, _) = broadcast::channel(LIVE_CAPACITY);
        Self {
            events: VecDeque::new(),
            bytes: 0,
            chat_events: 0,
            chat_bytes: 0,
            lost_through: 0,
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
        let ev = cap_event(make(seq), MAX_EVENT_BYTES.min(self.limits.max_bytes));
        debug_assert_eq!(ev.seq(), seq);
        let size = event_size(&ev);
        self.events.push_back((ev.clone(), size));
        self.bytes += size;
        if !matches!(ev, SessionEvent::Output { .. }) {
            self.chat_events += 1;
            self.chat_bytes += size;
        }
        // Keep at least the newest event, even when it alone is too large.
        let limits = self.limits;
        while self.events.len() > 1
            && (self.events.len() > limits.max_events || self.bytes > limits.max_bytes)
        {
            self.evict_one(limits);
        }
        // A burst of small events grew the buffer far beyond what a few
        // large ones need now: give the memory back.
        if self.events.capacity() > 64 && self.events.capacity() > 4 * self.events.len() {
            self.events.shrink_to(2 * self.events.len());
        }
        let _ = self.tx.send(ev.clone());
        ev
    }

    /// Drops one event to get back within the limits.
    ///
    /// A managed agent's terminal repaints many times a second, so output
    /// alone would push every message and tool call out of the buffer within
    /// minutes and leave the chat empty. While the non-output events use no
    /// more than half of the limits, the oldest output goes first (wherever
    /// it is); beyond that, or with no output left, the oldest event goes.
    /// The newest event is never the one removed here (see [`Self::push`]).
    fn evict_one(&mut self, limits: LogLimits) {
        let chat_fits =
            self.chat_events <= limits.max_events / 2 && self.chat_bytes <= limits.max_bytes / 2;
        let newest = self.events.len().saturating_sub(1);
        let at = if chat_fits {
            self.events
                .iter()
                .take(newest)
                .position(|(e, _)| matches!(e, SessionEvent::Output { .. }))
        } else {
            None
        };
        let at = at.unwrap_or(0);
        if let Some((ev, size)) = self.events.remove(at) {
            self.bytes -= size;
            if !matches!(ev, SessionEvent::Output { .. }) {
                self.chat_events -= 1;
                self.chat_bytes -= size;
            }
            if at > 0 {
                self.lost_through = self.lost_through.max(ev.seq());
            }
        }
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
    ///
    /// An `after` beyond the newest event belongs to an earlier life of this
    /// log (vcad restarted and numbers began again). The subscriber is told
    /// to reload and gets everything buffered; answering with nothing would
    /// leave it with an empty chat that also drops the new, lower numbers.
    pub fn replay_after(&self, after: u64) -> Replay {
        self.replay_latest(after, usize::MAX)
    }

    /// [`Self::replay_after`] limited to the newest `limit` events, like
    /// [`Replay::latest_page`], but copying only the events it returns
    /// (sequence numbers ascend: the start is a binary search).
    ///
    /// A gap always names the first seq replayed (`next_seq` when nothing
    /// is): the client drops its view, keeps what follows, and pages back
    /// from there. Output evicted from the middle ([`Self::evict_one`])
    /// leaves older messages buffered before that point; naming the
    /// front instead would tell the client nothing older exists.
    pub fn replay_latest(&self, after: u64, limit: usize) -> Replay {
        let first = self.first_available();
        let (mut start, mut gap) = if after >= self.next_seq {
            (0, true)
        } else {
            (
                self.events.partition_point(|(e, _)| e.seq() <= after),
                after + 1 < first || after < self.lost_through,
            )
        };
        if self.events.len() - start > limit {
            start = self.events.len() - limit;
            gap = true;
        }
        let gap = gap.then(|| {
            self.events
                .get(start)
                .map_or(self.next_seq, |(e, _)| e.seq())
        });
        let events = self.events.range(start..).map(|(e, _)| e.clone()).collect();
        Replay { gap, events }
    }

    /// Keeps at most `max_events` events within `max_bytes`, and gives the
    /// memory of the rest back (an exited session's log). Output goes
    /// first, as in [`Self::evict_one`], so the conversation survives.
    pub fn trim(&mut self, max_events: usize, max_bytes: usize) {
        let limits = LogLimits {
            max_events: max_events.max(1),
            max_bytes: max_bytes.max(1),
        };
        while self.events.len() > 1
            && (self.events.len() > limits.max_events || self.bytes > limits.max_bytes)
        {
            self.evict_one(limits);
        }
        self.events.shrink_to_fit();
    }

    /// Buffered events (for tests and diagnostics).
    pub fn buffered(&self) -> usize {
        self.events.len()
    }

    /// A page of buffered events older than `before` (`None`: the newest),
    /// ascending, and whether even older ones are still buffered. Sequence
    /// numbers are contiguous, so this is a direct slice, not a scan.
    pub fn page_before(&self, before: Option<u64>, limit: usize) -> (Vec<SessionEvent>, bool) {
        let limit = limit.clamp(1, MAX_PAGE_EVENTS);
        let end = before.map_or(self.events.len(), |b| {
            self.events.partition_point(|(e, _)| e.seq() < b)
        });
        let start = end.saturating_sub(limit);
        let page: Vec<SessionEvent> = self
            .events
            .range(start..end)
            .map(|(e, _)| e.clone())
            .collect();
        (page, start > 0)
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
    /// Weak: a subscriber must not keep a removed session's log (and the
    /// sender of its channel) alive, or its stream would never end.
    log: std::sync::Weak<Mutex<EventLog>>,
    rx: broadcast::Receiver<SessionEvent>,
    next_seq: u64,
    pending: VecDeque<StreamItem>,
}

impl EventStream {
    /// Atomically replays events after `after` (`None`: no replay) and
    /// subscribes to everything that follows.
    pub fn open(log: &Arc<Mutex<EventLog>>, after: Option<u64>) -> (Replay, Self) {
        Self::open_latest(log, after, usize::MAX)
    }

    /// [`Self::open`] replaying only the newest `limit` events (see
    /// [`EventLog::replay_latest`]); the mutex is held for that copy only.
    pub fn open_latest(
        log: &Arc<Mutex<EventLog>>,
        after: Option<u64>,
        limit: usize,
    ) -> (Replay, Self) {
        let guard = lock(log);
        let rx = guard.subscribe();
        let replay = match after {
            Some(a) => guard.replay_latest(a, limit),
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
                log: Arc::downgrade(log),
                rx,
                next_seq,
                pending: VecDeque::new(),
            },
        )
    }

    /// Catches up from the log; false when the log is gone.
    fn resync(&mut self) -> bool {
        let Some(log) = self.log.upgrade() else {
            return false;
        };
        let guard = lock(&log);
        let replay = guard.replay_latest(self.next_seq.saturating_sub(1), RESYNC_EVENTS);
        drop(guard);
        if let Some(first) = replay.gap {
            self.pending.push_back(StreamItem::Gap(first));
            self.next_seq = first;
        }
        for ev in replay.events {
            self.next_seq = ev.seq() + 1;
            self.pending.push_back(StreamItem::Event(ev));
        }
        true
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
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    if !self.resync() {
                        return None;
                    }
                }
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

    fn say(log: &mut EventLog, text: &str) -> u64 {
        log.push(|seq| SessionEvent::Message {
            seq,
            at: 0,
            role: vca_core::MessageRole::Assistant,
            text: text.to_owned(),
        })
        .seq()
    }

    /// A managed agent's terminal repaints constantly; that must not push
    /// the conversation out of the buffer (the chat of such a session was
    /// empty, and loading older pages found only output).
    #[test]
    fn output_does_not_evict_messages() {
        let mut log = EventLog::new(LogLimits {
            max_events: 100,
            max_bytes: 1 << 20,
        });
        let first = say(&mut log, "hello");
        log.push(|seq| out(seq, 10));
        let last = say(&mut log, "world");
        for _ in 0..1000 {
            log.push(|seq| out(seq, 10));
        }
        assert_eq!(log.buffered(), 100);
        let (page, _) = log.page_before(None, 500);
        let kept: Vec<u64> = page
            .iter()
            .filter(|e| matches!(e, SessionEvent::Message { .. }))
            .map(SessionEvent::seq)
            .collect();
        assert_eq!(kept, [first, last], "both messages survived");
        let outputs: Vec<u64> = page
            .iter()
            .filter(|e| matches!(e, SessionEvent::Output { .. }))
            .map(SessionEvent::seq)
            .collect();
        assert_eq!(outputs.len(), 98);
        assert_eq!(*outputs.last().unwrap(), log.next_seq() - 1);
        assert!(outputs.windows(2).all(|w| w[1] == w[0] + 1));
        // A reader behind the output that was dropped is told to reload,
        // from the first event it is sent.
        assert_eq!(log.replay_after(last).gap, Some(outputs[0]));
        assert_eq!(log.replay_after(log.next_seq() - 1).gap, None);
    }

    /// A client that reconnects after output was evicted from the middle
    /// of the log gets a gap at the first event it is sent, so its history
    /// request reaches the messages still buffered before it. (A gap at the
    /// front, seq 1, told the app nothing older existed: the chat vanished.)
    #[test]
    fn a_gap_after_mid_log_eviction_lets_the_client_page_back() {
        let mut log = EventLog::new(LogLimits {
            max_events: 40,
            max_bytes: 1 << 20,
        });
        let first = say(&mut log, "hello");
        log.push(|seq| out(seq, 10));
        let second = say(&mut log, "world");
        for _ in 0..200 {
            log.push(|seq| out(seq, 10));
        }
        assert_eq!(log.first_available(), first);
        for replay in [
            log.replay_after(second),
            log.replay_latest(second, PAGE_EVENTS),
            log.replay_latest(0, 5),
        ] {
            let start = replay.events[0].seq();
            assert_eq!(replay.gap, Some(start));
            assert!(start > second);
            let (older, more) = log.page_before(replay.gap, MAX_PAGE_EVENTS);
            let messages: Vec<u64> = older
                .iter()
                .filter(|e| matches!(e, SessionEvent::Message { .. }))
                .map(SessionEvent::seq)
                .collect();
            assert_eq!(messages, [first, second]);
            assert!(!more);
        }
    }

    /// The event just pushed stays, even when it is the only output and
    /// output is what goes first.
    #[test]
    fn the_newest_event_is_never_evicted() {
        let mut log = EventLog::new(LogLimits {
            max_events: 100,
            max_bytes: 1000,
        });
        say(&mut log, "hello");
        let big = log.push(|seq| out(seq, 900)).seq();
        assert_eq!(log.page_before(None, 1).0[0].seq(), big);
        assert_eq!(log.replay_after(big - 1).events.len(), 1);
    }

    /// An exited session's log is cut like a live one: output first, so
    /// the chat of the ended card is not empty.
    #[test]
    fn trimming_an_exited_log_keeps_the_conversation() {
        let mut log = EventLog::new(LogLimits::default());
        let messages: Vec<u64> = (0..30).map(|i| say(&mut log, &format!("m{i}"))).collect();
        for _ in 0..600 {
            log.push(|seq| out(seq, 1024));
        }
        log.trim(500, 512 * 1024);
        assert!(log.bytes <= 512 * 1024 && log.buffered() <= 500);
        let (page, _) = log.page_before(None, MAX_PAGE_EVENTS);
        let kept: Vec<u64> = page
            .iter()
            .filter(|e| matches!(e, SessionEvent::Message { .. }))
            .map(SessionEvent::seq)
            .collect();
        assert_eq!(kept, messages);
        assert_eq!(page.last().map(SessionEvent::seq), Some(log.next_seq() - 1));
    }

    /// Past half of the limits messages are evicted like any event, oldest
    /// first, so a long conversation cannot take the whole buffer.
    #[test]
    fn messages_are_bounded_too() {
        let mut log = EventLog::new(LogLimits {
            max_events: 10,
            max_bytes: 1 << 20,
        });
        let seqs: Vec<u64> = (0..20).map(|i| say(&mut log, &format!("m{i}"))).collect();
        assert_eq!(log.buffered(), 10);
        assert_eq!(log.first_available(), seqs[10]);
        log.push(|seq| out(seq, 1));
        assert_eq!(log.buffered(), 10);
    }

    #[test]
    fn one_event_never_holds_more_than_the_cap() {
        let mut log = EventLog::new(LogLimits::default());
        let mut big = vec![b'a'; MAX_EVENT_BYTES];
        big.extend_from_slice(b"END");
        let ev = log.push(|seq| SessionEvent::Output {
            seq,
            at: 0,
            data: big.clone(),
        });
        let SessionEvent::Output { data, .. } = ev else {
            panic!()
        };
        assert_eq!(data.len(), MAX_EVENT_BYTES);
        assert!(data.ends_with(b"END"), "output keeps its end");
        let text = "가".repeat(MAX_EVENT_BYTES);
        let ev = log.push(|seq| SessionEvent::Message {
            seq,
            at: 0,
            role: vca_core::MessageRole::Assistant,
            text: text.clone(),
        });
        let SessionEvent::Message { text: kept, .. } = ev else {
            panic!()
        };
        assert!(kept.len() <= MAX_EVENT_BYTES);
        assert!(kept.starts_with('가') && kept.ends_with(TRUNCATED));
    }

    #[test]
    fn question_forms_count_toward_the_byte_limit() {
        let question = vca_core::Question {
            header: String::new(),
            question: "q".repeat(10_000),
            options: vec![vca_core::QuestionOption {
                label: "l".into(),
                description: "d".repeat(10_000),
            }],
            multi_select: false,
            free_text: false,
        };
        let ev = SessionEvent::DecisionRequested {
            seq: 1,
            at: 0,
            decision: vca_core::Decision {
                id: "d".into(),
                prompt: String::new(),
                options: Vec::new(),
                inferred: false,
                questions: vec![question],
                command: None,
            },
        };
        assert!(event_size(&ev) > 20_000);
    }

    #[test]
    fn the_buffer_gives_memory_back_after_a_burst() {
        let mut log = EventLog::new(LogLimits {
            max_events: 5000,
            max_bytes: 64 * 1024,
        });
        for _ in 0..4000 {
            log.push(|seq| out(seq, 1));
        }
        for _ in 0..10 {
            log.push(|seq| out(seq, 16 * 1024));
        }
        assert!(log.buffered() <= 10);
        assert!(log.events.capacity() <= 256, "{}", log.events.capacity());
    }

    #[tokio::test]
    async fn a_stream_ends_when_its_log_is_dropped() {
        let log = Arc::new(Mutex::new(EventLog::new(LogLimits::default())));
        let (_, mut stream) = EventStream::open(&log, None);
        lock(&log).push(|seq| out(seq, 1));
        assert!(matches!(stream.recv().await, Some(StreamItem::Event(_))));
        // The session is removed: its log goes, and the subscriber's task
        // ends instead of holding the log forever.
        drop(log);
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
            .await
            .expect("the stream ends");
        assert!(next.is_none());
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

    #[test]
    fn resume_ahead_of_the_log_reloads_everything() {
        // The client last saw seq 4000; vcad restarted and is at seq 4.
        let mut log = EventLog::new(LogLimits::default());
        for _ in 0..3 {
            log.push(|s| out(s, 1));
        }
        let r = log.replay_after(4000);
        assert_eq!(r.gap, Some(1));
        assert_eq!(
            r.events.iter().map(SessionEvent::seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // Being exactly up to date is not a gap.
        let r = log.replay_after(3);
        assert_eq!(r.gap, None);
        assert!(r.events.is_empty());
        // An empty log has nothing to reload, but the client must reset.
        let empty = EventLog::new(LogLimits::default());
        assert_eq!(empty.replay_after(7).gap, Some(1));
    }

    #[test]
    fn pages_walk_back_through_the_buffer() {
        let mut log = EventLog::new(LogLimits::default());
        for _ in 0..25 {
            log.push(|s| out(s, 1));
        }
        let seqs = |v: &[SessionEvent]| v.iter().map(SessionEvent::seq).collect::<Vec<_>>();
        let (page, more) = log.page_before(None, 10);
        assert_eq!(seqs(&page), (16..=25).collect::<Vec<_>>());
        assert!(more);
        let (page, more) = log.page_before(Some(16), 10);
        assert_eq!(seqs(&page), (6..=15).collect::<Vec<_>>());
        assert!(more);
        let (page, more) = log.page_before(Some(6), 10);
        assert_eq!(seqs(&page), (1..=5).collect::<Vec<_>>());
        assert!(!more, "reached the beginning");
        let (page, more) = log.page_before(Some(1), 10);
        assert!(page.is_empty() && !more);
        // Eviction moves the beginning: nothing older than the buffer.
        let mut small = EventLog::new(LogLimits {
            max_events: 8,
            max_bytes: 1 << 20,
        });
        for _ in 0..20 {
            small.push(|s| out(s, 1));
        }
        let (page, more) = small.page_before(None, 100);
        assert_eq!(seqs(&page), (13..=20).collect::<Vec<_>>());
        assert!(!more);
    }

    #[test]
    fn limited_replays_match_latest_pages_and_trim_frees() {
        let mut log = EventLog::new(LogLimits {
            max_events: 100,
            max_bytes: usize::MAX,
        });
        for _ in 0..250 {
            log.push(|s| out(s, 1));
        }
        for after in [0, 100, 149, 150, 200, 240, 250, 400] {
            assert_eq!(
                log.replay_latest(after, PAGE_EVENTS),
                log.replay_after(after).latest_page(PAGE_EVENTS),
                "after {after}"
            );
        }
        log.trim(10, usize::MAX);
        assert_eq!(log.buffered(), 10);
        assert_eq!(log.first_available(), 241);
        assert_eq!(log.next_seq(), 251);
    }

    #[test]
    fn first_subscription_gets_only_the_latest_page() {
        let mut log = EventLog::new(LogLimits::default());
        for _ in 0..200 {
            log.push(|s| out(s, 1));
        }
        // A new client: everything after seq 0, cut to the newest page.
        let r = log.replay_after(0).latest_page(PAGE_EVENTS);
        assert_eq!(r.events.len(), PAGE_EVENTS);
        assert_eq!(r.events.last().map(SessionEvent::seq), Some(200));
        assert_eq!(r.gap, Some(200 - PAGE_EVENTS as u64 + 1));
        // A client that is nearly up to date is not cut and sees no gap.
        let r = log.replay_after(195).latest_page(PAGE_EVENTS);
        assert_eq!(r.events.len(), 5);
        assert_eq!(r.gap, None);
        // A short log is replayed whole, as before.
        let mut short = EventLog::new(LogLimits::default());
        short.push(|s| out(s, 1));
        let r = short.replay_after(0).latest_page(PAGE_EVENTS);
        assert_eq!((r.events.len(), r.gap), (1, None));
    }

    /// `VCA_MEASURE_FILE=<transcript> cargo test -p vca-server measure_first_page
    /// -- --ignored --nocapture`: what a new subscriber is sent, before (the
    /// whole buffer) and after (the newest page).
    #[test]
    #[ignore = "measurement, not a check"]
    fn measure_first_page() {
        let path = std::env::var_os("VCA_MEASURE_FILE").expect("set VCA_MEASURE_FILE");
        let started = std::time::Instant::now();
        let items = vca_adapters::tail::read_all(
            std::path::Path::new(&path),
            vca_adapters::claude::transcript::parse_line,
        )
        .unwrap();
        let read = started.elapsed();
        let mut log = EventLog::new(LogLimits::default());
        for item in items
            .iter()
            .filter(|i| !matches!(i, vca_adapters::types::HistoryItem::ToolResult { .. }))
            .filter(|i| i.to_event(0, 0).is_some())
        {
            log.push(|seq| item.to_event(seq, 0).unwrap());
        }
        let total = log.next_seq() - 1;
        let size = |events: &[SessionEvent]| {
            events
                .iter()
                .map(|e| serde_json::to_vec(e).unwrap().len())
                .sum::<usize>()
        };
        let t = std::time::Instant::now();
        let all = log.replay_after(0);
        let all_bytes = size(&all.events);
        let before = t.elapsed();
        let t = std::time::Instant::now();
        let page = log.replay_after(0).latest_page(PAGE_EVENTS);
        let page_bytes = size(&page.events);
        let after = t.elapsed();
        println!(
            "read+parse {read:?}; {total} events pushed, {} buffered
             before: {} events, {all_bytes} bytes, prepared in {before:?}
             after:  {} events, {page_bytes} bytes, prepared in {after:?}",
            all.events.len(),
            all.events.len(),
            page.events.len()
        );
    }

    #[tokio::test]
    async fn live_events_follow_the_first_page_without_a_hole() {
        let log = Arc::new(Mutex::new(EventLog::new(LogLimits::default())));
        for _ in 0..150 {
            lock(&log).push(|s| out(s, 1));
        }
        // An old client sends no History request: it just gets this page.
        let (replay, mut stream) = EventStream::open(&log, Some(0));
        let replay = replay.latest_page(PAGE_EVENTS);
        assert_eq!(replay.events.len(), PAGE_EVENTS);
        assert_eq!(replay.gap, Some(91));
        assert_eq!(replay.events[0].seq(), 91);
        lock(&log).push(|s| out(s, 1));
        assert_eq!(stream.recv().await, Some(StreamItem::Event(out(151, 1))));
        // A newer client walks back from the page start and meets no overlap.
        let (older, more) = lock(&log).page_before(Some(91), 100);
        assert_eq!(older.len(), 90);
        assert_eq!(older.last().map(SessionEvent::seq), Some(90));
        assert!(!more);
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
