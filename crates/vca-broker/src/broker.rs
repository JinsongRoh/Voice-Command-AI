//! The [`Broker`] facade: monitor + admission + policy + containment, with a
//! threshold watcher that applies policy actions and reports what it did.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use vca_core::{ResourceSnapshot, SessionId};

use crate::admission::{AdmissionController, AdmissionEvent, Liveness, MachineInfo, SysLiveness};
use crate::clock::{Clock, LoadSample, LoadSource, SystemClock};
use crate::contain::{ContainError, OsProcessControl, Priority, ProcessControl, Scope};
use crate::ipc::{ConnectionOptions, IpcEndpoint, IpcServer};
use crate::monitor::{Monitor, MonitorConfig, QueueProvider, SessionRegistry};
use crate::policy::{Policy, ThresholdAction};
use crate::watcher::{AlertLevel, Decision, ThresholdWatcher};

/// Broker settings.
#[derive(Debug, Clone)]
pub struct BrokerConfig {
    pub policy: Policy,
    pub monitor: MonitorConfig,
    /// Time between `SIGTERM` and `SIGKILL` when terminating.
    pub terminate_grace: Duration,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            policy: Policy::default(),
            monitor: MonitorConfig::default(),
            terminate_grace: Duration::from_secs(5),
        }
    }
}

/// What was done (or proposed) to a session's processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Deprioritize,
    RestorePriority,
    PauseChildren,
    ResumeChildren,
    ProposeStop,
    Pause,
    Resume,
    Terminate,
}

/// Events for the UI and logs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrokerEvent {
    Admission {
        event: AdmissionEvent,
    },
    /// A containment action. `applied = false` means it was only proposed
    /// (manual mode, dry run, or a stop proposal).
    Action {
        session_id: SessionId,
        action: ActionKind,
        pids: Vec<u32>,
        applied: bool,
        dry_run: bool,
        reason: String,
        error: Option<String>,
    },
    Alert {
        level: AlertLevel,
        message: String,
    },
    PolicyChanged,
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("unknown session {0}")]
    UnknownSession(SessionId),
    #[error(transparent)]
    Contain(#[from] ContainError),
    #[error("process control task failed: {0}")]
    Task(String),
}

struct Inner {
    policy: Mutex<Policy>,
    admission: Arc<AdmissionController>,
    sessions: SessionRegistry,
    control: Arc<dyn ProcessControl>,
    clock: Arc<dyn Clock>,
    events: broadcast::Sender<BrokerEvent>,
    foreground: Mutex<Option<SessionId>>,
    watcher: Mutex<ThresholdWatcher>,
    terminate_grace: Duration,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The resource broker. Dropping it stops its background tasks.
pub struct Broker {
    inner: Arc<Inner>,
    monitor: Option<Monitor>,
    tasks: Vec<JoinHandle<()>>,
}

impl Broker {
    /// Starts the broker with the OS process control: a monitor thread, the
    /// admission controller fed by its smoothed load, and a driver task that
    /// ticks admission and runs the threshold watcher on every sample.
    /// Must be called inside a Tokio runtime.
    pub fn start(config: BrokerConfig) -> Self {
        Self::start_with_control(config, Arc::new(OsProcessControl))
    }

    /// Like [`Broker::start`] with a custom process control (for example a
    /// recording fake).
    pub fn start_with_control(config: BrokerConfig, control: Arc<dyn ProcessControl>) -> Self {
        let sessions = SessionRegistry::default();
        // The admission controller is created first so the monitor can
        // include its queue; it reads load from the monitor through a watch
        // channel that the monitor feeds.
        let (load_tx, load_rx) = watch::channel(LoadSample::default());
        let mut broker = Self::assemble(
            config.clone(),
            control,
            Arc::new(SystemClock),
            Arc::new(load_rx),
            Arc::new(SysLiveness::default()),
            sessions.clone(),
        );
        let admission = broker.inner.admission.clone();
        let queue: QueueProvider = Arc::new(move || admission.queue());
        let monitor = Monitor::spawn(config.monitor, sessions, Some(queue));

        let mut snapshots = monitor.subscribe();
        let monitor_load = monitor.load();
        let inner = broker.inner.clone();
        broker.tasks.push(tokio::spawn(async move {
            while snapshots.changed().await.is_ok() {
                let snapshot = snapshots.borrow_and_update().clone();
                let load = *monitor_load.borrow();
                load_tx.send_replace(load);
                inner.admission.tick();
                inner.process_sample(&snapshot, load).await;
            }
        }));
        broker.monitor = Some(monitor);
        broker
    }

    /// Builds a broker without a monitor or driver task. The caller feeds
    /// samples with [`Broker::process_sample`] and calls
    /// `admission().tick()`. Useful for tests and embedding.
    pub fn manual(
        config: BrokerConfig,
        control: Arc<dyn ProcessControl>,
        clock: Arc<dyn Clock>,
        load: Arc<dyn LoadSource>,
        liveness: Arc<dyn Liveness>,
    ) -> Self {
        Self::assemble(
            config,
            control,
            clock,
            load,
            liveness,
            SessionRegistry::default(),
        )
    }

    fn assemble(
        config: BrokerConfig,
        control: Arc<dyn ProcessControl>,
        clock: Arc<dyn Clock>,
        load: Arc<dyn LoadSource>,
        liveness: Arc<dyn Liveness>,
        sessions: SessionRegistry,
    ) -> Self {
        let admission = Arc::new(AdmissionController::new(
            config.policy.clone(),
            MachineInfo::detect(),
            clock.clone(),
            load,
            liveness,
        ));
        let (events, _) = broadcast::channel(512);
        let inner = Arc::new(Inner {
            policy: Mutex::new(config.policy),
            admission: admission.clone(),
            sessions,
            control,
            clock,
            events: events.clone(),
            foreground: Mutex::new(None),
            watcher: Mutex::new(ThresholdWatcher::new()),
            terminate_grace: config.terminate_grace,
        });
        let mut admission_events = admission.subscribe_events();
        let forward = tokio::spawn(async move {
            loop {
                match admission_events.recv().await {
                    Ok(event) => {
                        let _ = events.send(BrokerEvent::Admission { event });
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("dropped {n} admission events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Self {
            inner,
            monitor: None,
            tasks: vec![forward],
        }
    }

    /// The admission controller (for requests, queue edits, and IPC).
    pub fn admission(&self) -> &Arc<AdmissionController> {
        &self.inner.admission
    }

    /// Registers the root process (usually the agent) of a session so its
    /// tree is monitored and can be contained.
    pub fn register_session(&self, session_id: SessionId, root_pid: u32) {
        self.inner.sessions.register(session_id, root_pid);
    }

    pub fn unregister_session(&self, session_id: &SessionId) {
        self.inner.sessions.unregister(session_id);
        lock(&self.inner.watcher).forget(session_id);
    }

    /// Marks the session the user is looking at; it is admitted first and
    /// never paused automatically.
    pub fn set_foreground(&self, session_id: Option<SessionId>) {
        *lock(&self.inner.foreground) = session_id.clone();
        self.inner.admission.set_foreground(session_id);
    }

    pub fn policy(&self) -> Policy {
        lock(&self.inner.policy).clone()
    }

    /// Replaces the policy (already validated).
    pub fn set_policy(&self, policy: Policy) {
        *lock(&self.inner.policy) = policy.clone();
        self.inner.admission.set_policy(policy);
        let _ = self.inner.events.send(BrokerEvent::PolicyChanged);
    }

    /// The latest snapshot with the current queue. Without a monitor (see
    /// [`Broker::manual`]) only the queue is filled in.
    pub fn snapshot(&self) -> ResourceSnapshot {
        let mut snap = match &self.monitor {
            Some(m) => m.snapshot(),
            None => ResourceSnapshot {
                at: self.inner.clock.now(),
                cpu_percent: 0.0,
                memory_used_bytes: 0,
                memory_total_bytes: 0,
                disk_read_bytes_per_sec: 0,
                disk_write_bytes_per_sec: 0,
                net_rx_bytes_per_sec: 0,
                net_tx_bytes_per_sec: 0,
                sessions: Vec::new(),
                queue: Vec::new(),
            },
        };
        snap.queue = self.inner.admission.queue();
        snap
    }

    /// Snapshot updates (one per monitor interval), if a monitor runs.
    pub fn subscribe_snapshots(&self) -> Option<watch::Receiver<ResourceSnapshot>> {
        self.monitor.as_ref().map(Monitor::subscribe)
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<BrokerEvent> {
        self.inner.events.subscribe()
    }

    /// Feeds one sample to the threshold watcher and applies its decisions.
    pub async fn process_sample(&self, snapshot: &ResourceSnapshot, load: LoadSample) {
        self.inner.process_sample(snapshot, load).await;
    }

    /// Starts serving hook requests on `endpoint`.
    pub fn serve_ipc(&mut self, endpoint: IpcEndpoint) -> std::io::Result<()> {
        let wait = Duration::from_secs(self.policy().admission.queue_timeout_secs);
        let server = IpcServer::bind(endpoint, self.inner.admission.clone())?.with_options(
            ConnectionOptions {
                default_wait: wait,
                ..ConnectionOptions::default()
            },
        );
        self.tasks.push(server.spawn());
        Ok(())
    }

    /// Suspends a session's processes (children only by default, RES-07).
    pub async fn pause_session(
        &self,
        session_id: &SessionId,
        scope: Scope,
    ) -> Result<Vec<u32>, BrokerError> {
        self.inner
            .manual_action(session_id, ActionKind::Pause, move |c, pid| {
                c.suspend(pid, scope)
            })
            .await
    }

    pub async fn resume_session(
        &self,
        session_id: &SessionId,
        scope: Scope,
    ) -> Result<Vec<u32>, BrokerError> {
        lock(&self.inner.watcher).forget(session_id);
        self.inner
            .manual_action(session_id, ActionKind::Resume, move |c, pid| {
                c.resume(pid, scope)
            })
            .await
    }

    /// Terminates a session's processes: `SIGTERM`, then `SIGKILL` after the
    /// configured grace period.
    pub async fn terminate_session(
        &self,
        session_id: &SessionId,
        scope: Scope,
    ) -> Result<Vec<u32>, BrokerError> {
        let grace = self.inner.terminate_grace;
        self.inner
            .manual_action(session_id, ActionKind::Terminate, move |c, pid| {
                c.terminate(pid, scope, grace)
            })
            .await
    }

    pub async fn set_session_priority(
        &self,
        session_id: &SessionId,
        scope: Scope,
        priority: Priority,
    ) -> Result<Vec<u32>, BrokerError> {
        let kind = if priority == Priority::Normal {
            ActionKind::RestorePriority
        } else {
            ActionKind::Deprioritize
        };
        self.inner
            .manual_action(session_id, kind, move |c, pid| {
                c.set_priority(pid, scope, priority)
            })
            .await
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

type ControlOp = dyn FnOnce(&dyn ProcessControl, u32) -> Result<Vec<u32>, ContainError> + Send;

impl Inner {
    async fn run_control(&self, root: u32, op: Box<ControlOp>) -> Result<Vec<u32>, BrokerError> {
        let control = self.control.clone();
        tokio::task::spawn_blocking(move || op(control.as_ref(), root))
            .await
            .map_err(|e| BrokerError::Task(e.to_string()))?
            .map_err(BrokerError::from)
    }

    async fn manual_action(
        &self,
        session_id: &SessionId,
        action: ActionKind,
        op: impl FnOnce(&dyn ProcessControl, u32) -> Result<Vec<u32>, ContainError> + Send + 'static,
    ) -> Result<Vec<u32>, BrokerError> {
        let root = self
            .sessions
            .root_pid(session_id)
            .ok_or_else(|| BrokerError::UnknownSession(session_id.clone()))?;
        let result = self.run_control(root, Box::new(op)).await;
        let _ = self.events.send(BrokerEvent::Action {
            session_id: session_id.clone(),
            action,
            pids: result.as_ref().map(Clone::clone).unwrap_or_default(),
            applied: result.is_ok(),
            dry_run: false,
            reason: "requested by user".into(),
            error: result.as_ref().err().map(ToString::to_string),
        });
        result
    }

    async fn process_sample(&self, snapshot: &ResourceSnapshot, load: LoadSample) {
        let policy = lock(&self.policy).clone();
        let foreground = lock(&self.foreground).clone();
        let decisions = lock(&self.watcher).step(
            &policy,
            self.clock.now(),
            load,
            &snapshot.sessions,
            foreground.as_ref(),
        );
        for decision in decisions {
            self.apply(&policy, decision).await;
        }
    }

    async fn apply(&self, policy: &Policy, decision: Decision) {
        let (session_id, kind, reason): (SessionId, ActionKind, String) = match decision {
            Decision::Alert { level, message } => {
                tracing::info!(?level, "{message}");
                let _ = self.events.send(BrokerEvent::Alert { level, message });
                return;
            }
            Decision::Act {
                session_id,
                action,
                reason,
            } => {
                let kind = match action {
                    ThresholdAction::None => return,
                    ThresholdAction::Deprioritize => ActionKind::Deprioritize,
                    ThresholdAction::PauseChildren => ActionKind::PauseChildren,
                    ThresholdAction::ProposeStop => ActionKind::ProposeStop,
                };
                (session_id, kind, reason)
            }
            Decision::Restore { session_id, action } => {
                let kind = match action {
                    ThresholdAction::Deprioritize => ActionKind::RestorePriority,
                    ThresholdAction::PauseChildren => ActionKind::ResumeChildren,
                    ThresholdAction::None | ThresholdAction::ProposeStop => return,
                };
                (session_id, kind, "load is back below the threshold".into())
            }
        };
        let executable = !matches!(kind, ActionKind::ProposeStop);
        let apply = executable && policy.autonomous && !policy.dry_run;
        let root = self.sessions.root_pid(&session_id);
        let (pids, error) = match (apply, root) {
            (true, Some(root)) => {
                let op: Box<ControlOp> = match kind {
                    ActionKind::Deprioritize => {
                        Box::new(|c, pid| c.set_priority(pid, Scope::ChildrenOnly, Priority::Low))
                    }
                    ActionKind::RestorePriority => Box::new(|c, pid| {
                        c.set_priority(pid, Scope::ChildrenOnly, Priority::Normal)
                    }),
                    ActionKind::PauseChildren => {
                        Box::new(|c, pid| c.suspend(pid, Scope::ChildrenOnly))
                    }
                    _ => Box::new(|c, pid| c.resume(pid, Scope::ChildrenOnly)),
                };
                match self.run_control(root, op).await {
                    Ok(pids) => (pids, None),
                    Err(e) => (Vec::new(), Some(e.to_string())),
                }
            }
            (true, None) => (Vec::new(), Some(format!("unknown session {session_id}"))),
            (false, _) => (Vec::new(), None),
        };
        let applied = apply && error.is_none();
        tracing::info!(
            session = %session_id, action = ?kind, applied, dry_run = policy.dry_run, "{reason}"
        );
        let _ = self.events.send(BrokerEvent::Action {
            session_id,
            action: kind,
            pids,
            applied,
            dry_run: policy.dry_run,
            reason,
            error,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{FixedLoad, ManualClock};
    use crate::contain::ContainResult;
    use vca_core::SessionUsage;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl ProcessControl for Recorder {
        fn set_priority(&self, root: u32, scope: Scope, p: Priority) -> ContainResult {
            lock(&self.0).push(format!("priority {root} {scope:?} {p:?}"));
            Ok(vec![root + 1])
        }
        fn suspend(&self, root: u32, scope: Scope) -> ContainResult {
            lock(&self.0).push(format!("suspend {root} {scope:?}"));
            Ok(vec![root + 1])
        }
        fn resume(&self, root: u32, scope: Scope) -> ContainResult {
            lock(&self.0).push(format!("resume {root} {scope:?}"));
            Ok(vec![root + 1])
        }
        fn terminate(&self, root: u32, scope: Scope, _: Duration) -> ContainResult {
            lock(&self.0).push(format!("terminate {root} {scope:?}"));
            Ok(vec![root + 1])
        }
    }

    struct AlwaysAlive;
    impl Liveness for AlwaysAlive {
        fn is_alive(&self, _: u32) -> bool {
            true
        }
    }

    fn make(policy: Policy) -> (Broker, Arc<Recorder>, ManualClock) {
        let rec = Arc::new(Recorder::default());
        let clock = ManualClock::new(0);
        let broker = Broker::manual(
            BrokerConfig {
                policy,
                ..Default::default()
            },
            rec.clone(),
            Arc::new(clock.clone()),
            Arc::new(FixedLoad::default()),
            Arc::new(AlwaysAlive),
        );
        broker.register_session("a".into(), 100);
        (broker, rec, clock)
    }

    fn snapshot() -> ResourceSnapshot {
        ResourceSnapshot {
            at: 0,
            cpu_percent: 0.0,
            memory_used_bytes: 0,
            memory_total_bytes: 0,
            disk_read_bytes_per_sec: 0,
            disk_write_bytes_per_sec: 0,
            net_rx_bytes_per_sec: 0,
            net_tx_bytes_per_sec: 0,
            sessions: vec![SessionUsage {
                session_id: "a".into(),
                cpu_percent: 50.0,
                memory_bytes: 1 << 30,
                process_count: 4,
            }],
            queue: vec![],
        }
    }

    const HIGH: LoadSample = LoadSample {
        cpu_percent: 20.0,
        memory_percent: 92.0,
    };
    const LOW: LoadSample = LoadSample {
        cpu_percent: 20.0,
        memory_percent: 40.0,
    };

    fn actions(rx: &mut broadcast::Receiver<BrokerEvent>) -> Vec<(ActionKind, bool, bool)> {
        let mut v = Vec::new();
        while let Ok(e) = rx.try_recv() {
            if let BrokerEvent::Action {
                action,
                applied,
                dry_run,
                ..
            } = e
            {
                v.push((action, applied, dry_run));
            }
        }
        v
    }

    #[tokio::test]
    async fn autonomous_pauses_and_resumes_children() {
        let mut policy = Policy::default();
        policy.threshold.sustain_secs = 0;
        policy.threshold.cooldown_secs = 0;
        let (broker, rec, clock) = make(policy);
        let mut rx = broker.subscribe_events();
        broker.process_sample(&snapshot(), HIGH).await;
        clock.advance_ms(1000);
        broker.process_sample(&snapshot(), LOW).await;
        assert_eq!(
            *lock(&rec.0),
            vec!["suspend 100 ChildrenOnly", "resume 100 ChildrenOnly"]
        );
        assert_eq!(
            actions(&mut rx),
            vec![
                (ActionKind::PauseChildren, true, false),
                (ActionKind::ResumeChildren, true, false)
            ]
        );
    }

    #[tokio::test]
    async fn dry_run_and_manual_mode_only_propose() {
        for (autonomous, dry_run) in [(true, true), (false, false)] {
            let mut policy = Policy::default();
            policy.threshold.sustain_secs = 0;
            policy.threshold.action = ThresholdAction::Deprioritize;
            policy.autonomous = autonomous;
            policy.dry_run = dry_run;
            let (broker, rec, _) = make(policy);
            let mut rx = broker.subscribe_events();
            broker.process_sample(&snapshot(), HIGH).await;
            assert!(lock(&rec.0).is_empty());
            assert_eq!(
                actions(&mut rx),
                vec![(ActionKind::Deprioritize, false, dry_run)]
            );
        }
    }

    #[tokio::test]
    async fn propose_stop_is_never_applied() {
        let mut policy = Policy::default();
        policy.threshold.sustain_secs = 0;
        policy.threshold.action = ThresholdAction::ProposeStop;
        let (broker, rec, _) = make(policy);
        let mut rx = broker.subscribe_events();
        broker.process_sample(&snapshot(), HIGH).await;
        assert!(lock(&rec.0).is_empty());
        assert_eq!(
            actions(&mut rx),
            vec![(ActionKind::ProposeStop, false, false)]
        );
    }

    #[tokio::test]
    async fn manual_actions_and_unknown_session() {
        let (broker, rec, _) = make(Policy::default());
        let mut rx = broker.subscribe_events();
        let pids = broker
            .pause_session(&"a".into(), Scope::ChildrenOnly)
            .await
            .unwrap();
        assert_eq!(pids, vec![101]);
        broker
            .terminate_session(&"a".into(), Scope::WholeTree)
            .await
            .unwrap();
        assert!(matches!(
            broker
                .resume_session(&"zzz".into(), Scope::ChildrenOnly)
                .await,
            Err(BrokerError::UnknownSession(_))
        ));
        assert_eq!(
            *lock(&rec.0),
            vec!["suspend 100 ChildrenOnly", "terminate 100 WholeTree"]
        );
        assert_eq!(actions(&mut rx).len(), 2);
    }

    #[tokio::test]
    async fn admission_events_are_forwarded() {
        let (broker, _, _) = make(Policy::default());
        let mut rx = broker.subscribe_events();
        broker
            .admission()
            .request(crate::admission::AdmissionRequest::new("cargo build"));
        let e = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            e,
            BrokerEvent::Admission {
                event: AdmissionEvent::Granted { .. }
            }
        ));
        assert!(broker.snapshot().queue.is_empty());
    }

    #[tokio::test]
    async fn started_broker_publishes_snapshots() {
        let broker = Broker::start(BrokerConfig {
            monitor: MonitorConfig {
                interval: Duration::from_millis(50),
                ..Default::default()
            },
            ..Default::default()
        });
        broker.register_session("me".into(), std::process::id());
        let mut rx = broker.subscribe_snapshots().unwrap();
        for _ in 0..3 {
            tokio::time::timeout(Duration::from_secs(5), rx.changed())
                .await
                .unwrap()
                .unwrap();
        }
        let snap = broker.snapshot();
        assert!(snap.memory_total_bytes > 0);
        assert!(snap.sessions.iter().any(|s| s.session_id.0 == "me"));
    }
}
