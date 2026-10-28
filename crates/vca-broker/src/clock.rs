//! Injectable time and load sources, so that admission and policy decisions
//! are deterministic under test.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use vca_core::UnixMillis;

/// A source of wall-clock time in [`UnixMillis`].
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> UnixMillis;
}

/// The real system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixMillis {
        vca_core::now_millis()
    }
}

/// A clock that only moves when told to. Cheap to clone; clones share time.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    pub fn new(start: UnixMillis) -> Self {
        Self(Arc::new(AtomicU64::new(start)))
    }

    pub fn advance_ms(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn set(&self, at: UnixMillis) {
        self.0.store(at, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixMillis {
        self.0.load(Ordering::SeqCst)
    }
}

/// Smoothed system load used for admission and threshold decisions.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct LoadSample {
    /// Smoothed CPU usage averaged over all cores, 0–100.
    pub cpu_percent: f32,
    /// Smoothed memory usage, 0–100.
    pub memory_percent: f32,
}

/// Something that reports the current (smoothed) system load.
pub trait LoadSource: Send + Sync + 'static {
    fn load(&self) -> LoadSample;
}

/// A load source with a settable value, for tests and for callers that
/// compute load themselves. Clones share the value.
#[derive(Debug, Clone, Default)]
pub struct FixedLoad(Arc<Mutex<LoadSample>>);

impl FixedLoad {
    pub fn new(sample: LoadSample) -> Self {
        Self(Arc::new(Mutex::new(sample)))
    }

    pub fn set(&self, sample: LoadSample) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = sample;
    }
}

impl LoadSource for FixedLoad {
    fn load(&self) -> LoadSample {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl LoadSource for tokio::sync::watch::Receiver<LoadSample> {
    fn load(&self) -> LoadSample {
        *self.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances_shared() {
        let c = ManualClock::new(1_000);
        let c2 = c.clone();
        c.advance_ms(500);
        assert_eq!(c2.now(), 1_500);
        c2.set(10);
        assert_eq!(c.now(), 10);
    }

    #[test]
    fn fixed_load_is_settable() {
        let l = FixedLoad::default();
        l.set(LoadSample {
            cpu_percent: 50.0,
            memory_percent: 20.0,
        });
        assert_eq!(l.load().cpu_percent, 50.0);
    }
}
