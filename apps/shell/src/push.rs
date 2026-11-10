//! Approval alerts through DNSPilot's encrypted push relay (the design is in
//! [`vca_client::push`]).
//!
//! Desktop: the owner's opt-in, the separate DNSPilot approval, per-phone
//! mute, and the watcher that sends. Commands `vca_push_status`,
//! `vca_push_set_enabled`, `vca_push_set_muted`, `vca_push_authorize_start`,
//! `vca_push_authorize_wait`, `vca_push_authorize_cancel`,
//! `vca_push_open_activation` and `vca_push_disconnect`.
//!
//! Phone: `vca_push_phone_enable` and `vca_push_phone_disable` hand the device
//! token to vcad over the Noise channel; the webview never sees the token.
//! `vca_push_take_opened` says which session a tapped notification points at.
//! A tap only opens a view: nothing is approved from a notification.
//!
//! The mobile crate supplies the platform code through [`install_native`]; a
//! build without it reports `push_unavailable`.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio::sync::oneshot;
pub use vca_client::push::PushPlatform;
use vca_client::push::{ciphertext_from_notification, open_sealed};
use vca_client::{ClientError, ClientHub};
use vca_net::KeyStore;

use crate::{CommandError, CommandResult};

/// How long the OS may take to hand out a device token.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
/// Request id of the phone's own envelopes. The webview counts up from 1, so
/// the answers cannot match a request of its own.
const REQUEST_ID: u64 = 1 << 52;

/// This phone's key store, for the push key.
pub struct PhoneKeys(pub Arc<dyn KeyStore>);

/// Platform code of the mobile app.
pub trait NativePush: Send + Sync {
    fn platform(&self) -> PushPlatform;
    /// Asks the OS for permission and a token; the answer comes through
    /// [`record_native_token`] or [`record_native_failure`].
    fn request_token(&self);
    /// For platforms that are polled instead of calling [`record_opened`].
    fn poll_opened(&self) -> Option<OpenedPush> {
        None
    }
}

/// A notification the person tapped.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenedPush {
    /// The payload, when the platform's delivery format is known.
    pub ciphertext: Option<String>,
}

static NATIVE: OnceLock<Box<dyn NativePush>> = OnceLock::new();
type TokenResult = Result<String, String>;
static TOKEN_WAITER: Mutex<Option<oneshot::Sender<TokenResult>>> = Mutex::new(None);
static OPENED: Mutex<Option<OpenedPush>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called once by the mobile crate.
pub fn install_native(native: Box<dyn NativePush>) {
    let _ = NATIVE.set(native);
}

pub fn record_native_token(token: String) {
    if let Some(waiter) = lock(&TOKEN_WAITER).take() {
        let _ = waiter.send(Ok(token));
    }
}

/// `code`: a stable code such as `push_permission`.
pub fn record_native_failure(code: &str) {
    if let Some(waiter) = lock(&TOKEN_WAITER).take() {
        let _ = waiter.send(Err(code.to_owned()));
    }
}

pub fn record_opened(ciphertext: Option<String>) {
    *lock(&OPENED) = Some(OpenedPush { ciphertext });
}

/// A tapped notification's payload as JSON (iOS `userInfo`, Android FCM
/// `data`). The ciphertext key is pending DNSPilot's documentation (see
/// [`vca_client::push::PUSH_CIPHERTEXT_KEY_PENDING_DOCS`]); without it the tap
/// still opens the app.
pub fn record_opened_payload(payload: &serde_json::Value) {
    record_opened(ciphertext_from_notification(payload));
}

/// Where a tapped notification leads. Without ids the app shows the newest
/// pending decision: it always learns what is pending over the Noise channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedTarget {
    pub machine_id: Option<String>,
    pub session_id: Option<String>,
}

fn target(secrets: &dyn KeyStore, opened: &OpenedPush) -> OpenedTarget {
    let payload = opened
        .ciphertext
        .as_deref()
        .and_then(|c| open_sealed(secrets, c));
    OpenedTarget {
        machine_id: payload.as_ref().map(|p| p.m.clone()),
        session_id: payload.map(|p| p.s),
    }
}

/// Returns and clears the tapped notification, if any. The UI only navigates
/// with this.
#[tauri::command]
pub fn vca_push_take_opened(keys: State<'_, PhoneKeys>) -> Option<OpenedTarget> {
    let polled = NATIVE.get().and_then(|n| n.poll_opened());
    let opened = polled.or_else(|| lock(&OPENED).take())?;
    Some(target(keys.0.as_ref(), &opened))
}

fn unavailable() -> CommandError {
    CommandError::new(
        "push_unavailable",
        "this build has no notification support yet",
    )
}

/// Why the phone's message did not reach vcad.
fn send_error(e: ClientError) -> CommandError {
    match e {
        // `hub.send` refuses a message type this build does not know.
        ClientError::Protocol(_) => CommandError::new(
            "push_unsupported",
            "the VCA server does not take phone registrations yet",
        ),
        other => other.into(),
    }
}

async fn native_token(native: &dyn NativePush) -> CommandResult<String> {
    let (tx, rx) = oneshot::channel();
    *lock(&TOKEN_WAITER) = Some(tx);
    native.request_token();
    match tokio::time::timeout(TOKEN_TIMEOUT, rx).await {
        Ok(Ok(Ok(token))) => Ok(token),
        Ok(Ok(Err(code))) => Err(CommandError::new(
            &code,
            "notifications are not available on this phone",
        )),
        _ => Err(CommandError::new(
            "push_token_timeout",
            "the phone gave no notification token",
        )),
    }
}

/// Asks for notification permission and registers this phone with the PC of
/// `machine_id`. Call it again on launch: device tokens change.
#[tauri::command]
pub async fn vca_push_phone_enable(
    hub: State<'_, Arc<ClientHub>>,
    keys: State<'_, PhoneKeys>,
    machine_id: String,
    locale: Option<String>,
) -> CommandResult<()> {
    let native = NATIVE.get().ok_or_else(unavailable)?;
    let token = native_token(native.as_ref()).await?;
    let public = vca_client::push::phone_public_key(keys.0.as_ref())?;
    let envelope = vca_client::push::register_envelope(
        REQUEST_ID,
        &machine_id,
        native.platform(),
        &token,
        &public,
        locale.as_deref(),
    )?;
    hub.send(&machine_id, &envelope).await.map_err(send_error)
}

/// Stops alerts for this phone on the PC of `machine_id`.
#[tauri::command]
pub async fn vca_push_phone_disable(
    hub: State<'_, Arc<ClientHub>>,
    machine_id: String,
) -> CommandResult<()> {
    let envelope = vca_client::push::unregister_envelope(REQUEST_ID)?;
    hub.send(&machine_id, &envelope).await.map_err(send_error)
}

#[cfg(desktop)]
pub use desktop::*;

#[cfg(desktop)]
mod desktop {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tauri::{AppHandle, State};
    use tauri_plugin_opener::OpenerExt;
    use vca_client::ClientHub;
    use vca_client::push::{
        PushAuthorizationPrompt, PushConfig, PushManager, PushStatus, phones_from_body,
    };
    use vca_net::KeyStore;

    use super::lock;
    use crate::host::Host;
    use crate::{CommandError, CommandResult};

    /// How often vcad is asked for pending decisions while alerts are on.
    const WATCH_INTERVAL: Duration = Duration::from_secs(3);

    /// The running approval poll, so it can be cancelled.
    #[derive(Default)]
    pub struct PendingPushPoll(Mutex<Option<tauri::async_runtime::JoinHandle<()>>>);

    pub fn open(data_dir: &Path, secrets: Arc<dyn KeyStore>) -> Arc<PushManager> {
        Arc::new(PushManager::open(
            data_dir.join("push"),
            secrets,
            PushConfig::default(),
        ))
    }

    /// The sender: only this app holds the push grant. While it runs and
    /// alerts are on, asks vcad what is pending and which phones it knows.
    pub fn start_watcher(push: Arc<PushManager>, host: Arc<Host>, hub: Arc<ClientHub>) {
        tauri::async_runtime::spawn(async move {
            let mut agent_device = None;
            loop {
                tokio::time::sleep(WATCH_INTERVAL).await;
                if !push.enabled() {
                    continue;
                }
                let Some((sessions, phones)) = host.push_snapshot().await else {
                    continue;
                };
                if agent_device.is_none() {
                    agent_device = hub.dnspilot().local_device_id().await;
                }
                let phones = phones.as_ref().map(phones_from_body);
                if let Err(e) = push.tick(&sessions, phones.as_deref(), agent_device).await {
                    tracing::info!("approval alerts: {e}");
                }
            }
        });
    }

    #[tauri::command]
    pub fn vca_push_status(push: State<'_, Arc<PushManager>>) -> CommandResult<PushStatus> {
        Ok(push.status()?)
    }

    /// The owner's opt-in. Off by default.
    #[tauri::command]
    pub async fn vca_push_set_enabled(
        push: State<'_, Arc<PushManager>>,
        enabled: bool,
    ) -> CommandResult<PushStatus> {
        Ok(push.set_enabled(enabled).await?)
    }

    #[tauri::command]
    pub fn vca_push_set_muted(
        push: State<'_, Arc<PushManager>>,
        phone: String,
        muted: bool,
    ) -> CommandResult<PushStatus> {
        Ok(push.set_muted(&phone, muted)?)
    }

    #[tauri::command]
    pub async fn vca_push_authorize_start(
        push: State<'_, Arc<PushManager>>,
    ) -> CommandResult<PushAuthorizationPrompt> {
        Ok(push.start_authorization().await?)
    }

    /// Waits until the person approves (or denies) in DNSPilot.
    #[tauri::command]
    pub async fn vca_push_authorize_wait(
        push: State<'_, Arc<PushManager>>,
        pending: State<'_, PendingPushPoll>,
    ) -> CommandResult<PushStatus> {
        let push = Arc::clone(push.inner());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            let _ = tx.send(push.wait_for_authorization().await);
        });
        if let Some(previous) = lock(&pending.0).replace(task) {
            previous.abort();
        }
        match rx.await {
            Ok(result) => Ok(result?),
            Err(_) => Err(CommandError::new(
                "dnspilot_cancelled",
                "the DNSPilot approval was cancelled",
            )),
        }
    }

    #[tauri::command]
    pub fn vca_push_authorize_cancel(
        push: State<'_, Arc<PushManager>>,
        pending: State<'_, PendingPushPoll>,
    ) {
        if let Some(task) = lock(&pending.0).take() {
            task.abort();
        }
        push.cancel_authorization();
    }

    /// Opens the pending approval in the system browser (console URLs only).
    #[tauri::command]
    pub fn vca_push_open_activation(
        app: AppHandle,
        push: State<'_, Arc<PushManager>>,
    ) -> CommandResult<()> {
        let url = push.activation_url().ok_or_else(|| {
            CommandError::new("dnspilot_expired", "no DNSPilot approval is in progress")
        })?;
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

    /// Ends the push grant. The tunnel grant is not touched.
    #[tauri::command]
    pub async fn vca_push_disconnect(
        push: State<'_, Arc<PushManager>>,
    ) -> CommandResult<PushStatus> {
        Ok(push.disconnect().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_client::push::{PushPayload, phone_public_key, seal};
    use vca_net::MemoryKeyStore;

    fn opened_for(keys: &MemoryKeyStore) -> OpenedPush {
        let payload = PushPayload {
            v: 1,
            m: "0123456789abcdef".into(),
            s: "s1".into(),
            d: "d1".into(),
        };
        let sealed = seal(&phone_public_key(keys).unwrap(), &payload).unwrap();
        OpenedPush {
            ciphertext: Some(base64(&sealed)),
        }
    }

    fn base64(bytes: &[u8]) -> String {
        data_encoding::BASE64.encode(bytes)
    }

    #[test]
    fn a_tap_resolves_to_its_session() {
        let keys = MemoryKeyStore::default();
        let t = target(&keys, &opened_for(&keys));
        assert_eq!(t.machine_id.as_deref(), Some("0123456789abcdef"));
        assert_eq!(t.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn a_tap_without_a_readable_payload_still_opens_the_app() {
        let keys = MemoryKeyStore::default();
        let foreign = opened_for(&MemoryKeyStore::default());
        let none = OpenedTarget {
            machine_id: None,
            session_id: None,
        };
        assert_eq!(target(&keys, &foreign), none);
        assert_eq!(target(&keys, &OpenedPush { ciphertext: None }), none);
    }

    #[test]
    fn the_token_waiter_gets_the_native_answer() {
        let (tx, mut rx) = oneshot::channel();
        *lock(&TOKEN_WAITER) = Some(tx);
        record_native_token("tok".into());
        assert_eq!(rx.try_recv().unwrap(), Ok("tok".to_owned()));
        // Nobody waits: nothing to do.
        record_native_failure("push_permission");
    }

    #[test]
    fn a_tapped_payload_resolves_through_the_pending_ciphertext_key() {
        let keys = MemoryKeyStore::default();
        let sealed = opened_for(&keys).ciphertext.unwrap();
        let payload = serde_json::json!({"aps": {"alert": {"title": "DNSPilot"}}, "c": sealed});
        let opened = OpenedPush {
            ciphertext: ciphertext_from_notification(&payload),
        };
        assert_eq!(target(&keys, &opened).session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn an_old_server_is_reported_as_unsupported() {
        let e = send_error(ClientError::Protocol("x".into()));
        assert_eq!(e.code, "push_unsupported");
    }
}
