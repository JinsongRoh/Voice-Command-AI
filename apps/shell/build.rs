fn main() {
    // `cfg(desktop)`: Windows, macOS and Linux (the same split as Tauri's own
    // `desktop` / `mobile` cfgs, which only apply to crates using tauri-build).
    println!("cargo::rustc-check-cfg=cfg(desktop)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "android" && os != "ios" {
        println!("cargo::rustc-cfg=desktop");
    }

    // Unit-test executables link Tauri, which imports Common Controls v6
    // functions; without this manifest dependency Windows refuses to start
    // them (STATUS_ENTRYPOINT_NOT_FOUND). The app itself gets its manifest
    // from tauri-build; a library (rlib) is not linked, so this only affects
    // this crate's test binaries.
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        println!("cargo::rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo::rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}
