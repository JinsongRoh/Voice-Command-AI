use serde::{Deserialize, Serialize};

use crate::UnixMillis;
use crate::session::SessionId;

/// Resource usage of one session's process tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub session_id: SessionId,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub process_count: u32,
}

/// System-wide resource state at one point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    pub at: UnixMillis,
    /// Average over all cores, 0–100.
    pub cpu_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub disk_read_bytes_per_sec: u64,
    pub disk_write_bytes_per_sec: u64,
    pub net_rx_bytes_per_sec: u64,
    pub net_tx_bytes_per_sec: u64,
    pub sessions: Vec<SessionUsage>,
    pub queue: Vec<QueueEntry>,
}

/// How heavy a command is expected to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobClass {
    Light,
    Heavy,
}

/// A job waiting for an admission slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub job_id: String,
    pub session_id: Option<SessionId>,
    pub command: String,
    pub class: JobClass,
    pub queued_at: UnixMillis,
    /// 0 is the next job to be admitted.
    pub position: u32,
}

/// The broker's answer to an admission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdmissionVerdict {
    Grant { slot_id: String },
    Queue { job_id: String, position: u32 },
    Deny { reason: String },
}
