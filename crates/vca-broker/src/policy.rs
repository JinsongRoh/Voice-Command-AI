//! Broker policies, loaded from `policies.toml`.
//!
//! Every field has a default, so an empty or missing file yields the built-in
//! autonomous policy (RES-04). Example with all keys:
//!
//! ```toml
//! # Decide and act automatically. When false, threshold actions are only
//! # proposed (events with `applied = false`); admission still runs.
//! autonomous = true
//! # Log what would be done without touching any process.
//! dry_run = false
//!
//! [admission]
//! max_heavy_jobs = "auto"        # or a number, e.g. 2
//! memory_headroom_percent = 15   # no new heavy job above 100 - 15 = 85 % memory
//! cpu_threshold_percent = 90     # no new heavy job above this smoothed CPU load
//! deny_memory_percent = 97       # deny (instead of queue) above this memory load
//! max_queue_len = 64
//! queue_timeout_secs = 1800      # default time a client waits in the queue
//! slot_lease_secs = 1800         # a slot without heartbeat or live pid expires after this
//! foreground_first = true
//!
//! [threshold]
//! action = "pause_children"      # none | deprioritize | pause_children | propose_stop
//! sustain_secs = 5               # pressure must last this long before acting
//! cooldown_secs = 30             # minimum time between two actions
//! resume_hysteresis_percent = 5  # resume once load is this far below the threshold
//!
//! [sessions."my-session-id"]
//! max_heavy_jobs = 1
//! priority = 10                  # higher is admitted first
//! exempt = false                 # never paused or deprioritized when true
//!
//! [[classifier.heavy]]
//! program = "terraform"
//! args = ["apply", "plan"]
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::classify::ClassifierConfig;

/// Errors from loading or validating a policy.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("cannot read policy file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid policy file {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid policy{}: {}", .path.as_ref().map(|p| format!(" file {}", p.display())).unwrap_or_default(), .problems.join("; "))]
    Invalid {
        path: Option<PathBuf>,
        problems: Vec<String>,
    },
}

/// Concurrent heavy job limit: a fixed number or derived from the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "RawMaxJobs", into = "RawMaxJobs")]
pub enum MaxJobs {
    #[default]
    Auto,
    Fixed(u32),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RawMaxJobs {
    Num(i64),
    Str(String),
}

impl TryFrom<RawMaxJobs> for MaxJobs {
    type Error = String;
    fn try_from(raw: RawMaxJobs) -> Result<Self, String> {
        match raw {
            RawMaxJobs::Str(s) if s.eq_ignore_ascii_case("auto") => Ok(Self::Auto),
            RawMaxJobs::Str(s) => Err(format!("expected \"auto\" or a number, got \"{s}\"")),
            RawMaxJobs::Num(n) => u32::try_from(n)
                .map(Self::Fixed)
                .map_err(|_| format!("expected a non-negative number, got {n}")),
        }
    }
}

impl From<MaxJobs> for RawMaxJobs {
    fn from(v: MaxJobs) -> Self {
        match v {
            MaxJobs::Auto => Self::Str("auto".into()),
            MaxJobs::Fixed(n) => Self::Num(n.into()),
        }
    }
}

impl MaxJobs {
    /// Resolves the limit: `auto` is `max(1, min(cores / 4, memory_gb / 8))`.
    pub fn resolve(self, cores: usize, memory_total_bytes: u64) -> u32 {
        match self {
            Self::Fixed(n) => n,
            Self::Auto => {
                let by_cpu = (cores / 4) as u64;
                let by_mem = memory_total_bytes / (8 * 1024 * 1024 * 1024);
                by_cpu.min(by_mem).clamp(1, u32::MAX as u64) as u32
            }
        }
    }
}

/// What to do when sustained load crosses a threshold (RES-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdAction {
    /// Only alert.
    None,
    /// Lower the CPU priority of the session's child processes.
    Deprioritize,
    /// Suspend the session's child processes (the agent itself keeps running).
    #[default]
    PauseChildren,
    /// Propose terminating the session's child processes; never applied automatically.
    ProposeStop,
}

impl fmt::Display for ThresholdAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Deprioritize => "deprioritize",
            Self::PauseChildren => "pause_children",
            Self::ProposeStop => "propose_stop",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdmissionPolicy {
    pub max_heavy_jobs: MaxJobs,
    pub memory_headroom_percent: f32,
    pub cpu_threshold_percent: f32,
    pub deny_memory_percent: f32,
    pub max_queue_len: u32,
    pub queue_timeout_secs: u64,
    pub slot_lease_secs: u64,
    pub foreground_first: bool,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            max_heavy_jobs: MaxJobs::Auto,
            memory_headroom_percent: 15.0,
            cpu_threshold_percent: 90.0,
            deny_memory_percent: 97.0,
            max_queue_len: 64,
            queue_timeout_secs: 1800,
            slot_lease_secs: 1800,
            foreground_first: true,
        }
    }
}

impl AdmissionPolicy {
    /// Memory load (percent) above which no new heavy job is admitted.
    pub fn memory_limit_percent(&self) -> f32 {
        100.0 - self.memory_headroom_percent
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThresholdPolicy {
    pub action: ThresholdAction,
    pub sustain_secs: u64,
    pub cooldown_secs: u64,
    pub resume_hysteresis_percent: f32,
}

impl Default for ThresholdPolicy {
    fn default() -> Self {
        Self {
            action: ThresholdAction::PauseChildren,
            sustain_secs: 5,
            cooldown_secs: 30,
            resume_hysteresis_percent: 5.0,
        }
    }
}

/// Per-session overrides, keyed by session id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SessionPolicy {
    /// Maximum heavy jobs this session may run at once (0 denies heavy jobs).
    pub max_heavy_jobs: Option<u32>,
    /// Queue priority; higher is admitted first.
    pub priority: i32,
    /// Never paused or deprioritized by the threshold watcher.
    pub exempt: bool,
}

/// The complete broker policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Policy {
    pub autonomous: bool,
    pub dry_run: bool,
    pub admission: AdmissionPolicy,
    pub threshold: ThresholdPolicy,
    pub sessions: BTreeMap<String, SessionPolicy>,
    pub classifier: ClassifierConfig,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            autonomous: true,
            dry_run: false,
            admission: AdmissionPolicy::default(),
            threshold: ThresholdPolicy::default(),
            sessions: BTreeMap::new(),
            classifier: ClassifierConfig::default(),
        }
    }
}

impl Policy {
    /// Parses and validates a policy from TOML text.
    pub fn from_toml_str(text: &str) -> Result<Self, PolicyError> {
        Self::parse(text, None)
    }

    /// Loads a policy file. A missing file yields the default autonomous policy.
    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, Some(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(PolicyError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Serializes the policy back to TOML (for editing from the UI).
    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    fn parse(text: &str, path: Option<&Path>) -> Result<Self, PolicyError> {
        let policy: Policy = toml::from_str(text).map_err(|e| PolicyError::Parse {
            path: path
                .map(Path::to_owned)
                .unwrap_or_else(|| "<inline>".into()),
            message: e.to_string().trim_end().to_owned(),
        })?;
        policy.validate().map_err(|problems| PolicyError::Invalid {
            path: path.map(Path::to_owned),
            problems,
        })?;
        Ok(policy)
    }

    /// Checks value ranges and consistency. Returns every problem found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        let a = &self.admission;
        let mut percent = |name: &str, v: f32, lo: f32, hi: f32| {
            if !(lo..=hi).contains(&v) || v.is_nan() {
                problems.push(format!("{name} must be between {lo} and {hi}, got {v}"));
            }
        };
        percent(
            "admission.memory_headroom_percent",
            a.memory_headroom_percent,
            0.0,
            90.0,
        );
        percent(
            "admission.cpu_threshold_percent",
            a.cpu_threshold_percent,
            1.0,
            100.0,
        );
        percent(
            "admission.deny_memory_percent",
            a.deny_memory_percent,
            1.0,
            100.0,
        );
        percent(
            "threshold.resume_hysteresis_percent",
            self.threshold.resume_hysteresis_percent,
            0.0,
            50.0,
        );
        if a.deny_memory_percent < a.memory_limit_percent() {
            problems.push(format!(
                "admission.deny_memory_percent ({}) must not be below 100 - memory_headroom_percent ({})",
                a.deny_memory_percent,
                a.memory_limit_percent()
            ));
        }
        if a.max_heavy_jobs == MaxJobs::Fixed(0) {
            problems.push(
                "admission.max_heavy_jobs must be at least 1 (use a session limit of 0 to block a session)"
                    .into(),
            );
        }
        if a.max_queue_len == 0 {
            problems.push("admission.max_queue_len must be at least 1".into());
        }
        if a.slot_lease_secs == 0 {
            problems.push("admission.slot_lease_secs must be at least 1".into());
        }
        for (id, s) in &self.sessions {
            if id.trim().is_empty() {
                problems.push("sessions: session id must not be empty".into());
            }
            if let (Some(limit), MaxJobs::Fixed(global)) = (s.max_heavy_jobs, a.max_heavy_jobs) {
                if limit > global {
                    problems.push(format!(
                        "sessions.\"{id}\".max_heavy_jobs ({limit}) exceeds admission.max_heavy_jobs ({global})"
                    ));
                }
            }
        }
        for (kind, rules) in [
            ("heavy", &self.classifier.heavy),
            ("light", &self.classifier.light),
        ] {
            for (i, r) in rules.iter().enumerate() {
                if r.program.trim().is_empty() {
                    problems.push(format!("classifier.{kind}[{i}].program must not be empty"));
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// The session's overrides, if any.
    pub fn session(&self, session_id: &str) -> Option<&SessionPolicy> {
        self.sessions.get(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn empty_file_is_autonomous_default() {
        let p = Policy::from_toml_str("").unwrap();
        assert_eq!(p, Policy::default());
        assert!(p.autonomous);
        assert_eq!(p.admission.memory_headroom_percent, 15.0);
        assert_eq!(p.admission.max_heavy_jobs, MaxJobs::Auto);
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = Policy::load(&dir.path().join("policies.toml")).unwrap();
        assert_eq!(p, Policy::default());
    }

    #[test]
    fn auto_limit_from_machine() {
        assert_eq!(MaxJobs::Auto.resolve(16, 64 * GB), 4);
        assert_eq!(MaxJobs::Auto.resolve(16, 16 * GB), 2);
        assert_eq!(MaxJobs::Auto.resolve(2, 4 * GB), 1);
        assert_eq!(MaxJobs::Fixed(3).resolve(2, GB), 3);
    }

    #[test]
    fn parses_full_file() {
        let text = r#"
            autonomous = false
            dry_run = true
            [admission]
            max_heavy_jobs = 2
            memory_headroom_percent = 20
            [threshold]
            action = "deprioritize"
            [sessions."s1"]
            max_heavy_jobs = 1
            priority = 5
            [[classifier.heavy]]
            program = "terraform"
            args = ["apply"]
        "#;
        let p = Policy::from_toml_str(text).unwrap();
        assert!(!p.autonomous);
        assert!(p.dry_run);
        assert_eq!(p.admission.max_heavy_jobs, MaxJobs::Fixed(2));
        assert_eq!(p.admission.memory_limit_percent(), 80.0);
        assert_eq!(p.threshold.action, ThresholdAction::Deprioritize);
        assert_eq!(p.session("s1").unwrap().priority, 5);
        assert_eq!(p.classifier.heavy[0].program, "terraform");
        // Round trip.
        let again = Policy::from_toml_str(&p.to_toml_string()).unwrap();
        assert_eq!(again, p);
    }

    #[test]
    fn auto_string_accepted() {
        let p = Policy::from_toml_str("[admission]\nmax_heavy_jobs = \"AUTO\"").unwrap();
        assert_eq!(p.admission.max_heavy_jobs, MaxJobs::Auto);
    }

    #[test]
    fn unknown_keys_are_reported_with_location() {
        let err = Policy::from_toml_str("[admission]\nmax_heavy_job = 2").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("max_heavy_job"), "{msg}");
        assert!(msg.contains("line 2"), "{msg}");
    }

    #[test]
    fn bad_values_are_all_reported() {
        let text = r#"
            [admission]
            max_heavy_jobs = 2
            memory_headroom_percent = 95
            cpu_threshold_percent = 0
            [sessions.s1]
            max_heavy_jobs = 5
        "#;
        let err = Policy::from_toml_str(text).unwrap_err();
        let PolicyError::Invalid { problems, .. } = &err else {
            panic!("unexpected {err}");
        };
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(err.to_string().contains("memory_headroom_percent"));
        assert!(err.to_string().contains("sessions.\"s1\".max_heavy_jobs"));
    }

    #[test]
    fn bad_max_jobs_string() {
        let err = Policy::from_toml_str("[admission]\nmax_heavy_jobs = \"many\"").unwrap_err();
        assert!(err.to_string().contains("auto"), "{err}");
        assert!(Policy::from_toml_str("[admission]\nmax_heavy_jobs = 0").is_err());
    }

    #[test]
    fn bad_action_is_reported() {
        let err = Policy::from_toml_str("[threshold]\naction = \"explode\"").unwrap_err();
        assert!(err.to_string().contains("pause_children"), "{err}");
    }
}
