//! The human task inbox ("할 일"): things agents asked the person to do
//! (run a command in an administrator shell, sign in somewhere, approve
//! something in a console, provide a value, review), kept by the server of
//! each PC so every device sees the same list.
//!
//! These are the wire types of the task messages in [`crate::ClientMsg`]
//! and [`crate::ServerMsg`]. Rules enforced by the server (see the user
//! guide, "Your to-do list from agents"):
//!
//! * capturing a task never runs anything; a command in a task is only
//!   shown, copied, or opened in an administrator shell by an explicit
//!   action of the person on the PC itself (through the normal UAC prompt);
//! * viewing needs the view scope; changing a task and sending a result
//!   need the command scope; opening an administrator shell and changing
//!   the settings are refused from every paired device;
//! * agents (and the conductor) can create tasks and read them, never mark
//!   them done or send a result;
//! * every change is audited.

use serde::{Deserialize, Serialize};

use crate::UnixMillis;
use crate::protocol::DangerLevel;
use crate::session::SessionId;

/// `Welcome::features` entry of servers that answer the task messages.
pub const FEATURE_HUMAN_TASKS: &str = "human_tasks";
/// Longest task title, in characters.
pub const TASK_MAX_TITLE_CHARS: usize = 200;
/// Longest steps text (Markdown), in characters.
pub const TASK_MAX_STEPS_CHARS: usize = 8000;
/// Longest command, in characters.
pub const TASK_MAX_COMMAND_CHARS: usize = 4000;
/// Longest result text, in characters.
pub const TASK_MAX_RESULT_CHARS: usize = 8000;
/// Longest captured command output, in characters.
pub const TASK_MAX_OUTPUT_CHARS: usize = 16000;
/// Most attachments of one result.
pub const TASK_MAX_ATTACHMENTS: usize = 10;
/// Longest stored excerpt of a task's source message, in characters.
pub const TASK_MAX_EXCERPT_CHARS: usize = 600;

/// Where a task came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    /// An agent asked with the `request_human_action` tool.
    Explicit,
    /// Found in an agent's final message at the end of a turn.
    Extracted,
    /// Added by the person.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Open,
    InProgress,
    /// Waiting for someone else (the person marked it).
    Waiting,
    Done,
    Dismissed,
}

impl TaskStatus {
    /// Still on the to-do list (open, in progress or waiting).
    pub fn is_active(self) -> bool {
        matches!(self, Self::Open | Self::InProgress | Self::Waiting)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    High,
    #[default]
    Normal,
    Low,
}

/// The shell a task's command is meant for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskShell {
    Powershell,
    Cmd,
    Bash,
    Zsh,
    Sh,
    #[default]
    Other,
}

/// A command the person is asked to run, with the server's danger
/// classification (the same as [`crate::ClientMsg::ClassifyCommand`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCommand {
    pub text: String,
    #[serde(default)]
    pub shell: TaskShell,
    /// It must run with administrator rights (UAC on Windows, sudo
    /// elsewhere).
    #[serde(default)]
    pub needs_admin: bool,
    pub danger: DangerLevel,
    #[serde(default)]
    pub reasons: Vec<String>,
    /// `[start, end)` UTF-8 byte offsets of the risky parts of `text`.
    #[serde(default)]
    pub spans: Vec<[u32; 2]>,
}

/// What the person reported back.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskResult {
    pub text: String,
    /// Project-relative paths of files uploaded with the existing
    /// attachment messages.
    pub attachments: Vec<String>,
    /// Command output the person pasted.
    pub output: Option<String>,
    pub at: UnixMillis,
}

/// One task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanTask {
    pub id: String,
    /// The session that asked (none for a plain manual task).
    #[serde(default)]
    pub session_id: Option<SessionId>,
    /// The session's name when the task was created.
    #[serde(default)]
    pub session_name: Option<String>,
    /// The server name of the PC that keeps the task.
    #[serde(default)]
    pub machine: String,
    pub title: String,
    /// Markdown.
    #[serde(default)]
    pub steps: String,
    #[serde(default)]
    pub command: Option<TaskCommand>,
    pub source: TaskSource,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: TaskPriority,
    /// The agent waits on the person (its priority starts high).
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub due: Option<UnixMillis>,
    /// No reminders before this.
    #[serde(default)]
    pub snoozed_until: Option<UnixMillis>,
    pub created_at: UnixMillis,
    pub updated_at: UnixMillis,
    /// Manual order (smaller first) within a priority.
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub result: Option<TaskResult>,
    #[serde(default)]
    pub sent_to_session: bool,
    #[serde(default)]
    pub sent_at: Option<UnixMillis>,
    /// The chat message the task came from, when known.
    #[serde(default)]
    pub message_seq: Option<u64>,
    /// When that message was written (Unix milliseconds), when known.
    #[serde(default)]
    pub message_at: Option<UnixMillis>,
    /// A short excerpt of that message, secrets redacted (shown when the
    /// session or the message is gone). Tasks saved before this field have
    /// none.
    #[serde(default)]
    pub excerpt: Option<String>,
    /// The session finished a turn (or exited) after the task was created:
    /// the request may no longer be needed.
    #[serde(default)]
    pub agent_moved_on: bool,
    /// Open for longer than the stale limit.
    #[serde(default)]
    pub stale: bool,
}

/// A new task (manual, or explicit from an agent).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskDraft {
    pub session_id: Option<SessionId>,
    pub title: String,
    pub steps: String,
    pub command: Option<String>,
    pub shell: Option<TaskShell>,
    pub needs_admin: bool,
    pub blocking: bool,
    pub priority: Option<TaskPriority>,
    pub due: Option<UnixMillis>,
    /// The chat message it was made from ("할 일로 추가").
    pub message_seq: Option<u64>,
    /// Without `message_seq`: anchor the task to the session's newest agent
    /// message (a task made from an alert).
    pub anchor_latest: bool,
}

/// A change to a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TaskChange {
    /// Check off (`done`), dismiss, reopen, start or put on hold.
    Status { status: TaskStatus },
    /// No reminders until `until`; `None` ends the snooze.
    Snooze {
        #[serde(default)]
        until: Option<UnixMillis>,
    },
    /// Edits; absent fields stay. An empty `command` removes it.
    Edit {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        steps: Option<String>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        shell: Option<TaskShell>,
        #[serde(default)]
        needs_admin: Option<bool>,
        #[serde(default)]
        priority: Option<TaskPriority>,
        #[serde(default)]
        due: Option<UnixMillis>,
        #[serde(default)]
        clear_due: bool,
    },
    /// Moves the task to `position` among the active tasks (0 = first).
    Move { position: u32 },
    /// Stores pasted command output ("출력 붙여넣기") for the result.
    Output { output: String },
    /// "Still needed": clears the stale and agent-moved-on marks.
    StillNeeded,
}

/// The result to send to the requesting session ("결과 보내기").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskResultDraft {
    pub text: String,
    /// Project-relative paths of files already uploaded with the
    /// attachment messages.
    pub attachments: Vec<String>,
    /// Mentions the server did not type into the session itself
    /// (`Attached.typed == false`), added to the message.
    pub mentions: Vec<String>,
    /// Command output; the stored pasted output when `None`.
    pub output: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskExportFormat {
    Markdown,
    Csv,
}

/// Why a reminder was pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReminderKind {
    /// The due time passed.
    Due,
    /// An agent has been waiting on the person for a while.
    Blocking,
    /// Open for longer than the stale limit.
    Stale,
    /// The session moved on (or exited): is the task still needed?
    AgentMovedOn,
    /// The snooze ended.
    SnoozeEnded,
}

/// Pushed to every client (`id = 0`). Carries no task text beyond the
/// title; clients speak generic lines only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReminder {
    pub kind: TaskReminderKind,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    /// Active tasks right now (for the digest).
    #[serde(default)]
    pub open: u32,
    #[serde(default)]
    pub blocking: u32,
    pub at: UnixMillis,
}

/// The task settings of this PC. Changed only from the desktop app on the
/// PC itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskSettings {
    /// Scan agents' final messages for requests with fixed patterns.
    pub extract: bool,
    /// Also ask the configured LLM profile (opt-in; secrets are redacted
    /// before the text leaves the PC).
    pub extract_llm: bool,
    /// Remind about tasks open for longer than this (0: never).
    pub stale_hours: u32,
    /// Remind about blocking tasks this often (0: never).
    pub remind_blocking_minutes: u32,
    /// Local hour (0-23) after which clients read the daily "today's tasks"
    /// digest once ([`crate::ClientMsg::TaskDigest`]); `None`: no digest.
    pub digest_hour: Option<u8>,
}

impl Default for TaskSettings {
    fn default() -> Self {
        Self {
            extract: true,
            extract_llm: false,
            stale_hours: 8,
            remind_blocking_minutes: 30,
            digest_hour: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes() {
        let change: TaskChange =
            serde_json::from_str(r#"{"op":"status","status":"done"}"#).unwrap();
        assert_eq!(
            change,
            TaskChange::Status {
                status: TaskStatus::Done
            }
        );
        let edit: TaskChange = serde_json::from_str(r#"{"op":"edit","title":"x"}"#).unwrap();
        assert!(matches!(
            edit,
            TaskChange::Edit {
                title: Some(_),
                clear_due: false,
                ..
            }
        ));
        let draft: TaskDraft = serde_json::from_str(r#"{"title":"Sign in"}"#).unwrap();
        assert_eq!(draft.title, "Sign in");
        assert!(!draft.blocking);
        let s: TaskSettings = serde_json::from_str("{}").unwrap();
        assert!(s.extract && !s.extract_llm);
        assert_eq!(
            serde_json::to_value(TaskStatus::InProgress).unwrap(),
            "in_progress"
        );
        assert_eq!(
            serde_json::to_value(TaskReminderKind::AgentMovedOn).unwrap(),
            "agent_moved_on"
        );
        let bytes = rmp_serde::to_vec_named(&change).unwrap();
        assert_eq!(rmp_serde::from_slice::<TaskChange>(&bytes).unwrap(), change);
    }

    #[test]
    fn tasks_saved_before_source_context_still_load() {
        // A task as older servers stored it: no message time, no excerpt.
        let old = serde_json::json!({
            "id": "t1", "title": "Sign in", "source": "manual",
            "created_at": 1, "updated_at": 1, "message_seq": 4
        });
        let task: HumanTask = serde_json::from_value(old).unwrap();
        assert_eq!(task.message_seq, Some(4));
        assert_eq!(task.message_at, None);
        assert_eq!(task.excerpt, None);
        let bare: HumanTask = serde_json::from_str(
            r#"{"id":"t2","title":"x","source":"explicit","created_at":1,"updated_at":1}"#,
        )
        .unwrap();
        assert!(bare.session_id.is_none() && bare.message_seq.is_none());
        let draft: TaskDraft = serde_json::from_str(r#"{"title":"x"}"#).unwrap();
        assert!(!draft.anchor_latest);
    }

    #[test]
    fn protocol_messages_roundtrip() {
        use crate::{ClientMsg, ServerMsg};
        let list: ClientMsg = serde_json::from_str(r#"{"type":"task_list"}"#).unwrap();
        assert_eq!(list, ClientMsg::TaskList);
        let send: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "task_send_result", "task_id": "t1",
            "result": {"text": "done", "attachments": [".vca/attachments/a.png"]}
        }))
        .unwrap();
        let ClientMsg::TaskSendResult {
            preview, result, ..
        } = &send
        else {
            panic!("{send:?}");
        };
        assert!(!preview);
        assert_eq!(result.attachments.len(), 1);
        let update = ClientMsg::TaskUpdate {
            task_id: "t1".into(),
            change: TaskChange::Snooze { until: Some(5) },
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["type"], "task_update");
        assert_eq!(json["change"]["op"], "snooze");
        let export: ClientMsg =
            serde_json::from_str(r#"{"type":"task_export","format":"csv"}"#).unwrap();
        assert!(matches!(
            export,
            ClientMsg::TaskExport {
                format: TaskExportFormat::Csv,
                session_id: None
            }
        ));
        let task = HumanTask {
            id: "t1".into(),
            session_id: Some("s1".into()),
            session_name: Some("api".into()),
            machine: "pc".into(),
            title: "Run as administrator".into(),
            steps: String::new(),
            command: Some(TaskCommand {
                text: "net stop spooler".into(),
                shell: TaskShell::Cmd,
                needs_admin: true,
                danger: DangerLevel::Caution,
                reasons: vec!["service_control".into()],
                spans: vec![[0, 8]],
            }),
            source: TaskSource::Extracted,
            status: TaskStatus::Open,
            priority: TaskPriority::High,
            blocking: true,
            due: None,
            snoozed_until: None,
            created_at: 1,
            updated_at: 2,
            order: 0,
            result: None,
            sent_to_session: false,
            sent_at: None,
            message_seq: Some(9),
            message_at: Some(1),
            excerpt: Some("Run net stop spooler".into()),
            agent_moved_on: false,
            stale: false,
        };
        let msgs = [
            ServerMsg::Tasks {
                tasks: vec![task.clone()],
                settings: TaskSettings::default(),
            },
            ServerMsg::TaskResultSent {
                task: task.clone(),
                text: "x".into(),
                sent: false,
            },
            ServerMsg::TaskReminder {
                reminder: TaskReminder {
                    kind: TaskReminderKind::Blocking,
                    task_id: Some("t1".into()),
                    title: task.title.clone(),
                    session_id: None,
                    open: 1,
                    blocking: 1,
                    at: 3,
                },
            },
        ];
        assert_eq!(serde_json::to_value(&msgs[0]).unwrap()["type"], "tasks");
        assert_eq!(
            serde_json::to_value(&msgs[0]).unwrap()["tasks"][0]["command"]["shell"],
            "cmd"
        );
        for msg in msgs {
            let bytes = rmp_serde::to_vec_named(&msg).unwrap();
            assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), msg);
        }
        let bytes = rmp_serde::to_vec_named(&send).unwrap();
        assert_eq!(rmp_serde::from_slice::<ClientMsg>(&bytes).unwrap(), send);
    }
}
