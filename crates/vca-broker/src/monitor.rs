//! Periodic resource sampling (RES-01).
//!
//! A [`Monitor`] runs a sampler on a dedicated thread (default every second)
//! and publishes the latest [`ResourceSnapshot`] and smoothed [`LoadSample`]
//! through `watch` channels. Per-session usage is the sum over each
//! registered session root process and all of its descendants.
//!
//! Units: system `cpu_percent` is averaged over all cores (0–100). Session
//! `cpu_percent` uses the same scale, i.e. the share of the whole machine
//! (a session saturating 2 of 8 cores reports 25). Disk and network values
//! are rates in bytes per second computed from counter deltas; loopback
//! interfaces are excluded. GPU sampling is not implemented yet.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sysinfo::{DiskRefreshKind, Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::watch;
use vca_core::{QueueEntry, ResourceSnapshot, SessionId, SessionUsage, UnixMillis};

use crate::clock::LoadSample;
use crate::proctree::{ProcInfo, ProcTree};

/// Sampler settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorConfig {
    pub interval: Duration,
    /// Weight of the newest sample in the exponential moving average (0–1].
    pub smoothing_alpha: f32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            smoothing_alpha: 0.3,
        }
    }
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

/// Aggregates usage of each session's process tree (root included).
pub(crate) fn aggregate_sessions(
    procs: &[ProcInfo],
    roots: &[(SessionId, u32)],
    cores: usize,
) -> Vec<SessionUsage> {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let tree = ProcTree::from_pairs(procs.iter().map(|p| (p.pid, p.parent)));
    let cores = cores.max(1) as f32;
    roots
        .iter()
        .map(|(session_id, root)| {
            let mut usage = SessionUsage {
                session_id: session_id.clone(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                process_count: 0,
            };
            if by_pid.contains_key(root) {
                let mut seen = HashSet::new();
                for pid in std::iter::once(*root).chain(tree.descendants(*root)) {
                    if let (true, Some(p)) = (seen.insert(pid), by_pid.get(&pid)) {
                        usage.cpu_percent += p.cpu_percent / cores;
                        usage.memory_bytes += p.memory_bytes;
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

fn is_loopback(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "lo" || n.starts_with("lo0") || n.contains("loopback")
}

/// Samples the system. Not thread-safe; owned by the monitor thread.
pub struct Sampler {
    sys: System,
    networks: Networks,
    disks: Disks,
    rates: RateTracker,
    cpu: Ewma,
    memory: Ewma,
    cores: usize,
}

impl Sampler {
    pub fn new(smoothing_alpha: f32) -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        let cores = sys.cpus().len().max(1);
        Self {
            sys,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list_specifics(
                DiskRefreshKind::nothing().with_io_usage(),
            ),
            rates: RateTracker::default(),
            cpu: Ewma::new(smoothing_alpha),
            memory: Ewma::new(smoothing_alpha),
            cores,
        }
    }

    /// Takes one sample. CPU figures are meaningful from the second call on.
    pub fn sample(
        &mut self,
        at: UnixMillis,
        sessions: &[(SessionId, u32)],
    ) -> (ResourceSnapshot, LoadSample) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        self.networks.refresh(true);
        self.disks
            .refresh_specifics(true, DiskRefreshKind::nothing().with_io_usage());

        let mut seen_disks = HashSet::new();
        let (mut disk_read, mut disk_write) = (0u64, 0u64);
        for disk in self.disks.list() {
            if seen_disks.insert(disk.name().to_owned()) {
                let u = disk.usage();
                disk_read += u.total_read_bytes;
                disk_write += u.total_written_bytes;
            }
        }
        let (mut rx, mut tx) = (0u64, 0u64);
        for (name, data) in self.networks.list() {
            if !is_loopback(name) {
                rx += data.total_received();
                tx += data.total_transmitted();
            }
        }
        let [disk_r, disk_w, net_rx, net_tx] = self
            .rates
            .update(Instant::now(), [disk_read, disk_write, rx, tx]);

        let procs: Vec<ProcInfo> = self
            .sys
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .map(|p| ProcInfo {
                pid: p.pid().as_u32(),
                parent: p.parent().map(|pp| pp.as_u32()),
                cpu_percent: p.cpu_usage(),
                memory_bytes: p.memory(),
            })
            .collect();

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
        let snapshot = ResourceSnapshot {
            at,
            cpu_percent: cpu,
            memory_used_bytes: used,
            memory_total_bytes: total,
            disk_read_bytes_per_sec: disk_r,
            disk_write_bytes_per_sec: disk_w,
            net_rx_bytes_per_sec: net_rx,
            net_tx_bytes_per_sec: net_tx,
            sessions: aggregate_sessions(&procs, sessions, self.cores),
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
    /// Starts sampling on a background thread.
    pub fn spawn(
        config: MonitorConfig,
        sessions: SessionRegistry,
        queue: Option<QueueProvider>,
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
                loop {
                    let (mut snapshot, sample) =
                        sampler.sample(vca_core::now_millis(), &sessions.roots());
                    if let Some(q) = &queue {
                        snapshot.queue = q();
                    }
                    load_tx.send_replace(sample);
                    snap_tx.send_replace(snapshot);
                    match stop_rx.recv_timeout(config.interval) {
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
        let (snap, load) = s.sample(1, &roots);
        assert!(snap.memory_total_bytes > 0);
        assert!(snap.memory_used_bytes <= snap.memory_total_bytes);
        assert!((0.0..=100.0).contains(&load.memory_percent));
        assert_eq!(snap.sessions.len(), 1);
        assert!(snap.sessions[0].process_count >= 1);
        assert!(snap.sessions[0].memory_bytes > 0);
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
            Some(queue),
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
