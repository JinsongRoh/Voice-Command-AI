//! Tauri side of the Voice Command AI apps.
//!
//! The webview (`apps/ui`) never sees keys, tokens or the Noise channel: it
//! exchanges MessagePack envelopes with these commands, and [`vca_client`]
//! owns pairing, authentication, encryption and reconnects. The desktop
//! crate calls [`run`]; the mobile crate extends [`builder`] with its
//! mobile-only plugins.
//!
//! Commands (the contract mirrored by `apps/ui/src/native/bridge.ts`):
//! `vca_machines`, `vca_pair`, `vca_connect`, `vca_send`, `vca_status`,
//! `vca_login`, `vca_disconnect`, `vca_forget`, `vca_machine_rename`,
//! `vca_machine_set_default`, `vca_machine_move`, `vca_mic_start`,
//! `vca_mic_stop`, `vca_mic_peek`, `vca_mic_split`, `vca_mic_cancel`, `vca_mic_diag`, `vca_platform`,
//! `vca_launch_links`, the "Connect DNSPilot" commands `vca_dnspilot_*` (see [`dnspilot`])
//! and the approval alert commands `vca_push_*` (see [`push`]).
//! Phones only: `vca_share_text_file` (the system share sheet, see `share_file`).
//! Desktop only (`apps/ui/src/native/host.ts`): `vca_host_*` for the VCA
//! server on this PC (see [`host`]), whose sessions appear as the machine
//! `"local"` in the machine commands above, and the in-app updates
//! `vca_update_*` (see [`updater`], `apps/ui/src/native/updater.ts`), and
//! `vca_save_text_file` (the "Save as" dialog, see [`save_file`]), and
//! `vca_autostart_*` ("Start VCA when I sign in", see [`autostart`]).

#[cfg(desktop)]
mod autostart;
mod dnspilot;
#[cfg(desktop)]
mod host;
mod ios_audio;
#[cfg(target_os = "ios")]
mod ios_share;
mod mic;
mod noise;
pub mod push;
mod save_file;
mod secrets;
#[cfg(any(not(desktop), test))]
mod share_file;
#[cfg(desktop)]
mod updater;

use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response};
use tauri::{AppHandle, Manager, State};
use vca_client::{ClientError, ClientHub, LinkSink, LinkStatus, MachineInfo};

pub use mic::{Mic, MicOptions};

/// Error returned to the webview as `{ code, message }`.
#[derive(Debug, Serialize)]
pub struct CommandError {
    code: String,
    message: String,
    /// Seconds to wait, when the error is a rate limit that says so.
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_secs: Option<u64>,
}

impl CommandError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            retry_after_secs: None,
        }
    }
}

impl From<ClientError> for CommandError {
    fn from(e: ClientError) -> Self {
        let mut error = Self::new(e.code(), e.to_string());
        if let ClientError::DnsPilotRateLimited { retry_after_secs } = e {
            error.retry_after_secs = retry_after_secs;
        }
        error
    }
}

pub(crate) type CommandResult<T> = Result<T, CommandError>;

/// Forwards one machine's link events to the two webview channels.
struct ChannelSink {
    status: Channel<LinkStatus>,
    message: Channel<InvokeResponseBody>,
}

impl LinkSink for ChannelSink {
    fn status(&self, status: LinkStatus) {
        if let Err(e) = self.status.send(status) {
            tracing::debug!("status channel closed: {e}");
        }
    }

    fn message(&self, msgpack: Vec<u8>) {
        if let Err(e) = self.message.send(InvokeResponseBody::Raw(msgpack)) {
            tracing::debug!("message channel closed: {e}");
        }
    }
}

/// The VCA server on this PC, when `machine_id` names it (desktop).
#[cfg(desktop)]
fn local_host(app: &AppHandle, machine_id: &str) -> Option<Arc<host::Host>> {
    if machine_id != host::LOCAL_MACHINE_ID {
        return None;
    }
    app.try_state::<Arc<host::Host>>()
        .map(|h| Arc::clone(h.inner()))
}

/// Paired machines, plus this PC ("local") on desktop once its server was
/// reachable.
#[cfg_attr(not(desktop), allow(unused_variables))]
#[tauri::command]
async fn vca_machines(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
) -> CommandResult<Vec<MachineInfo>> {
    Ok(machine_list(&app, &hub).await)
}

/// The paired machines plus, on desktop, this PC once its server is seen.
async fn machine_list(
    #[allow(unused_variables)] app: &AppHandle,
    hub: &ClientHub,
) -> Vec<MachineInfo> {
    #[cfg(desktop)]
    if let Some(host) = app.try_state::<Arc<host::Host>>() {
        let host = Arc::clone(host.inner());
        return host::machine_list(hub.machines(), host.local_machine().await);
    }
    hub.machines()
}

#[tauri::command]
async fn vca_pair(
    hub: State<'_, Arc<ClientHub>>,
    uri: String,
    device_name: String,
    totp: Option<String>,
) -> CommandResult<MachineInfo> {
    let name = device_name.trim();
    let name = if name.is_empty() { "VCA app" } else { name };
    Ok(hub.pair(&uri, name, platform(), totp).await?)
}

/// The code to compare with the PC before pairing with `uri`
/// (`PairingPayload::confirm_code`).
#[tauri::command]
fn vca_pairing_code(uri: String) -> CommandResult<String> {
    vca_net::PairingPayload::parse(&uri)
        .map(|p| p.confirm_code())
        .map_err(|_| CommandError::new("invalid_uri", "not a pairing link"))
}

// Async so it runs on Tauri's tokio runtime, which the hub's supervisor
// tasks are spawned on.
#[cfg_attr(not(desktop), allow(unused_variables))]
#[tauri::command]
async fn vca_connect(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
    on_status: Channel<LinkStatus>,
    on_message: Channel<InvokeResponseBody>,
) -> CommandResult<()> {
    let sink = Arc::new(ChannelSink {
        status: on_status,
        message: on_message,
    });
    #[cfg(desktop)]
    if let Some(host) = local_host(&app, &machine_id) {
        host.connect_link(sink);
        return Ok(());
    }
    Ok(hub.connect(&machine_id, sink)?)
}

/// Body: the MessagePack envelope (raw bytes, or a JSON number array where
/// the platform cannot send raw bytes, i.e. Android). Header `vca-machine`.
#[cfg_attr(not(desktop), allow(unused_variables))]
#[tauri::command]
async fn vca_send(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
    request: Request<'_>,
) -> CommandResult<()> {
    let machine = request
        .headers()
        .get("vca-machine")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| CommandError::new("protocol", "missing vca-machine header"))?
        .to_owned();
    let bytes = body_bytes(request.body())?;
    #[cfg(desktop)]
    if let Some(host) = local_host(&app, &machine) {
        return Ok(host.link_send(&bytes).await?);
    }
    Ok(hub.send(&machine, &bytes).await?)
}

fn body_bytes(body: &InvokeBody) -> CommandResult<Vec<u8>> {
    match body {
        InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        InvokeBody::Json(value) => serde_json::from_value::<Vec<u8>>(value.clone())
            .map_err(|_| CommandError::new("protocol", "the body must be bytes")),
    }
}

#[cfg_attr(not(desktop), allow(unused_variables))]
#[tauri::command]
fn vca_status(app: AppHandle, hub: State<'_, Arc<ClientHub>>, machine_id: String) -> LinkStatus {
    #[cfg(desktop)]
    if let Some(host) = local_host(&app, &machine_id) {
        return host.link_status();
    }
    hub.status(&machine_id)
}

#[cfg_attr(not(desktop), allow(unused_variables))]
#[tauri::command]
async fn vca_login(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
    totp: Option<String>,
    backup_code: Option<String>,
) -> CommandResult<()> {
    // The owner-only local channel needs no sign-in.
    #[cfg(desktop)]
    if local_host(&app, &machine_id).is_some() {
        return Ok(());
    }
    Ok(hub.login(&machine_id, totp, backup_code).await?)
}

#[cfg_attr(not(desktop), allow(unused_variables))]
#[tauri::command]
fn vca_disconnect(app: AppHandle, hub: State<'_, Arc<ClientHub>>, machine_id: String) {
    #[cfg(desktop)]
    if let Some(host) = local_host(&app, &machine_id) {
        host.link_disconnect();
        return;
    }
    hub.disconnect(&machine_id);
}

#[cfg_attr(not(desktop), allow(unused_variables))]
#[tauri::command]
async fn vca_forget(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
) -> CommandResult<()> {
    #[cfg(desktop)]
    if local_host(&app, &machine_id).is_some() {
        return Err(CommandError::new(
            "unsupported",
            "this PC is not paired, so it cannot be forgotten; stop its server instead",
        ));
    }
    Ok(hub.forget(&machine_id).await?)
}

// --- Local machine management (alias, default, order). Metadata only; keys
// never leave the hub. Each returns the updated machine list. ---

#[tauri::command]
async fn vca_machine_rename(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
    alias: Option<String>,
) -> CommandResult<Vec<MachineInfo>> {
    hub.set_alias(&machine_id, alias.as_deref())?;
    Ok(machine_list(&app, &hub).await)
}

#[tauri::command]
async fn vca_machine_set_default(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
    machine_id: Option<String>,
) -> CommandResult<Vec<MachineInfo>> {
    hub.set_default(machine_id.as_deref())?;
    Ok(machine_list(&app, &hub).await)
}

#[tauri::command]
async fn vca_machine_move(
    app: AppHandle,
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
    offset: i32,
) -> CommandResult<Vec<MachineInfo>> {
    hub.move_machine(&machine_id, offset)?;
    Ok(machine_list(&app, &hub).await)
}

#[tauri::command]
/// Starts recording. On Android this first asks for the microphone
/// permission (a system dialog on first use) and fails with the code
/// `mic_permission` when the user denies it. `noise_suppression` (JS
/// `noiseSuppression`) turns the RNNoise stage on or off; a caller that does
/// not send it (an older UI) gets it on.
async fn vca_mic_start(
    app: tauri::AppHandle,
    mic: State<'_, Arc<Mic>>,
    noise_suppression: Option<bool>,
) -> CommandResult<()> {
    ensure_mic_permission(&app).await?;
    let options = MicOptions {
        noise_suppression: noise_suppression.unwrap_or(true),
    };
    let mic = mic.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // iOS: a photo picker or media playback may have left the audio
        // session unable to record; put it right before every capture.
        let session = ios_audio::activate();
        let started = mic.start(options);
        mic.set_note(format!(
            "session {session} | ns {} | start {}",
            if options.noise_suppression {
                "on"
            } else {
                "off"
            },
            match &started {
                Ok(()) => "ok".to_owned(),
                Err(e) => format!("failed: {e}"),
            }
        ));
        started
    })
    .await
    .map_err(|e| CommandError::new("mic", e.to_string()))?
    .map_err(|e| CommandError::new("mic", e))
}

#[tauri::command]
async fn vca_mic_stop(mic: State<'_, Arc<Mic>>) -> CommandResult<Response> {
    let mic = mic.inner().clone();
    let wav = tauri::async_runtime::spawn_blocking(move || mic.stop())
        .await
        .map_err(|e| CommandError::new("mic", e.to_string()))?
        .map_err(|e| CommandError::new("mic", e))?;
    Ok(Response::new(wav))
}

#[tauri::command]
async fn vca_mic_peek(mic: State<'_, Arc<Mic>>) -> CommandResult<Response> {
    let mic = mic.inner().clone();
    let wav = tauri::async_runtime::spawn_blocking(move || mic.peek())
        .await
        .map_err(|e| CommandError::new("mic", e.to_string()))?
        .map_err(|e| CommandError::new("mic", e))?;
    Ok(Response::new(wav))
}

/// Hands over the audio before the last pause once `min_seconds` are
/// buffered (empty body otherwise); recording continues.
#[tauri::command]
async fn vca_mic_split(mic: State<'_, Arc<Mic>>, min_seconds: f32) -> CommandResult<Response> {
    let mic = mic.inner().clone();
    let wav = tauri::async_runtime::spawn_blocking(move || mic.split(min_seconds))
        .await
        .map_err(|e| CommandError::new("mic", e.to_string()))?
        .map_err(|e| CommandError::new("mic", e))?;
    Ok(Response::new(wav))
}

/// A JSON snapshot of the latest capture and audio session for the debug log
/// (counters and levels only, never audio).
#[tauri::command]
fn vca_mic_diag(mic: State<'_, Arc<Mic>>) -> String {
    let mut value: serde_json::Value =
        serde_json::from_str(&mic.diagnostics()).unwrap_or(serde_json::Value::Null);
    value["session"] = ios_audio::describe().into();
    value.to_string()
}

#[tauri::command]
async fn vca_mic_cancel(mic: State<'_, Arc<Mic>>) -> CommandResult<()> {
    let mic = mic.inner().clone();
    tauri::async_runtime::spawn_blocking(move || mic.cancel())
        .await
        .map_err(|e| CommandError::new("mic", e.to_string()))
}

#[tauri::command]
fn vca_platform() -> &'static str {
    platform()
}

/// Phones never change an agent's settings: exact approval alerts (agent
/// hooks) are turned on in the PC app (desktop: `host::hooks`).
#[cfg(not(desktop))]
mod hooks_refused {
    use crate::{CommandError, CommandResult};

    fn refuse() -> CommandError {
        CommandError::new(
            "forbidden",
            "agent hooks can only be installed in the PC app's settings",
        )
    }

    #[tauri::command]
    pub fn vca_host_hooks_status() -> CommandResult<()> {
        Err(refuse())
    }

    #[tauri::command]
    pub fn vca_host_hooks_install(agent: String) -> CommandResult<()> {
        let _ = agent;
        Err(refuse())
    }

    #[tauri::command]
    pub fn vca_host_hooks_uninstall(agent: String) -> CommandResult<()> {
        let _ = agent;
        Err(refuse())
    }
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "android" => "android",
        "ios" => "ios",
        _ => "linux",
    }
}

/// Launch links captured natively before the webview existed (see
/// [`record_launch_link`]).
static LAUNCH_LINKS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Records a URL that launched the app, for platforms where the deep-link
/// plugin misses it. On iOS with the scene lifecycle, a cold start delivers
/// the URL only in the scene's connection options, which tao does not turn
/// into an `Opened` event, so the plugin's `getCurrent()` stays empty; the
/// mobile crate reads the options itself and hands the URL over here.
pub fn record_launch_link(url: String) {
    let mut links = LAUNCH_LINKS.lock().unwrap_or_else(|e| e.into_inner());
    if !links.contains(&url) {
        links.push(url);
    }
}

/// Returns and clears the recorded launch links. The UI only pre-fills the
/// pairing form with them; it never pairs on its own.
#[tauri::command]
fn vca_launch_links() -> Vec<String> {
    std::mem::take(&mut *LAUNCH_LINKS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The app builder shared by the desktop and mobile apps: the deep-link
/// plugin, app state and all `vca_*` commands. The mobile crate adds its
/// mobile-only plugins (barcode scanner) before building.
#[cfg(target_os = "android")]
async fn ensure_mic_permission(app: &tauri::AppHandle) -> CommandResult<()> {
    match tauri_plugin_vca_mic::ensure_microphone(app).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(CommandError::new(
            "mic_permission",
            "microphone access was denied; allow it in the system settings",
        )),
        Err(e) => Err(CommandError::new("mic_permission", e)),
    }
}

/// Desktop and iOS ask for microphone access when the stream opens.
#[cfg(not(target_os = "android"))]
async fn ensure_mic_permission(_app: &tauri::AppHandle) -> CommandResult<()> {
    Ok(())
}

pub fn builder() -> tauri::Builder<tauri::Wry> {
    let builder = tauri::Builder::default()
        // `vca://pair?...` links (the pairing QR code read by the phone's
        // camera) open the app; the UI only pre-fills the pairing form, it
        // never pairs without the user's tap.
        .plugin(tauri_plugin_deep_link::init())
        // Opens the DNSPilot approval page in the system browser (called from
        // Rust only, for URLs on the DNSPilot console).
        .plugin(tauri_plugin_opener::init())
        // OS notifications for approval alerts (desktop and phones; on
        // Android 13+ the plugin's manifest adds POST_NOTIFICATIONS).
        .plugin(tauri_plugin_notification::init());
    // In-app updates from GitHub Releases (desktop only; the phone apps are
    // updated by their stores). Only `vca_update_*` use it: the webview has
    // no updater permission.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    // The "Save as" dialog of `vca_save_text_file` (desktop only). Opened
    // from Rust: the webview has no dialog permission.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_dialog::init());
    // RECORD_AUDIO runtime permission (see `ensure_mic_permission`).
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_vca_mic::init());
    // The ACTION_SEND chooser of `vca_share_text_file` (called from Rust only).
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_vca_share::init());
    builder
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let secrets = secrets::open(&data_dir)?;
            let hub = Arc::new(ClientHub::open(
                data_dir.join("machines"),
                Arc::clone(&secrets),
            )?);
            app.manage(Arc::clone(&hub));
            app.manage(Arc::new(Mic::default()));
            app.manage(Arc::new(dnspilot::PendingPoll::default()));
            app.manage(push::PhoneKeys(Arc::clone(&secrets)));
            #[cfg(desktop)]
            {
                let host = Arc::new(host::Host::new(data_dir.join("host")));
                host.start_background(app.handle().clone());
                let alerts = push::open(&data_dir, secrets);
                push::start_watcher(Arc::clone(&alerts), Arc::clone(&host), hub);
                app.manage(alerts);
                app.manage(push::PendingPushPoll::default());
                app.manage(host);
                app.manage(updater::UpdaterState::default());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vca_machines,
            vca_pair,
            vca_pairing_code,
            vca_connect,
            vca_send,
            vca_status,
            vca_login,
            vca_disconnect,
            vca_forget,
            vca_machine_rename,
            vca_machine_set_default,
            vca_machine_move,
            vca_mic_start,
            vca_mic_stop,
            vca_mic_peek,
            vca_mic_split,
            vca_mic_cancel,
            vca_mic_diag,
            vca_platform,
            dnspilot::vca_dnspilot_status,
            dnspilot::vca_dnspilot_identity,
            dnspilot::vca_dnspilot_enroll,
            dnspilot::vca_dnspilot_app_clients,
            dnspilot::vca_dnspilot_revoke_app_client,
            dnspilot::vca_dnspilot_invites,
            dnspilot::vca_dnspilot_revoke_invite,
            dnspilot::vca_dnspilot_authorize_start,
            dnspilot::vca_dnspilot_authorize_wait,
            dnspilot::vca_dnspilot_authorize_cancel,
            dnspilot::vca_dnspilot_open_activation,
            dnspilot::vca_dnspilot_open_console,
            dnspilot::vca_dnspilot_create_tunnel,
            dnspilot::vca_dnspilot_add_tunnel,
            dnspilot::vca_dnspilot_set_tunnel_id,
            dnspilot::vca_dnspilot_register,
            dnspilot::vca_dnspilot_mark_registered,
            dnspilot::vca_dnspilot_remove_tunnel,
            dnspilot::vca_dnspilot_issue_app_token,
            dnspilot::vca_dnspilot_disconnect,
            dnspilot::vca_dnspilot_devices,
            dnspilot::vca_dnspilot_zones,
            dnspilot::vca_dnspilot_remote_tunnels,
            dnspilot::vca_dnspilot_use_tunnel,
            dnspilot::vca_dnspilot_refresh,
            dnspilot::vca_dnspilot_local_device,
            vca_launch_links,
            push::vca_push_take_opened,
            push::vca_push_phone_enable,
            push::vca_push_phone_disable,
            #[cfg(desktop)]
            push::vca_push_status,
            #[cfg(desktop)]
            push::vca_push_set_enabled,
            #[cfg(desktop)]
            push::vca_push_set_muted,
            #[cfg(desktop)]
            push::vca_push_authorize_start,
            #[cfg(desktop)]
            push::vca_push_authorize_wait,
            #[cfg(desktop)]
            push::vca_push_authorize_cancel,
            #[cfg(desktop)]
            push::vca_push_open_activation,
            #[cfg(desktop)]
            push::vca_push_disconnect,
            #[cfg(desktop)]
            host::vca_host_status,
            #[cfg(desktop)]
            host::vca_host_pair,
            #[cfg(desktop)]
            host::vca_host_start,
            #[cfg(desktop)]
            host::vca_host_stop,
            #[cfg(desktop)]
            host::vca_host_set_run_server,
            #[cfg(desktop)]
            host::vca_host_totp_begin,
            #[cfg(desktop)]
            host::vca_host_totp_confirm,
            #[cfg(desktop)]
            host::vca_host_totp_backup_codes,
            #[cfg(desktop)]
            host::vca_host_restore_status,
            #[cfg(desktop)]
            host::vca_host_restore_shortcut,
            #[cfg(desktop)]
            host::vca_host_restore_sign_in,
            #[cfg(desktop)]
            host::engines::vca_host_engines,
            #[cfg(desktop)]
            host::engines::vca_host_engine_save,
            #[cfg(desktop)]
            host::engines::vca_host_engine_remove,
            #[cfg(desktop)]
            host::engines::vca_host_engine_set_default,
            #[cfg(desktop)]
            host::engines::vca_host_engine_test,
            #[cfg(desktop)]
            host::security::vca_host_security_status,
            #[cfg(desktop)]
            host::security::vca_host_security_step_up,
            #[cfg(desktop)]
            host::security::vca_host_security_step_up_test,
            #[cfg(desktop)]
            host::security::vca_host_security_app_unlock,
            #[cfg(desktop)]
            host::security::vca_host_security_step_up_end,
            #[cfg(desktop)]
            host::security::vca_host_security_totp_begin,
            #[cfg(desktop)]
            host::security::vca_host_security_totp_confirm,
            #[cfg(desktop)]
            host::security::vca_host_security_backup_codes,
            #[cfg(desktop)]
            host::security::vca_host_security_totp_disable,
            #[cfg(desktop)]
            host::security::vca_host_security_settings,
            #[cfg(desktop)]
            host::security::vca_host_security_wizard,
            #[cfg(desktop)]
            host::security::vca_host_mail_get,
            #[cfg(desktop)]
            host::security::vca_host_mail_set,
            #[cfg(desktop)]
            host::security::vca_host_mail_secret,
            #[cfg(desktop)]
            host::security::vca_host_mail_test,
            #[cfg(desktop)]
            host::security::vca_host_recovery_start,
            #[cfg(desktop)]
            host::security::vca_host_recovery_finish,
            #[cfg(desktop)]
            save_file::vca_save_text_file,
            #[cfg(not(desktop))]
            share_file::vca_share_text_file,
            #[cfg(desktop)]
            updater::vca_update_check,
            #[cfg(desktop)]
            updater::vca_update_download,
            #[cfg(desktop)]
            updater::vca_update_preflight,
            #[cfg(desktop)]
            updater::vca_update_install,
            #[cfg(desktop)]
            autostart::vca_autostart_status,
            #[cfg(desktop)]
            autostart::vca_autostart_set,
            #[cfg(desktop)]
            host::hooks::vca_host_hooks_status,
            #[cfg(desktop)]
            host::hooks::vca_host_hooks_install,
            #[cfg(desktop)]
            host::hooks::vca_host_hooks_uninstall,
            #[cfg(not(desktop))]
            hooks_refused::vca_host_hooks_status,
            #[cfg(not(desktop))]
            hooks_refused::vca_host_hooks_install,
            #[cfg(not(desktop))]
            hooks_refused::vca_host_hooks_uninstall,
        ])
}

/// Builds and runs the app with the shared [`builder`]. Called by the
/// desktop `main` with its own `tauri::generate_context!()`.
pub fn run(context: tauri::Context<tauri::Wry>) {
    // The desktop and sign-in shortcuts: reopen the saved AI CLI windows
    // through the local server, without a window, then exit.
    #[cfg(desktop)]
    if let Some(delay) = host::restore_flag(std::env::args().skip(1)) {
        let code = host::run_restore(&context.config().identifier, delay);
        std::process::exit(code);
    }
    // Started by "Start VCA when I sign in": a normal launch (window and
    // server), only noted.
    #[cfg(desktop)]
    autostart::note_launch(std::env::args().skip(1));
    builder()
        .run(context)
        .expect("error while running the Voice Command AI app");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_links_are_deduplicated_and_taken_once() {
        record_launch_link("vca://pair?v=1&k=a".into());
        record_launch_link("vca://pair?v=1&k=a".into());
        record_launch_link("vca://pair?v=1&k=b".into());
        assert_eq!(
            vca_launch_links(),
            vec![
                "vca://pair?v=1&k=a".to_owned(),
                "vca://pair?v=1&k=b".to_owned()
            ]
        );
        assert!(vca_launch_links().is_empty());
    }
}
