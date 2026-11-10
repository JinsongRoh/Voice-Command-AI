//! Local verification, mail and recovery for the desktop app: Tauri
//! commands over vcad's `security` request (`vca_server::security`).
//!
//! vcad shows the OS prompt itself, so this module only passes the app's
//! window for the prompt to belong to. The nonce of a challenge and the
//! step-up token stay in this process: the webview never sees them. A gated
//! command fails with the code `step_up_required`; the UI then calls
//! `vca_host_security_step_up` for that action and retries. What the prompt
//! says comes from the gated request in vcad, not from here.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncWriteExt, BufReader};

use super::control::{
    CONNECT_TIMEOUT, ControlError, StepUpChallenge, connect_stream, read_raw_line, write_line,
};
use super::{HostState, QR_PX, TOTP_ACCOUNT, qr};
use crate::{CommandError, CommandResult};

/// Long enough for mail to go out.
const CALL_TIMEOUT: Duration = Duration::from_secs(45);
/// A person may take their time at the OS prompt.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(200);

/// The token of the last verification (one app, one owner).
static STEP_UP: Mutex<Option<String>> = Mutex::new(None);
/// The latest nonce per action, until its verification succeeded.
static CHALLENGES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn token() -> MutexGuard<'static, Option<String>> {
    STEP_UP.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps vcad's answer to a gated request for the verification.
pub(super) fn note_challenge(challenge: StepUpChallenge) {
    CHALLENGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .insert(challenge.action, challenge.nonce);
}

fn challenge_for(action: &str) -> Option<String> {
    CHALLENGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .get(action)
        .cloned()
}

fn close_challenge(action: &str) {
    if let Some(open) = CHALLENGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        open.remove(action);
    }
}

/// The held token, for the requests that carry it themselves.
pub(super) fn held_token() -> Option<String> {
    token().clone()
}

fn current_token() -> Value {
    token().clone().map_or(Value::Null, Value::String)
}

fn io_error(what: &str) -> CommandError {
    CommandError::new("unreachable", what)
}

/// Sends one security request and returns its reply (`type` tagged).
async fn raw_call(endpoint: &Path, call: &Value, timeout: Duration) -> CommandResult<Value> {
    let stream = connect_stream(endpoint, CONNECT_TIMEOUT)
        .await
        .map_err(ControlError::Unreachable)?;
    let (recv, mut send) = tokio::io::split(stream);
    let mut recv = BufReader::new(recv);
    let exchange = async {
        write_line(&mut send, &request(1, call))
            .await
            .map_err(|_| io_error("control channel write failed"))?;
        let mut buf = Vec::new();
        loop {
            let line = read_raw_line(&mut recv, &mut buf)
                .await
                .map_err(|_| io_error("control channel read failed"))?;
            if line.is_none() {
                return Err(io_error("vcad closed the connection"));
            }
            let Ok(value) = serde_json::from_slice::<Value>(buf.trim_ascii()) else {
                continue;
            };
            if is_answer(&value) {
                return Ok(value);
            }
        }
    };
    let response = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| io_error("vcad did not answer in time"))??;
    let _ = send.shutdown().await;
    reply_of(response)
}

/// Whether a response line answers the request (id 1). vcad answers a line
/// it cannot read (a call an older vcad does not know) with an error of id
/// 0 and then closes the channel: that is the answer too, so the caller
/// sees `invalid_request` instead of a lost connection.
fn is_answer(value: &Value) -> bool {
    match value.get("id").and_then(Value::as_u64) {
        Some(1) => true,
        Some(0) => value.get("op").and_then(Value::as_str) == Some("error"),
        _ => false,
    }
}

fn request(id: u64, call: &Value) -> Value {
    json!({ "op": "security", "id": id, "call": call })
}

/// The reply inside a response line; a failure becomes a [`CommandError`]
/// whose code is the failure reason.
fn reply_of(response: Value) -> CommandResult<Value> {
    match response.get("op").and_then(Value::as_str) {
        Some("security") => {}
        Some("error") => {
            let code = response["code"].as_str().unwrap_or("internal");
            let message = response["message"].as_str().unwrap_or_default();
            return Err(CommandError::new(code, message));
        }
        _ => return Err(CommandError::new("protocol", "unexpected answer from vcad")),
    }
    let reply = response["reply"].clone();
    if reply["type"] == "step_up_required" {
        let challenge = serde_json::from_value(reply["challenge"].clone())
            .map_err(|_| CommandError::new("protocol", "unexpected answer from vcad"))?;
        note_challenge(challenge);
        return Err(CommandError::new(
            "step_up_required",
            "local verification needed",
        ));
    }
    if reply["type"] == "failed" {
        return Err(CommandError::new(
            reply["reason"].as_str().unwrap_or("internal"),
            reply["message"].as_str().unwrap_or_default(),
        ));
    }
    Ok(reply)
}

async fn call(host: &HostState<'_>, body: Value) -> CommandResult<Value> {
    raw_call(&host.endpoint, &body, CALL_TIMEOUT).await
}

/// The field `key` of a reply.
fn field(mut reply: Value, key: &str) -> CommandResult<Value> {
    match reply.get_mut(key) {
        Some(value) => Ok(value.take()),
        None => Err(CommandError::new("protocol", "unexpected answer from vcad")),
    }
}

/// The app's window, for the OS prompt (an `HWND` on Windows).
#[cfg(windows)]
fn window_handle(app: &AppHandle) -> Option<u64> {
    let windows = app.webview_windows();
    let window = windows.values().next()?;
    window.hwnd().ok().map(|h| h.0 as usize as u64)
}

#[cfg(not(windows))]
fn window_handle(_app: &AppHandle) -> Option<u64> {
    None
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_status(host: HostState<'_>) -> CommandResult<Value> {
    let body = json!({ "type": "status", "step_up": current_token() });
    field(call(&host, body).await?, "status")
}

/// Answers the challenge of a gated request for `action`. What the person
/// approves was fixed by vcad when it refused that request: `action` only
/// picks which of this app's open challenges to answer. `confirmed` is the
/// first-run confirmation in the app, for a computer with nothing to verify
/// against yet.
pub(super) async fn answer_challenge(
    endpoint: &Path,
    action: &str,
    window: Option<u64>,
    totp: Option<String>,
    confirmed: bool,
    language: Option<String>,
) -> CommandResult<Value> {
    let nonce = challenge_for(action).ok_or_else(|| {
        CommandError::new(
            "step_up_required",
            "start the action again to verify for it",
        )
    })?;
    let body = json!({
        "type": "step_up",
        "nonce": nonce,
        "window": window,
        "totp": totp,
        "presence": confirmed.then_some("desktop_app"),
        "language": language,
    });
    let reply = raw_call(endpoint, &body, PROMPT_TIMEOUT).await?;
    let issued = field(reply.clone(), "token")?;
    *token() = issued.as_str().map(str::to_owned);
    close_challenge(action);
    Ok(json!({ "method": reply["method"], "expires_at": reply["expires_at"] }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_step_up(
    app: AppHandle,
    host: HostState<'_>,
    action: String,
    totp: Option<String>,
    confirmed: Option<bool>,
    language: Option<String>,
) -> CommandResult<Value> {
    answer_challenge(
        &host.endpoint,
        &action,
        window_handle(&app),
        totp,
        confirmed.unwrap_or(false),
        language,
    )
    .await
}

/// Tries the verification; nothing is approved by it. Used by Settings ("test")
/// and by the app lock, which unlocks the app with it (Windows Hello / Touch
/// ID, or the authenticator code where the PC has neither).
#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_step_up_test(
    app: AppHandle,
    host: HostState<'_>,
    totp: Option<String>,
    language: Option<String>,
) -> CommandResult<()> {
    let body = json!({
        "type": "step_up_test",
        "window": window_handle(&app),
        "totp": totp,
        "language": language,
    });
    raw_call(&host.endpoint, &body, PROMPT_TIMEOUT).await?;
    Ok(())
}

/// Unlocks the app (the app lock): the same check as the test, approving
/// nothing and issuing no token, but the OS prompt says "Unlock Voice
/// Command AI". An older vcad answers `invalid_request`; the app then falls
/// back to [`vca_host_security_step_up_test`].
#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_app_unlock(
    app: AppHandle,
    host: HostState<'_>,
    totp: Option<String>,
    language: Option<String>,
) -> CommandResult<()> {
    raw_call(
        &host.endpoint,
        &app_unlock_body(window_handle(&app), totp, language),
        PROMPT_TIMEOUT,
    )
    .await?;
    Ok(())
}

fn app_unlock_body(window: Option<u64>, totp: Option<String>, language: Option<String>) -> Value {
    json!({
        "type": "app_unlock",
        "window": window,
        "totp": totp,
        "language": language,
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_step_up_end(host: HostState<'_>) -> CommandResult<()> {
    let held = token().take();
    if let Some(step_up) = held {
        call(&host, json!({ "type": "step_up_end", "step_up": step_up })).await?;
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_totp_begin(
    host: HostState<'_>,
    replace: bool,
) -> CommandResult<Value> {
    let body = json!({
        "type": "totp_begin",
        "account": TOTP_ACCOUNT,
        "replace": replace,
        "step_up": current_token(),
    });
    let reply = call(&host, body).await?;
    let uri = reply["otpauth_uri"]
        .as_str()
        .ok_or_else(|| CommandError::new("protocol", "unexpected answer from vcad"))?;
    let qr_svg = qr::svg(uri, QR_PX)
        .ok_or_else(|| CommandError::new("internal", "cannot draw the QR code"))?;
    Ok(json!({ "otpauth_uri": uri, "secret": reply["secret_base32"], "qr_svg": qr_svg }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_totp_confirm(
    host: HostState<'_>,
    code: String,
    current_code: Option<String>,
) -> CommandResult<Value> {
    let body = json!({
        "type": "totp_confirm",
        "code": code,
        "current_code": current_code,
        "step_up": current_token(),
    });
    call(&host, body).await.map(codes)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_backup_codes(
    host: HostState<'_>,
    current_code: Option<String>,
) -> CommandResult<Value> {
    let body = json!({
        "type": "backup_codes",
        "current_code": current_code,
        "step_up": current_token(),
    });
    call(&host, body).await.map(codes)
}

fn codes(reply: Value) -> Value {
    json!({ "backup_codes": reply["backup_codes"] })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_totp_disable(
    host: HostState<'_>,
    current_code: Option<String>,
) -> CommandResult<Value> {
    let body = json!({
        "type": "totp_disable",
        "current_code": current_code,
        "step_up": current_token(),
    });
    field(call(&host, body).await?, "status")
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_settings(
    host: HostState<'_>,
    grace_secs: Option<u64>,
    remote_shell: Option<bool>,
) -> CommandResult<Value> {
    let body = json!({
        "type": "settings",
        "grace_secs": grace_secs,
        "remote_shell": remote_shell,
        "step_up": current_token(),
    });
    field(call(&host, body).await?, "status")
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_security_wizard(host: HostState<'_>, state: String) -> CommandResult<Value> {
    let body = json!({ "type": "wizard", "state": state });
    field(call(&host, body).await?, "status")
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_mail_get(host: HostState<'_>) -> CommandResult<Value> {
    field(call(&host, json!({ "type": "mail_get" })).await?, "view")
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_mail_set(
    host: HostState<'_>,
    settings: Value,
    current_code: Option<String>,
) -> CommandResult<Value> {
    let body = json!({
        "type": "mail_set",
        "settings": settings,
        "current_code": current_code,
        "step_up": current_token(),
    });
    field(call(&host, body).await?, "view")
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_mail_secret(
    host: HostState<'_>,
    which: String,
    secret: Option<String>,
    current_code: Option<String>,
) -> CommandResult<Value> {
    let body = json!({
        "type": "mail_secret",
        "which": which,
        "secret": secret,
        "current_code": current_code,
        "step_up": current_token(),
    });
    field(call(&host, body).await?, "view")
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_mail_test(host: HostState<'_>) -> CommandResult<Value> {
    let reply = call(&host, json!({ "type": "mail_test" })).await?;
    Ok(json!({ "sent_to": reply["sent_to"] }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_recovery_start(host: HostState<'_>) -> CommandResult<Value> {
    let body = json!({ "type": "recovery_start", "step_up": current_token() });
    let reply = call(&host, body).await?;
    Ok(json!({ "sent_to": reply["sent_to"], "expires_at": reply["expires_at"] }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn vca_host_recovery_finish(
    host: HostState<'_>,
    code: Option<String>,
    backup_code: Option<String>,
    purpose: Option<String>,
) -> CommandResult<Value> {
    let body = json!({
        "type": "recovery_finish",
        "code": code,
        "backup_code": backup_code,
        "purpose": purpose.unwrap_or_else(|| "replace".to_owned()),
        "step_up": current_token(),
    });
    let reply = call(&host, body).await?;
    Ok(json!({ "grant_expires_at": reply["grant_expires_at"] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(err: CommandError) -> String {
        serde_json::to_value(err).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn app_unlock_matches_the_line_vcad_reads() {
        assert_eq!(
            request(1, &app_unlock_body(Some(123), None, Some("ko".into()))),
            json!({
                "op": "security", "id": 1,
                "call": { "type": "app_unlock", "window": 123, "totp": null, "language": "ko" }
            })
        );
    }

    #[test]
    fn an_older_vcad_refusing_the_line_is_an_answer() {
        let refused = json!({
            "op": "error", "id": 0, "code": "invalid_request",
            "message": "unknown variant `app_unlock`"
        });
        assert!(is_answer(&refused));
        assert_eq!(code_of(reply_of(refused).unwrap_err()), "invalid_request");
        // Other lines of id 0 (none today) and other ids are not.
        assert!(!is_answer(&json!({ "op": "security", "id": 0 })));
        assert!(!is_answer(&json!({ "op": "error", "id": 2 })));
        assert!(is_answer(&json!({ "op": "security", "id": 1 })));
    }

    #[test]
    fn request_shape_matches_vcad() {
        assert_eq!(
            request(7, &json!({ "type": "mail_get" })),
            json!({ "op": "security", "id": 7, "call": { "type": "mail_get" } })
        );
    }

    #[test]
    fn a_challenge_stays_in_this_process_until_verified() {
        let answer = json!({
            "op": "security", "id": 1,
            "reply": {
                "type": "step_up_required",
                "challenge": { "nonce": "n-1", "action": "backup_codes_export", "expires_at": 1 }
            }
        });
        let err = reply_of(answer).unwrap_err();
        let shown = serde_json::to_string(&err).unwrap();
        assert!(shown.contains("step_up_required"), "{shown}");
        assert!(!shown.contains("n-1"), "the webview never sees the nonce");
        assert_eq!(challenge_for("backup_codes_export").as_deref(), Some("n-1"));
        close_challenge("backup_codes_export");
        assert_eq!(challenge_for("backup_codes_export"), None);
    }

    #[test]
    fn replies_and_failures() {
        let ok = json!({ "op": "security", "id": 1, "reply": { "type": "done" } });
        assert_eq!(reply_of(ok).unwrap()["type"], "done");

        let failed = json!({
            "op": "security", "id": 1,
            "reply": { "type": "failed", "reason": "step_up_required", "message": "verify" }
        });
        assert_eq!(code_of(reply_of(failed).unwrap_err()), "step_up_required");

        let error = json!({ "op": "error", "id": 1, "code": "unsupported", "message": "off" });
        assert_eq!(code_of(reply_of(error).unwrap_err()), "unsupported");

        let other = json!({ "op": "done", "id": 1 });
        assert_eq!(code_of(reply_of(other).unwrap_err()), "protocol");
    }

    #[test]
    fn missing_fields_are_a_protocol_error() {
        let reply = json!({ "type": "status", "status": { "setup_complete": true } });
        assert_eq!(
            field(reply.clone(), "status").unwrap()["setup_complete"],
            true
        );
        assert_eq!(code_of(field(reply, "view").unwrap_err()), "protocol");
    }
}
