//! Voice Command AI server (`vcad`).
//!
//! The daemon owns managed agent sessions (PTY, attach server, broker
//! registration), observes sessions it did not start, keeps a per-session
//! event log for resumable streams and exposes everything through a
//! transport-agnostic [`api::Api`]. Transports: the owner-only local
//! control channel used by the `vca` CLI and agent hooks ([`control`]),
//! and an optional development WebSocket on 127.0.0.1 ([`devws`]).

pub mod api;
pub mod config;
pub mod control;
pub mod daemon;
pub mod devws;
mod discovery;
pub mod events;
mod hooks;
mod local;
pub mod remote;
pub mod sessions;
mod util;
pub mod workspace;
