//! Threshold watcher: decides when to act on sustained resource pressure
//! (RES-03) and when to warn about approaching exhaustion (RES-08).
//!
//! The watcher is a pure state machine fed with samples; the [`crate::Broker`]
//! executes its decisions. Behaviour:
//!
//! - **Pressure** means smoothed memory above `100 - memory_headroom_percent`
//!   or smoothed CPU above `cpu_threshold_percent`.
//! - Once pressure has lasted `sustain_secs`, the configured action is taken
//!   against **one** session: the heaviest (by memory under memory pressure,
//!   otherwise by CPU) that has child processes, is not the foreground
//!   session, is not exempt, and has not been acted on yet. Further sessions
//!   are acted on at most once per `cooldown_secs` while pressure continues.
//! - Once load falls `resume_hysteresis_percent` below both thresholds,
//!   actions are undone one session per cooldown, most recent first.
//! - Alerts are emitted on level changes: warning above the memory limit,
//!   critical above `deny_memory_percent`, and normal on recovery.

use serde::{Deserialize, Serialize};
use vca_core::{SessionId, SessionUsage, UnixMillis};

use crate::clock::LoadSample;
use crate::policy::{Policy, ThresholdAction};

/// Alert severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertLevel {
    Normal,
    Warning,
    Critical,
}

/// A decision for the broker to carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Apply `action` to the session's child processes.
    Act {
        session_id: SessionId,
        action: ThresholdAction,
        reason: String,
    },
    /// Undo a previous action (resume, or restore normal priority).
    Restore {
        session_id: SessionId,
        action: ThresholdAction,
    },
    Alert {
        level: AlertLevel,
        message: String,
    },
}

/// Pressure state across samples.
#[derive(Debug, Default)]
pub struct ThresholdWatcher {
    pressure_since: Option<UnixMillis>,
    last_change_at: Option<UnixMillis>,
    acted: Vec<(SessionId, ThresholdAction)>,
    alert: Option<AlertLevel>,
}

impl ThresholdWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sessions currently acted on (oldest first).
    pub fn acted(&self) -> &[(SessionId, ThresholdAction)] {
        &self.acted
    }

    /// Forgets a session (for example when it ended or was resumed manually).
    pub fn forget(&mut self, session_id: &SessionId) {
        self.acted.retain(|(s, _)| s != session_id);
    }

    /// Processes one sample and returns the decisions to carry out.
    pub fn step(
        &mut self,
        policy: &Policy,
        now: UnixMillis,
        load: LoadSample,
        sessions: &[SessionUsage],
        foreground: Option<&SessionId>,
    ) -> Vec<Decision> {
        let a = &policy.admission;
        let t = &policy.threshold;
        let mem_limit = a.memory_limit_percent();
        let cpu_limit = a.cpu_threshold_percent;
        let mut out = Vec::new();

        // Alerts on level changes.
        let level = if load.memory_percent >= a.deny_memory_percent {
            AlertLevel::Critical
        } else if load.memory_percent > mem_limit {
            AlertLevel::Warning
        } else {
            AlertLevel::Normal
        };
        if self.alert.unwrap_or(AlertLevel::Normal) != level {
            let message = match level {
                AlertLevel::Critical => format!(
                    "memory critical: {:.0}% used (limit {:.0}%); new heavy jobs are denied",
                    load.memory_percent, a.deny_memory_percent
                ),
                AlertLevel::Warning => format!(
                    "memory high: {:.0}% used, headroom below {:.0}%; new heavy jobs wait",
                    load.memory_percent, a.memory_headroom_percent
                ),
                AlertLevel::Normal => {
                    format!("memory back to normal: {:.0}% used", load.memory_percent)
                }
            };
            out.push(Decision::Alert { level, message });
        }
        self.alert = Some(level);

        // Drop sessions that no longer exist.
        self.acted
            .retain(|(id, _)| sessions.iter().any(|s| &s.session_id == id));

        let cooled = |last: Option<UnixMillis>| {
            last.is_none_or(|at| now.saturating_sub(at) >= t.cooldown_secs * 1000)
        };
        let memory_pressure = load.memory_percent > mem_limit;
        let cpu_pressure = load.cpu_percent > cpu_limit;

        if memory_pressure || cpu_pressure {
            let since = *self.pressure_since.get_or_insert(now);
            let sustained = now.saturating_sub(since) >= t.sustain_secs * 1000;
            if sustained && t.action != ThresholdAction::None && cooled(self.last_change_at) {
                let mut candidates: Vec<&SessionUsage> = sessions
                    .iter()
                    .filter(|s| s.process_count > 1)
                    .filter(|s| Some(&s.session_id) != foreground)
                    .filter(|s| !policy.session(&s.session_id.0).is_some_and(|p| p.exempt))
                    .filter(|s| !self.acted.iter().any(|(id, _)| id == &s.session_id))
                    .collect();
                if memory_pressure {
                    candidates.sort_by_key(|s| std::cmp::Reverse(s.memory_bytes));
                } else {
                    candidates.sort_by(|x, y| y.cpu_percent.total_cmp(&x.cpu_percent));
                }
                if let Some(target) = candidates.first() {
                    let reason = if memory_pressure {
                        format!(
                            "memory at {:.0}% (limit {:.0}%) for {}s; session uses {} MiB",
                            load.memory_percent,
                            mem_limit,
                            t.sustain_secs,
                            target.memory_bytes / (1024 * 1024)
                        )
                    } else {
                        format!(
                            "CPU at {:.0}% (limit {:.0}%) for {}s; session uses {:.0}%",
                            load.cpu_percent, cpu_limit, t.sustain_secs, target.cpu_percent
                        )
                    };
                    self.acted.push((target.session_id.clone(), t.action));
                    self.last_change_at = Some(now);
                    out.push(Decision::Act {
                        session_id: target.session_id.clone(),
                        action: t.action,
                        reason,
                    });
                }
            }
        } else {
            self.pressure_since = None;
            let hyst = t.resume_hysteresis_percent;
            let relieved =
                load.memory_percent < mem_limit - hyst && load.cpu_percent < cpu_limit - hyst;
            if relieved && cooled(self.last_change_at) {
                if let Some((session_id, action)) = self.acted.pop() {
                    self.last_change_at = Some(now);
                    out.push(Decision::Restore { session_id, action });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::SessionPolicy;

    const MIB: u64 = 1024 * 1024;

    fn usage(id: &str, mem_mib: u64, cpu: f32, procs: u32) -> SessionUsage {
        SessionUsage {
            session_id: id.into(),
            cpu_percent: cpu,
            memory_bytes: mem_mib * MIB,
            process_count: procs,
        }
    }

    fn load(cpu: f32, mem: f32) -> LoadSample {
        LoadSample {
            cpu_percent: cpu,
            memory_percent: mem,
        }
    }

    fn acts(d: &[Decision]) -> Vec<(String, ThresholdAction)> {
        d.iter()
            .filter_map(|d| match d {
                Decision::Act {
                    session_id, action, ..
                } => Some((session_id.0.clone(), *action)),
                _ => None,
            })
            .collect()
    }

    fn restores(d: &[Decision]) -> Vec<String> {
        d.iter()
            .filter_map(|d| match d {
                Decision::Restore { session_id, .. } => Some(session_id.0.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn acts_after_sustained_pressure_then_restores() {
        let policy = Policy::default(); // limit 85 %, sustain 5 s, cooldown 30 s
        let sessions = [
            usage("small", 100, 10.0, 3),
            usage("big", 4000, 10.0, 5),
            usage("fg", 9000, 10.0, 5),
        ];
        let fg = SessionId::from("fg");
        let mut w = ThresholdWatcher::new();
        let mut t = 0;
        let d = w.step(&policy, t, load(10.0, 90.0), &sessions, Some(&fg));
        assert!(acts(&d).is_empty(), "not sustained yet");
        assert!(matches!(
            d[0],
            Decision::Alert {
                level: AlertLevel::Warning,
                ..
            }
        ));
        t += 5_000;
        let d = w.step(&policy, t, load(10.0, 90.0), &sessions, Some(&fg));
        assert_eq!(
            acts(&d),
            vec![("big".into(), ThresholdAction::PauseChildren)]
        );
        // Cooldown: no second action right away.
        t += 1_000;
        assert!(acts(&w.step(&policy, t, load(10.0, 90.0), &sessions, Some(&fg))).is_empty());
        t += 30_000;
        let d = w.step(&policy, t, load(10.0, 90.0), &sessions, Some(&fg));
        assert_eq!(
            acts(&d),
            vec![("small".into(), ThresholdAction::PauseChildren)]
        );
        // Nothing left to act on (foreground is never touched).
        t += 30_000;
        assert!(acts(&w.step(&policy, t, load(10.0, 90.0), &sessions, Some(&fg))).is_empty());

        // In the hysteresis band: no restore yet.
        t += 30_000;
        let d = w.step(&policy, t, load(10.0, 82.0), &sessions, Some(&fg));
        assert!(restores(&d).is_empty());
        assert!(matches!(
            d[0],
            Decision::Alert {
                level: AlertLevel::Normal,
                ..
            }
        ));
        // Relieved: restore most recent first, one per cooldown.
        let d = w.step(&policy, t, load(10.0, 70.0), &sessions, Some(&fg));
        assert_eq!(restores(&d), vec!["small".to_string()]);
        t += 1_000;
        assert!(restores(&w.step(&policy, t, load(10.0, 70.0), &sessions, Some(&fg))).is_empty());
        t += 30_000;
        let d = w.step(&policy, t, load(10.0, 70.0), &sessions, Some(&fg));
        assert_eq!(restores(&d), vec!["big".to_string()]);
        assert!(w.acted().is_empty());
    }

    #[test]
    fn cpu_pressure_targets_cpu_heaviest_and_skips_exempt() {
        let mut policy = Policy::default();
        policy.threshold.sustain_secs = 0;
        policy.threshold.action = ThresholdAction::Deprioritize;
        policy.sessions.insert(
            "hot".into(),
            SessionPolicy {
                exempt: true,
                ..Default::default()
            },
        );
        let sessions = [
            usage("hot", 10, 80.0, 4),
            usage("warm", 10, 40.0, 4),
            usage("solo", 10, 99.0, 1), // no children
        ];
        let mut w = ThresholdWatcher::new();
        let d = w.step(&policy, 0, load(95.0, 10.0), &sessions, None);
        assert_eq!(
            acts(&d),
            vec![("warm".into(), ThresholdAction::Deprioritize)]
        );
        assert!(!d.iter().any(|d| matches!(d, Decision::Alert { .. })));
    }

    #[test]
    fn pressure_must_be_continuous() {
        let policy = Policy::default();
        let sessions = [usage("a", 100, 10.0, 3)];
        let mut w = ThresholdWatcher::new();
        w.step(&policy, 0, load(10.0, 90.0), &sessions, None);
        w.step(&policy, 3_000, load(10.0, 50.0), &sessions, None);
        assert!(acts(&w.step(&policy, 6_000, load(10.0, 90.0), &sessions, None)).is_empty());
        assert_eq!(
            acts(&w.step(&policy, 11_000, load(10.0, 90.0), &sessions, None)).len(),
            1
        );
    }

    #[test]
    fn action_none_only_alerts_and_critical_level() {
        let mut policy = Policy::default();
        policy.threshold.action = ThresholdAction::None;
        policy.threshold.sustain_secs = 0;
        let sessions = [usage("a", 100, 10.0, 3)];
        let mut w = ThresholdWatcher::new();
        let d = w.step(&policy, 0, load(10.0, 98.0), &sessions, None);
        assert_eq!(d.len(), 1);
        assert!(matches!(
            d[0],
            Decision::Alert {
                level: AlertLevel::Critical,
                ..
            }
        ));
        // Same level again: no duplicate alert.
        assert!(
            w.step(&policy, 1, load(10.0, 98.0), &sessions, None)
                .is_empty()
        );
    }

    #[test]
    fn vanished_sessions_are_forgotten() {
        let mut policy = Policy::default();
        policy.threshold.sustain_secs = 0;
        let mut w = ThresholdWatcher::new();
        let d = w.step(
            &policy,
            0,
            load(10.0, 90.0),
            &[usage("a", 100, 1.0, 2)],
            None,
        );
        assert_eq!(acts(&d).len(), 1);
        w.step(&policy, 1, load(10.0, 90.0), &[], None);
        assert!(w.acted().is_empty());
    }
}
