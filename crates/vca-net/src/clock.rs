//! Injectable wall clock, so that expiry logic can be tested deterministically.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vca_core::UnixMillis;

/// Source of wall-clock time in milliseconds since the Unix epoch.
pub trait Clock: Send + Sync + Debug {
    fn now_millis(&self) -> UnixMillis;

    /// Seconds since the Unix epoch.
    fn now_secs(&self) -> u64 {
        self.now_millis() / 1000
    }
}

/// Shared, type-erased clock.
pub type SharedClock = Arc<dyn Clock>;

/// The real system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> UnixMillis {
        vca_core::now_millis()
    }
}

/// Returns a shared [`SystemClock`].
pub fn system_clock() -> SharedClock {
    Arc::new(SystemClock)
}

/// A manually driven clock for tests.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub fn new(start_millis: UnixMillis) -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(start_millis)))
    }

    pub fn set(&self, millis: UnixMillis) {
        self.0.store(millis, Ordering::SeqCst);
    }

    pub fn advance_millis(&self, millis: u64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }

    pub fn advance_secs(&self, secs: u64) {
        self.advance_millis(secs * 1000);
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> UnixMillis {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances() {
        let c = ManualClock::new(1_000);
        c.advance_secs(2);
        assert_eq!(c.now_millis(), 3_000);
        assert_eq!(c.now_secs(), 3);
        c.set(10);
        assert_eq!(c.now_millis(), 10);
    }

    #[test]
    fn system_clock_is_recent() {
        // 2020-01-01 in millis.
        assert!(SystemClock.now_millis() > 1_577_836_800_000);
    }
}
