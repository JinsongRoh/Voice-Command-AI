//! Voice Command AI server daemon (`vcad`).

use std::path::PathBuf;
use std::process::ExitCode;

use tracing_subscriber::EnvFilter;
use vca_server::config::{Config, Paths};
use vca_server::daemon::{Options, Server, shutdown_signal};

const USAGE: &str = "\
Usage: vcad [--config <PATH>] [--control <PATH>] [--data-dir <PATH>]

Runs the Voice Command AI server in the foreground. Use `vca daemon` to
start it in the background.

Options:
  --config <PATH>    Configuration file (default: the OS config directory)
  --control <PATH>   Local control endpoint (socket path or pipe name)
  --data-dir <PATH>  Data directory
  -h, --help         Show this help
  -V, --version      Show the version";

fn parse_args() -> Result<Option<Options>, String> {
    let mut opts = Options::default();
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
    Ok(Some(opts))
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
    let opts = match parse_args() {
        Ok(Some(opts)) => opts,
        Ok(None) => return ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("vcad: {msg}");
            return ExitCode::from(2);
        }
    };
    init_logging(&opts);
    let server = match Server::start(opts, None).await {
        Ok(s) => s,
        Err(err) => {
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
    shutdown_signal().await;
    server.shutdown().await;
    ExitCode::SUCCESS
}
