//! `vca run` and `vca attach`.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use vca_pty::AttachOutcome;
use vca_server::control::{
    CONTROL_ENV, LocalRequest, LocalResponse, RunRequest, SESSION_ENV, TermSize,
};

use crate::client::Client;
use crate::daemon;
use crate::error::{CliError, CliResult, EXIT_FAILURE};
use crate::output::short_id;

/// Exported to managed sessions so shell integration does not nest them.
const MANAGED_ENV: &str = "VCA_MANAGED";
/// Exit code when the command cannot be started, as in POSIX shells.
const EXIT_NOT_FOUND: i32 = 127;

/// Converts an exit status into a process exit code (128 + signal for
/// processes killed by a signal on Unix).
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    EXIT_FAILURE
}

/// Runs a child with inherited stdio and waits for it. Ctrl-C is left to the
/// child: this process ignores it while waiting.
pub async fn wait_inherited(mut cmd: tokio::process::Command) -> io::Result<ExitStatus> {
    let mut child = cmd.spawn()?;
    loop {
        tokio::select! {
            status = child.wait() => return status,
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

/// Runs the command directly, without vcad.
async fn run_direct(program: &str, args: &[String]) -> i32 {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    match wait_inherited(cmd).await {
        Ok(status) => exit_code(status),
        Err(err) => {
            eprintln!("vca: {program}: {err}");
            EXIT_NOT_FOUND
        }
    }
}

fn terminal_size() -> Option<TermSize> {
    if !io::stdout().is_terminal() {
        return None;
    }
    crossterm::terminal::size()
        .ok()
        .filter(|&(cols, rows)| cols > 0 && rows > 0)
        .map(|(cols, rows)| TermSize { cols, rows })
}

/// The environment passed to the managed session. `VCA_CONTROL` points at
/// the daemon that launches it so hooks reach the same vcad.
fn session_env(endpoint: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| k != MANAGED_ENV && k != CONTROL_ENV)
        .collect();
    env.push((MANAGED_ENV.into(), "1".into()));
    if let Some(endpoint) = endpoint.to_str() {
        env.push((CONTROL_ENV.into(), endpoint.into()));
    }
    env
}

fn build_request(
    program: String,
    args: Vec<String>,
    name: Option<String>,
    endpoint: &Path,
) -> io::Result<RunRequest> {
    Ok(RunRequest {
        program,
        args,
        cwd: std::env::current_dir()?,
        env: session_env(endpoint),
        size: terminal_size(),
        agent: None,
        display_name: name,
    })
}

/// Maps the end of an attachment to the exit code of `vca run`/`attach`.
fn finish(outcome: AttachOutcome, session: &str) -> i32 {
    match outcome {
        AttachOutcome::Exited { code } => code.unwrap_or(EXIT_FAILURE),
        AttachOutcome::Detached => {
            eprintln!("detached; session {session} keeps running (vca attach {session})");
            0
        }
        AttachOutcome::Disconnected => {
            eprintln!("vca: connection to session {session} lost");
            EXIT_FAILURE
        }
    }
}

async fn attach_endpoint(endpoint: &Path, session: &str) -> CliResult<i32> {
    let outcome = vca_pty::attach_stdio(endpoint)
        .await
        .map_err(|err| CliError::failure(format!("cannot attach to session {session}: {err}")))?;
    Ok(finish(outcome, session))
}

/// Connects to vcad, starting it when it is not running.
async fn connect_or_start(endpoint: &Path, control: Option<&Path>) -> CliResult<Client> {
    match Client::connect(endpoint).await {
        Ok(client) => Ok(client),
        Err(_) => {
            daemon::start_detached(endpoint, control).await?;
            Client::connect(endpoint).await
        }
    }
}

/// Asks vcad to launch the command and returns the session id and its
/// attach endpoint.
async fn launch(
    endpoint: &Path,
    control: Option<&Path>,
    request: RunRequest,
) -> CliResult<(String, PathBuf)> {
    let mut client = connect_or_start(endpoint, control).await?;
    let id = client.next_id();
    match client.local(LocalRequest::Run { id, request }).await? {
        LocalResponse::Run {
            session, endpoint, ..
        } => Ok((session.id.0, endpoint)),
        other => Err(CliError::failure(format!(
            "unexpected reply from vcad: {other:?}"
        ))),
    }
}

/// `vca run [--name NAME] -- <cmd> [args...]`. Never fails because of vcad:
/// without it the command runs directly.
pub async fn run(
    endpoint: &Path,
    control: Option<&Path>,
    name: Option<String>,
    command: Vec<String>,
) -> CliResult<i32> {
    let mut command = command.into_iter();
    let Some(program) = command.next() else {
        return Err(CliError::usage("no command given"));
    };
    let args: Vec<String> = command.collect();

    // Already inside a managed session: do not nest.
    if std::env::var_os(SESSION_ENV).is_some_and(|v| !v.is_empty()) {
        return Ok(run_direct(&program, &args).await);
    }

    let request = build_request(program.clone(), args.clone(), name, endpoint)
        .map_err(|err| CliError::failure(format!("cannot read the current directory: {err}")))?;
    let (session, attach) = match launch(endpoint, control, request).await {
        Ok(launched) => launched,
        Err(err) => {
            eprintln!("vca: {err}; running {program} without VCA");
            return Ok(run_direct(&program, &args).await);
        }
    };
    let short = short_id(&session.as_str().into()).to_owned();
    attach_endpoint(&attach, &short).await
}

/// `vca attach <session>`.
pub async fn attach(endpoint: &Path, session: &str) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let session_id = client.resolve(session).await?;
    let id = client.next_id();
    match client
        .local(LocalRequest::Attach { id, session_id })
        .await?
    {
        LocalResponse::Attach {
            session_id,
            endpoint,
            ..
        } => {
            drop(client);
            attach_endpoint(&endpoint, short_id(&session_id)).await
        }
        other => Err(CliError::failure(format!(
            "unexpected reply from vcad: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_env_marks_managed() {
        let env = session_env(Path::new("/tmp/x.sock"));
        assert_eq!(env.iter().filter(|(k, _)| k == MANAGED_ENV).count(), 1);
        assert!(env.contains(&(CONTROL_ENV.into(), "/tmp/x.sock".into())));
        assert!(env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")));
    }

    #[test]
    fn attach_outcomes() {
        assert_eq!(finish(AttachOutcome::Exited { code: Some(7) }, "s"), 7);
        assert_eq!(finish(AttachOutcome::Exited { code: None }, "s"), 1);
        assert_eq!(finish(AttachOutcome::Detached, "s"), 0);
        assert_eq!(finish(AttachOutcome::Disconnected, "s"), 1);
    }
}
