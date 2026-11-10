//! Exponential reconnect backoff with jitter.

use std::time::Duration;

/// Exponential backoff: the ceiling starts at `initial` and doubles per
/// attempt up to `max`; the actual delay is drawn uniformly from
/// `[ceiling / 2, ceiling]` ("equal jitter"), so many devices reconnecting
/// after a server restart do not arrive in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    /// Upper bound of the delay before retry number `attempt` (0-based).
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.min(20);
        self.initial.saturating_mul(factor).min(self.max)
    }

    /// Delay before retry number `attempt`, with `unit` in `[0, 1]` choosing
    /// the point inside the jitter range.
    pub fn delay_with(&self, attempt: u32, unit: f64) -> Duration {
        let ceiling = self.ceiling(attempt);
        let half = ceiling / 2;
        half + half.mul_f64(unit.clamp(0.0, 1.0))
    }

    /// Delay before retry number `attempt` with random jitter.
    pub fn delay(&self, attempt: u32) -> Duration {
        self.delay_with(attempt, random_unit())
    }
}

/// A uniformly distributed value in `[0, 1)`. Jitter does not need a
/// cryptographic RNG, but the v4 UUID generator is already available and is
/// backed by the OS RNG.
fn random_unit() -> f64 {
    // The low 53 bits of a v4 UUID are fully random (the version and
    // variant bits are higher up).
    let bits = uuid::Uuid::new_v4().as_u128() as u64 & ((1u64 << 53) - 1);
    bits as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_doubles_up_to_the_maximum() {
        let b = Backoff::default();
        assert_eq!(b.ceiling(0), Duration::from_millis(500));
        assert_eq!(b.ceiling(1), Duration::from_secs(1));
        assert_eq!(b.ceiling(2), Duration::from_secs(2));
        assert_eq!(b.ceiling(5), Duration::from_secs(16));
        assert_eq!(b.ceiling(6), Duration::from_secs(30));
        assert_eq!(b.ceiling(1000), Duration::from_secs(30));
    }

    #[test]
    fn jitter_stays_within_half_ceiling_and_ceiling() {
        let b = Backoff::default();
        assert_eq!(b.delay_with(3, 0.0), Duration::from_secs(2));
        assert_eq!(b.delay_with(3, 1.0), Duration::from_secs(4));
        assert_eq!(b.delay_with(3, 7.0), Duration::from_secs(4));
        for attempt in 0..40 {
            for _ in 0..20 {
                let d = b.delay(attempt);
                let c = b.ceiling(attempt);
                assert!(d >= c / 2 && d <= c, "{d:?} outside [{:?}, {c:?}]", c / 2);
            }
        }
    }

    #[test]
    fn random_unit_is_in_range_and_varies() {
        let values: Vec<f64> = (0..64).map(|_| random_unit()).collect();
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
        assert!(values.windows(2).any(|w| w[0] != w[1]));
    }
}
