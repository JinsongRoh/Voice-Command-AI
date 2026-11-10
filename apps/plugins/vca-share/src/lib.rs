//! System share sheet for text files in the mobile app (Android).
//!
//! Android System WebView has no Web Share API, so `vca_share_text_file`
//! (vca-shell) asks this plugin instead. The Kotlin side writes the text to
//! the app's cache (`cache/vca-share/`, the only folder its FileProvider
//! exposes), opens an `ACTION_SEND` chooser with a read-only content URI,
//! and reports whether the user picked a target.
//!
//! On every other platform [`share_text_file`] reports an error; iOS presents
//! its share sheet from vca-shell.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Runtime};

/// The plugin; register it with `tauri::Builder::plugin`.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("vca-share")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = _api
                    .register_android_plugin("io.metavu.voicecommandai.share", "ShareFilePlugin")?;
                _app.manage(android::Share(handle));
            }
            Ok(())
        })
        .build()
}

/// Shares `text` as a file named `name` (already sanitized: a plain file
/// name with a text extension) of type `mime`. Returns `Ok(true)` when the
/// user picked a share target, `Ok(false)` when the chooser was closed.
pub async fn share_text_file<R: Runtime>(
    app: &AppHandle<R>,
    name: &str,
    text: &str,
    mime: &str,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        android::share(app, name, text, mime).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, name, text, mime);
        Err("the share sheet plugin is Android only".to_owned())
    }
}

#[cfg(target_os = "android")]
mod android {
    use serde::{Deserialize, Serialize};
    use tauri::plugin::PluginHandle;
    use tauri::{AppHandle, Manager, Runtime};

    pub struct Share<R: Runtime>(pub PluginHandle<R>);

    #[derive(Serialize)]
    struct Args<'a> {
        name: &'a str,
        text: &'a str,
        mime: &'a str,
    }

    #[derive(Deserialize)]
    struct Outcome {
        shared: bool,
    }

    pub async fn share<R: Runtime>(
        app: &AppHandle<R>,
        name: &str,
        text: &str,
        mime: &str,
    ) -> Result<bool, String> {
        let plugin = app
            .try_state::<Share<R>>()
            .ok_or_else(|| "the share sheet plugin is not registered".to_owned())?;
        let outcome: Outcome = plugin
            .0
            .run_mobile_plugin_async("shareFile", Args { name, text, mime })
            .await
            .map_err(|e| format!("could not open the share sheet: {e}"))?;
        Ok(outcome.shared)
    }
}
