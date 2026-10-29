//! Mobile entry point. `cargo tauri android|ios` builds this library; on a
//! desktop it can also run through `main.rs` for quick UI checks.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    vca_shell::run(tauri::generate_context!());
}
