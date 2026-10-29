//! Where device keys and tokens are kept.
//!
//! Desktop and iOS use the OS keychain (Windows Credential Manager, macOS
//! and iOS Keychain, Secret Service on Linux). When no keychain is usable
//! (for example a Linux session without a Secret Service) and on Android,
//! secrets go to files in the app's private data directory.

use std::path::Path;
use std::sync::Arc;

use vca_net::{FileKeyStore, KeyStore};

#[cfg(not(target_os = "android"))]
const SERVICE: &str = "io.github.voicecommandai.app";

pub fn open(data_dir: &Path) -> Result<Arc<dyn KeyStore>, Box<dyn std::error::Error>> {
    #[cfg(not(target_os = "android"))]
    {
        let keychain = vca_net::KeychainKeyStore::new(SERVICE);
        match probe(&keychain) {
            Ok(()) => return Ok(Arc::new(keychain)),
            Err(e) => tracing::warn!(
                "the OS keychain is not usable ({e}); keeping secrets in the app data directory"
            ),
        }
    }
    let dir = data_dir.join("secrets");
    std::fs::create_dir_all(&dir)?;
    Ok(Arc::new(FileKeyStore::new(dir)))
}

/// Round-trips a throwaway secret so a broken keychain is detected at start
/// rather than after pairing.
#[cfg(not(target_os = "android"))]
fn probe(store: &dyn KeyStore) -> vca_net::Result<()> {
    const NAME: &str = "probe";
    store.save(NAME, b"probe")?;
    let back = store.load(NAME)?;
    store.delete(NAME)?;
    match back {
        Some(v) if v.as_slice() == b"probe" => Ok(()),
        _ => Err(vca_net::NetError::InvalidArgument(
            "the keychain did not return the probe secret".into(),
        )),
    }
}
