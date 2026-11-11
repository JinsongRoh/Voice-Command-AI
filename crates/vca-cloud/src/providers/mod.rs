//! One adapter per provider in the architecture §5.1 matrix.

pub mod claude_managed;
pub mod claude_routines;
pub mod codex_cli;
pub mod copilot;
pub mod cursor;
pub mod devin;
pub mod jules;

pub use claude_managed::ClaudeManagedAdapter;
pub use claude_routines::ClaudeRoutineAdapter;
pub use codex_cli::CodexCloudAdapter;
pub use copilot::CopilotAdapter;
pub use cursor::{CursorAdapter, CursorAuth};
pub use devin::DevinAdapter;
pub use jules::JulesAdapter;
