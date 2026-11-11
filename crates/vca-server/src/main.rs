//! Voice Command AI server daemon (`vcad`).

use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use tracing_subscriber::EnvFilter;
use vca_core::{ClientMsg, RestoreRunRequest, ServerMsg};
use vca_server::config::{Config, Paths};
use vca_server::control::{self, ControlClient};
use vca_server::daemon::{Options, Server, install_panic_hook, shutdown_signal, spawn_heartbeat};

/// How long `--restore` waits for a server it started.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How often a running vcad says so in its log.
const HEARTBEAT: Duration = Duration::from_secs(60 * 60);
/// Lets discovery see the running sessions, so open windows are skipped.
const DISCOVERY_SETTLE: Duration = Duration::from_secs(6);

const USAGE: &str = "\
Usage: vcad [--config <PATH>] [--control <PATH>] [--data-dir <PATH>]
       vcad --restore [--restore-delay <SECONDS>] [--control <PATH>]

Runs the Voice Command AI server in the foreground. Use `vca daemon` to
start it in the background.

Options:
  --config <PATH>    Configuration file (default: the OS config directory)
  --control <PATH>   Local control endpoint (socket path or pipe name)
  --data-dir <PATH>  Data directory
  --restore          Reopen the saved AI CLI windows, starting the server
                     first when it is not running, then exit (the desktop
                     and sign-in shortcuts use this)
  --restore-delay <SECONDS>  Wait this long first
  -h, --help         Show this help
  -V, --version      Show the version";

/// `--restore`.
#[derive(Debug, Clone, Copy, Default)]
struct RestoreFlag {
    delay_secs: u64,
}

fn parse_args() -> Result<Option<(Options, Option<RestoreFlag>)>, String> {
    let mut opts = Options::default();
    let mut restore = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg = arg.to_string_lossy().into_owned();
        let mut value = |name: &str| {
            args.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--config" => opts.config_file = Some(value("--config")?),
            "--control" => opts.control = Some(value("--control")?),
            "--data-dir" => opts.data_dir = Some(value("--data-dir")?),
            "--restore" => {
                restore.get_or_insert(RestoreFlag::default());
            }
            "--restore-delay" => {
                let secs = value("--restore-delay")?;
                let delay_secs = secs
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| "--restore-delay needs a number of seconds".to_owned())?;
                restore.get_or_insert(RestoreFlag::default()).delay_secs = delay_secs;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("vcad {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    Ok(Some((opts, restore)))
}

/// Asks the running server to reopen the saved windows, starting it first
/// when needed. The run itself belongs to the server; this process exits.
async fn request_restore(opts: &Options, flag: RestoreFlag) -> Result<(), String> {
    tokio::time::sleep(Duration::from_secs(flag.delay_secs)).await;
    let endpoint = opts
        .control
        .clone()
        .map(|p| control::parse_endpoint(&p.to_string_lossy()))
        .unwrap_or_else(control::default_endpoint);
    let wait = Duration::from_millis(500);
    let mut client = match ControlClient::connect_timeout(&endpoint, wait).await {
        Ok(client) => client,
        Err(_) => {
            start_server(opts)?;
            let deadline = Instant::now() + START_TIMEOUT;
            let client = loop {
                if let Ok(client) = ControlClient::connect_timeout(&endpoint, wait).await {
                    break client;
                }
                if Instant::now() >= deadline {
                    return Err("the server did not start in time".into());
                }
                tokio::time::sleep(wait).await;
            };
            tokio::time::sleep(DISCOVERY_SETTLE).await;
            client
        }
    };
    let request = RestoreRunRequest::default();
    match client.api(ClientMsg::RestoreRun { request }).await {
        Ok(ServerMsg::RestoreProgress { job }) => {
            println!("vcad: restoring {} windows", job.items.len());
            Ok(())
        }
        Ok(ServerMsg::Error { message, .. }) => Err(message),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(err) => Err(err.to_string()),
    }
}

/// Starts this program as the server, detached from this window.
fn start_server(opts: &Options) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    if let Some(path) = &opts.config_file {
        cmd.arg("--config").arg(path);
    }
    if let Some(path) = &opts.control {
        cmd.arg("--control").arg(path);
    }
    if let Some(path) = &opts.data_dir {
        cmd.arg("--data-dir").arg(path);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
        .map(drop)
        .map_err(|e| format!("cannot start the server: {e}"))
}

fn init_logging(opts: &Options) {
    let file = opts
        .config_file
        .clone()
        .unwrap_or_else(|| Paths::from_env().config_file);
    // The configured filter is only a default; RUST_LOG wins.
    let configured = Config::load(&file).ok().and_then(|c| c.log);
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(configured.as_deref().unwrap_or("info")))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> ExitCode {
    let (opts, restore) = match parse_args() {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("vcad: {msg}");
            return ExitCode::from(2);
        }
    };
    if let Some(flag) = restore {
        return match request_restore(&opts, flag).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("vcad: {msg}");
                ExitCode::FAILURE
            }
        };
    }
    init_logging(&opts);
    install_panic_hook();
    tracing::info!(
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        "vcad starting"
    );
    let server = match Server::start(opts, None).await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!("vcad cannot start, exiting: {err:#}");
            eprintln!("vcad: {err:#}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(ws) = &server.dev_ws {
        eprintln!(
            "vcad: development WebSocket on ws://{}/?token={} (development only)",
            ws.addr, ws.token
        );
    }
    let heartbeat = spawn_heartbeat(HEARTBEAT);
    let reason = shutdown_signal().await;
    tracing::info!("vcad stopping: {reason}");
    heartbeat.abort();
    server.shutdown().await;
    tracing::info!("vcad exiting (code 0)");
    ExitCode::SUCCESS
}
