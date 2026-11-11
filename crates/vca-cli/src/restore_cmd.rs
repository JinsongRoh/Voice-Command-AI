//! `vca restore`: the managed list of AI CLI windows of this computer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use vca_core::{
    ClientMsg, RestoreItem, RestoreItemStatus, RestoreJob, RestoreOverview, RestoreRunRequest,
    ServerMsg,
};

use crate::client::{Client, ping, unexpected};
use crate::daemon;
use crate::error::{CliError, CliResult};

/// How often `run` asks how the restore stands.
const POLL: Duration = Duration::from_millis(500);
const DISCOVERY_SETTLE: Duration = Duration::from_secs(6);

#[derive(Debug, Subcommand)]
pub enum RestoreCmd {
    /// Show the saved windows.
    List {
        /// Profile id or name (default: the active one).
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Measure the open AI CLI windows into the list now.
    Save {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Reopen the saved windows that are not open.
    Run {
        #[arg(long)]
        profile: Option<String>,
        /// Only windows whose title contains one of these (comma separated).
        #[arg(long, value_delimiter = ',', value_name = "A,B")]
        only: Vec<String>,
        /// Show what would happen; open nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Add the windows of a layout.json to the list.
    Import {
        file: PathBuf,
        #[arg(long)]
        profile: Option<String>,
    },
}

async fn overview(client: &mut Client, msg: ClientMsg) -> CliResult<RestoreOverview> {
    match client.api(msg).await? {
        ServerMsg::RestoreList { overview } => Ok(overview),
        other => Err(unexpected(&other)),
    }
}

fn status_label(status: RestoreItemStatus) -> &'static str {
    match status {
        RestoreItemStatus::Waiting => "waiting",
        RestoreItemStatus::Opening => "opening",
        RestoreItemStatus::Placed => "placed",
        RestoreItemStatus::Opened => "opened, not placed",
        RestoreItemStatus::SkippedOpen => "skipped: already open",
        RestoreItemStatus::SkippedMissing => "skipped: folder missing",
        RestoreItemStatus::SkippedOutside => "skipped: outside the workspace",
        RestoreItemStatus::Failed => "failed",
    }
}

fn item_line(item: &RestoreItem) -> String {
    let mut line = format!("{:<30} {}", status_label(item.status), item.title);
    if !item.command.is_empty() {
        line.push_str(&format!("  [{}]", item.command));
    }
    if let Some(message) = &item.message {
        line.push_str(&format!("  ({message})"));
    }
    line
}

fn print_list(overview: &RestoreOverview, profile: Option<&str>) -> CliResult<()> {
    if let Some(message) = &overview.message {
        println!("{message}");
    }
    let wanted = profile.unwrap_or(&overview.active_profile);
    let found = overview
        .profiles
        .iter()
        .find(|p| p.id == wanted || p.name.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| CliError::failure(format!("no profile {wanted}")))?;
    println!(
        "{} ({} windows, {} s between windows)",
        found.name,
        found.entries.len(),
        found.delay_secs
    );
    for entry in &found.entries {
        let open = overview
            .live
            .iter()
            .any(|l| l.entry_id == entry.id && l.open);
        let place = entry
            .rect
            .map(|r| format!("{},{} {}x{}", r.x, r.y, r.w, r.h))
            .unwrap_or_else(|| "no position".to_owned());
        println!(
            "{} {} {:<32} {:<24} {}",
            if entry.enabled { "on " } else { "off" },
            if open { "open  " } else { "closed" },
            entry.title,
            place,
            entry.folder.display(),
        );
    }
    Ok(())
}

/// Connects, starting vcad first when it is not running: after a reboot
/// this is often the first thing that runs.
async fn connect_or_start(endpoint: &Path, control: Option<&Path>) -> CliResult<Client> {
    if ping(endpoint).await.is_err() {
        daemon::start_detached(endpoint, control).await?;
        // Discovery needs a moment, so open windows are recognised.
        tokio::time::sleep(DISCOVERY_SETTLE).await;
    }
    Client::connect(endpoint).await
}

async fn run(
    endpoint: &Path,
    control: Option<&Path>,
    request: RestoreRunRequest,
) -> CliResult<i32> {
    let dry_run = request.dry_run;
    let mut client = if dry_run {
        Client::connect(endpoint).await?
    } else {
        connect_or_start(endpoint, control).await?
    };
    let reply = client.api(ClientMsg::RestoreRun { request }).await?;
    let ServerMsg::RestoreProgress { job } = reply else {
        return Err(unexpected(&reply));
    };
    if dry_run {
        for item in &job.items {
            println!("{}", item_line(item));
        }
        println!("dry run: nothing was opened");
        return Ok(0);
    }
    let finished = follow(&mut client, job).await?;
    let failed = finished
        .items
        .iter()
        .filter(|i| i.status == RestoreItemStatus::Failed)
        .count();
    Ok(i32::from(failed > 0))
}

/// Prints each window as it changes, until the job is done.
async fn follow(client: &mut Client, mut job: RestoreJob) -> CliResult<RestoreJob> {
    let mut shown: Vec<Option<RestoreItemStatus>> = vec![None; job.items.len()];
    loop {
        for (item, last) in job.items.iter().zip(shown.iter_mut()) {
            if *last != Some(item.status) && item.status != RestoreItemStatus::Waiting {
                println!("{}", item_line(item));
                *last = Some(item.status);
            }
        }
        if job.finished_at.is_some() {
            return Ok(job);
        }
        tokio::time::sleep(POLL).await;
        let list = overview(client, ClientMsg::RestoreList).await?;
        match list.job {
            Some(current) if current.id == job.id => job = current,
            _ => return Ok(job),
        }
    }
}

pub async fn command(
    endpoint: &Path,
    control: Option<&Path>,
    action: RestoreCmd,
) -> CliResult<i32> {
    match action {
        RestoreCmd::List { profile, json } => {
            let mut client = Client::connect(endpoint).await?;
            let list = overview(&mut client, ClientMsg::RestoreList).await?;
            if json {
                let text = serde_json::to_string_pretty(&list)
                    .map_err(|e| CliError::failure(e.to_string()))?;
                println!("{text}");
            } else {
                print_list(&list, profile.as_deref())?;
            }
            Ok(0)
        }
        RestoreCmd::Save { profile } => {
            let mut client = Client::connect(endpoint).await?;
            let msg = ClientMsg::RestoreCapture {
                profile: profile.clone(),
            };
            let list = overview(&mut client, msg).await?;
            print_list(&list, profile.as_deref())?;
            Ok(0)
        }
        RestoreCmd::Run {
            profile,
            only,
            dry_run,
        } => {
            let request = RestoreRunRequest {
                profile,
                only,
                dry_run,
                ..RestoreRunRequest::default()
            };
            run(endpoint, control, request).await
        }
        RestoreCmd::Import { file, profile } => {
            let layout = std::fs::read_to_string(&file)
                .map_err(|e| CliError::failure(format!("cannot read {}: {e}", file.display())))?;
            let mut client = Client::connect(endpoint).await?;
            let msg = ClientMsg::RestoreImport {
                layout,
                profile: profile.clone(),
            };
            let list = overview(&mut client, msg).await?;
            print_list(&list, profile.as_deref())?;
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use vca_core::RestoreResume;

    use super::*;

    fn item(status: RestoreItemStatus, message: Option<&str>) -> RestoreItem {
        RestoreItem {
            entry_id: "e1".into(),
            title: "Payments".into(),
            folder: PathBuf::from("/w"),
            status,
            resume: RestoreResume::Exact,
            command: "claude --resume s1".into(),
            rect: None,
            delay_after_secs: 0,
            message: message.map(str::to_owned),
        }
    }

    #[test]
    fn opened_but_not_placed_reads_differently_from_placed_and_failed() {
        let line = item_line(&item(
            RestoreItemStatus::Opened,
            Some("opened, but not placed: Wayland does not let an application move windows"),
        ));
        assert!(line.starts_with("opened, not placed"), "{line}");
        assert!(line.contains("Payments"), "{line}");
        assert!(line.contains("(opened, but not placed: Wayland"), "{line}");
        assert_ne!(
            status_label(RestoreItemStatus::Opened),
            status_label(RestoreItemStatus::Placed)
        );
        assert_ne!(
            status_label(RestoreItemStatus::Opened),
            status_label(RestoreItemStatus::Failed)
        );
    }
}
