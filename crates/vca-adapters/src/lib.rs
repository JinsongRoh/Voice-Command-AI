//! Local coding agent adapters and terminal input injection.
//!
//! * [`AgentAdapter`]: discovery, launch specs, history, hooks and profile
//!   isolation for one agent (architecture §5). Built-in adapters:
//!   [`claude::ClaudeAdapter`], [`codex::CodexAdapter`] and
//!   [`generic::GenericAdapter`] (Gemini CLI, Antigravity, Grok, Cursor CLI,
//!   Copilot CLI, Aider, opencode, Qwen Code and any other command).
//! * [`hook`]: agent hook events translated into VCA states, and replies.
//! * [`heuristics`]: state detection from terminal output.
//! * [`process`]: running agent processes and process trees.
//! * [`terminal`]: delivery tier 3, typing into tmux, WezTerm, kitty,
//!   iTerm2 and Windows consoles without focus.
//! * [`tail`]: incremental JSONL transcript reading.
//!
//! PTY handling lives in `vca-pty`, not here.

pub mod adapter;
pub mod claude;
pub mod codex;
pub mod error;
mod fsutil;
pub mod generic;
pub mod heuristics;
pub mod hook;
pub mod process;
pub mod tail;
pub mod terminal;
pub mod types;

pub use adapter::{AgentAdapter, SessionLocator, adapter_for, builtin_adapters, discover_all};
pub use error::{AdapterError, Result};
pub use heuristics::{HeuristicMatch, HeuristicRule, OutputHeuristics};
pub use hook::{AgentHookEvent, HookDecision, HookNotification, HookResponse, HookToolUse};
pub use process::{ProcInfo, ProcessMatcher, ProcessTable, ProcessTree};
pub use tail::HistoryStream;
pub use terminal::TerminalTarget;
pub use types::{
    Capabilities, DiscoveredSession, HistoryItem, HookEntryStatus, HookOptions, HookScope,
    HookState, HookStatus, LaunchSpec, Profile,
};
