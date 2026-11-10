//! Mobile entry point. `cargo tauri android|ios` builds this library; on a
//! desktop it can also run through `main.rs` for quick UI checks.

#[cfg(all(target_os = "android", feature = "native-push"))]
mod android_push;
#[cfg(target_os = "ios")]
mod ios_launch_url;
#[cfg(all(target_os = "ios", feature = "native-push"))]
mod ios_push;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = vca_shell::builder();
    // Scanning the pairing QR code in the app (phones only; desktops paste
    // the link). A scanned link only pre-fills the pairing form.
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    // Confirms the person holding the phone (biometrics or the device
    // passcode): the app lock at launch and after idle time, and before a
    // remote shell opens or runs a dangerous command.
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_biometric::init());
    #[cfg(all(target_os = "android", feature = "native-push"))]
    let builder = builder.plugin(tauri_plugin_vca_push::init());
    let app = builder
        .build(tauri::generate_context!())
        .expect("error while building the Voice Command AI app");
    #[cfg(all(target_os = "android", feature = "native-push"))]
    vca_shell::push::install_native(Box::new(android_push::AndroidPush {
        app: app.handle().clone(),
    }));
    #[cfg(all(target_os = "ios", feature = "native-push"))]
    {
        ios_push::install();
        let handle = app.handle().clone();
        vca_shell::push::install_native(Box::new(ios_push::IosPush {
            run_on_main: std::sync::Arc::new(move |f| {
                let _ = handle.run_on_main_thread(f);
            }),
        }));
    }
    // Building created tao's event loop and its scene delegate class; the
    // first scene connects only once `run` starts UIKit, so this is the
    // window to hook it.
    #[cfg(target_os = "ios")]
    ios_launch_url::install();
    app.run(|_, _| {});
}
