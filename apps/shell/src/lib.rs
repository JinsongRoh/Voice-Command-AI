//! Tauri side of the Voice Command AI apps.
//!
//! The webview (`apps/ui`) never sees keys, tokens or the Noise channel: it
//! exchanges MessagePack envelopes with these commands, and [`vca_client`]
//! owns pairing, authentication, encryption and reconnects. The desktop and
//! mobile crates only call [`run`].
//!
//! Commands (the contract mirrored by `apps/ui/src/native/bridge.ts`):
//! `vca_machines`, `vca_pair`, `vca_connect`, `vca_send`, `vca_status`,
//! `vca_login`, `vca_disconnect`, `vca_forget`, `vca_mic_start`,
//! `vca_mic_stop`, `vca_mic_peek`, `vca_mic_split`, `vca_mic_cancel` and `vca_platform`.

mod mic;
mod secrets;

use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response};
use tauri::{Manager, State};
use vca_client::{ClientError, ClientHub, LinkSink, LinkStatus, MachineInfo};

pub use mic::Mic;

/// Error returned to the webview as `{ code, message }`.
#[derive(Debug, Serialize)]
pub struct CommandError {
    code: String,
    message: String,
}

impl CommandError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl From<ClientError> for CommandError {
    fn from(e: ClientError) -> Self {
        Self::new(e.code(), e.to_string())
    }
}

type CommandResult<T> = Result<T, CommandError>;

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

#[tauri::command]
fn vca_machines(hub: State<'_, Arc<ClientHub>>) -> Vec<MachineInfo> {
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

// Async so it runs on Tauri's tokio runtime, which the hub's supervisor
// tasks are spawned on.
#[tauri::command]
async fn vca_connect(
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
    on_status: Channel<LinkStatus>,
    on_message: Channel<InvokeResponseBody>,
) -> CommandResult<()> {
    let sink = Arc::new(ChannelSink {
        status: on_status,
        message: on_message,
    });
    Ok(hub.connect(&machine_id, sink)?)
}

/// Body: the MessagePack envelope (raw bytes, or a JSON number array where
/// the platform cannot send raw bytes, i.e. Android). Header `vca-machine`.
#[tauri::command]
async fn vca_send(hub: State<'_, Arc<ClientHub>>, request: Request<'_>) -> CommandResult<()> {
    let machine = request
        .headers()
        .get("vca-machine")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| CommandError::new("protocol", "missing vca-machine header"))?
        .to_owned();
    let bytes = body_bytes(request.body())?;
    Ok(hub.send(&machine, &bytes).await?)
}

fn body_bytes(body: &InvokeBody) -> CommandResult<Vec<u8>> {
    match body {
        InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        InvokeBody::Json(value) => serde_json::from_value::<Vec<u8>>(value.clone())
            .map_err(|_| CommandError::new("protocol", "the body must be bytes")),
    }
}

#[tauri::command]
fn vca_status(hub: State<'_, Arc<ClientHub>>, machine_id: String) -> LinkStatus {
    hub.status(&machine_id)
}

#[tauri::command]
async fn vca_login(
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
    totp: Option<String>,
    backup_code: Option<String>,
) -> CommandResult<()> {
    Ok(hub.login(&machine_id, totp, backup_code).await?)
}

#[tauri::command]
fn vca_disconnect(hub: State<'_, Arc<ClientHub>>, machine_id: String) {
    hub.disconnect(&machine_id);
}

#[tauri::command]
async fn vca_forget(hub: State<'_, Arc<ClientHub>>, machine_id: String) -> CommandResult<()> {
    Ok(hub.forget(&machine_id).await?)
}

#[tauri::command]
async fn vca_mic_start(mic: State<'_, Arc<Mic>>) -> CommandResult<()> {
    let mic = mic.inner().clone();
    tauri::async_runtime::spawn_blocking(move || mic.start())
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

fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "android" => "android",
        "ios" => "ios",
        _ => "linux",
    }
}

/// Builds and runs the app. Called by the desktop `main` and the mobile
/// entry point with their own `tauri::generate_context!()`.
pub fn run(context: tauri::Context<tauri::Wry>) {
    tauri::Builder::default()
        // `vca://pair?...` links (the pairing QR code read by the phone's
        // camera) open the app; the UI only pre-fills the pairing form, it
        // never pairs without the user's tap.
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let secrets = secrets::open(&data_dir)?;
            let hub = ClientHub::open(data_dir.join("machines"), secrets)?;
            app.manage(Arc::new(hub));
            app.manage(Arc::new(Mic::default()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vca_machines,
            vca_pair,
            vca_connect,
            vca_send,
            vca_status,
            vca_login,
            vca_disconnect,
            vca_forget,
            vca_mic_start,
            vca_mic_stop,
            vca_mic_peek,
            vca_mic_split,
            vca_mic_cancel,
            vca_platform,
        ])
        .run(context)
        .expect("error while running the Voice Command AI app");
}
