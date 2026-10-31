//! Owner-only administration of the remote transport: pairing codes, TOTP
//! enrollment, paired devices and the kill switch.

use std::io::{BufRead, Write};
use std::path::Path;

use vca_core::{ClientMsg, ServerMsg};
use vca_server::control::{LocalRequest, LocalResponse};

use crate::client::{Client, unexpected};
use crate::error::{CliError, CliResult};

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
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::Pair {
            id,
            scope: scope.to_owned(),
            endpoints,
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
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::TotpBegin {
            id,
            account: account.to_owned(),
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
    let id = client.next_id();
    let resp = client.local(LocalRequest::TotpConfirm { id, code }).await?;
    let LocalResponse::TotpEnrolled { backup_codes, .. } = resp else {
        return Err(unexpected_local(&resp));
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
    println!("{:<20} {:<24} {:<10}", "ID", "NAME", "PLATFORM");
    for d in devices {
        println!("{:<20} {:<24} {:<10}", d.id, d.name, d.platform);
    }
    Ok(0)
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
        let id = client.next_id();
        client.local(LocalRequest::KillSwitchReset { id }).await?;
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
