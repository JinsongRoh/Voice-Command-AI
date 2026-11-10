//! "Connect DNSPilot" commands. The webview only sees public data (the
//! approval code, tunnel hostnames, the client key fingerprint); the client
//! private key, the DNSPilot OAuth tokens and app tokens stay in the native
//! key store behind [`vca_client::dnspilot::DnsPilotManager`].

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use vca_client::ClientHub;
use vca_client::dnspilot::{
    AuthorizationPrompt, CreateTunnelRequest, DnsPilotAppClient, DnsPilotDevice,
    DnsPilotEnrollment, DnsPilotIdentityInfo, DnsPilotInvite, DnsPilotRemoteTunnel, DnsPilotStatus,
    DnsPilotTunnel, DnsPilotZone, Registration,
};

/// Default lifetime of an (advanced, fallback) app token: short on purpose.
const APP_TOKEN_DEFAULT_DAYS: u16 = 7;

use crate::{CommandError, CommandResult};

/// The running authorization poll, so it can be cancelled.
#[derive(Default)]
pub struct PendingPoll(Mutex<Option<tauri::async_runtime::JoinHandle<()>>>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
pub fn vca_dnspilot_status(hub: State<'_, Arc<ClientHub>>) -> CommandResult<DnsPilotStatus> {
    Ok(hub.dnspilot().status()?)
}

/// Creates the app's client key on first use and returns its public parts.
#[tauri::command]
pub fn vca_dnspilot_identity(
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<DnsPilotIdentityInfo> {
    Ok(hub.dnspilot().identity()?)
}

#[tauri::command]
pub async fn vca_dnspilot_authorize_start(
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<AuthorizationPrompt> {
    Ok(hub.dnspilot().start_authorization().await?)
}

/// Waits until the person approves (or denies) in DNSPilot.
#[tauri::command]
pub async fn vca_dnspilot_authorize_wait(
    hub: State<'_, Arc<ClientHub>>,
    pending: State<'_, Arc<PendingPoll>>,
) -> CommandResult<DnsPilotStatus> {
    let hub = Arc::clone(hub.inner());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tauri::async_runtime::spawn(async move {
        let _ = tx.send(hub.dnspilot().wait_for_authorization().await);
    });
    if let Some(previous) = lock(&pending.0).replace(task) {
        previous.abort();
    }
    match rx.await {
        Ok(result) => Ok(result?),
        Err(_) => Err(CommandError::new(
            "dnspilot_cancelled",
            "the DNSPilot connection was cancelled",
        )),
    }
}

#[tauri::command]
pub fn vca_dnspilot_authorize_cancel(
    hub: State<'_, Arc<ClientHub>>,
    pending: State<'_, Arc<PendingPoll>>,
) {
    if let Some(task) = lock(&pending.0).take() {
        task.abort();
    }
    hub.dnspilot().cancel_authorization();
}

/// Opens the activation page of the pending authorization in the system
/// browser. Only URLs on the DNSPilot console are opened.
#[tauri::command]
pub fn vca_dnspilot_open_activation(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<()> {
    let url = hub.dnspilot().activation_url().ok_or_else(|| {
        CommandError::new("dnspilot_expired", "no DNSPilot approval is in progress")
    })?;
    open(&app, &url)
}

/// Opens the DNSPilot console (for steps only the person can do there).
#[tauri::command]
pub fn vca_dnspilot_open_console(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<()> {
    let console = hub.dnspilot().status()?.console_url;
    open(&app, &console)
}

fn open(app: &AppHandle, url: &str) -> CommandResult<()> {
    if !url.starts_with("https://") {
        return Err(CommandError::new(
            "dnspilot_invalid",
            "refusing to open a non-https URL",
        ));
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| CommandError::new("open_failed", e.to_string()))
}

/// Creates a tunnel on a PC. Phones never do: a tunnel belongs to the PC it
/// serves, so a phone only uses existing tunnels and invite codes.
#[tauri::command]
pub async fn vca_dnspilot_create_tunnel(
    hub: State<'_, Arc<ClientHub>>,
    request: CreateTunnelRequest,
) -> CommandResult<DnsPilotTunnel> {
    if !cfg!(desktop) {
        return Err(CommandError::new(
            "dnspilot_phone_no_create",
            "tunnels are created from the app on the PC",
        ));
    }
    Ok(hub.dnspilot().create_tunnel(&request).await?)
}

/// Registers this app's key with the invite in `link`: the console's QR link
/// or a `vca://pair` link. `null` when a pairing link has no invite.
#[tauri::command]
pub async fn vca_dnspilot_enroll(
    hub: State<'_, Arc<ClientHub>>,
    link: String,
) -> CommandResult<Option<DnsPilotEnrollment>> {
    Ok(hub.dnspilot().enroll_link(&link).await?)
}

/// The active app clients of a tunnel.
#[tauri::command]
pub async fn vca_dnspilot_app_clients(
    hub: State<'_, Arc<ClientHub>>,
    tunnel_id: i64,
) -> CommandResult<Vec<DnsPilotAppClient>> {
    Ok(hub.dnspilot().app_clients(tunnel_id).await?)
}

#[tauri::command]
pub async fn vca_dnspilot_revoke_app_client(
    hub: State<'_, Arc<ClientHub>>,
    tunnel_id: i64,
    client_id: i64,
) -> CommandResult<()> {
    Ok(hub
        .dnspilot()
        .revoke_app_client(tunnel_id, client_id)
        .await?)
}

/// The invites of a tunnel (never the codes).
#[tauri::command]
pub async fn vca_dnspilot_invites(
    hub: State<'_, Arc<ClientHub>>,
    tunnel_id: i64,
) -> CommandResult<Vec<DnsPilotInvite>> {
    Ok(hub.dnspilot().invites(tunnel_id).await?)
}

#[tauri::command]
pub async fn vca_dnspilot_revoke_invite(
    hub: State<'_, Arc<ClientHub>>,
    tunnel_id: i64,
    invite_id: i64,
) -> CommandResult<()> {
    Ok(hub.dnspilot().revoke_invite(tunnel_id, invite_id).await?)
}

#[tauri::command]
pub fn vca_dnspilot_add_tunnel(
    hub: State<'_, Arc<ClientHub>>,
    hostname: String,
    public_port: Option<u16>,
    tunnel_id: Option<i64>,
) -> CommandResult<DnsPilotTunnel> {
    Ok(hub
        .dnspilot()
        .add_tunnel(&hostname, public_port.unwrap_or(443), tunnel_id)?)
}

#[tauri::command]
pub fn vca_dnspilot_set_tunnel_id(
    hub: State<'_, Arc<ClientHub>>,
    hostname: String,
    tunnel_id: i64,
) -> CommandResult<DnsPilotTunnel> {
    Ok(hub.dnspilot().set_tunnel_id(&hostname, tunnel_id)?)
}

#[tauri::command]
pub async fn vca_dnspilot_register(
    hub: State<'_, Arc<ClientHub>>,
    hostname: String,
) -> CommandResult<Registration> {
    Ok(hub.dnspilot().register_client(&hostname).await?)
}

#[tauri::command]
pub fn vca_dnspilot_mark_registered(
    hub: State<'_, Arc<ClientHub>>,
    hostname: String,
) -> CommandResult<DnsPilotTunnel> {
    Ok(hub.dnspilot().mark_registered(&hostname)?)
}

#[tauri::command]
pub fn vca_dnspilot_remove_tunnel(
    hub: State<'_, Arc<ClientHub>>,
    hostname: String,
) -> CommandResult<()> {
    Ok(hub.dnspilot().remove_tunnel(&hostname)?)
}

#[tauri::command]
pub async fn vca_dnspilot_issue_app_token(
    hub: State<'_, Arc<ClientHub>>,
    hostname: String,
    expires_days: Option<u16>,
) -> CommandResult<DnsPilotTunnel> {
    Ok(hub
        .dnspilot()
        .issue_app_token(&hostname, expires_days.unwrap_or(APP_TOKEN_DEFAULT_DAYS))
        .await?)
}

#[tauri::command]
pub async fn vca_dnspilot_disconnect(hub: State<'_, Arc<ClientHub>>) -> CommandResult<()> {
    Ok(hub.dnspilot().disconnect().await?)
}

/// Active Agent devices for the device picker.
#[tauri::command]
pub async fn vca_dnspilot_devices(
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<Vec<DnsPilotDevice>> {
    Ok(hub.dnspilot().devices().await?)
}

/// Zones, or `null` while DNSPilot does not offer the zones call.
#[tauri::command]
pub async fn vca_dnspilot_zones(
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<Option<Vec<DnsPilotZone>>> {
    Ok(hub.dnspilot().zones().await?)
}

/// Existing app-mode tunnels of the organization.
#[tauri::command]
pub async fn vca_dnspilot_remote_tunnels(
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<Vec<DnsPilotRemoteTunnel>> {
    Ok(hub.dnspilot().remote_tunnels().await?)
}

/// Reuses an existing app-mode tunnel and registers this app's key on it.
#[tauri::command]
pub async fn vca_dnspilot_use_tunnel(
    hub: State<'_, Arc<ClientHub>>,
    tunnel_id: i64,
) -> CommandResult<Registration> {
    Ok(hub.dnspilot().use_tunnel(tunnel_id).await?)
}

/// Refreshes the console status of the known tunnels.
#[tauri::command]
pub async fn vca_dnspilot_refresh(hub: State<'_, Arc<ClientHub>>) -> CommandResult<DnsPilotStatus> {
    Ok(hub.dnspilot().refresh().await?)
}

/// The DNSPilot device id of the Agent on this computer, if any.
#[tauri::command]
pub async fn vca_dnspilot_local_device(
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<Option<i64>> {
    Ok(hub.dnspilot().local_device_id().await)
}
