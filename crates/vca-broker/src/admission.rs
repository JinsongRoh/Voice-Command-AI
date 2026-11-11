//! Admission control for heavy jobs (RES-02).
//!
//! Agents ask for a slot before running a heavy command. The controller
//! grants a slot when one is free and the machine has headroom, queues the
//! job otherwise, or denies it with a reason (queue full, memory critical, or
//! heavy jobs blocked for the session by policy). Light commands are always
//! granted the untracked [`LIGHT_SLOT_ID`].
//!
//! Slots are leases: each one expires `slot_lease_secs` after it was granted
//! or last renewed with [`AdmissionController::heartbeat`], and is freed early
//! when its holder process (if one was given) exits. Expiry is checked by
//! [`AdmissionController::tick`], which the owner calls periodically, and on
//! every request.
//!
//! The controller is deterministic: time, load and process liveness are
//! injected through [`Clock`], [`LoadSource`] and [`Liveness`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use vca_core::{AdmissionVerdict, JobClass, QueueEntry, SessionId, UnixMillis};

use crate::classify::Classifier;
use crate::clock::{Clock, LoadSample, LoadSource};
use crate::policy::Policy;

/// Slot id returned for light jobs; they are not tracked and need no release.
pub const LIGHT_SLOT_ID: &str = "light";

/// Priority bonus for the foreground session when `foreground_first` is set.
const FOREGROUND_BONUS: i32 = 1_000;

/// How many resolved job outcomes are remembered for late waiters.
const RECENT_OUTCOMES: usize = 1_024;

/// Reports whether a process is still running.
pub trait Liveness: Send + Sync + 'static {
    fn is_alive(&self, pid: u32) -> bool;
}

/// [`Liveness`] backed by `sysinfo`; zombie processes count as dead.
#[derive(Default)]
pub struct SysLiveness(Mutex<sysinfo::System>);

impl Liveness for SysLiveness {
    fn is_alive(&self, pid: u32) -> bool {
        let mut sys = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let pid = sysinfo::Pid::from_u32(pid);
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        sys.process(pid)
            .is_some_and(|p| p.status() != sysinfo::ProcessStatus::Zombie)
    }
}

/// Facts about the machine needed to resolve `max_heavy_jobs = "auto"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineInfo {
    pub cores: usize,
    pub memory_total_bytes: u64,
}

impl MachineInfo {
    /// Detects the current machine.
    pub fn detect() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        Self {
            cores: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            memory_total_bytes: sys.total_memory(),
        }
    }
}

/// A request for an admission slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRequest {
    /// The command line about to run.
    pub command: String,
    pub session_id: Option<SessionId>,
    /// Pre-computed class; classified from `command` when `None`.
    pub class: Option<JobClass>,
    /// The slot is released when this process exits.
    pub holder_pid: Option<u32>,
    /// Caller-chosen key (for example a tool-use id) for [`AdmissionController::release_by_key`].
    pub key: Option<String>,
}

impl AdmissionRequest {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            ..Self::default()
        }
    }

    pub fn session(mut self, session_id: impl Into<SessionId>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn class(mut self, class: JobClass) -> Self {
        self.class = Some(class);
        self
    }

    pub fn holder_pid(mut self, pid: u32) -> Self {
        self.holder_pid = Some(pid);
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

/// Why a slot was freed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    Released,
    HolderExited,
    LeaseExpired,
}

/// Something that happened in the admission controller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdmissionEvent {
    Granted {
        slot_id: String,
        job_id: Option<String>,
        session_id: Option<SessionId>,
        command: String,
        waited_ms: u64,
    },
    Queued {
        job_id: String,
        session_id: Option<SessionId>,
        command: String,
        position: u32,
    },
    Denied {
        session_id: Option<SessionId>,
        command: String,
        reason: String,
    },
    Released {
        slot_id: String,
        session_id: Option<SessionId>,
        reason: ReleaseReason,
    },
    Cancelled {
        job_id: String,
        reason: String,
    },
    Reordered {
        job_id: String,
        position: u32,
    },
}

/// A granted, active slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotInfo {
    pub slot_id: String,
    pub session_id: Option<SessionId>,
    pub command: String,
    pub granted_at: UnixMillis,
    pub expires_at: UnixMillis,
    pub holder_pid: Option<u32>,
    pub key: Option<String>,
}

/// State of a submitted job, observable through a [`JobTicket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Queued { position: u32 },
    Granted { slot_id: String },
    Denied { reason: String },
}

impl JobStatus {
    fn verdict(&self, job_id: &str) -> AdmissionVerdict {
        match self {
            Self::Queued { position } => AdmissionVerdict::Queue {
                job_id: job_id.to_owned(),
                position: *position,
            },
            Self::Granted { slot_id } => AdmissionVerdict::Grant {
                slot_id: slot_id.clone(),
            },
            Self::Denied { reason } => AdmissionVerdict::Deny {
                reason: reason.clone(),
            },
        }
    }
}

/// Handle to a queued job; use it with [`AdmissionController::wait`].
#[derive(Debug, Clone)]
pub struct JobTicket {
    pub job_id: String,
    rx: watch::Receiver<JobStatus>,
}

impl JobTicket {
    /// The latest status.
    pub fn status(&self) -> JobStatus {
        self.rx.borrow().clone()
    }

    /// Waits until the status changes (for example the queue position).
    pub async fn changed(&mut self) -> JobStatus {
        let _ = self.rx.changed().await;
        self.rx.borrow_and_update().clone()
    }
}

/// Result of [`AdmissionController::submit`].
#[derive(Debug)]
pub struct Submitted {
    pub verdict: AdmissionVerdict,
    /// Present when the job was queued.
    pub ticket: Option<JobTicket>,
}

struct Slot {
    info: SlotInfo,
}

struct Job {
    job_id: String,
    request: AdmissionRequest,
    class: JobClass,
    base_priority: i32,
    priority: i32,
    queued_at: UnixMillis,
    tx: watch::Sender<JobStatus>,
}

struct State {
    policy: Policy,
    classifier: Classifier,
    machine: MachineInfo,
    max_heavy: u32,
    slots: Vec<Slot>,
    queue: Vec<Job>,
    foreground: Option<SessionId>,
    recent: HashMap<String, JobStatus>,
    recent_order: VecDeque<String>,
}

/// The admission controller. Share it behind an [`Arc`].
pub struct AdmissionController {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    load: Arc<dyn LoadSource>,
    liveness: Arc<dyn Liveness>,
    events: broadcast::Sender<AdmissionEvent>,
    queue_tx: watch::Sender<Vec<QueueEntry>>,
}

impl AdmissionController {
    pub fn new(
        policy: Policy,
        machine: MachineInfo,
        clock: Arc<dyn Clock>,
        load: Arc<dyn LoadSource>,
        liveness: Arc<dyn Liveness>,
    ) -> Self {
        let classifier = Classifier::new(&policy.classifier);
        let max_heavy = policy
            .admission
            .max_heavy_jobs
            .resolve(machine.cores, machine.memory_total_bytes);
        let (events, _) = broadcast::channel(256);
        let (queue_tx, _) = watch::channel(Vec::new());
        Self {
            state: Mutex::new(State {
                policy,
                classifier,
                machine,
                max_heavy,
                slots: Vec::new(),
                queue: Vec::new(),
                foreground: None,
                recent: HashMap::new(),
                recent_order: VecDeque::new(),
            }),
            clock,
            load,
            liveness,
            events,
            queue_tx,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, event: AdmissionEvent) {
        let _ = self.events.send(event);
    }

    /// Subscribes to admission events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<AdmissionEvent> {
        self.events.subscribe()
    }

    /// Subscribes to the queue contents (updated on every change).
    pub fn subscribe_queue(&self) -> watch::Receiver<Vec<QueueEntry>> {
        self.queue_tx.subscribe()
    }

    /// The effective concurrent heavy job limit.
    pub fn max_heavy_jobs(&self) -> u32 {
        self.lock().max_heavy
    }

    /// Classifies a command with the current policy's rules.
    pub fn classify(&self, command: &str) -> JobClass {
        self.lock().classifier.class_of(command)
    }

    /// Classifies a command and names the deciding rule.
    pub fn classify_explained(&self, command: &str) -> crate::classify::Classification {
        self.lock().classifier.classify(command)
    }

    /// The current policy.
    pub fn policy(&self) -> Policy {
        self.lock().policy.clone()
    }

    /// Replaces the policy and re-evaluates the queue.
    pub fn set_policy(&self, policy: Policy) {
        let mut st = self.lock();
        st.classifier = Classifier::new(&policy.classifier);
        st.max_heavy = policy
            .admission
            .max_heavy_jobs
            .resolve(st.machine.cores, st.machine.memory_total_bytes);
        st.policy = policy;
        recompute_priorities(&mut st);
        self.reconcile(&mut st);
    }

    /// Sets the foreground session, which is admitted first when
    /// `foreground_first` is enabled. Re-sorts the queue by priority.
    pub fn set_foreground(&self, session_id: Option<SessionId>) {
        let mut st = self.lock();
        st.foreground = session_id;
        recompute_priorities(&mut st);
        self.reconcile(&mut st);
    }

    /// Requests a slot. See [`AdmissionController::submit`].
    pub fn request(&self, request: AdmissionRequest) -> AdmissionVerdict {
        self.submit(request).verdict
    }

    /// Requests a slot and, when queued, returns a ticket to wait on.
    pub fn submit(&self, request: AdmissionRequest) -> Submitted {
        let mut st = self.lock();
        let class = request
            .class
            .unwrap_or_else(|| st.classifier.class_of(&request.command));
        if class == JobClass::Light {
            return Submitted {
                verdict: AdmissionVerdict::Grant {
                    slot_id: LIGHT_SLOT_ID.to_owned(),
                },
                ticket: None,
            };
        }
        let now = self.clock.now();
        self.expire(&mut st, now);

        if let Some(reason) = deny_reason(&st, &request, self.load.load()) {
            self.emit(AdmissionEvent::Denied {
                session_id: request.session_id.clone(),
                command: request.command.clone(),
                reason: reason.clone(),
            });
            return Submitted {
                verdict: AdmissionVerdict::Deny { reason },
                ticket: None,
            };
        }

        let job_id = uuid::Uuid::new_v4().to_string();
        let base_priority = request
            .session_id
            .as_ref()
            .and_then(|s| st.policy.session(&s.0))
            .map_or(0, |s| s.priority);
        let priority = effective_priority(&st, base_priority, request.session_id.as_ref());
        let index = st
            .queue
            .iter()
            .position(|j| j.priority < priority)
            .unwrap_or(st.queue.len());
        let (tx, rx) = watch::channel(JobStatus::Queued {
            position: index as u32,
        });
        st.queue.insert(
            index,
            Job {
                job_id: job_id.clone(),
                request,
                class,
                base_priority,
                priority,
                queued_at: now,
                tx,
            },
        );
        self.reconcile(&mut st);

        let status = rx.borrow().clone();
        if let JobStatus::Queued { position } = status {
            let job = &st.queue[position as usize];
            self.emit(AdmissionEvent::Queued {
                job_id: job_id.clone(),
                session_id: job.request.session_id.clone(),
                command: job.request.command.clone(),
                position,
            });
        }
        Submitted {
            verdict: status.verdict(&job_id),
            ticket: matches!(status, JobStatus::Queued { .. }).then_some(JobTicket { job_id, rx }),
        }
    }

    /// Waits until a queued job is granted or denied. On timeout the job is
    /// removed from the queue and a `Deny` is returned.
    pub async fn wait(&self, mut ticket: JobTicket, timeout: Duration) -> AdmissionVerdict {
        let resolved = tokio::time::timeout(timeout, async {
            loop {
                let status = ticket.rx.borrow_and_update().clone();
                if !matches!(status, JobStatus::Queued { .. }) {
                    return status;
                }
                if ticket.rx.changed().await.is_err() {
                    // The job was dropped without a final status.
                    return ticket.rx.borrow().clone();
                }
            }
        })
        .await;
        match resolved {
            Ok(JobStatus::Queued { .. }) | Err(_) => {
                let reason = format!("timed out after {}s in the queue", timeout.as_secs());
                if !self.cancel_with_reason(&ticket.job_id, &reason) {
                    // Resolved concurrently with the timeout.
                    let status = ticket.rx.borrow().clone();
                    if !matches!(status, JobStatus::Queued { .. }) {
                        return status.verdict(&ticket.job_id);
                    }
                }
                AdmissionVerdict::Deny { reason }
            }
            Ok(status) => status.verdict(&ticket.job_id),
        }
    }

    /// Waits for a job by id (for callers that only kept the id).
    pub async fn wait_for_job(&self, job_id: &str, timeout: Duration) -> AdmissionVerdict {
        let ticket = {
            let st = self.lock();
            if let Some(job) = st.queue.iter().find(|j| j.job_id == job_id) {
                JobTicket {
                    job_id: job_id.to_owned(),
                    rx: job.tx.subscribe(),
                }
            } else if let Some(status) = st.recent.get(job_id) {
                return status.verdict(job_id);
            } else {
                return AdmissionVerdict::Deny {
                    reason: format!("unknown job {job_id}"),
                };
            }
        };
        self.wait(ticket, timeout).await
    }

    /// Submits and waits up to `timeout` while queued.
    pub async fn request_and_wait(
        &self,
        request: AdmissionRequest,
        timeout: Duration,
    ) -> AdmissionVerdict {
        let submitted = self.submit(request);
        match submitted.ticket {
            Some(ticket) => self.wait(ticket, timeout).await,
            None => submitted.verdict,
        }
    }

    /// Releases a slot. Returns whether it existed. Releasing
    /// [`LIGHT_SLOT_ID`] is a no-op that returns `false`.
    pub fn release(&self, slot_id: &str) -> bool {
        let mut st = self.lock();
        let Some(i) = st.slots.iter().position(|s| s.info.slot_id == slot_id) else {
            return false;
        };
        self.free_slot(&mut st, i, ReleaseReason::Released);
        self.reconcile(&mut st);
        true
    }

    /// Releases every slot granted with this key, and cancels queued jobs
    /// with it. Returns the number of slots released.
    pub fn release_by_key(&self, key: &str) -> usize {
        let mut st = self.lock();
        let mut released = 0;
        while let Some(i) = st
            .slots
            .iter()
            .position(|s| s.info.key.as_deref() == Some(key))
        {
            self.free_slot(&mut st, i, ReleaseReason::Released);
            released += 1;
        }
        let queued: Vec<String> = st
            .queue
            .iter()
            .filter(|j| j.request.key.as_deref() == Some(key))
            .map(|j| j.job_id.clone())
            .collect();
        for job_id in queued {
            self.remove_job(&mut st, &job_id, "released by key before admission");
        }
        self.reconcile(&mut st);
        released
    }

    /// Renews a slot's lease. Returns whether the slot exists.
    pub fn heartbeat(&self, slot_id: &str) -> bool {
        let mut st = self.lock();
        let lease_ms = st.policy.admission.slot_lease_secs * 1000;
        let now = self.clock.now();
        match st.slots.iter_mut().find(|s| s.info.slot_id == slot_id) {
            Some(slot) => {
                slot.info.expires_at = now + lease_ms;
                true
            }
            None => false,
        }
    }

    /// Removes a queued job. Returns whether it was queued.
    pub fn cancel(&self, job_id: &str) -> bool {
        self.cancel_with_reason(job_id, "cancelled")
    }

    fn cancel_with_reason(&self, job_id: &str, reason: &str) -> bool {
        let mut st = self.lock();
        let found = self.remove_job(&mut st, job_id, reason);
        if found {
            self.reconcile(&mut st);
        }
        found
    }

    /// Moves a queued job to `position` (0 = next; clamped to the end).
    /// Returns whether the job was queued.
    pub fn reorder(&self, job_id: &str, position: u32) -> bool {
        let mut st = self.lock();
        let Some(from) = st.queue.iter().position(|j| j.job_id == job_id) else {
            return false;
        };
        let mut job = st.queue.remove(from);
        let to = (position as usize).min(st.queue.len());
        // Adopt the neighbour's priority so that later insertions respect
        // the manual order.
        job.priority = match st.queue.get(to) {
            Some(next) => next.priority,
            None => st.queue.last().map_or(job.priority, |prev| prev.priority),
        };
        st.queue.insert(to, job);
        self.emit(AdmissionEvent::Reordered {
            job_id: job_id.to_owned(),
            position: to as u32,
        });
        self.reconcile(&mut st);
        true
    }

    /// Grants a queued job immediately, ignoring limits and load (the user
    /// approved it, RES-06). Returns the slot id.
    pub fn admit_now(&self, job_id: &str) -> Option<String> {
        let mut st = self.lock();
        let i = st.queue.iter().position(|j| j.job_id == job_id)?;
        let now = self.clock.now();
        let slot_id = self.grant(&mut st, i, now);
        self.reconcile(&mut st);
        Some(slot_id)
    }

    /// Expires dead or stale slots and admits queued jobs if possible. Call
    /// periodically (for example every second) and whenever load changes.
    pub fn tick(&self) {
        let mut st = self.lock();
        let now = self.clock.now();
        self.expire(&mut st, now);
        self.reconcile(&mut st);
    }

    /// The current queue, in admission order.
    pub fn queue(&self) -> Vec<QueueEntry> {
        queue_entries(&self.lock())
    }

    /// The active slots.
    pub fn slots(&self) -> Vec<SlotInfo> {
        self.lock().slots.iter().map(|s| s.info.clone()).collect()
    }

    fn expire(&self, st: &mut State, now: UnixMillis) {
        let mut i = 0;
        while i < st.slots.len() {
            let info = &st.slots[i].info;
            let reason = if info.holder_pid.is_some_and(|p| !self.liveness.is_alive(p)) {
                Some(ReleaseReason::HolderExited)
            } else if now >= info.expires_at {
                Some(ReleaseReason::LeaseExpired)
            } else {
                None
            };
            match reason {
                Some(reason) => self.free_slot(st, i, reason),
                None => i += 1,
            }
        }
    }

    fn free_slot(&self, st: &mut State, index: usize, reason: ReleaseReason) {
        let slot = st.slots.remove(index);
        tracing::debug!(slot_id = %slot.info.slot_id, ?reason, "slot freed");
        self.emit(AdmissionEvent::Released {
            slot_id: slot.info.slot_id,
            session_id: slot.info.session_id,
            reason,
        });
    }

    fn remove_job(&self, st: &mut State, job_id: &str, reason: &str) -> bool {
        let Some(i) = st.queue.iter().position(|j| j.job_id == job_id) else {
            return false;
        };
        let job = st.queue.remove(i);
        let status = JobStatus::Denied {
            reason: reason.to_owned(),
        };
        job.tx.send_replace(status.clone());
        remember(st, job.job_id.clone(), status);
        self.emit(AdmissionEvent::Cancelled {
            job_id: job.job_id,
            reason: reason.to_owned(),
        });
        true
    }

    fn grant(&self, st: &mut State, index: usize, now: UnixMillis) -> String {
        let job = st.queue.remove(index);
        let slot_id = uuid::Uuid::new_v4().to_string();
        let info = SlotInfo {
            slot_id: slot_id.clone(),
            session_id: job.request.session_id.clone(),
            command: job.request.command.clone(),
            granted_at: now,
            expires_at: now + st.policy.admission.slot_lease_secs * 1000,
            holder_pid: job.request.holder_pid,
            key: job.request.key.clone(),
        };
        st.slots.push(Slot { info });
        let status = JobStatus::Granted {
            slot_id: slot_id.clone(),
        };
        job.tx.send_replace(status.clone());
        remember(st, job.job_id.clone(), status);
        self.emit(AdmissionEvent::Granted {
            slot_id: slot_id.clone(),
            job_id: Some(job.job_id),
            session_id: job.request.session_id,
            command: job.request.command,
            waited_ms: now.saturating_sub(job.queued_at),
        });
        slot_id
    }

    /// Admits as many queued jobs as limits and load allow, then publishes
    /// queue positions.
    fn reconcile(&self, st: &mut State) {
        let now = self.clock.now();
        let load = self.load.load();
        let a = &st.policy.admission;
        let load_ok = load.memory_percent <= a.memory_limit_percent()
            && load.cpu_percent <= a.cpu_threshold_percent;
        if load_ok {
            let mut i = 0;
            while i < st.queue.len() && (st.slots.len() as u32) < st.max_heavy {
                let job = &st.queue[i];
                if session_at_limit(st, job.request.session_id.as_ref()) {
                    i += 1;
                    continue;
                }
                self.grant(st, i, now);
            }
        }
        for (pos, job) in st.queue.iter().enumerate() {
            job.tx.send_if_modified(|s| {
                let new = JobStatus::Queued {
                    position: pos as u32,
                };
                if *s == new {
                    false
                } else {
                    *s = new;
                    true
                }
            });
        }
        let entries = queue_entries(st);
        self.queue_tx.send_if_modified(|q| {
            if *q == entries {
                false
            } else {
                *q = entries;
                true
            }
        });
    }
}

fn remember(st: &mut State, job_id: String, status: JobStatus) {
    if st.recent_order.len() >= RECENT_OUTCOMES {
        if let Some(old) = st.recent_order.pop_front() {
            st.recent.remove(&old);
        }
    }
    st.recent_order.push_back(job_id.clone());
    st.recent.insert(job_id, status);
}

fn effective_priority(st: &State, base: i32, session: Option<&SessionId>) -> i32 {
    let fg = st.policy.admission.foreground_first
        && session.is_some()
        && session == st.foreground.as_ref();
    base + if fg { FOREGROUND_BONUS } else { 0 }
}

fn recompute_priorities(st: &mut State) {
    let prios: Vec<i32> = st
        .queue
        .iter()
        .map(|j| effective_priority(st, j.base_priority, j.request.session_id.as_ref()))
        .collect();
    for (job, p) in st.queue.iter_mut().zip(prios) {
        job.priority = p;
    }
    st.queue.sort_by_key(|j| std::cmp::Reverse(j.priority));
}

fn session_limit(st: &State, session: Option<&SessionId>) -> Option<u32> {
    session
        .and_then(|s| st.policy.session(&s.0))
        .and_then(|p| p.max_heavy_jobs)
}

fn session_at_limit(st: &State, session: Option<&SessionId>) -> bool {
    let Some(limit) = session_limit(st, session) else {
        return false;
    };
    let active = st
        .slots
        .iter()
        .filter(|s| s.info.session_id.as_ref() == session)
        .count() as u32;
    active >= limit
}

fn deny_reason(st: &State, request: &AdmissionRequest, load: LoadSample) -> Option<String> {
    let a = &st.policy.admission;
    if let Some(0) = session_limit(st, request.session_id.as_ref()) {
        return Some(format!(
            "heavy jobs are disabled for session {} by policy",
            request.session_id.as_ref().map_or("?", |s| s.0.as_str())
        ));
    }
    if load.memory_percent >= a.deny_memory_percent {
        return Some(format!(
            "memory usage is critical ({:.0}% used, limit {:.0}%); try again later",
            load.memory_percent, a.deny_memory_percent
        ));
    }
    if st.queue.len() as u32 >= a.max_queue_len {
        return Some(format!(
            "admission queue is full ({} jobs waiting)",
            st.queue.len()
        ));
    }
    None
}

fn queue_entries(st: &State) -> Vec<QueueEntry> {
    st.queue
        .iter()
        .enumerate()
        .map(|(pos, j)| QueueEntry {
            job_id: j.job_id.clone(),
            session_id: j.request.session_id.clone(),
            command: j.request.command.clone(),
            class: j.class,
            queued_at: j.queued_at,
            position: pos as u32,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::clock::{FixedLoad, ManualClock};
    use crate::policy::{MaxJobs, SessionPolicy};

    #[derive(Default, Clone)]
    struct FakeLiveness(Arc<Mutex<HashSet<u32>>>);

    impl FakeLiveness {
        fn kill(&self, pid: u32) {
            self.0.lock().unwrap().insert(pid);
        }
    }

    impl Liveness for FakeLiveness {
        fn is_alive(&self, pid: u32) -> bool {
            !self.0.lock().unwrap().contains(&pid)
        }
    }

    struct Fixture {
        ctl: Arc<AdmissionController>,
        clock: ManualClock,
        load: FixedLoad,
        live: FakeLiveness,
    }

    fn fixture_with(policy: Policy) -> Fixture {
        let clock = ManualClock::new(1_000_000);
        let load = FixedLoad::new(LoadSample {
            cpu_percent: 10.0,
            memory_percent: 30.0,
        });
        let live = FakeLiveness::default();
        let ctl = Arc::new(AdmissionController::new(
            policy,
            MachineInfo {
                cores: 8,
                memory_total_bytes: 16 << 30,
            },
            Arc::new(clock.clone()),
            Arc::new(load.clone()),
            Arc::new(live.clone()),
        ));
        Fixture {
            ctl,
            clock,
            load,
            live,
        }
    }

    fn fixture(max: u32) -> Fixture {
        let mut p = Policy::default();
        p.admission.max_heavy_jobs = MaxJobs::Fixed(max);
        fixture_with(p)
    }

    fn heavy(cmd: &str, session: &str) -> AdmissionRequest {
        AdmissionRequest::new(cmd).session(session)
    }

    fn slot(v: &AdmissionVerdict) -> String {
        match v {
            AdmissionVerdict::Grant { slot_id } => slot_id.clone(),
            other => panic!("expected grant, got {other:?}"),
        }
    }

    fn job(v: &AdmissionVerdict) -> (String, u32) {
        match v {
            AdmissionVerdict::Queue { job_id, position } => (job_id.clone(), *position),
            other => panic!("expected queue, got {other:?}"),
        }
    }

    #[test]
    fn auto_limit_resolves_from_machine() {
        let f = fixture_with(Policy::default());
        // 8 cores / 4 = 2, 16 GiB / 8 = 2.
        assert_eq!(f.ctl.max_heavy_jobs(), 2);
    }

    #[test]
    fn light_jobs_always_granted_untracked() {
        let f = fixture(1);
        let v = f.ctl.request(heavy("git status", "a"));
        assert_eq!(slot(&v), LIGHT_SLOT_ID);
        assert!(f.ctl.slots().is_empty());
        assert!(!f.ctl.release(LIGHT_SLOT_ID));
    }

    #[test]
    fn grants_until_limit_then_queues_in_order() {
        let f = fixture(2);
        let s1 = slot(&f.ctl.request(heavy("cargo build", "a")));
        let _s2 = slot(&f.ctl.request(heavy("cargo test", "b")));
        let (j1, p1) = job(&f.ctl.request(heavy("npm install", "c")));
        let (j2, p2) = job(&f.ctl.request(heavy("make", "d")));
        assert_eq!((p1, p2), (0, 1));
        assert_eq!(f.ctl.queue().len(), 2);

        assert!(f.ctl.release(&s1));
        assert!(!f.ctl.release(&s1), "double release");
        let q = f.ctl.queue();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].job_id, j2);
        assert_eq!(q[0].position, 0);
        assert!(f.ctl.slots().iter().any(|s| s.command == "npm install"));
        let _ = j1;
    }

    #[test]
    fn explicit_class_overrides_classifier() {
        let f = fixture(1);
        let _ = slot(&f.ctl.request(heavy("cargo build", "a")));
        let v = f
            .ctl
            .request(AdmissionRequest::new("./run-everything.sh").class(JobClass::Heavy));
        job(&v);
        let v = f
            .ctl
            .request(AdmissionRequest::new("cargo test").class(JobClass::Light));
        assert_eq!(slot(&v), LIGHT_SLOT_ID);
    }

    #[test]
    fn memory_pressure_queues_and_critical_denies() {
        let f = fixture(4);
        f.load.set(LoadSample {
            cpu_percent: 10.0,
            memory_percent: 90.0, // above 100 - 15
        });
        let (_, pos) = job(&f.ctl.request(heavy("cargo build", "a")));
        assert_eq!(pos, 0);
        f.load.set(LoadSample {
            cpu_percent: 10.0,
            memory_percent: 98.0,
        });
        match f.ctl.request(heavy("cargo test", "a")) {
            AdmissionVerdict::Deny { reason } => assert!(reason.contains("memory"), "{reason}"),
            other => panic!("{other:?}"),
        }
        // Load recovers: the queued job is admitted on the next tick.
        f.load.set(LoadSample {
            cpu_percent: 10.0,
            memory_percent: 50.0,
        });
        f.ctl.tick();
        assert!(f.ctl.queue().is_empty());
        assert_eq!(f.ctl.slots().len(), 1);
    }

    #[test]
    fn cpu_threshold_holds_new_jobs() {
        let f = fixture(4);
        f.load.set(LoadSample {
            cpu_percent: 95.0,
            memory_percent: 10.0,
        });
        job(&f.ctl.request(heavy("cargo build", "a")));
        f.load.set(LoadSample {
            cpu_percent: 50.0,
            memory_percent: 10.0,
        });
        f.ctl.tick();
        assert_eq!(f.ctl.slots().len(), 1);
    }

    #[test]
    fn queue_full_denies() {
        let mut p = Policy::default();
        p.admission.max_heavy_jobs = MaxJobs::Fixed(1);
        p.admission.max_queue_len = 1;
        let f = fixture_with(p);
        slot(&f.ctl.request(heavy("cargo build", "a")));
        job(&f.ctl.request(heavy("cargo build", "b")));
        match f.ctl.request(heavy("cargo build", "c")) {
            AdmissionVerdict::Deny { reason } => assert!(reason.contains("queue is full")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn session_limits_and_blocked_sessions() {
        let mut p = Policy::default();
        p.admission.max_heavy_jobs = MaxJobs::Fixed(3);
        p.sessions.insert(
            "a".into(),
            SessionPolicy {
                max_heavy_jobs: Some(1),
                ..Default::default()
            },
        );
        p.sessions.insert(
            "blocked".into(),
            SessionPolicy {
                max_heavy_jobs: Some(0),
                ..Default::default()
            },
        );
        let f = fixture_with(p);
        let a1 = slot(&f.ctl.request(heavy("cargo build", "a")));
        let (ja, _) = job(&f.ctl.request(heavy("cargo test", "a")));
        // Session b is not blocked by a's queued job (no head-of-line blocking).
        slot(&f.ctl.request(heavy("cargo test", "b")));
        assert!(matches!(
            f.ctl.request(heavy("cargo test", "blocked")),
            AdmissionVerdict::Deny { .. }
        ));
        assert_eq!(f.ctl.queue()[0].job_id, ja);
        f.ctl.release(&a1);
        assert!(f.ctl.queue().is_empty());
    }

    #[test]
    fn priorities_and_foreground_first() {
        let mut p = Policy::default();
        p.admission.max_heavy_jobs = MaxJobs::Fixed(1);
        p.sessions.insert(
            "vip".into(),
            SessionPolicy {
                priority: 5,
                ..Default::default()
            },
        );
        let f = fixture_with(p);
        let s = slot(&f.ctl.request(heavy("cargo build", "x")));
        let (j_bg, _) = job(&f.ctl.request(heavy("cargo test", "bg")));
        let (j_vip, pos) = job(&f.ctl.request(heavy("cargo test", "vip")));
        assert_eq!(pos, 0, "higher priority goes first");
        f.ctl.set_foreground(Some("fg".into()));
        let (j_fg, pos) = job(&f.ctl.request(heavy("cargo test", "fg")));
        assert_eq!(pos, 0, "foreground beats session priority");
        let ids: Vec<_> = f.ctl.queue().into_iter().map(|e| e.job_id).collect();
        assert_eq!(ids, vec![j_fg.clone(), j_vip, j_bg.clone()]);
        // Switching foreground re-sorts the queue.
        f.ctl.set_foreground(Some("bg".into()));
        assert_eq!(f.ctl.queue()[0].job_id, j_bg);
        f.ctl.release(&s);
        assert_eq!(f.ctl.slots()[0].session_id, Some("bg".into()));
        let _ = j_fg;
    }

    #[test]
    fn reorder_and_cancel() {
        let f = fixture(1);
        slot(&f.ctl.request(heavy("cargo build", "x")));
        let (j1, _) = job(&f.ctl.request(heavy("cargo test", "a")));
        let (j2, _) = job(&f.ctl.request(heavy("cargo test", "b")));
        let (j3, _) = job(&f.ctl.request(heavy("cargo test", "c")));
        assert!(f.ctl.reorder(&j3, 0));
        let ids =
            |f: &Fixture| -> Vec<String> { f.ctl.queue().into_iter().map(|e| e.job_id).collect() };
        assert_eq!(ids(&f), vec![j3.clone(), j1.clone(), j2.clone()]);
        assert!(f.ctl.reorder(&j3, 99));
        assert_eq!(ids(&f), vec![j1.clone(), j2.clone(), j3.clone()]);
        // A new equal-priority job goes to the end, after the manual order.
        let (j4, pos) = job(&f.ctl.request(heavy("cargo test", "d")));
        assert_eq!(pos, 3);
        assert!(f.ctl.cancel(&j2));
        assert!(!f.ctl.cancel(&j2));
        assert!(!f.ctl.reorder("nope", 0));
        assert_eq!(ids(&f), vec![j1, j3, j4]);
    }

    #[test]
    fn admit_now_bypasses_limits() {
        let f = fixture(1);
        slot(&f.ctl.request(heavy("cargo build", "x")));
        let (j, _) = job(&f.ctl.request(heavy("cargo test", "a")));
        let s = f.ctl.admit_now(&j).unwrap();
        assert_eq!(f.ctl.slots().len(), 2);
        assert!(f.ctl.queue().is_empty());
        assert!(f.ctl.release(&s));
        assert!(f.ctl.admit_now("missing").is_none());
    }

    #[test]
    fn slots_expire_when_holder_dies() {
        let f = fixture(1);
        slot(&f.ctl.request(heavy("cargo build", "x").holder_pid(4242)));
        let (j, _) = job(&f.ctl.request(heavy("cargo test", "a")));
        f.ctl.tick();
        assert_eq!(f.ctl.queue().len(), 1);
        let mut events = f.ctl.subscribe_events();
        f.live.kill(4242);
        f.ctl.tick();
        assert!(f.ctl.queue().is_empty());
        assert_eq!(f.ctl.slots()[0].command, "cargo test");
        assert!(matches!(
            events.try_recv().unwrap(),
            AdmissionEvent::Released {
                reason: ReleaseReason::HolderExited,
                ..
            }
        ));
        assert!(matches!(
            events.try_recv().unwrap(),
            AdmissionEvent::Granted { job_id: Some(id), .. } if id == j
        ));
    }

    #[test]
    fn slots_expire_without_heartbeat() {
        let mut p = Policy::default();
        p.admission.max_heavy_jobs = MaxJobs::Fixed(1);
        p.admission.slot_lease_secs = 30;
        let f = fixture_with(p);
        let s = slot(&f.ctl.request(heavy("cargo build", "x")));
        f.clock.advance_ms(20_000);
        assert!(f.ctl.heartbeat(&s));
        f.clock.advance_ms(20_000);
        f.ctl.tick();
        assert_eq!(f.ctl.slots().len(), 1, "heartbeat renewed the lease");
        f.clock.advance_ms(10_001);
        f.ctl.tick();
        assert!(f.ctl.slots().is_empty());
        assert!(!f.ctl.heartbeat(&s));
    }

    #[test]
    fn release_by_key() {
        let f = fixture(1);
        slot(&f.ctl.request(heavy("cargo build", "x").key("tool-1")));
        let (_, _) = job(&f.ctl.request(heavy("cargo test", "x").key("tool-2")));
        assert_eq!(f.ctl.release_by_key("tool-1"), 1);
        // tool-2 got the slot.
        assert_eq!(f.ctl.slots()[0].key.as_deref(), Some("tool-2"));
        assert_eq!(f.ctl.release_by_key("tool-2"), 1);
        assert_eq!(f.ctl.release_by_key("tool-2"), 0);
    }

    #[test]
    fn queue_watch_tracks_changes() {
        let f = fixture(1);
        let rx = f.ctl.subscribe_queue();
        slot(&f.ctl.request(heavy("cargo build", "x")));
        let (j, _) = job(&f.ctl.request(heavy("cargo test", "a")));
        assert_eq!(rx.borrow()[0].job_id, j);
        f.ctl.cancel(&j);
        assert!(rx.borrow().is_empty());
    }

    #[test]
    fn policy_change_reevaluates_queue() {
        let f = fixture(1);
        slot(&f.ctl.request(heavy("cargo build", "x")));
        job(&f.ctl.request(heavy("cargo test", "a")));
        let mut p = Policy::default();
        p.admission.max_heavy_jobs = MaxJobs::Fixed(2);
        f.ctl.set_policy(p);
        assert_eq!(f.ctl.max_heavy_jobs(), 2);
        assert_eq!(f.ctl.slots().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn waiter_is_granted_on_release() {
        let f = fixture(1);
        let s = slot(&f.ctl.request(heavy("cargo build", "x")));
        let submitted = f.ctl.submit(heavy("cargo test", "a"));
        let ticket = submitted.ticket.unwrap();
        let ctl = f.ctl.clone();
        let waiter = tokio::spawn(async move { ctl.wait(ticket, Duration::from_secs(60)).await });
        tokio::time::sleep(Duration::from_secs(5)).await;
        f.ctl.release(&s);
        let v = waiter.await.unwrap();
        assert!(matches!(v, AdmissionVerdict::Grant { .. }), "{v:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn waiter_times_out_and_leaves_queue() {
        let f = fixture(1);
        slot(&f.ctl.request(heavy("cargo build", "x")));
        let v = f
            .ctl
            .request_and_wait(heavy("cargo test", "a"), Duration::from_secs(10))
            .await;
        match v {
            AdmissionVerdict::Deny { reason } => assert!(reason.contains("timed out")),
            other => panic!("{other:?}"),
        }
        assert!(f.ctl.queue().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_wakes_waiter_and_late_waiter_sees_outcome() {
        let f = fixture(1);
        let s = slot(&f.ctl.request(heavy("cargo build", "x")));
        let (j, _) = job(&f.ctl.request(heavy("cargo test", "a")));
        let ctl = f.ctl.clone();
        let jid = j.clone();
        let waiter =
            tokio::spawn(async move { ctl.wait_for_job(&jid, Duration::from_secs(60)).await });
        tokio::task::yield_now().await;
        f.ctl.cancel(&j);
        assert!(matches!(
            waiter.await.unwrap(),
            AdmissionVerdict::Deny { .. }
        ));

        // A job granted before anyone waited still reports its grant.
        let (j2, _) = job(&f.ctl.request(heavy("cargo test", "b")));
        f.ctl.release(&s);
        let v = f.ctl.wait_for_job(&j2, Duration::from_secs(1)).await;
        assert!(matches!(v, AdmissionVerdict::Grant { .. }), "{v:?}");
        let v = f.ctl.wait_for_job("nope", Duration::from_secs(1)).await;
        assert!(matches!(v, AdmissionVerdict::Deny { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn ticket_reports_position_changes() {
        let f = fixture(1);
        let s = slot(&f.ctl.request(heavy("cargo build", "x")));
        let (j1, _) = job(&f.ctl.request(heavy("cargo test", "a")));
        let mut t2 = f.ctl.submit(heavy("cargo test", "b")).ticket.unwrap();
        assert_eq!(t2.status(), JobStatus::Queued { position: 1 });
        f.ctl.cancel(&j1);
        assert_eq!(t2.changed().await, JobStatus::Queued { position: 0 });
        f.ctl.release(&s);
        assert!(matches!(t2.changed().await, JobStatus::Granted { .. }));
    }
}
