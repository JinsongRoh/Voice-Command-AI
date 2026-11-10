//! `vca_share_text_file`: hands text (backup codes, exports) to the phone's
//! share sheet as a file. The UI (`apps/ui/src/native/saveFile.ts`) calls it
//! on phones instead of the Web Share API: WKWebView lays its share sheet out
//! wrongly (squeezed into a corner, oversized icons) and Android's WebView has
//! no Web Share API at all.
//!
//! The text is written to a file in the app's own cache directory (only the
//! sanitized file name comes from the webview), which is replaced by the next
//! share. iOS presents `UIActivityViewController` ([`crate::ios_share`]);
//! Android opens an `ACTION_SEND` chooser with a content URI
//! (`tauri-plugin-vca-share`).

// Phones only; desktop builds compile the pure parts for the unit tests.

use serde::Serialize;
#[cfg(not(desktop))]
use tauri::AppHandle;

use crate::save_file::extension;
#[cfg(not(desktop))]
use crate::save_file::{MAX_TEXT_BYTES, sanitize_name};
#[cfg(not(desktop))]
use crate::{CommandError, CommandResult};

/// What happened, returned to the webview. Failures are a [`CommandError`].
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ShareOutcome {
    /// The user picked a target (or finished an activity) in the share sheet.
    Shared {
        #[serde(rename = "fileName")]
        file_name: String,
    },
    /// The user closed the share sheet without sharing.
    Cancelled,
}

/// MIME types by the extensions `sanitize_name` keeps.
const MIME_TYPES: &[(&str, &str)] = &[
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
    ("json", "application/json"),
];

/// The type announced to the share targets: the UI's choice when it is one of
/// the plain text types, otherwise the one that matches the file name.
fn mime_for(name: &str, requested: Option<&str>) -> &'static str {
    if let Some(known) = requested.and_then(|r| MIME_TYPES.iter().find(|(_, m)| *m == r)) {
        return known.1;
    }
    extension(name)
        .and_then(|e| MIME_TYPES.iter().find(|(x, _)| x.eq_ignore_ascii_case(e)))
        .map_or("text/plain", |(_, m)| m)
}

/// Shares `text` as a file named `name` (sanitized like the desktop "Save
/// as" name) through the system share sheet.
#[cfg(not(desktop))]
#[tauri::command]
pub async fn vca_share_text_file(
    app: AppHandle,
    name: String,
    text: String,
    mime: Option<String>,
) -> CommandResult<ShareOutcome> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(CommandError::new(
            "save_too_large",
            "the text is too large to share",
        ));
    }
    let name = sanitize_name(&name);
    let mime = mime_for(&name, mime.as_deref());
    let shared = share(&app, &name, text, mime).await?;
    Ok(if shared {
        ShareOutcome::Shared { file_name: name }
    } else {
        ShareOutcome::Cancelled
    })
}

#[cfg(target_os = "ios")]
async fn share(app: &AppHandle, name: &str, text: String, _mime: &str) -> CommandResult<bool> {
    use tauri::Manager;

    let failed = |e: String| CommandError::new("share_failed", e);
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| failed(format!("no cache directory: {e}")))?
        .join("vca-share");
    let path = dir.join(name);
    let file = path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Only the latest shared file is kept.
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(format!("could not clear old files: {e}"))),
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| failed(format!("could not create the folder: {e}")))?;
        crate::save_file::write_text(&file, &text)
    })
    .await
    .map_err(|e| failed(e.to_string()))??;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        crate::ios_share::present(&path, move |result| {
            let _ = tx.send(result);
        });
    })
    .map_err(|e| failed(e.to_string()))?;
    // A dropped sender (the sheet went away without its completion handler)
    // counts as cancelled.
    rx.await.unwrap_or(Ok(false)).map_err(failed)
}

#[cfg(target_os = "android")]
async fn share(app: &AppHandle, name: &str, text: String, mime: &str) -> CommandResult<bool> {
    tauri_plugin_vca_share::share_text_file(app, name, &text, mime)
        .await
        .map_err(|e| CommandError::new("share_failed", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types_follow_the_file_name() {
        assert_eq!(mime_for("vca-tasks.csv", None), "text/csv");
        assert_eq!(mime_for("vca-tasks.md", None), "text/markdown");
        assert_eq!(mime_for("codes.txt", None), "text/plain");
        assert_eq!(mime_for("a.json", Some("text/html")), "application/json");
        assert_eq!(mime_for("a.txt", Some("text/csv")), "text/csv");
    }

    #[test]
    fn outcomes_serialize_for_the_webview() {
        assert_eq!(
            serde_json::to_value(ShareOutcome::Shared {
                file_name: "vca-tasks.csv".into()
            })
            .unwrap(),
            serde_json::json!({ "status": "shared", "fileName": "vca-tasks.csv" })
        );
        assert_eq!(
            serde_json::to_value(ShareOutcome::Cancelled).unwrap(),
            serde_json::json!({ "status": "cancelled" })
        );
    }
}
