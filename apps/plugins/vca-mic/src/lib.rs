//! Microphone runtime permission for the mobile app.
//!
//! vca-shell records with cpal (AAudio on Android), which reads silence or
//! fails to open unless `android.permission.RECORD_AUDIO` is both declared
//! (this plugin's `AndroidManifest.xml`, merged into the app) and granted at
//! runtime. The Kotlin side is an empty plugin whose `@TauriPlugin`
//! permission alias `microphone` lets Tauri's built-in `checkPermissions`
//! and `requestPermissions` commands show the system prompt.
//!
//! On every other platform [`ensure_microphone`] reports the permission as
//! granted without doing anything (desktop and iOS ask on their own when
//! the input stream opens).

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Runtime};

/// The plugin; register it with `tauri::Builder::plugin`.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("vca-mic")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = _api.register_android_plugin(
                    "io.metavu.voicecommandai.mic",
                    "MicPermissionPlugin",
                )?;
                _app.manage(android::MicPermission(handle));
            }
            Ok(())
        })
        .build()
}

/// Makes sure the app may record, asking the user if needed. Returns
/// `Ok(false)` when the user denied the permission.
pub async fn ensure_microphone<R: Runtime>(app: &AppHandle<R>) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        android::ensure(app).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(true)
    }
}

#[cfg(target_os = "android")]
mod android {
    use serde::{Deserialize, Serialize};
    use tauri::plugin::{PermissionState, PluginHandle};
    use tauri::{AppHandle, Manager, Runtime};

    pub struct MicPermission<R: Runtime>(pub PluginHandle<R>);

    /// The plugin's permission aliases and their state.
    #[derive(Deserialize)]
    struct States {
        microphone: PermissionState,
    }

    #[derive(Serialize)]
    struct RequestArgs {
        permissions: [&'static str; 1],
    }

    pub async fn ensure<R: Runtime>(app: &AppHandle<R>) -> Result<bool, String> {
        let plugin = app
            .try_state::<MicPermission<R>>()
            .ok_or_else(|| "the microphone permission plugin is not registered".to_owned())?;
        let current: States = plugin
            .0
            .run_mobile_plugin_async("checkPermissions", ())
            .await
            .map_err(|e| format!("cannot check the microphone permission: {e}"))?;
        if current.microphone == PermissionState::Granted {
            return Ok(true);
        }
        // Shows the system dialog (or answers at once if the user chose
        // "don't ask again"); resolves once the user has answered.
        let after: States = plugin
            .0
            .run_mobile_plugin_async(
                "requestPermissions",
                RequestArgs {
                    permissions: ["microphone"],
                },
            )
            .await
            .map_err(|e| format!("cannot request the microphone permission: {e}"))?;
        Ok(after.microphone == PermissionState::Granted)
    }
}
