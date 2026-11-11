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
const SERVICE: &str = "io.metavu.voicecommandai.app";

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
    create_secrets_dir(&dir)?;
    Ok(Arc::new(FileKeyStore::new(dir)))
}

/// The fallback secrets directory. On Linux it is owner-only (0700), also
/// when an earlier version created it with the default umask, so the names
/// of the stored keys are not listable by other users.
fn create_secrets_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn secrets_dir_is_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("secrets");
        // An existing directory with the usual umask is tightened too.
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::create_secrets_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
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
