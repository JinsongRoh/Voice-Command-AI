//! Locating and starting vcad.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{CliError, CliResult};
use crate::{client, run};

/// How long to wait for a freshly started vcad to answer `ping`.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

const VCAD: &str = if cfg!(windows) { "vcad.exe" } else { "vcad" };

/// `vcad` next to the current executable, or `vcad` on `PATH`.
pub fn vcad_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(VCAD)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(VCAD))
}

/// The vcad command line. `control` is passed as `--control` when the user
/// chose an endpoint explicitly.
fn vcad_command(control: Option<&Path>) -> Command {
    let mut cmd = Command::new(vcad_path());
    if let Some(control) = control {
        cmd.arg("--control").arg(control);
    }
    cmd
}

/// Runs vcad attached to this terminal and returns its exit code.
pub async fn run_foreground(control: Option<&Path>) -> CliResult<i32> {
    let status = run::wait_inherited(vcad_command(control).into())
        .await
        .map_err(|err| CliError::failure(format!("cannot run {}: {err}", vcad_path().display())))?;
    Ok(run::exit_code(status))
}

/// Starts vcad in the background, detached from this terminal, and waits
/// until it answers `ping` at `endpoint`.
pub async fn start_detached(endpoint: &Path, control: Option<&Path>) -> CliResult<()> {
    let mut cmd = vcad_command(control);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    let mut child = cmd.spawn().map_err(|err| {
        CliError::failure(format!("cannot start {}: {err}", vcad_path().display()))
    })?;

    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if client::ping(endpoint).await.is_ok() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(CliError::failure(format!(
                "vcad exited during startup ({status}); run `vca daemon --foreground` to see why"
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(CliError::failure(format!(
                "vcad did not become reachable at {} within {} s",
                endpoint.display(),
                START_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(any(unix, windows)))]
fn detach(_cmd: &mut Command) {}

/// `vca daemon`.
pub async fn daemon(endpoint: &Path, control: Option<&Path>, foreground: bool) -> CliResult<i32> {
    if foreground {
        return run_foreground(control).await;
    }
    if client::ping(endpoint).await.is_ok() {
        println!("vcad is already running at {}", endpoint.display());
        return Ok(0);
    }
    start_detached(endpoint, control).await?;
    println!("vcad started at {}", endpoint.display());
    Ok(0)
}
