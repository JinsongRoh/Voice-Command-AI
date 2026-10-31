//! Failed-attempt rate limiting with lockout (architecture §8 "Detection").

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use vca_core::UnixMillis;

use crate::clock::SharedClock;

/// Policy for one limiter.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// Failures allowed within `window_ms` before locking out.
    pub max_failures: u32,
    pub window_ms: u64,
    /// How long a key stays locked.
    pub lockout_ms: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_failures: 5,
            window_ms: 5 * 60 * 1000,
            lockout_ms: 15 * 60 * 1000,
        }
    }
}

#[derive(Debug, Default)]
struct Entry {
    failures: VecDeque<UnixMillis>,
    locked_until: Option<UnixMillis>,
}

/// Keyed failure counter. Keys are free-form (e.g. `ip:203.0.113.5`,
/// `device:dev_x`, `pair:global`).
#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimitConfig,
    clock: SharedClock,
    entries: Mutex<HashMap<String, Entry>>,
}

/// Upper bound on tracked keys, to bound memory under attack.
const MAX_KEYS: usize = 10_000;

impl RateLimiter {
    pub fn new(config: RateLimitConfig, clock: SharedClock) -> Self {
        Self {
            config,
            clock,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `Err(locked_until)` if `key` is locked out.
    pub fn check(&self, key: &str) -> Result<(), UnixMillis> {
        let now = self.clock.now_millis();
        let entries = self.entries.lock().expect("poisoned");
        match entries.get(key).and_then(|e| e.locked_until) {
            Some(until) if until > now => Err(until),
            _ => Ok(()),
        }
    }

    /// Records a failure. Returns `true` if this failure caused a lockout.
    pub fn record_failure(&self, key: &str) -> bool {
        let now = self.clock.now_millis();
        let mut entries = self.entries.lock().expect("poisoned");
        if entries.len() >= MAX_KEYS && !entries.contains_key(key) {
            Self::prune(&mut entries, now, self.config.window_ms);
        }
        let entry = entries.entry(key.to_string()).or_default();
        if let Some(until) = entry.locked_until {
            if until > now {
                return false;
            }
            entry.locked_until = None;
        }
        let cutoff = now.saturating_sub(self.config.window_ms);
        while entry.failures.front().is_some_and(|&t| t <= cutoff) {
            entry.failures.pop_front();
        }
        entry.failures.push_back(now);
        if entry.failures.len() as u32 >= self.config.max_failures {
            entry.failures.clear();
            entry.locked_until = Some(now + self.config.lockout_ms);
            return true;
        }
        false
    }

    /// Clears the failure history of `key` (after a success).
    pub fn record_success(&self, key: &str) {
        self.entries.lock().expect("poisoned").remove(key);
    }

    /// Unlocks every key (e.g. administrator action).
    pub fn reset_all(&self) {
        self.entries.lock().expect("poisoned").clear();
    }

    fn prune(entries: &mut HashMap<String, Entry>, now: UnixMillis, window: u64) {
        entries.retain(|_, e| {
            e.locked_until.is_some_and(|u| u > now)
                || e.failures.back().is_some_and(|&t| t + window > now)
        });
        if entries.len() >= MAX_KEYS {
            // Still full of live entries: drop non-locked ones first.
            entries.retain(|_, e| e.locked_until.is_some_and(|u| u > now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    #[test]
    fn locks_after_max_failures_and_unlocks_later() {
        let clock = ManualClock::new(1_000_000);
        let rl = RateLimiter::new(
            RateLimitConfig {
                max_failures: 3,
                window_ms: 60_000,
                lockout_ms: 120_000,
            },
            clock.clone(),
        );
        assert!(rl.check("k").is_ok());
        assert!(!rl.record_failure("k"));
        assert!(!rl.record_failure("k"));
        assert!(rl.record_failure("k"));
        assert!(rl.check("k").is_err());
        assert!(rl.check("other").is_ok());
        clock.advance_millis(119_999);
        assert!(rl.check("k").is_err());
        clock.advance_millis(2);
        assert!(rl.check("k").is_ok());
        // History was cleared at lockout.
        assert!(!rl.record_failure("k"));
    }

    #[test]
    fn failures_outside_window_expire() {
        let clock = ManualClock::new(0);
        let rl = RateLimiter::new(
            RateLimitConfig {
                max_failures: 2,
                window_ms: 1_000,
                lockout_ms: 10_000,
            },
            clock.clone(),
        );
        rl.record_failure("k");
        clock.advance_millis(1_500);
        assert!(!rl.record_failure("k"));
        assert!(rl.check("k").is_ok());
    }

    #[test]
    fn success_clears_history() {
        let clock = ManualClock::new(0);
        let rl = RateLimiter::new(
            RateLimitConfig {
                max_failures: 2,
                window_ms: 1_000,
                lockout_ms: 10_000,
            },
            clock,
        );
        rl.record_failure("k");
        rl.record_success("k");
        assert!(!rl.record_failure("k"));
    }

    #[test]
    fn memory_is_bounded() {
        let clock = ManualClock::new(0);
        let rl = RateLimiter::new(RateLimitConfig::default(), clock.clone());
        for i in 0..(MAX_KEYS + 10) {
            if i % 1000 == 0 {
                clock.advance_millis(10 * 60 * 1000);
            }
            rl.record_failure(&format!("k{i}"));
        }
        assert!(rl.entries.lock().unwrap().len() <= MAX_KEYS);
    }
}
