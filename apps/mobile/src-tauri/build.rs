fn main() {
    // Google Play requires native libraries that work on devices with 16 KB
    // memory pages: align the app library's LOAD segments to 16 KB. Only the
    // Android cdylib is affected; NDK r27 still defaults to 4 KB.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg=-Wl,-z,max-page-size=16384");
    }
    tauri_build::build();
}
