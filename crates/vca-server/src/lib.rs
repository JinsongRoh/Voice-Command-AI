//! Voice Command AI server (`vcad`).
//!
//! The daemon owns managed agent sessions (PTY, attach server, broker
//! registration), observes sessions it did not start, keeps a per-session
//! event log for resumable streams and exposes everything through a
//! transport-agnostic [`api::Api`]. Transports: the owner-only local
//! control channel used by the `vca` CLI and agent hooks ([`control`]),
//! and an optional development WebSocket on 127.0.0.1 ([`devws`]). Cloud
//! coding agents appear as session cards too ([`cloud`]).

pub mod api;
mod asks;
pub mod attachments;
pub mod cloud;
pub mod conductor;
pub mod config;
pub mod connectivity;
pub mod control;
pub mod daemon;
pub mod devws;
mod discovery;
pub mod engines;
pub mod events;
pub mod folders;
mod hooks;
mod hooks_watch;
mod local;
pub mod names;
mod observed_watch;
pub mod order;
pub mod peer;
mod personal_terms;
mod progress;
pub mod pronunciations;
pub mod push;
mod questions;
pub mod remote;
pub mod restore;
pub mod security;
pub mod sessions;
pub mod shell;
pub mod speech;
mod speech_profiles;
mod subagents;
pub mod tasks;
mod term_size;
pub mod tts;
mod util;
pub mod voice_brief;
#[cfg(windows)]
mod win_shell;
pub mod workspace;
