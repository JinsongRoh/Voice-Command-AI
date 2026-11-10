//! The task tools of `vca mcp`: `request_human_action` and
//! `list_my_requests`. Agents can ask the person for something and read
//! the state of what they asked; there is deliberately no tool that checks
//! a task off or sends a result (only the person does that, in VCA).
//!
//! The `vca mcp` server (owned by the conductor) lists [`tool_specs`] next
//! to its own tools and forwards calls whose name passes [`is_task_tool`]
//! to vcad, which answers them with [`call`].

use serde_json::{Value, json};
use vca_core::error::ErrorCode;
use vca_core::task::{HumanTask, TaskStatus};
use vca_core::{Error, SessionId};

use super::{HumanActionRequest, Tasks};

pub const REQUEST_HUMAN_ACTION: &str = "request_human_action";
pub const LIST_MY_REQUESTS: &str = "list_my_requests";
/// The `origin` of the conductor, which may read every task.
pub const CONDUCTOR_ORIGIN: &str = "conductor";

/// A tool as MCP `tools/list` describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    pub read_only: bool,
}

/// The task tools, in a stable order.
pub fn tool_specs() -> Vec<TaskToolSpec> {
    vec![
        TaskToolSpec {
            name: REQUEST_HUMAN_ACTION,
            description: "Ask the person to do something you cannot do yourself (run a command \
                          as administrator, sign in, approve something in a console, check a \
                          device, provide a value, review). It goes on the person's to-do list \
                          in VCA; nothing is run for them. The person sends the result back to \
                          you as a message. Asking again for the same thing returns the open \
                          task.",
            schema: json!({"type": "object", "properties": {
                "title": {"type": "string", "maxLength": 200,
                          "description": "Short imperative, e.g. \"Run the installer as administrator\"."},
                "steps": {"description": "Markdown, or a list of steps.",
                          "anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
                "command": {"type": "string", "maxLength": 4000,
                            "description": "The exact command to run, if any. Never run by VCA."},
                "needs_admin": {"type": "boolean", "description": "It needs administrator rights (UAC or sudo)."},
                "blocking": {"type": "boolean", "description": "You wait for this before you can continue."},
                "due": {"description": "Unix milliseconds or YYYY-MM-DDTHH:MM (UTC).",
                        "anyOf": [{"type": "integer"}, {"type": "string"}]}},
                "required": ["title"], "additionalProperties": false}),
            read_only: false,
        },
        TaskToolSpec {
            name: LIST_MY_REQUESTS,
            description: "The tasks you asked the person for and their state (open, in \
                          progress, waiting, done, dismissed), newest first, and whether a \
                          result was sent to you.",
            schema: json!({"type": "object", "properties": {
                "include_closed": {"type": "boolean", "description": "Also done and dismissed ones (default true)."},
                "all": {"type": "boolean", "description": "Every session's tasks (the conductor only)."}},
                "additionalProperties": false}),
            read_only: true,
        },
    ]
}

pub fn is_task_tool(name: &str) -> bool {
    name == REQUEST_HUMAN_ACTION || name == LIST_MY_REQUESTS
}

fn view(t: &HumanTask) -> Value {
    json!({
        "id": t.id,
        "title": t.title,
        "status": t.status,
        "priority": t.priority,
        "blocking": t.blocking,
        "needs_admin": t.command.as_ref().is_some_and(|c| c.needs_admin),
        "session_id": t.session_id,
        "created_at": t.created_at,
        "updated_at": t.updated_at,
        "due": t.due,
        "result_sent": t.sent_to_session,
    })
}

/// Runs a task tool for `caller` (the session whose `vca mcp` asks, from
/// its `VCA_SESSION_ID`; `None` when unknown). `origin` names the asker in
/// the audit log (`conductor`, or `mcp`).
pub fn call(
    tasks: &Tasks,
    caller: Option<SessionId>,
    origin: &str,
    name: &str,
    args: &Value,
) -> Result<Value, Error> {
    let args = match args {
        Value::Null => json!({}),
        Value::Object(_) => args.clone(),
        _ => {
            return Err(Error::api(
                ErrorCode::InvalidRequest,
                "the arguments must be an object",
            ));
        }
    };
    match name {
        REQUEST_HUMAN_ACTION => {
            let req: HumanActionRequest = serde_json::from_value(args).map_err(|e| {
                Error::api(ErrorCode::InvalidRequest, format!("invalid arguments: {e}"))
            })?;
            let (task, created) = tasks.request_human_action(caller, origin, req)?;
            Ok(json!({"task": view(&task), "created": created,
                      "note": "The person was asked. Do not wait in a loop; they send the result as a message."}))
        }
        LIST_MY_REQUESTS => {
            let include_closed = args
                .get("include_closed")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let list: Vec<HumanTask> = if all && origin == CONDUCTOR_ORIGIN {
                match tasks.list() {
                    vca_core::ServerMsg::Tasks { tasks, .. } => tasks,
                    _ => Vec::new(),
                }
            } else {
                let Some(caller) = &caller else {
                    return Err(Error::api(
                        ErrorCode::InvalidRequest,
                        "list_my_requests needs the session that asks (VCA_SESSION_ID)",
                    ));
                };
                tasks.for_session(caller)
            };
            let items: Vec<Value> = list
                .iter()
                .filter(|t| include_closed || t.status.is_active())
                .map(view)
                .collect();
            let open = list
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::Open | TaskStatus::InProgress))
                .count();
            Ok(json!({"tasks": items, "open": open}))
        }
        _ => Err(Error::api(
            ErrorCode::InvalidRequest,
            format!("unknown tool {name:?}"),
        )),
    }
}
