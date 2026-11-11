//! FCM device token and notification tap for the mobile app (Android).
//!
//! No maintained official Tauri plugin hands out the token
//! (`tauri-plugin-notification` is local notifications only). Needs a Firebase
//! project and its `google-services.json`, so the app only builds this with
//! its `native-push` feature. The token goes to Rust and is never logged.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Runtime};

/// The plugin; register it with `tauri::Builder::plugin`.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("vca-push")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle =
                    _api.register_android_plugin("io.metavu.voicecommandai.push", "PushPlugin")?;
                _app.manage(android::Push(handle));
            }
            Ok(())
        })
        .build()
}

/// Asks for the notification permission and returns the FCM token. The error
/// is a stable code: `push_permission`, `push_not_configured` or
/// `push_registration_failed`.
pub async fn token<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        android::token(app).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err("push_not_configured".to_owned())
    }
}

/// True once after the app was opened from a notification.
pub fn take_opened<R: Runtime>(app: &AppHandle<R>) -> bool {
    #[cfg(target_os = "android")]
    {
        android::take_opened(app)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        false
    }
}

#[cfg(target_os = "android")]
mod android {
    use serde::{Deserialize, Serialize};
    use tauri::plugin::{PermissionState, PluginHandle};
    use tauri::{AppHandle, Manager, Runtime};

    pub struct Push<R: Runtime>(pub PluginHandle<R>);

    #[derive(Deserialize)]
    struct States {
        notifications: PermissionState,
    }

    #[derive(Serialize)]
    struct RequestArgs {
        permissions: [&'static str; 1],
    }

    #[derive(Deserialize)]
    struct Token {
        token: String,
    }

    #[derive(Deserialize)]
    struct Opened {
        opened: bool,
    }

    pub async fn token<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
        let plugin = app
            .try_state::<Push<R>>()
            .ok_or_else(|| "push_not_configured".to_owned())?;
        let failed = |_| "push_registration_failed".to_owned();
        let current: States = plugin
            .0
            .run_mobile_plugin_async("checkPermissions", ())
            .await
            .map_err(failed)?;
        if current.notifications != PermissionState::Granted {
            let after: States = plugin
                .0
                .run_mobile_plugin_async(
                    "requestPermissions",
                    RequestArgs {
                        permissions: ["notifications"],
                    },
                )
                .await
                .map_err(failed)?;
            if after.notifications != PermissionState::Granted {
                return Err("push_permission".to_owned());
            }
        }
        let token: Token = plugin
            .0
            .run_mobile_plugin_async("token", ())
            .await
            .map_err(|_| "push_not_configured".to_owned())?;
        Ok(token.token)
    }

    pub fn take_opened<R: Runtime>(app: &AppHandle<R>) -> bool {
        app.try_state::<Push<R>>()
            .and_then(|p| p.0.run_mobile_plugin::<Opened>("takeOpened", ()).ok())
            .is_some_and(|o| o.opened)
    }
}
