//! Polling helper: turns activity pages into [`SessionEvent`]s with stable
//! sequence numbers, with exponential backoff.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use vca_core::{SessionEvent, SessionState, now_millis};

use crate::adapter::CloudAgentAdapter;
use crate::credential::Credential;
use crate::error::{CloudError, Result};
use crate::types::{ActivityKind, CloudSessionId, Cursor};

/// Upper bound on pages fetched in one poll, so a huge backlog cannot
/// starve other work.
const MAX_PAGES_PER_POLL: usize = 50;

/// Exponential backoff between polls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backoff {
    /// Delay after a poll that produced new events.
    pub min: Duration,
    /// Upper bound for the delay.
    pub max: Duration,
    /// Growth factor after a poll without new events.
    pub factor: f64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            min: Duration::from_secs(5),
            max: Duration::from_secs(120),
            factor: 2.0,
        }
    }
}

impl Backoff {
    fn grow(&self, current: Duration) -> Duration {
        let next = current.mul_f64(self.factor.max(1.0));
        next.clamp(self.min, self.max)
    }
}

/// Persistable poller position, so that sequence numbers stay stable
/// across server restarts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PollerState {
    pub cursor: Option<Cursor>,
    /// Sequence number the next event will get.
    pub next_seq: u64,
    /// Activity IDs already turned into events.
    pub seen: Vec<String>,
    pub last_state: Option<SessionState>,
}

/// Update delivered by [`ActivityPoller::run`].
#[derive(Debug)]
pub enum PollUpdate {
    Events(Vec<SessionEvent>),
    /// A transient error; polling continues after a backoff.
    TransientError(CloudError),
    /// Polling ended: the session reached a terminal state (`None`) or a
    /// permanent error occurred.
    Finished(Option<CloudError>),
}

/// Polls one cloud session.
pub struct ActivityPoller {
    adapter: Arc<dyn CloudAgentAdapter>,
    cred: Credential,
    id: CloudSessionId,
    cursor: Option<Cursor>,
    seen: HashSet<String>,
    next_seq: u64,
    last_state: Option<SessionState>,
    backoff: Backoff,
    delay: Duration,
}

impl std::fmt::Debug for ActivityPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityPoller")
            .field("provider", &self.adapter.provider())
            .field("id", &self.id)
            .field("next_seq", &self.next_seq)
            .finish_non_exhaustive()
    }
}

impl ActivityPoller {
    /// Creates a poller whose first event gets `first_seq`.
    pub fn new(
        adapter: Arc<dyn CloudAgentAdapter>,
        cred: Credential,
        id: CloudSessionId,
        first_seq: u64,
    ) -> Self {
        Self::resume(
            adapter,
            cred,
            id,
            PollerState {
                next_seq: first_seq,
                ..PollerState::default()
            },
        )
    }

    /// Restores a poller from a saved [`PollerState`].
    pub fn resume(
        adapter: Arc<dyn CloudAgentAdapter>,
        cred: Credential,
        id: CloudSessionId,
        state: PollerState,
    ) -> Self {
        let backoff = Backoff::default();
        Self {
            adapter,
            cred,
            id,
            cursor: state.cursor,
            seen: state.seen.into_iter().collect(),
            next_seq: state.next_seq,
            last_state: state.last_state,
            backoff,
            delay: backoff.min,
        }
    }

    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self.delay = backoff.min;
        self
    }

    /// Snapshot of the position, for persistence.
    pub fn state(&self) -> PollerState {
        let mut seen: Vec<String> = self.seen.iter().cloned().collect();
        seen.sort();
        PollerState {
            cursor: self.cursor.clone(),
            next_seq: self.next_seq,
            seen,
            last_state: self.last_state.clone(),
        }
    }

    /// Latest state seen in the activity feed.
    pub fn last_state(&self) -> Option<&SessionState> {
        self.last_state.as_ref()
    }

    /// True once the session reached a terminal state.
    pub fn is_finished(&self) -> bool {
        matches!(
            self.last_state,
            Some(SessionState::Exited { .. } | SessionState::Error { .. })
        )
    }

    /// Fetches all new activity and returns it as events. Items already
    /// seen are skipped; consecutive identical states are collapsed.
    pub async fn poll_once(&mut self) -> Result<Vec<SessionEvent>> {
        let mut events = Vec::new();
        for _ in 0..MAX_PAGES_PER_POLL {
            let page = self
                .adapter
                .activity(&self.cred, &self.id, self.cursor.clone())
                .await?;
            for activity in &page.activities {
                if !self.seen.insert(activity.id.clone()) {
                    continue;
                }
                if let ActivityKind::StateChanged { state } = &activity.kind {
                    if self.last_state.as_ref() == Some(state) {
                        continue;
                    }
                    self.last_state = Some(state.clone());
                }
                events.push(activity.to_event(self.take_seq()));
            }
            if let Some(state) = page.state
                && self.last_state.as_ref() != Some(&state)
            {
                self.last_state = Some(state.clone());
                let seq = self.take_seq();
                events.push(SessionEvent::StateChanged {
                    seq,
                    at: now_millis(),
                    state,
                });
            }
            if page.next.is_some() {
                self.cursor = page.next;
            }
            if !page.has_more {
                break;
            }
        }
        Ok(events)
    }

    fn take_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Delay before the next poll, given the outcome of the last one.
    pub fn next_delay(&mut self, outcome: &Result<Vec<SessionEvent>>) -> Duration {
        self.delay = match outcome {
            Ok(events) if !events.is_empty() => self.backoff.min,
            Ok(_) => self.backoff.grow(self.delay),
            Err(CloudError::RateLimited {
                retry_after: Some(d),
            }) => (*d).max(self.backoff.grow(self.delay)),
            Err(_) => self.backoff.grow(self.delay),
        };
        self.delay
    }

    /// Polls until the session ends, a permanent error occurs, or the
    /// receiver is dropped.
    pub async fn run(mut self, tx: mpsc::Sender<PollUpdate>) {
        loop {
            let outcome = self.poll_once().await;
            let delay = self.next_delay(&outcome);
            let update = match outcome {
                Ok(events) if events.is_empty() => None,
                Ok(events) => Some(PollUpdate::Events(events)),
                Err(e) if e.is_transient() => Some(PollUpdate::TransientError(e)),
                Err(e) => {
                    let _ = tx.send(PollUpdate::Finished(Some(e))).await;
                    return;
                }
            };
            if let Some(u) = update
                && tx.send(u).await.is_err()
            {
                return;
            }
            if self.is_finished() {
                let _ = tx.send(PollUpdate::Finished(None)).await;
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = tx.closed() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Activity, ActivityPage, CloudCapabilities, CloudLaunch, CloudSession};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use vca_core::{CloudProvider, MessageRole};

    /// Adapter that serves scripted pages.
    struct Scripted {
        pages: Mutex<Vec<Result<ActivityPage>>>,
        cursors: Mutex<Vec<Option<Cursor>>>,
    }

    #[async_trait]
    impl CloudAgentAdapter for Scripted {
        fn provider(&self) -> CloudProvider {
            CloudProvider::Jules
        }
        fn capabilities(&self) -> CloudCapabilities {
            CloudCapabilities::default()
        }
        async fn list_sessions(&self, _: &Credential) -> Result<Vec<CloudSession>> {
            Ok(vec![])
        }
        async fn get_session(&self, _: &Credential, _: &CloudSessionId) -> Result<CloudSession> {
            Err(CloudError::Unsupported("get"))
        }
        async fn create_session(&self, _: &Credential, _: &CloudLaunch) -> Result<CloudSession> {
            Err(CloudError::Unsupported("create"))
        }
        async fn send_message(&self, _: &Credential, _: &CloudSessionId, _: &str) -> Result<()> {
            Ok(())
        }
        async fn activity(
            &self,
            _: &Credential,
            _: &CloudSessionId,
            since: Option<Cursor>,
        ) -> Result<ActivityPage> {
            self.cursors.lock().unwrap().push(since);
            let mut pages = self.pages.lock().unwrap();
            if pages.is_empty() {
                Ok(ActivityPage::default())
            } else {
                pages.remove(0)
            }
        }
    }

    fn msg(id: &str, text: &str) -> Activity {
        Activity::message(id, 1, MessageRole::Assistant, text)
    }

    #[tokio::test]
    async fn dedupes_and_assigns_stable_seq() {
        let adapter = Arc::new(Scripted {
            pages: Mutex::new(vec![
                Ok(ActivityPage {
                    activities: vec![msg("a", "one")],
                    next: Some(Cursor("p2".into())),
                    has_more: true,
                    state: None,
                }),
                Ok(ActivityPage {
                    activities: vec![msg("a", "one"), msg("b", "two")],
                    next: Some(Cursor("p2".into())),
                    has_more: false,
                    state: Some(SessionState::Running),
                }),
                Ok(ActivityPage {
                    activities: vec![msg("b", "two"), msg("c", "three")],
                    next: None,
                    has_more: false,
                    state: Some(SessionState::Running),
                }),
            ]),
            cursors: Mutex::new(vec![]),
        });
        let mut p = ActivityPoller::new(adapter.clone(), Credential::new("k"), "s".into(), 10);
        let first = p.poll_once().await.unwrap();
        let seqs: Vec<u64> = first.iter().map(SessionEvent::seq).collect();
        assert_eq!(seqs, vec![10, 11, 12]);
        assert!(matches!(first[2], SessionEvent::StateChanged { .. }));
        let second = p.poll_once().await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].seq(), 13);
        // The cursor is kept when a page carries none.
        assert_eq!(p.state().cursor, Some(Cursor("p2".into())));
        assert_eq!(
            adapter.cursors.lock().unwrap().clone(),
            vec![None, Some(Cursor("p2".into())), Some(Cursor("p2".into()))]
        );

        // A resumed poller continues the numbering and skips seen items.
        let saved = p.state();
        let adapter2 = Arc::new(Scripted {
            pages: Mutex::new(vec![Ok(ActivityPage {
                activities: vec![msg("c", "three"), msg("d", "four")],
                ..ActivityPage::default()
            })]),
            cursors: Mutex::new(vec![]),
        });
        let mut p2 = ActivityPoller::resume(adapter2, Credential::new("k"), "s".into(), saved);
        let ev = p2.poll_once().await.unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].seq(), 14);
    }

    #[test]
    fn backoff_grows_and_resets() {
        let adapter = Arc::new(Scripted {
            pages: Mutex::new(vec![]),
            cursors: Mutex::new(vec![]),
        });
        let mut p =
            ActivityPoller::new(adapter, Credential::none(), "s".into(), 0).with_backoff(Backoff {
                min: Duration::from_secs(1),
                max: Duration::from_secs(5),
                factor: 2.0,
            });
        assert_eq!(p.next_delay(&Ok(vec![])), Duration::from_secs(2));
        assert_eq!(p.next_delay(&Ok(vec![])), Duration::from_secs(4));
        assert_eq!(p.next_delay(&Ok(vec![])), Duration::from_secs(5));
        let ev = SessionEvent::Notification {
            seq: 0,
            at: 0,
            title: String::new(),
            body: String::new(),
        };
        assert_eq!(p.next_delay(&Ok(vec![ev])), Duration::from_secs(1));
        assert_eq!(
            p.next_delay(&Err(CloudError::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            })),
            Duration::from_secs(30)
        );
    }

    #[tokio::test]
    async fn run_stops_at_terminal_state() {
        let adapter = Arc::new(Scripted {
            pages: Mutex::new(vec![Ok(ActivityPage {
                activities: vec![
                    msg("a", "done"),
                    Activity::state("st", 2, SessionState::Exited { code: Some(0) }),
                ],
                ..ActivityPage::default()
            })]),
            cursors: Mutex::new(vec![]),
        });
        let (tx, mut rx) = mpsc::channel(8);
        let p = ActivityPoller::new(adapter, Credential::new("k"), "s".into(), 0);
        tokio::spawn(p.run(tx));
        match rx.recv().await.unwrap() {
            PollUpdate::Events(ev) => assert_eq!(ev.len(), 2),
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            rx.recv().await.unwrap(),
            PollUpdate::Finished(None)
        ));
    }

    #[tokio::test]
    async fn run_reports_permanent_error() {
        let adapter = Arc::new(Scripted {
            pages: Mutex::new(vec![Err(CloudError::Auth("bad key".into()))]),
            cursors: Mutex::new(vec![]),
        });
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(ActivityPoller::new(adapter, Credential::new("k"), "s".into(), 0).run(tx));
        assert!(matches!(
            rx.recv().await.unwrap(),
            PollUpdate::Finished(Some(CloudError::Auth(_)))
        ));
    }
}
