//! Owner-only administration of the remote transport: pairing codes, TOTP
//! enrollment, paired devices and the kill switch.

use std::io::{BufRead, Write};
use std::path::Path;

use vca_core::error::ErrorCode;
use vca_core::{ClientMsg, DeviceScope, PairedDevice, ServerMsg};
use vca_server::control::{LocalRequest, LocalResponse};
use vca_server::security::{CODE_JUST_USED, Sensitive};

use crate::client::{Client, unexpected};
use crate::cloud_cmd::read_secret;
use crate::error::{CliError, CliResult};
use crate::security_cmd::gated_local;

/// Renders `text` as a QR code with Unicode half blocks.
fn qr(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

fn unexpected_local(resp: &LocalResponse) -> CliError {
    CliError::failure(format!("unexpected response from vcad: {resp:?}"))
}

fn expiry(unix_secs: u64) -> String {
    let now = vca_core::now_millis() / 1000;
    let left = unix_secs.saturating_sub(now);
    format!("{}m {:02}s", left / 60, left % 60)
}

pub async fn pair(
    endpoint: &Path,
    scope: &str,
    endpoints: Vec<String>,
    show_qr: bool,
) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let (resp, _) = gated_local(&mut client, |id, step_up| LocalRequest::Pair {
        id,
        scope: scope.to_owned(),
        endpoints: endpoints.clone(),
        step_up,
    })
    .await?;
    let LocalResponse::Pairing {
        uri,
        expires_at,
        server_fingerprint,
        ..
    } = resp
    else {
        return Err(unexpected_local(&resp));
    };
    if show_qr {
        if let Some(code) = qr(&uri) {
            println!("{code}");
        }
    }
    println!("Scan the code with the VCA app, or paste this pairing link:");
    println!("{uri}");
    println!();
    println!(
        "Scope: {scope}. Single use, expires in {}.",
        expiry(expires_at)
    );
    println!("Server fingerprint: {server_fingerprint}");
    if let Ok(payload) = vca_net::PairingPayload::parse(&uri) {
        println!(
            "Confirm code: {} (the phone must show the same code)",
            payload.confirm_code()
        );
    }
    println!("Anyone who sees this code can pair until it is used or expires.");
    Ok(0)
}

pub async fn totp_enroll(
    endpoint: &Path,
    account: &str,
    code: Option<String>,
    show_qr: bool,
) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let (resp, step_up) = gated_local(&mut client, |id, step_up| LocalRequest::TotpBegin {
        id,
        account: account.to_owned(),
        step_up,
    })
    .await?;
    let LocalResponse::TotpPending {
        otpauth_uri,
        secret_base32,
        ..
    } = resp
    else {
        return Err(unexpected_local(&resp));
    };
    if code.is_none() {
        if show_qr {
            if let Some(qr) = qr(&otpauth_uri) {
                println!("{qr}");
            }
        }
        println!("Add this account to an authenticator app (scan the code above),");
        println!("or enter the secret manually: {secret_base32}");
    }
    let code = match code {
        Some(c) => c,
        None => {
            print!("Code from the app: ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| CliError::failure(e.to_string()))?;
            line.trim().to_owned()
        }
    };
    // Replacing needs the old authenticator, unless vcad has its proof
    // already: a recovery, or a code it just checked for the verification.
    // Without it: `vca security recover`.
    let mut current_code = None;
    let mut tries = 0;
    let backup_codes = loop {
        let id = client.next_id();
        let resp = client
            .local_answer(LocalRequest::TotpConfirm {
                id,
                code: code.clone(),
                current_code: current_code.clone(),
                step_up: step_up.clone(),
            })
            .await?;
        tries += 1;
        match resp {
            LocalResponse::TotpEnrolled { backup_codes, .. } => break backup_codes,
            LocalResponse::Error {
                code: ErrorCode::CurrentCodeRequired,
                ..
            } if tries < 4 => {
                let typed = read_secret("Code from the current authenticator (input hidden): ")?;
                current_code = Some(Sensitive::from(typed.expose()));
            }
            // A code that was right, but used a moment ago: the next one is.
            LocalResponse::Error { message, .. } if message == CODE_JUST_USED && tries < 4 => {
                eprintln!("{CODE_JUST_USED}");
                let typed = read_secret("Next code (input hidden): ")?;
                current_code = Some(Sensitive::from(typed.expose()));
            }
            LocalResponse::Error { message, .. } => return Err(CliError::failure(message)),
            other => return Err(unexpected_local(&other)),
        }
    };
    println!("TOTP is enabled. Backup codes (each works once; store them safely):");
    for c in backup_codes {
        println!("  {c}");
    }
    Ok(0)
}

pub async fn devices(endpoint: &Path, json: bool) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let resp = client.api(ClientMsg::ListDevices).await?;
    let ServerMsg::Devices { devices } = resp else {
        return Err(unexpected(&resp));
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&devices).map_err(|e| CliError::failure(e.to_string()))?
        );
        return Ok(0);
    }
    if devices.is_empty() {
        println!("no paired devices (pair one with `vca pair`)");
        return Ok(0);
    }
    print!("{}", device_table(&devices, vca_core::now_millis()));
    Ok(0)
}

// --- `vca devices rename|scope` ---

/// Permission of a paired device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScopeArg {
    /// Watch sessions and resources only.
    View,
    /// Also send input and control sessions.
    Command,
    /// Everything, including managing devices.
    Admin,
}

impl From<ScopeArg> for DeviceScope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::View => DeviceScope::View,
            ScopeArg::Command => DeviceScope::Command,
            ScopeArg::Admin => DeviceScope::Admin,
        }
    }
}

#[derive(Debug, clap::Subcommand)]
pub enum DevicesCmd {
    /// Rename a paired device (1-64 characters).
    Rename { device_id: String, name: String },
    /// Change what a paired device may do; applies to its open connections
    /// from the next request on.
    Scope {
        device_id: String,
        #[arg(value_enum)]
        scope: ScopeArg,
    },
}

pub async fn manage_device(endpoint: &Path, action: DevicesCmd) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let (resp, done) = match action {
        DevicesCmd::Rename { device_id, name } => {
            let done = format!("renamed {device_id}");
            let msg = ClientMsg::RenameDevice { device_id, name };
            (client.api(msg).await?, done)
        }
        DevicesCmd::Scope { device_id, scope } => {
            let done = format!(
                "{device_id} now has {} permission",
                scope_label(scope.into())
            );
            // vcad only requires the verification to widen a permission.
            let scope: vca_core::DeviceScope = scope.into();
            let (resp, _) = gated_local(&mut client, |id, step_up| LocalRequest::DeviceScope {
                id,
                device_id: device_id.clone(),
                scope,
                step_up,
            })
            .await?;
            match resp {
                LocalResponse::Api { body, .. } => (body, done),
                other => return Err(unexpected_local(&other)),
            }
        }
    };
    if let ServerMsg::Error { message, .. } = &resp {
        return Err(CliError::failure(message.clone()));
    }
    let ServerMsg::Devices { devices } = resp else {
        return Err(unexpected(&resp));
    };
    println!("{done}");
    print!("{}", device_table(&devices, vca_core::now_millis()));
    Ok(0)
}

fn scope_label(scope: DeviceScope) -> &'static str {
    match scope {
        DeviceScope::View => "view",
        DeviceScope::Command => "command",
        DeviceScope::Admin => "admin",
    }
}

/// "just now", "5m ago", "3h ago", "2d ago".
fn ago(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at) / 1000;
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn device_table(devices: &[PairedDevice], now: u64) -> String {
    let mut out = format!(
        "{:<20} {:<24} {:<10} {:<8} {:<10} {}\n",
        "ID", "NAME", "PLATFORM", "SCOPE", "ONLINE", "LAST SEEN"
    );
    for d in devices {
        let scope = d.scope.map(scope_label).unwrap_or("-");
        let online = if d.online {
            format!("yes ({})", d.connections)
        } else {
            "no".into()
        };
        let seen = if d.online {
            "now".into()
        } else {
            ago(d.last_seen, now)
        };
        out.push_str(&format!(
            "{:<20} {:<24} {:<10} {:<8} {:<10} {}\n",
            d.id, d.name, d.platform, scope, online, seen
        ));
    }
    out
}

pub async fn revoke(endpoint: &Path, device_id: &str) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    client
        .api(ClientMsg::RevokeDevice {
            device_id: device_id.to_owned(),
        })
        .await?;
    println!("revoked {device_id}; its connections were closed");
    Ok(0)
}

pub async fn kill_switch(endpoint: &Path, reset: bool) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    if reset {
        gated_local(&mut client, |id, step_up| LocalRequest::KillSwitchReset {
            id,
            step_up,
        })
        .await?;
        println!("kill switch reset; paired devices can connect again");
    } else {
        client.api(ClientMsg::KillSwitch).await?;
        println!("kill switch tripped: all remote connections dropped and refused");
        println!("(undo with `vca kill-switch --reset`)");
    }
    Ok(0)
}

pub async fn status(endpoint: &Path) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let id = client.next_id();
    let resp = client.local(LocalRequest::RemoteStatus { id }).await?;
    let LocalResponse::RemoteStatus {
        enabled,
        listen,
        server_fingerprint,
        totp_enrolled,
        require_totp,
        kill_switch_tripped,
        paired_devices,
        connections,
        provider,
        provider_state,
        provider_diagnostics,
        ..
    } = resp
    else {
        return Err(unexpected_local(&resp));
    };
    if !enabled {
        println!("remote transport: off");
        return Ok(0);
    }
    println!("remote transport: on");
    println!("listen:           {}", listen.unwrap_or_default());
    println!(
        "fingerprint:      {}",
        server_fingerprint.unwrap_or_default()
    );
    println!(
        "totp:             {}{}",
        if totp_enrolled {
            "enrolled"
        } else {
            "not enrolled"
        },
        if require_totp && !totp_enrolled {
            " (required: run `vca totp enroll` before pairing)"
        } else {
            ""
        }
    );
    println!(
        "kill switch:      {}",
        if kill_switch_tripped {
            "TRIPPED"
        } else {
            "off"
        }
    );
    println!("paired devices:   {paired_devices}");
    println!("connections:      {connections}");
    match provider {
        Some(p) => {
            println!(
                "provider:         {p} ({})",
                provider_state.unwrap_or_default()
            );
            for d in provider_diagnostics {
                println!("                  {d}");
            }
        }
        None => println!(
            "provider:         none (pairing codes use remote.endpoints or the listen address)"
        ),
    }
    Ok(0)
}

#[cfg(test)]
mod device_tests {
    use super::*;

    fn device(id: &str, online: bool, scope: Option<DeviceScope>) -> PairedDevice {
        PairedDevice {
            id: id.into(),
            name: "Phone".into(),
            platform: "ios".into(),
            paired_at: 0,
            last_seen: 1_000,
            current: false,
            scope,
            online,
            connections: u32::from(online),
            last_ip: None,
            app_version: None,
        }
    }

    #[test]
    fn table_shows_scope_online_and_last_seen() {
        let now = 1_000 + 3 * 3_600_000;
        let table = device_table(
            &[
                device("dev_a", true, Some(DeviceScope::Admin)),
                device("dev_b", false, None),
            ],
            now,
        );
        let lines: Vec<_> = table.lines().collect();
        assert!(lines[0].contains("SCOPE") && lines[0].contains("LAST SEEN"));
        assert!(lines[1].contains("admin") && lines[1].contains("yes (1)"));
        assert!(lines[1].ends_with("now"));
        assert!(lines[2].contains(" - ") && lines[2].contains("no"));
        assert!(lines[2].ends_with("3h ago"), "{}", lines[2]);
    }

    #[test]
    fn relative_times() {
        assert_eq!(ago(0, 59_000), "just now");
        assert_eq!(ago(0, 5 * 60_000), "5m ago");
        assert_eq!(ago(0, 2 * 86_400_000), "2d ago");
        assert_eq!(ago(10, 0), "just now");
    }

    #[test]
    fn scope_args_map_to_protocol() {
        assert_eq!(DeviceScope::from(ScopeArg::View), DeviceScope::View);
        assert_eq!(DeviceScope::from(ScopeArg::Admin), DeviceScope::Admin);
    }
}
