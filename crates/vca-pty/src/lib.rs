//! Cross-platform pseudo-terminal host for managed agent sessions.
//!
//! VCA runs coding agents inside pseudo-terminals it owns, so that input can
//! be delivered without window focus (architecture §4, tier 1 "Managed
//! PTY"). The user's terminal becomes an attached view of that PTY.
//!
//! * [`PtySession`] spawns a command in a PTY (ConPTY on Windows, openpty on
//!   Unix), delivers input, resizes, kills and waits for exit. Output is
//!   fanned out to any number of [`OutputSubscription`]s and kept in a
//!   bounded ring buffer with sequence numbers for replay.
//! * [`ScreenSnapshot`] is an optional vt100 view of the screen for
//!   previews; [`TerminalModes`] tracks bracketed paste and cursor-key modes.
//! * [`input`] converts [`vca_core::Input`] into terminal bytes.
//! * [`attach`] is the local IPC protocol that lets terminals attach to a
//!   session ([`AttachServer`], [`attach_stdio`]).
//! * [`shell`] generates the shell integration snippets that route agent
//!   commands through `vca run`.

pub mod attach;
mod error;
pub mod input;
mod modes;
mod ring;
mod screen;
mod session;
pub mod shell;

pub use attach::{
    AttachClient, AttachOptions, AttachOutcome, AttachServer, AttachStart, attach_stdio,
};
pub use error::PtyError;
pub use modes::{ModeTracker, TerminalModes};
pub use ring::{BufferLimits, Gap, OutputChunk, OutputRing, Replay};
pub use screen::{ScreenOptions, ScreenSnapshot};
pub use session::{ExitStatus, OutputEvent, OutputSubscription, PtySession, PtySize, SpawnOptions};
