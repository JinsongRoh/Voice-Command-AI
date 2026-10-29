//! Voice Command AI command-line interface (vca).
//!
//! `vca` talks to the local daemon (`vcad`) over the owner-only control
//! channel: it lists and controls sessions, runs commands under management
//! (`vca run`, used by shell integration), forwards agent hook events and
//! installs hooks and shell integration.

mod client;
mod cloud_cmd;
mod daemon;
mod error;
mod hook;
mod hooks_cmd;
mod output;
mod remote_cmd;
mod run;
mod speech_cmd;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use vca_core::{ClientMsg, ControlKey, Input, PROTOCOL_VERSION, ServerMsg, SessionAction};
use vca_pty::shell::{Shell, ShellIntegration};

use crate::client::{Client, Delivery};
use crate::error::{CliError, CliResult, EXIT_FAILURE};
use crate::hooks_cmd::{HookAgent, HooksAction, ScopeArg};
use crate::output::short_id;

#[derive(Debug, Parser)]
#[command(
    name = "vca",
    version,
    about = "Voice Command AI command-line interface"
)]
struct Cli {
    /// Control endpoint of vcad (socket path or named pipe).
    #[arg(long, global = true, value_name = "PATH")]
    endpoint: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Check that vcad is reachable.
    Ping {
        /// Print nothing; only set the exit code.
        #[arg(long, short)]
        quiet: bool,
    },
    /// Run a command in a managed session and attach to it.
    Run {
        /// Display name of the session.
        #[arg(long)]
        name: Option<String>,
        /// The command and its arguments.
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Attach this terminal to a managed session (detach with Ctrl-] d).
    Attach { session: String },
    /// List sessions.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Type text into a session.
    Send {
        session: String,
        /// Do not press Enter after the text.
        #[arg(long)]
        no_submit: bool,
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// Press a key in a session.
    Key { session: String, key: KeyArg },
    /// Stop a session.
    Stop { session: String },
    /// Pause a session.
    Pause { session: String },
    /// Resume a paused session.
    Resume { session: String },
    /// Restart a session.
    Restart { session: String },
    /// Show system and per-session resource usage.
    Resources {
        #[arg(long)]
        json: bool,
    },
    /// Forward an agent hook event to vcad (called by the agent).
    Hook {
        /// `claude` or `codex`.
        agent: String,
        #[arg(num_args = 0.., allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Install, remove or inspect agent hooks.
    Hooks {
        action: HooksAction,
        #[arg(long, value_enum, default_value_t)]
        agent: HookAgent,
        #[arg(long, value_enum, default_value_t)]
        scope: ScopeArg,
        /// Project directory for project scopes (default: current directory).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Print the shell integration snippet.
    ShellInit {
        /// bash, zsh, fish or powershell (pwsh).
        shell: String,
    },
    /// Start vcad in the background.
    Daemon {
        /// Run vcad in this terminal instead.
        #[arg(long)]
        foreground: bool,
    },
    /// Create a single-use pairing code (QR) for a phone or another device.
    Pair {
        /// What the device may do: view, command or admin.
        #[arg(long, default_value = "command")]
        scope: String,
        /// Endpoint URL to put in the code (repeatable; default: configured).
        #[arg(long = "url", value_name = "URL")]
        urls: Vec<String>,
        /// Print only the link, without the QR code.
        #[arg(long)]
        no_qr: bool,
    },
    /// Two-factor sign-in for paired devices.
    Totp {
        #[command(subcommand)]
        action: TotpCmd,
    },
    /// List paired devices.
    Devices {
        #[arg(long)]
        json: bool,
    },
    /// Revoke a paired device and close its connections.
    Revoke { device_id: String },
    /// Drop every remote connection and refuse new ones (or undo it).
    KillSwitch {
        /// Allow remote connections again.
        #[arg(long)]
        reset: bool,
    },
    /// Show the state of the remote transport.
    Remote,
    /// Transcribe an audio file with the server's speech pipeline.
    Transcribe {
        /// WAV, WebM, Ogg or M4A file.
        file: PathBuf,
        /// Primary language (BCP-47); the server's setting by default.
        #[arg(long)]
        language: Option<String>,
        /// Session whose project vocabulary and names are used.
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Cloud coding agents: providers, launch, login and logout.
    Cloud {
        #[command(subcommand)]
        action: cloud_cmd::CloudCmd,
    },
    /// Print version information.
    Version,
}

#[derive(Debug, Subcommand)]
enum TotpCmd {
    /// Enroll an authenticator app.
    Enroll {
        /// Account label shown in the authenticator app.
        #[arg(long, default_value = "owner")]
        account: String,
        /// Confirm with this code instead of asking for it.
        #[arg(long)]
        code: Option<String>,
        /// Print only the secret, without the QR code.
        #[arg(long)]
        no_qr: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum KeyArg {
    Enter,
    Escape,
    Interrupt,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Backspace,
}

impl From<KeyArg> for ControlKey {
    fn from(key: KeyArg) -> Self {
        match key {
            KeyArg::Enter => Self::Enter,
            KeyArg::Escape => Self::Escape,
            KeyArg::Interrupt => Self::Interrupt,
            KeyArg::Tab => Self::Tab,
            KeyArg::Up => Self::Up,
            KeyArg::Down => Self::Down,
            KeyArg::Left => Self::Left,
            KeyArg::Right => Self::Right,
            KeyArg::Backspace => Self::Backspace,
        }
    }
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some((endpoint, hook_args)) = hook::detect(&args) {
        std::process::exit(hook::main(endpoint, hook_args));
    }
    let cli = Cli::parse();
    let code = match tokio::runtime::Runtime::new() {
        Ok(runtime) => match runtime.block_on(dispatch(cli)) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("error: {err}");
                err.exit_code
            }
        },
        Err(err) => {
            eprintln!("error: cannot start the async runtime: {err}");
            EXIT_FAILURE
        }
    };
    // Exit right away: attach may leave a thread blocked on stdin.
    std::process::exit(code);
}

async fn dispatch(cli: Cli) -> CliResult<i32> {
    let control = cli.endpoint.as_deref();
    let endpoint = cli
        .endpoint
        .as_deref()
        .map(|p| vca_server::control::parse_endpoint(&p.to_string_lossy()))
        .unwrap_or_else(vca_server::control::default_endpoint);
    let endpoint = endpoint.as_path();
    match cli.command {
        Cmd::Ping { quiet } => ping(endpoint, quiet).await,
        Cmd::Run { name, command } => run::run(endpoint, control, name, command).await,
        Cmd::Attach { session } => run::attach(endpoint, &session).await,
        Cmd::Ls { json } => ls(endpoint, json).await,
        Cmd::Send {
            session,
            no_submit,
            text,
        } => {
            let input = Input::Text {
                text: text.join(" "),
                submit: !no_submit,
            };
            send(endpoint, &session, input).await
        }
        Cmd::Key { session, key } => send(endpoint, &session, Input::Key { key: key.into() }).await,
        Cmd::Stop { session } => control_session(endpoint, &session, SessionAction::Stop).await,
        Cmd::Pause { session } => control_session(endpoint, &session, SessionAction::Pause).await,
        Cmd::Resume { session } => control_session(endpoint, &session, SessionAction::Resume).await,
        Cmd::Restart { session } => {
            control_session(endpoint, &session, SessionAction::Restart).await
        }
        Cmd::Resources { json } => resources(endpoint, json).await,
        Cmd::Hook { agent, args } => {
            // Normally handled before clap; kept for completeness.
            let args = std::iter::once(agent).chain(args).map(OsString::from);
            Ok(hook::main(cli.endpoint.clone(), args.collect()))
        }
        Cmd::Hooks {
            action,
            agent,
            scope,
            dir,
        } => hooks_cmd::hooks(action, agent, scope, dir).await,
        Cmd::ShellInit { shell } => shell_init(&shell),
        Cmd::Daemon { foreground } => daemon::daemon(endpoint, control, foreground).await,
        Cmd::Pair { scope, urls, no_qr } => remote_cmd::pair(endpoint, &scope, urls, !no_qr).await,
        Cmd::Totp {
            action:
                TotpCmd::Enroll {
                    account,
                    code,
                    no_qr,
                },
        } => remote_cmd::totp_enroll(endpoint, &account, code, !no_qr).await,
        Cmd::Devices { json } => remote_cmd::devices(endpoint, json).await,
        Cmd::Revoke { device_id } => remote_cmd::revoke(endpoint, &device_id).await,
        Cmd::KillSwitch { reset } => remote_cmd::kill_switch(endpoint, reset).await,
        Cmd::Remote => remote_cmd::status(endpoint).await,
        Cmd::Transcribe {
            file,
            language,
            session,
            json,
        } => speech_cmd::transcribe(endpoint, &file, language, session, json).await,
        Cmd::Cloud { action } => cloud_cmd::run(endpoint, action).await,
        Cmd::Version => {
            println!(
                "vca {} (protocol {PROTOCOL_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            Ok(0)
        }
    }
}

async fn ping(endpoint: &Path, quiet: bool) -> CliResult<i32> {
    match client::ping(endpoint).await {
        Ok(rtt) => {
            if !quiet {
                println!(
                    "vcad is reachable at {} (round trip {:.1} ms)",
                    endpoint.display(),
                    rtt.as_secs_f64() * 1000.0
                );
            }
            Ok(0)
        }
        Err(err) => {
            if !quiet {
                eprintln!("error: {err}");
            }
            Ok(EXIT_FAILURE)
        }
    }
}

async fn ls(endpoint: &Path, json: bool) -> CliResult<i32> {
    let sessions = Client::connect(endpoint).await?.sessions().await?;
    if json {
        println!("{}", to_json(&sessions)?);
    } else {
        print!("{}", output::sessions_table(&sessions));
    }
    Ok(0)
}

async fn send(endpoint: &Path, session: &str, input: Input) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let session_id = client.resolve(session).await?;
    let short = short_id(&session_id).to_owned();
    match client.send_input(session_id, input).await? {
        Delivery::Delivered => println!("delivered to session {short}"),
        Delivery::Queued => println!(
            "queued for session {short} (delivery not confirmed within {} s)",
            client::DELIVERY_TIMEOUT.as_secs()
        ),
    }
    Ok(0)
}

async fn control_session(endpoint: &Path, session: &str, action: SessionAction) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let session_id = client.resolve(session).await?;
    let short = short_id(&session_id).to_owned();
    match client
        .api(ClientMsg::Control { session_id, action })
        .await?
    {
        ServerMsg::Ok | ServerMsg::SessionUpdated { .. } => {
            let verb = match action {
                SessionAction::Stop => "stop",
                SessionAction::Pause => "pause",
                SessionAction::Resume => "resume",
                SessionAction::Restart => "restart",
            };
            println!("{verb}: ok (session {short})");
            Ok(0)
        }
        other => Err(client::unexpected(&other)),
    }
}

async fn resources(endpoint: &Path, json: bool) -> CliResult<i32> {
    match Client::connect(endpoint)
        .await?
        .api(ClientMsg::GetResources)
        .await?
    {
        ServerMsg::Resources { snapshot } => {
            if json {
                println!("{}", to_json(&snapshot)?);
            } else {
                print!("{}", output::resources(&snapshot));
            }
            Ok(0)
        }
        other => Err(client::unexpected(&other)),
    }
}

fn shell_init(name: &str) -> CliResult<i32> {
    let shell = Shell::from_name(name).ok_or_else(|| {
        CliError::usage(format!(
            "unsupported shell {name:?} (supported: bash, zsh, fish, powershell)"
        ))
    })?;
    let vca_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| "vca".into());
    let cfg = ShellIntegration {
        vca_bin,
        ..ShellIntegration::default()
    };
    let script =
        vca_pty::shell::script(shell, &cfg).map_err(|e| CliError::failure(e.to_string()))?;
    print!("{script}");
    Ok(0)
}

fn to_json<T: serde::Serialize>(value: &T) -> CliResult<String> {
    serde_json::to_string_pretty(value).map_err(|e| CliError::failure(e.to_string()))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("vca").chain(args.iter().copied()))
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_run() {
        let cli = parse(&["run", "--name", "x", "--", "claude", "-p", "--flag", "a b"]).unwrap();
        match cli.command {
            Cmd::Run { name, command } => {
                assert_eq!(name.as_deref(), Some("x"));
                assert_eq!(command, ["claude", "-p", "--flag", "a b"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse(&["run"]).is_err());
        assert!(parse(&["run", "claude"]).is_err());
    }

    #[test]
    fn parses_global_endpoint() {
        let cli = parse(&["ls", "--endpoint", "/tmp/s.sock", "--json"]).unwrap();
        assert_eq!(cli.endpoint.as_deref(), Some(Path::new("/tmp/s.sock")));
        assert!(matches!(cli.command, Cmd::Ls { json: true }));
    }

    #[test]
    fn parses_send_and_key() {
        let cli = parse(&["send", "ab12", "--no-submit", "fix", "the", "-v", "bug"]).unwrap();
        match cli.command {
            Cmd::Send {
                session,
                no_submit,
                text,
            } => {
                assert_eq!(session, "ab12");
                assert!(no_submit);
                assert_eq!(text, ["fix", "the", "-v", "bug"]);
            }
            other => panic!("{other:?}"),
        }
        let cli = parse(&["key", "ab12", "interrupt"]).unwrap();
        assert!(matches!(
            cli.command,
            Cmd::Key {
                key: KeyArg::Interrupt,
                ..
            }
        ));
        assert!(parse(&["key", "ab12", "f1"]).is_err());
        assert_eq!(ControlKey::from(KeyArg::Backspace), ControlKey::Backspace);
    }

    #[test]
    fn parses_hooks() {
        let cli = parse(&[
            "hooks",
            "install",
            "--agent",
            "claude",
            "--scope",
            "project-local",
        ])
        .unwrap();
        match cli.command {
            Cmd::Hooks {
                action,
                agent,
                scope,
                dir,
            } => {
                assert_eq!(action, HooksAction::Install);
                assert_eq!(agent, HookAgent::Claude);
                assert_eq!(scope, ScopeArg::ProjectLocal);
                assert!(dir.is_none());
            }
            other => panic!("{other:?}"),
        }
        let cli = parse(&["hooks", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Cmd::Hooks {
                agent: HookAgent::All,
                scope: ScopeArg::User,
                ..
            }
        ));
    }

    #[test]
    fn parses_cloud_commands() {
        let cli = parse(&[
            "cloud",
            "launch",
            "--provider",
            "cursor",
            "--repo",
            "octo/hello",
            "--name",
            "x",
            "fix",
            "-v",
            "it",
        ])
        .unwrap();
        match cli.command {
            Cmd::Cloud {
                action:
                    cloud_cmd::CloudCmd::Launch {
                        provider,
                        repo,
                        name,
                        prompt,
                    },
            } => {
                assert_eq!(provider, "cursor");
                assert_eq!(repo, "octo/hello");
                assert_eq!(name.as_deref(), Some("x"));
                assert_eq!(prompt, ["fix", "-v", "it"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse(&["cloud", "launch", "--provider", "cursor", "p"]).is_err());
        match parse(&["cloud", "login", "jules"]).unwrap().command {
            Cmd::Cloud {
                action: cloud_cmd::CloudCmd::Login { provider, account },
            } => {
                assert_eq!(provider, "jules");
                assert_eq!(account, "default");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(&["cloud", "logout", "devin", "--account", "work"])
                .unwrap()
                .command,
            Cmd::Cloud {
                action: cloud_cmd::CloudCmd::Logout { .. }
            }
        ));
        assert!(matches!(
            parse(&["cloud", "providers", "--json"]).unwrap().command,
            Cmd::Cloud {
                action: cloud_cmd::CloudCmd::Providers { json: true }
            }
        ));
    }

    #[test]
    fn parses_control_commands() {
        for verb in ["stop", "pause", "resume", "restart", "attach"] {
            assert!(parse(&[verb, "abc"]).is_ok(), "{verb}");
            assert!(parse(&[verb]).is_err(), "{verb}");
        }
        assert!(matches!(
            parse(&["daemon", "--foreground"]).unwrap().command,
            Cmd::Daemon { foreground: true }
        ));
        assert!(matches!(
            parse(&["ping", "--quiet"]).unwrap().command,
            Cmd::Ping { quiet: true }
        ));
    }
}
