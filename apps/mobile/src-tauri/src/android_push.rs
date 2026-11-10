//! Android side of approval alerts: joins the Firebase plugin to the shell.
//! Built only with the `native-push` feature, which needs the Firebase setup.

use tauri::{AppHandle, Runtime};
use vca_shell::push::{
    NativePush, OpenedPush, PushPlatform, record_native_failure, record_native_token,
};

pub struct AndroidPush<R: Runtime> {
    pub app: AppHandle<R>,
}

impl<R: Runtime> NativePush for AndroidPush<R> {
    fn platform(&self) -> PushPlatform {
        PushPlatform::Android
    }

    fn request_token(&self) {
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            match tauri_plugin_vca_push::token(&app).await {
                Ok(token) => record_native_token(token),
                Err(code) => record_native_failure(&code),
            }
        });
    }

    fn poll_opened(&self) -> Option<OpenedPush> {
        tauri_plugin_vca_push::take_opened(&self.app).then_some(OpenedPush { ciphertext: None })
    }
}
