// No webview commands: the app calls the plugin from Rust only. The Android
// project is copied into the app's Gradle build by tauri-build.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
