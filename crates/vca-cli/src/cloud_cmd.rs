//! Cloud coding agents: list configured providers, launch a session and
//! store or delete account credentials (owner-only local channel).

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use clap::Subcommand;
use vca_core::{ClientMsg, CloudLaunchRequest, CloudProvider, ServerMsg};
use vca_server::cloud::{DEFAULT_ACCOUNT, Secret, parse_provider};
use vca_server::control::{LocalRequest, LocalResponse};

use crate::client::{Client, unexpected};
use crate::error::{CliError, CliResult};
use crate::output::short_id;

#[derive(Debug, Subcommand)]
pub enum CloudCmd {
    /// List configured cloud accounts and what each provider supports.
    Providers {
        #[arg(long)]
        json: bool,
    },
    /// Start a cloud session.
    Launch {
        /// jules, cursor, devin, copilot, claude-managed, claude-routines or codex.
        #[arg(long)]
        provider: String,
        /// Repository, usually `owner/repo`.
        #[arg(long = "repo", value_name = "REPO")]
        repo: String,
        /// Display name of the session.
        #[arg(long)]
        name: Option<String>,
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        prompt: Vec<String>,
    },
    /// Store the API key or token of a configured account (read from stdin).
    Login {
        provider: String,
        #[arg(long, default_value = DEFAULT_ACCOUNT)]
        account: String,
    },
    /// Delete the stored credential of an account.
    Logout {
        provider: String,
        #[arg(long, default_value = DEFAULT_ACCOUNT)]
        account: String,
    },
}

fn provider_arg(name: &str) -> CliResult<CloudProvider> {
    parse_provider(name).ok_or_else(|| {
        CliError::usage(format!(
            "unknown cloud provider {name:?} (jules, cursor, devin, copilot, \
             claude-managed, claude-routines, codex)"
        ))
    })
}

fn unexpected_local(resp: &LocalResponse) -> CliError {
    CliError::failure(format!("unexpected response from vcad: {resp:?}"))
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "-" }
}

pub async fn run(endpoint: &Path, cmd: CloudCmd) -> CliResult<i32> {
    match cmd {
        CloudCmd::Providers { json } => providers(endpoint, json).await,
        CloudCmd::Launch {
            provider,
            repo,
            name,
            prompt,
        } => launch(endpoint, &provider, repo, name, prompt.join(" ")).await,
        CloudCmd::Login { provider, account } => login(endpoint, &provider, account).await,
        CloudCmd::Logout { provider, account } => logout(endpoint, &provider, account).await,
    }
}

async fn providers(endpoint: &Path, json: bool) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let resp = client.api(ClientMsg::ListCloudProviders).await?;
    let ServerMsg::CloudProviders { providers } = resp else {
        return Err(unexpected(&resp));
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&providers)
                .map_err(|e| CliError::failure(e.to_string()))?
        );
        return Ok(0);
    }
    if providers.is_empty() {
        println!("no cloud accounts configured (add [[cloud.accounts]] to the vcad config)");
        return Ok(0);
    }
    println!(
        "{:<24} {:<12} {:<10} {:<7} {:<9} {:<5} {:<8}",
        "PROVIDER", "ACCOUNT", "LOGGED IN", "LAUNCH", "FOLLOW-UP", "STOP", "PROGRESS"
    );
    for p in providers {
        let mut progress = if p.status_only { "status" } else { "full" }.to_owned();
        if p.unverified {
            progress.push_str(" (unverified)");
        }
        println!(
            "{:<24} {:<12} {:<10} {:<7} {:<9} {:<5} {:<8}",
            provider_name(p.provider),
            p.account,
            yes_no(p.configured),
            yes_no(p.create),
            yes_no(p.follow_up),
            yes_no(p.stop),
            progress
        );
    }
    Ok(0)
}

/// The provider's protocol name, as used in the configuration file.
fn provider_name(p: CloudProvider) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn launch(
    endpoint: &Path,
    provider: &str,
    repository: String,
    display_name: Option<String>,
    prompt: String,
) -> CliResult<i32> {
    let provider = provider_arg(provider)?;
    let mut client = Client::connect(endpoint).await?;
    let resp = client
        .api(ClientMsg::LaunchCloud {
            request: CloudLaunchRequest {
                provider,
                repository,
                prompt,
                display_name,
            },
        })
        .await?;
    let ServerMsg::Launched { session } = resp else {
        return Err(unexpected(&resp));
    };
    println!(
        "launched cloud session {} ({})",
        short_id(&session.id),
        session.display_name
    );
    if let vca_core::SessionOrigin::Cloud { url: Some(url), .. } = &session.origin {
        println!("open: {url}");
    }
    Ok(0)
}

/// Reads one line from stdin without echoing it when stdin is a terminal.
fn read_secret(prompt: &str) -> CliResult<Secret> {
    let stdin = std::io::stdin();
    let line = if stdin.is_terminal() {
        eprint!("{prompt}");
        std::io::stderr().flush().ok();
        let line = read_hidden_line().map_err(|e| CliError::failure(e.to_string()))?;
        eprintln!();
        line
    } else {
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| CliError::failure(e.to_string()))?;
        line
    };
    let secret = line.trim();
    if secret.is_empty() {
        return Err(CliError::usage("no credential given"));
    }
    Ok(Secret::new(secret))
}

/// Reads keys in raw mode until Enter, so the secret is not shown.
fn read_hidden_line() -> std::io::Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

    crossterm::terminal::enable_raw_mode()?;
    let result = (|| {
        let mut line = String::new();
        loop {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Enter => return Ok(line),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "cancelled",
                        ));
                    }
                    KeyCode::Char(c) => line.push(c),
                    KeyCode::Backspace => {
                        line.pop();
                    }
                    _ => {}
                },
                Event::Paste(text) => line.push_str(&text),
                _ => {}
            }
        }
    })();
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

async fn login(endpoint: &Path, provider: &str, account: String) -> CliResult<i32> {
    let provider = provider_arg(provider)?;
    let label = format!("{}/{account}", provider_name(provider));
    let secret = read_secret(&format!("API key or token for {label} (input hidden): "))?;
    let mut client = Client::connect(endpoint).await?;
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::CloudCredentialSet {
            id,
            provider,
            account,
            secret,
        })
        .await?;
    let LocalResponse::Done { .. } = resp else {
        return Err(unexpected_local(&resp));
    };
    println!("stored the credential for {label}");
    Ok(0)
}

async fn logout(endpoint: &Path, provider: &str, account: String) -> CliResult<i32> {
    let provider = provider_arg(provider)?;
    let label = format!("{}/{account}", provider_name(provider));
    let mut client = Client::connect(endpoint).await?;
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::CloudCredentialDelete {
            id,
            provider,
            account,
        })
        .await?;
    let LocalResponse::Done { .. } = resp else {
        return Err(unexpected_local(&resp));
    };
    println!("deleted the stored credential for {label}");
    Ok(0)
}
