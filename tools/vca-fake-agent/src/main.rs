//! Scriptable fake coding agent used in tests.
//!
//! `vca-fake-agent` behaves like an interactive CLI coding agent: it prints a
//! banner and a prompt marker, reads one message per line from stdin and
//! answers according to a deterministic script. It is meant to run inside a
//! pseudo-terminal so that the PTY host, adapters and server can be tested
//! end to end without a real agent or network access.
//!
//! # Built-in commands (always active, checked before script rules)
//!
//! | Input            | Behavior                                          |
//! |------------------|---------------------------------------------------|
//! | `/exit [code]`   | Exit with `code` (default 0).                     |
//! | `/size`          | Print `size <cols>x<rows>` of the terminal.       |
//! | `/env NAME`      | Print `env NAME=<value>` (empty when unset).      |
//! | `/cwd`           | Print `cwd <current directory>`.                  |
//! | `/cpr`           | Query the cursor position (`ESC[6n`) and print    |
//! |                  | `cpr <row>;<col>` (one based) or `cpr error ...`. |
//!
//! # Default rules (used after script rules)
//!
//! * input containing `think` -> "thinking" dots over time, then a reply;
//! * input containing `run tests` or `permission` -> a permission prompt
//!   ``Allow command `cargo test`? [y/n] ``;
//! * anything else -> `Echo: <input>`.
//!
//! Every non-built-in turn ends with the line `Done.` followed by the prompt.
//!
//! # Script file (`--script file.json`)
//!
//! ```json
//! {
//!   "banner": "My agent v1",
//!   "prompt": "> ",
//!   "bracketed_paste": false,
//!   "rules": [
//!     { "pattern": "hello", "mode": "contains",
//!       "actions": [ { "type": "say", "text": "Hi! You said {input}" } ] },
//!     { "pattern": "deploy", "mode": "prefix",
//!       "actions": [ { "type": "permission", "command": "make deploy",
//!                      "yes": [ { "type": "say", "text": "deployed" } ],
//!                      "no":  [ { "type": "say", "text": "cancelled" } ] } ] }
//!   ]
//! }
//! ```
//!
//! Action types: `say {text}`, `think {steps, interval_ms, label?}`,
//! `sleep {ms}`, `permission {command, yes, no}`, `hook {event, extra?}`,
//! `exit {code}`. `{input}` in texts is replaced by the user's message.
//!
//! # Hooks (`--hook-cmd CMD`)
//!
//! When set, the agent runs `CMD` through the platform shell (`sh -c` or
//! `cmd /C`) for Claude-Code-style hook events, writes a JSON payload to its
//! stdin and waits for it to finish. Events: `SessionStart`,
//! `UserPromptSubmit`, `Notification` (permission prompt), `PreToolUse`
//! (permission granted), `Stop` (end of each turn) and `SessionEnd`.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::Duration;

use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

#[derive(Debug, Parser)]
#[command(
    name = "vca-fake-agent",
    version,
    about = "Scriptable fake coding agent for tests"
)]
struct Cli {
    /// JSON script with banner, prompt and rules.
    #[arg(long)]
    script: Option<PathBuf>,
    /// Shell command run for hook events, with a JSON payload on stdin.
    #[arg(long)]
    hook_cmd: Option<String>,
    /// Session id reported in hook payloads (defaults to $VCA_SESSION_ID or "fake-session").
    #[arg(long)]
    session_id: Option<String>,
    /// Override the prompt marker.
    #[arg(long)]
    prompt: Option<String>,
    /// Enable bracketed paste mode (ESC[?2004h) while running.
    #[arg(long)]
    bracketed_paste: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Script {
    banner: Option<String>,
    prompt: Option<String>,
    #[serde(default)]
    bracketed_paste: bool,
    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    pattern: String,
    #[serde(default)]
    mode: MatchMode,
    actions: Vec<Action>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MatchMode {
    #[default]
    Contains,
    Exact,
    Prefix,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Say {
        text: String,
    },
    Think {
        #[serde(default = "default_steps")]
        steps: u32,
        #[serde(default = "default_interval")]
        interval_ms: u64,
        #[serde(default)]
        label: Option<String>,
    },
    Sleep {
        ms: u64,
    },
    Permission {
        command: String,
        #[serde(default)]
        yes: Vec<Action>,
        #[serde(default)]
        no: Vec<Action>,
    },
    Hook {
        event: String,
        #[serde(default)]
        extra: Option<Value>,
    },
    Exit {
        #[serde(default)]
        code: i32,
    },
}

fn default_steps() -> u32 {
    3
}

fn default_interval() -> u64 {
    100
}

impl Rule {
    fn matches(&self, input: &str) -> bool {
        match self.mode {
            MatchMode::Contains => input.contains(&self.pattern),
            MatchMode::Exact => input == self.pattern,
            MatchMode::Prefix => input.starts_with(&self.pattern),
        }
    }
}

fn builtin_rules() -> Vec<Rule> {
    vec![
        Rule {
            pattern: "think".into(),
            mode: MatchMode::Contains,
            actions: vec![
                Action::Think {
                    steps: 3,
                    interval_ms: 100,
                    label: None,
                },
                Action::Say {
                    text: "Thought about: {input}".into(),
                },
            ],
        },
        permission_rule("run tests"),
        permission_rule("permission"),
    ]
}

fn permission_rule(pattern: &str) -> Rule {
    Rule {
        pattern: pattern.into(),
        mode: MatchMode::Contains,
        actions: vec![Action::Permission {
            command: "cargo test".into(),
            yes: vec![Action::Say {
                text: "Running `cargo test`... ok".into(),
            }],
            no: vec![Action::Say {
                text: "Skipped `cargo test`.".into(),
            }],
        }],
    }
}

/// Control flow after running actions.
enum Flow {
    Continue,
    Exit(i32),
}

struct Agent {
    rules: Vec<Rule>,
    prompt: String,
    bracketed_paste: bool,
    hook_cmd: Option<String>,
    session_id: String,
    lines: io::Lines<io::StdinLock<'static>>,
    out: io::Stdout,
}

impl Agent {
    fn print(&mut self, text: &str) {
        // Ignore write errors: the terminal may already be gone.
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }

    fn println(&mut self, text: &str) {
        self.print(text);
        self.print("\n");
    }

    /// Reads one logical message. A bracketed paste spanning several lines
    /// is returned as a single message joined with `\n`. `None` on EOF.
    fn read_message(&mut self) -> Option<String> {
        let first = self.lines.next()?.ok()?;
        let first = first.trim_end_matches('\r').to_owned();
        let Some(start) = first.find(PASTE_START) else {
            return Some(first);
        };
        let mut collected = String::new();
        collected.push_str(&first[..start]);
        let mut rest = first[start + PASTE_START.len()..].to_owned();
        loop {
            if let Some(end) = rest.find(PASTE_END) {
                collected.push_str(&rest[..end]);
                collected.push_str(&rest[end + PASTE_END.len()..]);
                return Some(collected);
            }
            collected.push_str(&rest);
            collected.push('\n');
            match self.lines.next() {
                Some(Ok(line)) => rest = line.trim_end_matches('\r').to_owned(),
                _ => return Some(collected),
            }
        }
    }

    fn hook(&mut self, event: &str, extra: Value) {
        let Some(cmd) = self.hook_cmd.clone() else {
            return;
        };
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let mut payload = json!({
            "session_id": self.session_id,
            "transcript_path": "",
            "cwd": cwd,
            "hook_event_name": event,
        });
        if let (Value::Object(base), Value::Object(more)) = (&mut payload, extra) {
            base.extend(more);
        }
        if let Err(err) = run_hook(&cmd, event, &payload) {
            eprintln!("vca-fake-agent: hook {event} failed: {err}");
        }
    }

    fn run_actions(&mut self, actions: &[Action], input: &str) -> Flow {
        for action in actions {
            match action {
                Action::Say { text } => {
                    let text = text.replace("{input}", input);
                    self.println(&text);
                }
                Action::Think {
                    steps,
                    interval_ms,
                    label,
                } => {
                    let label = label.clone().unwrap_or_else(|| "Thinking".into());
                    self.print(&label);
                    for _ in 0..*steps {
                        thread::sleep(Duration::from_millis(*interval_ms));
                        self.print(".");
                    }
                    self.print("\n");
                }
                Action::Sleep { ms } => thread::sleep(Duration::from_millis(*ms)),
                Action::Permission { command, yes, no } => {
                    let question = format!("Allow command `{command}`? [y/n] ");
                    self.hook(
                        "Notification",
                        json!({ "message": question.trim_end(), "notification_type": "permission_prompt" }),
                    );
                    self.print(&question);
                    let answer = self.read_message().unwrap_or_default();
                    let answer = answer.trim().to_ascii_lowercase();
                    let flow = if answer == "y" || answer == "yes" {
                        self.hook(
                            "PreToolUse",
                            json!({ "tool_name": "Bash", "tool_input": { "command": command } }),
                        );
                        self.run_actions(yes, input)
                    } else {
                        self.run_actions(no, input)
                    };
                    if let Flow::Exit(code) = flow {
                        return Flow::Exit(code);
                    }
                }
                Action::Hook { event, extra } => {
                    self.hook(event, extra.clone().unwrap_or(Value::Null));
                }
                Action::Exit { code } => return Flow::Exit(*code),
            }
        }
        Flow::Continue
    }

    /// Handles built-in commands. Returns `None` when the input is not one.
    fn builtin(&mut self, input: &str) -> Option<Flow> {
        let mut words = input.split_whitespace();
        match words.next()? {
            "/exit" => {
                let code = words.next().and_then(|c| c.parse().ok()).unwrap_or(0);
                Some(Flow::Exit(code))
            }
            "/size" => {
                let line = match crossterm::terminal::size() {
                    Ok((cols, rows)) => format!("size {cols}x{rows}"),
                    Err(err) => format!("size unknown ({err})"),
                };
                self.println(&line);
                Some(Flow::Continue)
            }
            "/env" => {
                let name = words.next().unwrap_or_default();
                let value = std::env::var(name).unwrap_or_default();
                self.println(&format!("env {name}={value}"));
                Some(Flow::Continue)
            }
            "/cpr" => {
                // crossterm writes ESC[6n and reads the reply on Unix; on
                // Windows it asks the console API instead.
                let line = match crossterm::cursor::position() {
                    Ok((col, row)) => format!("cpr {};{}", row + 1, col + 1),
                    Err(err) => format!("cpr error {err}"),
                };
                self.println(&line);
                Some(Flow::Continue)
            }
            "/cwd" => {
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.println(&format!("cwd {cwd}"));
                Some(Flow::Continue)
            }
            _ => None,
        }
    }

    fn turn(&mut self, input: &str) -> Flow {
        self.hook("UserPromptSubmit", json!({ "prompt": input }));
        if let Some(flow) = self.builtin(input) {
            return flow;
        }
        let actions = self
            .rules
            .iter()
            .find(|r| r.matches(input))
            .map(|r| r.actions.clone())
            .unwrap_or_else(|| {
                vec![Action::Say {
                    text: "Echo: {input}".into(),
                }]
            });
        let flow = self.run_actions(&actions, input);
        if matches!(flow, Flow::Continue) {
            self.println("Done.");
            self.hook("Stop", json!({ "stop_hook_active": false }));
        }
        flow
    }

    fn run(&mut self, banner: &str) -> i32 {
        if self.bracketed_paste {
            self.print("\x1b[?2004h");
        }
        self.println(banner);
        self.hook("SessionStart", json!({ "source": "startup" }));
        let code = loop {
            let prompt = self.prompt.clone();
            self.print(&prompt);
            let Some(message) = self.read_message() else {
                break 0;
            };
            let message = message.trim().to_owned();
            if message.is_empty() {
                continue;
            }
            if let Flow::Exit(code) = self.turn(&message) {
                break code;
            }
        };
        self.hook("SessionEnd", json!({ "reason": "exit" }));
        if self.bracketed_paste {
            self.print("\x1b[?2004l");
        }
        self.println("Bye.");
        code
    }
}

fn run_hook(cmd: &str, event: &str, payload: &Value) -> io::Result<()> {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    let mut child = command
        .env("VCA_HOOK_EVENT", event)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(payload.to_string().as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("exited with {status}")))
    }
}

fn load_script(path: Option<&PathBuf>) -> Result<Script, String> {
    let Some(path) = path else {
        return Ok(Script::default());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read script {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid script {}: {e}", path.display()))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let script = match load_script(cli.script.as_ref()) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("vca-fake-agent: {err}");
            return ExitCode::from(2);
        }
    };
    let mut rules = script.rules;
    rules.extend(builtin_rules());
    let session_id = cli
        .session_id
        .or_else(|| std::env::var("VCA_SESSION_ID").ok())
        .unwrap_or_else(|| "fake-session".into());
    let banner = script
        .banner
        .unwrap_or_else(|| "Fake Agent (vca-fake-agent) ready.".into());
    let mut agent = Agent {
        rules,
        prompt: cli.prompt.or(script.prompt).unwrap_or_else(|| "> ".into()),
        bracketed_paste: cli.bracketed_paste || script.bracketed_paste,
        hook_cmd: cli.hook_cmd,
        session_id,
        lines: io::stdin().lock().lines(),
        out: io::stdout(),
    };
    let code = agent.run(&banner);
    // Exit codes are truncated to a byte, as on every platform's shell.
    ExitCode::from((code & 0xff) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_matching_modes() {
        let rule = |mode| Rule {
            pattern: "ab".into(),
            mode,
            actions: vec![],
        };
        assert!(rule(MatchMode::Contains).matches("xaby"));
        assert!(!rule(MatchMode::Exact).matches("abc"));
        assert!(rule(MatchMode::Exact).matches("ab"));
        assert!(rule(MatchMode::Prefix).matches("abc"));
        assert!(!rule(MatchMode::Prefix).matches("cab"));
    }

    #[test]
    fn script_parses_all_action_types() {
        let script: Script = serde_json::from_str(
            r#"{
              "banner": "b", "prompt": "$ ", "bracketed_paste": true,
              "rules": [
                { "pattern": "x", "mode": "exact", "actions": [
                  { "type": "say", "text": "t" },
                  { "type": "think", "steps": 2, "interval_ms": 1 },
                  { "type": "sleep", "ms": 1 },
                  { "type": "permission", "command": "ls",
                    "yes": [ { "type": "exit", "code": 3 } ] },
                  { "type": "hook", "event": "Stop", "extra": { "a": 1 } },
                  { "type": "exit" }
                ] }
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(script.rules.len(), 1);
        assert_eq!(script.rules[0].actions.len(), 6);
        assert!(script.bracketed_paste);
    }

    #[test]
    fn script_rejects_unknown_fields() {
        assert!(serde_json::from_str::<Script>(r#"{ "banner": "b", "oops": 1 }"#).is_err());
    }
}
