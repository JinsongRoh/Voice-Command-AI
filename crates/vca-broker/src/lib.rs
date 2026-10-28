//! Resource monitoring, admission control and containment for Voice Command AI.
//!
//! Many coding agents running in parallel can exhaust a machine. This crate
//! keeps it usable (requirements RES-01..08):
//!
//! - [`monitor`] samples system and per-session resource usage every second;
//! - [`classify`] decides whether a shell command is a heavy job;
//! - [`admission`] grants a limited number of heavy-job slots and queues the rest;
//! - [`ipc`] lets short-lived agent hooks ask for slots over a local socket;
//! - [`contain`] lowers priority of, suspends, resumes and terminates process trees;
//! - [`policy`] loads `policies.toml`;
//! - [`watcher`] decides threshold actions, and [`Broker`] ties everything together.

pub mod admission;
pub mod broker;
pub mod classify;
pub mod clock;
pub mod contain;
pub mod ipc;
pub mod monitor;
pub mod policy;
mod proctree;
pub mod watcher;

pub use admission::{AdmissionController, AdmissionEvent, AdmissionRequest, MachineInfo};
pub use broker::{Broker, BrokerConfig, BrokerError, BrokerEvent};
pub use classify::{Classification, Classifier};
pub use policy::Policy;
