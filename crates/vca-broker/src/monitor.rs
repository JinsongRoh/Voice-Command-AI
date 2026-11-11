//! Periodic resource sampling (RES-01).
//!
//! A [`Monitor`] runs a sampler on a dedicated thread and publishes the
//! latest [`ResourceSnapshot`] and smoothed [`LoadSample`] through `watch`
//! channels. Per-session usage is the sum over each registered session root
//! process and all of its descendants.
//!
//! Cost: only the processes of registered and monitor-only sessions (and
//! their descendants) are sampled for CPU and memory; the rest of the
//! machine is one cheap process listing plus the global CPU and memory
//! figures. Disk and network rates are read only while someone looks at
//! them ([`ResourceDemand`]). The interval is [`MonitorConfig::interval`]
//! while someone looks, VCA-started sessions run or the admission queue is
//! busy, and [`MonitorConfig::idle_interval`] otherwise.
//!
//! Units: system `cpu_percent` is averaged over all cores (0–100). Session
//! `cpu_percent` uses the same scale, i.e. the share of the whole machine
//! (a session saturating 2 of 8 cores reports 25). Disk and network values
//! are rates in bytes per second computed from counter deltas; loopback
//! interfaces are excluded. GPU sampling is not implemented yet.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sysinfo::{
    DiskRefreshKind, Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System,
};
use tokio::sync::watch;
use vca_core::{QueueEntry, ResourceSnapshot, SessionId, SessionUsage, UnixMillis};

use crate::clock::LoadSample;
use crate::proctree::{ProcInfo, ProcTree};

/// Sampler settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorConfig {
    /// Time between samples while they are needed: someone looks at the
    /// resources, a session VCA started runs, or the admission queue holds
    /// or runs a job.
    pub interval: Duration,
    /// Time between samples otherwise (load figures for alerts and the
    /// monitor-only usage of observed sessions).
    pub idle_interval: Duration,
    /// Weight of the newest sample in the exponential moving average (0–1].
    pub smoothing_alpha: f32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            idle_interval: Duration::from_secs(5),
            smoothing_alpha: 0.3,
        }
    }
}

/// Shortest sampling interval: a zero or tiny configured interval must not
/// spin the monitor thread.
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

impl MonitorConfig {
    /// The configured interval, never below [`MIN_INTERVAL`].
    pub fn tick(&self) -> Duration {
        self.interval.max(MIN_INTERVAL)
    }

    /// The interval for the current activity.
    pub fn interval_for(&self, needed: bool) -> Duration {
        if needed {
            self.tick()
        } else {
            self.idle_interval.max(self.tick())
        }
    }
}

/// How long processes that left the session trees but still run may keep
/// their entries (and, on Windows, a handle each) before the per-process
/// figures start over. Starting over at once zeroed every session's CPU
/// each time a helper (a build server, a file monitor) detached.
const DETACHED_GRACE: Duration = Duration::from_secs(30);
/// Detached processes kept at most within [`DETACHED_GRACE`].
const MAX_DETACHED: usize = 16;

/// How many clients look at the resource figures. Cheap to clone; clones
/// share the count.
#[derive(Debug, Clone, Default)]
pub struct ResourceDemand(Arc<AtomicUsize>);

impl ResourceDemand {
    /// Registers one viewer for as long as the returned guard lives.
    pub fn viewer(&self) -> ResourceViewer {
        self.0.fetch_add(1, Ordering::SeqCst);
        ResourceViewer(Arc::clone(&self.0))
    }

    pub fn viewers(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// One client looking at the resource figures; dropping it ends that.
#[derive(Debug)]
pub struct ResourceViewer(Arc<AtomicUsize>);

impl Drop for ResourceViewer {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Tells the monitor whether something besides viewers needs frequent
/// samples (the admission queue).
pub type BusyProvider = Arc<dyn Fn() -> bool + Send + Sync>;

/// What decides the monitor's pace.
#[derive(Clone, Default)]
pub struct Activity {
    pub demand: ResourceDemand,
    pub busy: Option<BusyProvider>,
}

/// Exponentially weighted moving average.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ewma {
    alpha: f32,
    value: Option<f32>,
}

impl Ewma {
    pub fn new(alpha: f32) -> Self {
        Self {
            alpha: alpha.clamp(f32::EPSILON, 1.0),
            value: None,
        }
    }

    /// Adds a sample and returns the smoothed value. The first sample is
    /// taken as is.
    pub fn update(&mut self, sample: f32) -> f32 {
        let v = match self.value {
            None => sample,
            Some(prev) => prev + self.alpha * (sample - prev),
        };
        self.value = Some(v);
        v
    }

    pub fn value(&self) -> Option<f32> {
        self.value
    }
}

/// Session id → root process id. Cheap to clone; clones share state.
#[derive(Debug, Clone, Default)]
pub struct SessionRegistry(Arc<Mutex<HashMap<SessionId, u32>>>);

impl SessionRegistry {
    /// Registers (or replaces) the root process of a session, usually the
    /// agent process itself.
    pub fn register(&self, session_id: SessionId, root_pid: u32) {
        self.lock().insert(session_id, root_pid);
    }

    pub fn unregister(&self, session_id: &SessionId) -> Option<u32> {
        self.lock().remove(session_id)
    }

    pub fn root_pid(&self, session_id: &SessionId) -> Option<u32> {
        self.lock().get(session_id).copied()
    }

    /// All registered sessions, sorted by id.
    pub fn roots(&self) -> Vec<(SessionId, u32)> {
        let mut v: Vec<_> = self.lock().iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort();
        v
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, u32>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Sessions VCA did not start, sampled for display only.
///
/// Deliberately not a [`SessionRegistry`]: it has no way to look a pid up,
/// and the code that acts on processes (user actions, the watcher) only ever
/// sees the registry. A session on this list cannot be paused, killed,
/// throttled or reprioritized, because nothing there can find its pid.
#[derive(Debug, Clone, Default)]
pub struct MonitorOnlyRegistry(SessionRegistry);

impl MonitorOnlyRegistry {
    pub fn add(&self, session_id: SessionId, root_pid: u32) {
        self.0.register(session_id, root_pid);
    }

    pub fn remove(&self, session_id: &SessionId) {
        self.0.unregister(session_id);
    }

    fn roots(&self) -> Vec<(SessionId, u32)> {
        self.0.roots()
    }
}

/// Usage of the monitor-only sessions from the processes already sampled.
/// A pid that exited simply drops out, and a session that is also managed
/// keeps its managed entry.
fn aggregate_monitor_only(
    procs: &[ProcInfo],
    monitored: &[(SessionId, u32)],
    managed: &[(SessionId, u32)],
    cores: usize,
) -> Vec<SessionUsage> {
    let roots: Vec<(SessionId, u32)> = monitored
        .iter()
        .filter(|(id, _)| !managed.iter().any(|(m, _)| m == id))
        .cloned()
        .collect();
    aggregate_sessions(procs, &roots, cores)
        .into_iter()
        .filter(|u| u.process_count > 0)
        .map(|u| SessionUsage {
            monitor_only: true,
            ..u
        })
        .collect()
}

/// Aggregates usage of each session's process tree (root included).
pub(crate) fn aggregate_sessions(
    procs: &[ProcInfo],
    roots: &[(SessionId, u32)],
    cores: usize,
) -> Vec<SessionUsage> {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let tree = ProcTree::from_pairs(procs.iter().map(|p| (p.pid, p.parent)));
    let cores = cores.max(1) as f32;
    // A session started inside another one (a conductor's agent, an agent
    // in a managed shell) owns its own tree: it is not counted again in the
    // outer session.
    let all_roots: HashSet<u32> = roots.iter().map(|(_, pid)| *pid).collect();
    roots
        .iter()
        .map(|(session_id, root)| {
            let mut usage = SessionUsage {
                session_id: session_id.clone(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                process_count: 0,
                monitor_only: false,
            };
            if by_pid.contains_key(root) {
                let others: HashSet<u32> =
                    all_roots.iter().copied().filter(|r| r != root).collect();
                for pid in std::iter::once(*root).chain(tree.descendants_until(*root, &others)) {
                    if let Some(p) = by_pid.get(&pid) {
                        usage.cpu_percent += p.cpu_percent / cores;
                        usage.memory_bytes = usage.memory_bytes.saturating_add(p.memory_bytes);
                        usage.process_count += 1;
                    }
                }
            }
            usage
        })
        .collect()
}

/// Turns monotonically increasing counters into per-second rates.
#[derive(Debug, Default)]
pub(crate) struct RateTracker {
    prev: Option<(Instant, [u64; 4])>,
}

impl RateTracker {
    pub fn update(&mut self, at: Instant, totals: [u64; 4]) -> [u64; 4] {
        let rates = match self.prev {
            Some((then, prev)) => {
                let secs = at.saturating_duration_since(then).as_secs_f64();
                if secs <= 0.0 {
                    [0; 4]
                } else {
                    std::array::from_fn(|i| {
                        (totals[i].saturating_sub(prev[i]) as f64 / secs) as u64
                    })
                }
            }
            None => [0; 4],
        };
        self.prev = Some((at, totals));
        rates
    }
}

/// What makes a disk one disk when counting I/O. On Windows the name is
/// the volume label, often empty for every drive, so the drive (mount point)
/// counts; elsewhere the device name, so one device mounted twice (a bind
/// mount, subvolumes) is counted once.
fn disk_key(disk: &sysinfo::Disk) -> std::ffi::OsString {
    if cfg!(windows) {
        disk.mount_point().as_os_str().to_owned()
    } else {
        disk.name().to_owned()
    }
}

fn is_loopback(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "lo" || n.starts_with("lo0") || n.contains("loopback")
}

/// Samples the system. Not thread-safe; owned by the monitor thread.
///
/// Process handles: on Windows a `sysinfo` `System` keeps an `OpenProcess`
/// handle in every process entry it caches. So the machine-wide listing
/// comes from [`vca_core::proclist`] (no handles), and the `System` that
/// measures CPU and memory (`tree`) holds only the session trees' processes:
/// entries of processes that exited are dropped on the next sample, and it
/// is replaced by an empty one when a session ended, when processes that
/// left a live session's tree outstay [`DETACHED_GRACE`], or when no
/// session is left.
pub struct Sampler {
    /// Machine-wide CPU and memory only (no process entries).
    sys: System,
    /// The process listing where [`vca_core::proclist`] is not available.
    listing: System,
    /// CPU and memory of the session trees' processes.
    tree: System,
    networks: Networks,
    disks: Disks,
    rates: RateTracker,
    cpu: Ewma,
    memory: Ewma,
    cores: usize,
    /// Processes sampled for CPU and memory by the last call.
    last_sampled: usize,
    /// Since when `tree` holds processes that left the session trees.
    detached_since: Option<Instant>,
    /// The roots of the last sample.
    last_roots: Vec<u32>,
}

impl Sampler {
    pub fn new(smoothing_alpha: f32) -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        let cores = sys.cpus().len().max(1);
        Self {
            sys,
            listing: System::new(),
            tree: System::new(),
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list_specifics(
                DiskRefreshKind::nothing().with_io_usage(),
            ),
            rates: RateTracker::default(),
            cpu: Ewma::new(smoothing_alpha),
            memory: Ewma::new(smoothing_alpha),
            cores,
            last_sampled: 0,
            detached_since: None,
            last_roots: Vec::new(),
        }
    }

    /// Processes sampled for CPU and memory by the last call: the session
    /// trees, not every process on the machine.
    pub fn last_sampled(&self) -> usize {
        self.last_sampled
    }

    /// Processes whose CPU and memory are being tracked (and, on Windows,
    /// that hold a process handle).
    pub fn tracked(&self) -> usize {
        self.tree.processes().len()
    }

    /// Every process: PID, parent and start time.
    fn list(&mut self) -> Vec<(u32, Option<u32>, Option<u64>)> {
        #[cfg(windows)]
        if let Some(list) = vca_core::proclist::list() {
            return list
                .into_iter()
                .map(|p| (p.pid, p.parent, Some(p.created)))
                .collect();
        }
        self.listing.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        let list = self
            .listing
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .map(|p| {
                let start = p.start_time();
                (
                    p.pid().as_u32(),
                    p.parent().map(Pid::as_u32),
                    // Windows reports 0 for a process it could not open.
                    (start > 0 || !cfg!(windows)).then_some(start),
                )
            })
            .collect();
        if cfg!(windows) {
            // The fallback must not keep a handle to every process.
            self.listing = System::new();
        }
        list
    }

    /// CPU and memory of the session trees (roots and descendants) only.
    fn sample_trees(&mut self, roots: &[u32]) -> Vec<ProcInfo> {
        if roots.is_empty() {
            self.last_sampled = 0;
            if !self.tree.processes().is_empty() {
                self.tree = System::new();
            }
            return Vec::new();
        }
        // Parents of every process (a listing without per-process reads),
        // an edge kept only when the child started after its parent: a
        // reused PID does not adopt a stranger's children.
        let listed = self.list();
        let alive: HashSet<u32> = listed.iter().map(|(pid, _, _)| *pid).collect();
        let tree = ProcTree::from_timed(listed);
        let mut wanted: Vec<u32> = Vec::new();
        let mut seen = HashSet::new();
        for root in roots {
            if !alive.contains(root) {
                continue;
            }
            for pid in std::iter::once(*root).chain(tree.descendants(*root)) {
                if seen.insert(pid) {
                    wanted.push(pid);
                }
            }
        }
        // A session ended: whatever of its tree still runs is let go at
        // once.
        let ended = self.last_roots.iter().any(|r| !roots.contains(r));
        self.last_roots = roots.to_vec();
        // Processes still running that left a live session's tree keep
        // their entry (and handle) for a bounded time only; then everything
        // starts over.
        let detached = self
            .tree
            .processes()
            .keys()
            .filter(|pid| alive.contains(&pid.as_u32()) && !seen.contains(&pid.as_u32()))
            .count();
        if detached == 0 {
            self.detached_since = None;
        } else {
            let since = *self.detached_since.get_or_insert_with(Instant::now);
            if ended || detached > MAX_DETACHED || since.elapsed() >= DETACHED_GRACE {
                self.tree = System::new();
                self.detached_since = None;
            }
        }
        let pids: Vec<Pid> = wanted.iter().copied().map(Pid::from_u32).collect();
        // Entries of exited processes are refreshed too, so they are
        // removed.
        let mut refresh = pids.clone();
        refresh.extend(
            self.tree
                .processes()
                .keys()
                .filter(|pid| !seen.contains(&pid.as_u32())),
        );
        if !refresh.is_empty() {
            self.tree.refresh_processes_specifics(
                ProcessesToUpdate::Some(&refresh),
                true,
                ProcessRefreshKind::nothing()
                    .with_cpu()
                    .with_memory()
                    .without_tasks(),
            );
        }
        self.last_sampled = pids.len();
        pids.iter()
            .filter_map(|pid| self.tree.process(*pid))
            .map(|p| ProcInfo {
                pid: p.pid().as_u32(),
                parent: p.parent().map(|pp| pp.as_u32()),
                cpu_percent: p.cpu_usage(),
                memory_bytes: p.memory(),
            })
            .collect()
    }

    /// Takes one full sample, disk and network included. CPU figures are
    /// meaningful from the second call on.
    pub fn sample(
        &mut self,
        at: UnixMillis,
        sessions: &[(SessionId, u32)],
        monitored: &[(SessionId, u32)],
    ) -> (ResourceSnapshot, LoadSample) {
        self.sample_with(at, sessions, monitored, true)
    }

    /// Takes one sample. Only the session trees are sampled per process;
    /// `monitored` sessions share that work. Disk and network rates are
    /// read only with `io` (they read 0 otherwise).
    pub fn sample_with(
        &mut self,
        at: UnixMillis,
        sessions: &[(SessionId, u32)],
        monitored: &[(SessionId, u32)],
        io: bool,
    ) -> (ResourceSnapshot, LoadSample) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let roots: Vec<u32> = sessions
            .iter()
            .chain(monitored)
            .map(|(_, pid)| *pid)
            .collect();
        let procs = self.sample_trees(&roots);

        let [disk_r, disk_w, net_rx, net_tx] = if io {
            self.networks.refresh(true);
            self.disks
                .refresh_specifics(true, DiskRefreshKind::nothing().with_io_usage());
            let mut seen_disks = HashSet::new();
            let (mut disk_read, mut disk_write) = (0u64, 0u64);
            for disk in self.disks.list() {
                if seen_disks.insert(disk_key(disk)) {
                    let u = disk.usage();
                    disk_read = disk_read.saturating_add(u.total_read_bytes);
                    disk_write = disk_write.saturating_add(u.total_written_bytes);
                }
            }
            let (mut rx, mut tx) = (0u64, 0u64);
            for (name, data) in self.networks.list() {
                if !is_loopback(name) {
                    rx = rx.saturating_add(data.total_received());
                    tx = tx.saturating_add(data.total_transmitted());
                }
            }
            self.rates
                .update(Instant::now(), [disk_read, disk_write, rx, tx])
        } else {
            // The next rate starts afresh instead of averaging over the
            // time nobody looked.
            self.rates = RateTracker::default();
            [0; 4]
        };

        let cpu = self.sys.global_cpu_usage();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        let mem_percent = if total == 0 {
            0.0
        } else {
            (used as f64 / total as f64 * 100.0) as f32
        };
        let load = LoadSample {
            cpu_percent: self.cpu.update(cpu),
            memory_percent: self.memory.update(mem_percent),
        };
        let mut usage = aggregate_sessions(&procs, sessions, self.cores);
        usage.extend(aggregate_monitor_only(
            &procs, monitored, sessions, self.cores,
        ));
        let snapshot = ResourceSnapshot {
            at,
            cpu_percent: cpu,
            memory_used_bytes: used,
            memory_total_bytes: total,
            disk_read_bytes_per_sec: disk_r,
            disk_write_bytes_per_sec: disk_w,
            net_rx_bytes_per_sec: net_rx,
            net_tx_bytes_per_sec: net_tx,
            sessions: usage,
            queue: Vec::new(),
        };
        (snapshot, load)
    }
}

/// Supplies the current admission queue for snapshots.
pub type QueueProvider = Arc<dyn Fn() -> Vec<QueueEntry> + Send + Sync>;

/// A running monitor. Sampling stops when this is dropped.
pub struct Monitor {
    snapshots: watch::Receiver<ResourceSnapshot>,
    load: watch::Receiver<LoadSample>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    /// Starts sampling on a background thread. `activity` sets the pace
    /// (see [`MonitorConfig::interval_for`]).
    pub fn spawn(
        config: MonitorConfig,
        sessions: SessionRegistry,
        monitored: MonitorOnlyRegistry,
        queue: Option<QueueProvider>,
        activity: Activity,
    ) -> Self {
        let empty = ResourceSnapshot {
            at: vca_core::now_millis(),
            cpu_percent: 0.0,
            memory_used_bytes: 0,
            memory_total_bytes: 0,
            disk_read_bytes_per_sec: 0,
            disk_write_bytes_per_sec: 0,
            net_rx_bytes_per_sec: 0,
            net_tx_bytes_per_sec: 0,
            sessions: Vec::new(),
            queue: Vec::new(),
        };
        let (snap_tx, snapshots) = watch::channel(empty);
        let (load_tx, load) = watch::channel(LoadSample::default());
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("vca-broker-monitor".into())
            .spawn(move || {
                let mut sampler = Sampler::new(config.smoothing_alpha);
                let mut last: Option<Instant> = None;
                loop {
                    // Checked every `interval`, so a viewer or a new session
                    // speeds sampling up within one interval.
                    let roots = sessions.roots();
                    let viewers = activity.demand.viewers();
                    let needed = viewers > 0
                        || !roots.is_empty()
                        || activity.busy.as_ref().is_some_and(|busy| busy());
                    let due = last.is_none_or(|t| t.elapsed() >= config.interval_for(needed));
                    if due {
                        last = Some(Instant::now());
                        let (mut snapshot, sample) = sampler.sample_with(
                            vca_core::now_millis(),
                            &roots,
                            &monitored.roots(),
                            viewers > 0,
                        );
                        if let Some(q) = &queue {
                            snapshot.queue = q();
                        }
                        load_tx.send_replace(sample);
                        snap_tx.send_replace(snapshot);
                    }
                    match stop_rx.recv_timeout(config.tick()) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            })
            .expect("failed to spawn monitor thread");
        Self {
            snapshots,
            load,
            stop: Some(stop_tx),
            thread: Some(thread),
        }
    }

    /// The latest snapshot.
    pub fn snapshot(&self) -> ResourceSnapshot {
        self.snapshots.borrow().clone()
    }

    /// Subscribes to snapshots (one per interval).
    pub fn subscribe(&self) -> watch::Receiver<ResourceSnapshot> {
        self.snapshots.clone()
    }

    /// A receiver of smoothed load; implements [`crate::clock::LoadSource`].
    pub fn load(&self) -> watch::Receiver<LoadSample> {
        self.load.clone()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: Option<u32>, cpu: f32, mem: u64) -> ProcInfo {
        ProcInfo {
            pid,
            parent,
            cpu_percent: cpu,
            memory_bytes: mem,
        }
    }

    #[test]
    fn ewma_smooths() {
        let mut e = Ewma::new(0.5);
        assert_eq!(e.update(100.0), 100.0);
        assert_eq!(e.update(0.0), 50.0);
        assert_eq!(e.update(0.0), 25.0);
        assert_eq!(e.value(), Some(25.0));
    }

    #[test]
    fn rates_from_deltas() {
        let mut r = RateTracker::default();
        let t0 = Instant::now();
        assert_eq!(r.update(t0, [100, 0, 0, 0]), [0; 4]);
        let rates = r.update(t0 + Duration::from_secs(2), [300, 50, 10, 0]);
        assert_eq!(rates, [100, 25, 5, 0]);
        // Counter reset does not underflow.
        let rates = r.update(t0 + Duration::from_secs(3), [0, 50, 10, 0]);
        assert_eq!(rates, [0, 0, 0, 0]);
    }

    #[test]
    fn sessions_aggregate_process_trees() {
        let procs = [
            p(1, None, 0.0, 1),
            p(100, Some(1), 50.0, 1000), // agent A
            p(101, Some(100), 200.0, 4000),
            p(102, Some(101), 100.0, 500),
            p(200, Some(1), 10.0, 300),   // agent B
            p(300, Some(1), 400.0, 9999), // unrelated
        ];
        let roots = vec![
            (SessionId::from("a"), 100),
            (SessionId::from("b"), 200),
            (SessionId::from("gone"), 999),
        ];
        let usage = aggregate_sessions(&procs, &roots, 4);
        assert_eq!(usage[0].process_count, 3);
        assert_eq!(usage[0].memory_bytes, 5500);
        assert!((usage[0].cpu_percent - 87.5).abs() < 1e-3);
        assert_eq!(usage[1].process_count, 1);
        assert_eq!(usage[2].process_count, 0);
        assert_eq!(usage[2].memory_bytes, 0);
    }

    #[test]
    fn a_session_inside_another_is_counted_once() {
        let procs = [
            p(100, None, 10.0, 100),       // conductor (outer session)
            p(101, Some(100), 20.0, 10),   // its own helper
            p(200, Some(101), 50.0, 4000), // an agent session it started
            p(201, Some(200), 30.0, 1000),
        ];
        let roots = vec![
            (SessionId::from("outer"), 100),
            (SessionId::from("inner"), 200),
        ];
        let usage = aggregate_sessions(&procs, &roots, 1);
        assert_eq!(usage[0].process_count, 2);
        assert_eq!(usage[0].memory_bytes, 110);
        assert_eq!(usage[1].process_count, 2);
        assert_eq!(usage[1].memory_bytes, 5000);
    }

    #[test]
    fn a_zero_interval_does_not_spin() {
        let cfg = MonitorConfig {
            interval: Duration::ZERO,
            idle_interval: Duration::ZERO,
            ..Default::default()
        };
        assert_eq!(cfg.tick(), MIN_INTERVAL);
        assert_eq!(cfg.interval_for(true), MIN_INTERVAL);
        assert_eq!(cfg.interval_for(false), MIN_INTERVAL);
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("lo"));
        assert!(is_loopback("lo0"));
        assert!(is_loopback("Loopback Pseudo-Interface 1"));
        assert!(!is_loopback("eth0"));
        assert!(!is_loopback("wlan0"));
    }

    #[test]
    fn real_sampler_reports_memory_and_own_session() {
        let mut s = Sampler::new(0.3);
        let roots = vec![(SessionId::from("me"), std::process::id())];
        let (snap, load) = s.sample(1, &roots, &[]);
        assert!(snap.memory_total_bytes > 0);
        assert!(snap.memory_used_bytes <= snap.memory_total_bytes);
        assert!((0.0..=100.0).contains(&load.memory_percent));
        assert_eq!(snap.sessions.len(), 1);
        assert!(snap.sessions[0].process_count >= 1);
        assert!(snap.sessions[0].memory_bytes > 0);
    }

    #[test]
    fn only_session_trees_are_sampled_per_process() {
        let mut s = Sampler::new(0.3);
        // No sessions: no per-process work at all, but the machine's load.
        let (snap, _) = s.sample_with(1, &[], &[], false);
        assert_eq!(s.last_sampled(), 0);
        assert!(snap.memory_total_bytes > 0);
        // One session: its tree only, however many processes run.
        let roots = vec![(SessionId::from("me"), std::process::id())];
        let (snap, _) = s.sample_with(2, &[], &roots, false);
        assert_eq!(snap.sessions.len(), 1);
        assert!(snap.sessions[0].monitor_only);
        assert_eq!(s.last_sampled(), snap.sessions[0].process_count as usize);
        // A root that is gone costs nothing.
        let gone = vec![(SessionId::from("gone"), u32::MAX - 2)];
        s.sample_with(3, &gone, &[], false);
        assert_eq!(s.last_sampled(), 0);
        assert_eq!(s.tracked(), 0, "nothing is kept for a tree that is gone");
        // Once no session is left, no process entry (or handle) is kept.
        s.sample_with(4, &[], &roots, false);
        assert!(s.tracked() >= 1);
        s.sample_with(5, &[], &[], false);
        assert_eq!(s.tracked(), 0);
    }

    #[test]
    fn disk_and_network_are_read_only_for_viewers() {
        let mut s = Sampler::new(0.3);
        s.sample_with(1, &[], &[], true);
        assert!(s.rates.prev.is_some());
        let (snap, _) = s.sample_with(2, &[], &[], false);
        assert!(s.rates.prev.is_none(), "rates restart after a pause");
        assert_eq!(snap.disk_read_bytes_per_sec, 0);
        assert_eq!(snap.net_rx_bytes_per_sec, 0);
    }

    #[test]
    fn the_pace_follows_activity() {
        let cfg = MonitorConfig {
            interval: Duration::from_secs(1),
            idle_interval: Duration::from_secs(5),
            ..Default::default()
        };
        assert_eq!(cfg.interval_for(true), Duration::from_secs(1));
        assert_eq!(cfg.interval_for(false), Duration::from_secs(5));
        // An idle interval below the active one is no faster.
        let odd = MonitorConfig {
            idle_interval: Duration::from_millis(10),
            ..cfg
        };
        assert_eq!(odd.interval_for(false), Duration::from_secs(1));

        let demand = ResourceDemand::default();
        let a = demand.viewer();
        let b = demand.clone().viewer();
        assert_eq!(demand.viewers(), 2);
        drop(a);
        drop(b);
        assert_eq!(demand.viewers(), 0);
    }

    /// Waits until a new snapshot is published (the `at` changes) or
    /// `timeout` passes; true when a new one arrived. Robust on a loaded
    /// machine, where the monitor's thread may be starved: it waits for the
    /// behaviour instead of counting against the wall clock.
    async fn changed_within(rx: &watch::Receiver<ResourceSnapshot>, timeout: Duration) -> bool {
        let start = rx.borrow().at;
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if rx.borrow().at != start {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Generous allowance for the monitor thread to run under heavy load.
    const PATIENCE: Duration = Duration::from_secs(5);
    /// Long enough that, at the active pace, several samples would land;
    /// used to assert the idle pace produces none.
    const IDLE_CHECK: Duration = Duration::from_millis(600);

    #[tokio::test]
    async fn monitor_slows_down_while_nobody_looks() {
        let demand = ResourceDemand::default();
        let monitor = Monitor::spawn(
            MonitorConfig {
                interval: Duration::from_millis(20),
                idle_interval: Duration::from_secs(3600),
                ..Default::default()
            },
            SessionRegistry::default(),
            MonitorOnlyRegistry::default(),
            None,
            Activity {
                demand: demand.clone(),
                busy: None,
            },
        );
        let rx = monitor.subscribe();
        // The first sample is taken at once; then nothing while idle.
        assert!(changed_within(&rx, PATIENCE).await, "first sample");
        assert!(
            !changed_within(&rx, IDLE_CHECK).await,
            "idle: no new samples"
        );
        // Someone opens the resources view: samples resume.
        let viewer = demand.viewer();
        assert!(
            changed_within(&rx, PATIENCE).await,
            "a viewer resumes sampling"
        );
        assert!(changed_within(&rx, PATIENCE).await, "and keeps sampling");
        // Closed again: back to the idle pace.
        drop(viewer);
        // Let any sample already in flight land, then confirm it is quiet.
        changed_within(&rx, Duration::from_millis(100)).await;
        assert!(!changed_within(&rx, IDLE_CHECK).await, "idle again");
        drop(monitor);
    }

    #[tokio::test]
    async fn a_busy_queue_keeps_the_monitor_at_full_pace() {
        let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = Arc::clone(&busy);
        let monitor = Monitor::spawn(
            MonitorConfig {
                interval: Duration::from_millis(20),
                idle_interval: Duration::from_secs(3600),
                ..Default::default()
            },
            SessionRegistry::default(),
            MonitorOnlyRegistry::default(),
            None,
            Activity {
                demand: ResourceDemand::default(),
                busy: Some(Arc::new(move || flag.load(Ordering::SeqCst))),
            },
        );
        let rx = monitor.subscribe();
        // A busy queue keeps samples coming with no viewer and no session.
        assert!(changed_within(&rx, PATIENCE).await, "busy: sampling");
        assert!(changed_within(&rx, PATIENCE).await, "busy: keeps sampling");
        busy.store(false, Ordering::SeqCst);
        changed_within(&rx, Duration::from_millis(100)).await;
        assert!(!changed_within(&rx, IDLE_CHECK).await, "idle once not busy");
    }

    fn proc_info(pid: u32, parent: Option<u32>, memory_bytes: u64) -> ProcInfo {
        ProcInfo {
            pid,
            parent,
            cpu_percent: 8.0,
            memory_bytes,
        }
    }

    #[test]
    fn monitor_only_usage_sums_the_process_tree_and_is_marked() {
        let procs = [proc_info(10, None, 100), proc_info(11, Some(10), 50)];
        let usage = aggregate_monitor_only(&procs, &[("seen".into(), 10)], &[], 4);
        assert_eq!(usage.len(), 1);
        assert!(usage[0].monitor_only);
        assert_eq!(usage[0].memory_bytes, 150);
        assert_eq!(usage[0].process_count, 2);
        assert_eq!(usage[0].cpu_percent, 4.0);
    }

    #[test]
    fn a_monitored_pid_that_exited_drops_out() {
        let procs = [proc_info(10, None, 100)];
        let usage = aggregate_monitor_only(&procs, &[("gone".into(), 999)], &[], 4);
        assert!(usage.is_empty());
    }

    #[test]
    fn a_managed_session_is_never_listed_as_monitor_only() {
        let procs = [proc_info(10, None, 100)];
        let roots = [("both".into(), 10)];
        assert!(aggregate_monitor_only(&procs, &roots, &roots, 4).is_empty());
    }

    #[test]
    fn the_registry_keeps_monitored_sessions_apart() {
        let monitored = MonitorOnlyRegistry::default();
        monitored.add("seen".into(), 10);
        assert_eq!(monitored.roots().len(), 1);
        monitored.remove(&"seen".into());
        assert!(monitored.roots().is_empty());
    }

    #[tokio::test]
    async fn monitor_publishes_snapshots_with_queue() {
        let registry = SessionRegistry::default();
        registry.register("me".into(), std::process::id());
        let queue: QueueProvider = Arc::new(|| {
            vec![QueueEntry {
                job_id: "j".into(),
                session_id: None,
                command: "make".into(),
                class: vca_core::JobClass::Heavy,
                queued_at: 0,
                position: 0,
            }]
        });
        let monitor = Monitor::spawn(
            MonitorConfig {
                interval: Duration::from_millis(50),
                ..Default::default()
            },
            registry.clone(),
            MonitorOnlyRegistry::default(),
            Some(queue),
            Activity::default(),
        );
        let mut rx = monitor.subscribe();
        rx.changed().await.unwrap();
        rx.changed().await.unwrap();
        let snap = monitor.snapshot();
        assert!(snap.memory_total_bytes > 0);
        assert_eq!(snap.queue.len(), 1);
        assert_eq!(snap.sessions[0].session_id, SessionId::from("me"));
        assert!(monitor.load().borrow().memory_percent > 0.0);
        assert_eq!(registry.unregister(&"me".into()), Some(std::process::id()));
        drop(monitor);
    }
}
