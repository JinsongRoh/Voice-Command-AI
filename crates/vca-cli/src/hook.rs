//! `vca hook claude <event>` and `vca hook codex <args...>`.
//!
//! Hooks run inside the agent, so they must never get in its way: on any
//! failure (vcad down, timeout, bad payload or arguments, even a panic) the
//! hook exits 0 without writing to stdout. Diagnostics go to stderr only
//! when `VCA_HOOK_DEBUG` is set.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use vca_adapters::claude::{ClaudeHookEventName, ClaudeHookPayload, render_response};
use vca_adapters::codex::{CodexNotifyPayload, parse_notify_args};
use vca_adapters::{AgentHookEvent, HookDecision, HookResponse};
use vca_core::SessionId;
use vca_server::control::{ControlClient, LocalRequest, LocalResponse, SESSION_ENV};

const DEBUG_ENV: &str = "VCA_HOOK_DEBUG";
/// Total time a blocking hook (for example `PreToolUse`) waits for vcad.
const BLOCKING_TIMEOUT: Duration = Duration::from_secs(55);
/// Total time any other hook waits for vcad.
const NON_BLOCKING_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the agent to finish writing the payload.
const STDIN_TIMEOUT: Duration = Duration::from_secs(10);

fn debug_enabled() -> bool {
    std::env::var_os(DEBUG_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Detects a hook invocation in the raw arguments (without the program
/// name): `[--endpoint PATH | --endpoint=PATH] hook <agent> [args...]`.
/// Hooks are handled before clap so that argument errors fail open too.
/// Returns the endpoint override and the arguments after `hook`.
pub fn detect(args: &[OsString]) -> Option<(Option<PathBuf>, Vec<OsString>)> {
    let mut endpoint = None;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let text = arg.to_str()?;
        if text == "--endpoint" {
            endpoint = Some(PathBuf::from(args.get(i + 1)?));
            i += 2;
        } else if let Some(value) = text.strip_prefix("--endpoint=") {
            endpoint = Some(PathBuf::from(value));
            i += 1;
        } else if text == "hook" {
            return Some((endpoint, args[i + 1..].to_vec()));
        } else {
            return None;
        }
    }
    None
}

/// Runs a hook and returns the process exit code. Never panics and never
/// writes to stdout except for the agent's reply.
pub fn main(endpoint: Option<PathBuf>, args: Vec<OsString>) -> i32 {
    let debug = debug_enabled();
    if !debug {
        panic::set_hook(Box::new(|_| {}));
    }
    let result = panic::catch_unwind(AssertUnwindSafe(|| run(endpoint, args)));
    match result {
        Ok(Ok(response)) => {
            // Write errors (a closed pipe) are ignored: the agent is gone.
            let _ = std::io::stdout().write_all(response.stdout.as_bytes());
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().write_all(response.stderr.as_bytes());
            response.exit_code
        }
        Ok(Err(err)) => {
            if debug {
                eprintln!("vca hook: {err:#}");
            }
            0
        }
        Err(_) => {
            if debug {
                eprintln!("vca hook: panicked");
            }
            0
        }
    }
}

fn run(endpoint: Option<PathBuf>, args: Vec<OsString>) -> anyhow::Result<HookResponse> {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.into_string().map_err(|_| anyhow!("non-UTF-8 argument")))
        .collect::<anyhow::Result<_>>()?;
    let Some((agent, rest)) = args.split_first() else {
        bail!("missing agent name");
    };
    let endpoint = endpoint
        .map(|p| vca_server::control::parse_endpoint(&p.to_string_lossy()))
        .unwrap_or_else(vca_server::control::default_endpoint);
    match agent.as_str() {
        "claude" => claude(&endpoint, rest),
        "codex" => codex(&endpoint, rest),
        other => bail!("unsupported agent {other:?}"),
    }
}

fn claude(endpoint: &Path, args: &[String]) -> anyhow::Result<HookResponse> {
    let arg = args.first().context("missing hook event name")?;
    let event_name = ClaudeHookEventName::from_cli_arg(arg);
    let input = read_stdin(STDIN_TIMEOUT)?;
    let mut payload = ClaudeHookPayload::parse(&input).context("invalid hook payload")?;
    if payload.hook_event_name.is_empty() {
        payload.hook_event_name = event_name.as_str().to_owned();
    }
    let event = payload.to_agent_event();
    let decision = forward(endpoint, event)?;
    Ok(render_response(&event_name, &decision))
}

fn codex(endpoint: &Path, args: &[String]) -> anyhow::Result<HookResponse> {
    let notify = parse_notify_args(args).context("missing notify payload")?;
    if let Some((program, chain_args)) = notify.chain.split_first() {
        // Forward to the notify program VCA replaced; failures are ignored.
        let _ = Command::new(program)
            .args(chain_args)
            .arg(&notify.payload)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    let payload = CodexNotifyPayload::parse(&notify.payload).context("invalid notify payload")?;
    forward(endpoint, payload.to_agent_event())?;
    // Codex ignores the output of its notify program.
    Ok(HookResponse::ok())
}

/// Reads stdin to the end on a helper thread, giving up after `timeout`.
fn read_stdin(timeout: Duration) -> anyhow::Result<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut input = String::new();
        let result = std::io::stdin().read_to_string(&mut input).map(|_| input);
        let _ = tx.send(result);
    });
    rx.recv_timeout(timeout)
        .context("timed out reading the hook payload")?
        .context("reading the hook payload")
}

/// Sends the event to vcad and waits for its decision.
fn forward(endpoint: &Path, event: AgentHookEvent) -> anyhow::Result<HookDecision> {
    let timeout = if event.blocking {
        BLOCKING_TIMEOUT
    } else {
        NON_BLOCKING_TIMEOUT
    };
    let session_id = std::env::var(SESSION_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .map(SessionId);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(timeout, async {
            let mut client = ControlClient::connect(endpoint).await?;
            let id = client.next_id();
            let req = LocalRequest::Hook {
                id,
                session_id,
                event: Box::new(event),
            };
            match client.call(&req).await? {
                LocalResponse::Hook { decision, .. } => Ok(decision),
                other => Err(anyhow!("unexpected reply from vcad: {other:?}")),
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for vcad"))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn detects_hook_invocations() {
        assert_eq!(
            detect(&os(&["hook", "claude", "stop"])),
            Some((None, os(&["claude", "stop"])))
        );
        assert_eq!(
            detect(&os(&["--endpoint", "/x", "hook", "codex", "--", "a", "{}"])),
            Some((Some("/x".into()), os(&["codex", "--", "a", "{}"])))
        );
        assert_eq!(
            detect(&os(&["--endpoint=/y", "hook"])),
            Some((Some("/y".into()), vec![]))
        );
        assert_eq!(detect(&os(&["ls", "hook"])), None);
        assert_eq!(detect(&os(&["hooks", "status"])), None);
        assert_eq!(detect(&os(&["--endpoint"])), None);
        assert_eq!(detect(&[]), None);
    }

    #[test]
    fn bad_arguments_fail_open() {
        let endpoint = Some(PathBuf::from("unused"));
        assert!(run(endpoint.clone(), vec![]).is_err());
        assert!(run(endpoint.clone(), os(&["gemini"])).is_err());
        assert!(run(endpoint.clone(), os(&["claude"])).is_err());
        assert!(run(endpoint, os(&["codex"])).is_err());
    }
}
