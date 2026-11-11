//! Long-term static X25519 keys, fingerprints and key storage.
//!
//! The server has one static keypair (its Noise identity, published in the
//! pairing QR code). Each device has its own static keypair; on mobile the
//! private key lives in the Secure Enclave / Android Keystore, so this module
//! is used by the mobile app only through FFI or tests.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::{NetError, Result};
use crate::util::{b64d, b64e, fill_random, write_private_file};

/// Length of an X25519 key in bytes.
pub const KEY_LEN: usize = 32;

/// Key-store name of the server's static Noise key.
pub const SERVER_STATIC_KEY_NAME: &str = "server-static";

/// Key-store name of the token signing key.
pub const TOKEN_KEY_NAME: &str = "token-hmac";

/// An X25519 public key. Serialized as unpadded base64url.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; KEY_LEN]);

impl PublicKey {
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Parses a key from a byte slice, which must be exactly 32 bytes.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; KEY_LEN] = bytes.try_into().map_err(|_| {
            NetError::InvalidKey(format!("expected {KEY_LEN} bytes, got {}", bytes.len()))
        })?;
        Ok(Self(arr))
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    pub fn to_base64url(&self) -> String {
        b64e(&self.0)
    }

    pub fn from_base64url(s: &str) -> Result<Self> {
        Self::from_slice(&b64d(s).map_err(|_| NetError::InvalidKey("invalid base64url".into()))?)
    }

    /// SHA-256 fingerprint of the key, for display and identifiers.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint(Sha256::digest(self.0).into())
    }

    /// Constant-time equality.
    pub fn ct_eq(&self, other: &PublicKey) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.fingerprint().short())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64url())
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_base64url())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_base64url(&s).map_err(serde::de::Error::custom)
    }
}

/// SHA-256 digest of a public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Short human-readable form: the first 8 bytes as four groups of hex,
    /// e.g. `3f2a-9c01-77de-b40e`. Used for display and comparison by eye.
    pub fn short(&self) -> String {
        let h = crate::util::hex(&self.0[..8]);
        format!("{}-{}-{}-{}", &h[0..4], &h[4..8], &h[8..12], &h[12..16])
    }

    /// Full lowercase hex.
    pub fn to_hex(&self) -> String {
        crate::util::hex(&self.0)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

/// A static X25519 keypair. The secret is zeroized on drop and never
/// printed by `Debug`.
#[derive(Clone)]
pub struct StaticKeypair {
    secret: Zeroizing<[u8; KEY_LEN]>,
    public: PublicKey,
}

impl StaticKeypair {
    /// Generates a new keypair from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut secret = Zeroizing::new([0u8; KEY_LEN]);
        fill_random(secret.as_mut());
        Self::from_secret_bytes(*secret)
    }

    /// Builds a keypair from a raw 32-byte secret, deriving the public key.
    pub fn from_secret_bytes(bytes: [u8; KEY_LEN]) -> Self {
        let secret = Zeroizing::new(bytes);
        let s = x25519_dalek::StaticSecret::from(*secret);
        let public = PublicKey(*x25519_dalek::PublicKey::from(&s).as_bytes());
        Self { secret, public }
    }

    pub fn from_secret_slice(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; KEY_LEN] = bytes
            .try_into()
            .map_err(|_| NetError::InvalidKey(format!("expected {KEY_LEN} secret bytes")))?;
        let arr = Zeroizing::new(arr);
        Ok(Self::from_secret_bytes(*arr))
    }

    pub fn public(&self) -> &PublicKey {
        &self.public
    }

    /// Raw secret bytes. Handle with care.
    pub fn secret_bytes(&self) -> &[u8; KEY_LEN] {
        &self.secret
    }

    /// Secret as unpadded base64url (for storage).
    pub fn secret_to_base64url(&self) -> Zeroizing<String> {
        Zeroizing::new(b64e(self.secret.as_ref()))
    }

    pub fn from_secret_base64url(s: &str) -> Result<Self> {
        let bytes =
            Zeroizing::new(b64d(s).map_err(|_| NetError::InvalidKey("invalid base64url".into()))?);
        Self::from_secret_slice(&bytes)
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticKeypair")
            .field("public", &self.public)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Storage for long-term secrets (static private keys, token signing keys).
pub trait KeyStore: Send + Sync {
    /// Loads a secret, or `None` when it does not exist.
    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>>;
    /// Stores (or replaces) a secret.
    fn save(&self, name: &str, secret: &[u8]) -> Result<()>;
    /// Deletes a secret. Deleting a missing secret is not an error.
    fn delete(&self, name: &str) -> Result<()>;
}

fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(NetError::InvalidArgument(format!(
            "invalid key name {name:?}"
        )))
    }
}

/// Volatile in-memory key store (tests, ephemeral setups).
#[derive(Default)]
pub struct MemoryKeyStore {
    map: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
}

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        validate_name(name)?;
        Ok(self.map.lock().expect("poisoned").get(name).cloned())
    }

    fn save(&self, name: &str, secret: &[u8]) -> Result<()> {
        validate_name(name)?;
        self.map
            .lock()
            .expect("poisoned")
            .insert(name.to_string(), Zeroizing::new(secret.to_vec()));
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        self.map.lock().expect("poisoned").remove(name);
        Ok(())
    }
}

/// File-backed key store: one file per secret, `<dir>/<name>.key`,
/// containing base64url. Files are created with mode `0600` (Unix) inside a
/// `0700` directory.
#[derive(Debug, Clone)]
pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.key"))
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        validate_name(name)?;
        let path = self.path(name);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => Zeroizing::new(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)?.permissions().mode();
            if mode & 0o077 != 0 {
                tracing::warn!(path = %path.display(), "key file is accessible by other users; tightening to 0600");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(Some(Zeroizing::new(b64d(&text).map_err(|_| {
            NetError::Storage(format!("corrupt key file {}", path.display()))
        })?)))
    }

    fn save(&self, name: &str, secret: &[u8]) -> Result<()> {
        validate_name(name)?;
        let text = Zeroizing::new(b64e(secret));
        write_private_file(&self.path(name), text.as_bytes())
    }

    fn delete(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// OS keychain key store (Windows Credential Manager, macOS Keychain,
/// Secret Service on Linux) via the `keyring` crate.
///
/// Unverified on Windows and macOS; on Linux it needs a running Secret
/// Service (GNOME Keyring, KWallet).
#[cfg(feature = "keychain")]
#[derive(Debug, Clone)]
pub struct KeychainKeyStore {
    service: String,
}

#[cfg(feature = "keychain")]
impl KeychainKeyStore {
    /// `service` namespaces the entries, e.g. `"ai.voicecommand.vca"`.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        validate_name(name)?;
        keyring::Entry::new(&self.service, name).map_err(NetError::storage)
    }
}

#[cfg(feature = "keychain")]
impl KeyStore for KeychainKeyStore {
    fn load(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match self.entry(name)?.get_password() {
            Ok(s) => {
                let s = Zeroizing::new(s);
                Ok(Some(Zeroizing::new(b64d(&s)?)))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(NetError::storage(e)),
        }
    }

    fn save(&self, name: &str, secret: &[u8]) -> Result<()> {
        let text = Zeroizing::new(b64e(secret));
        self.entry(name)?
            .set_password(&text)
            .map_err(NetError::storage)
    }

    fn delete(&self, name: &str) -> Result<()> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(NetError::storage(e)),
        }
    }
}

/// Loads the static keypair stored under `name`, generating and saving a
/// new one if none exists.
pub fn load_or_generate_keypair(store: &dyn KeyStore, name: &str) -> Result<StaticKeypair> {
    if let Some(secret) = store.load(name)? {
        return StaticKeypair::from_secret_slice(&secret);
    }
    let kp = StaticKeypair::generate();
    store.save(name, kp.secret_bytes())?;
    Ok(kp)
}

/// Loads the random secret stored under `name`, generating `len` random
/// bytes if none exists.
pub fn load_or_generate_secret(
    store: &dyn KeyStore,
    name: &str,
    len: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    if let Some(secret) = store.load(name)? {
        if secret.len() == len {
            return Ok(secret);
        }
        return Err(NetError::Storage(format!(
            "secret {name:?} has unexpected length"
        )));
    }
    let mut s = Zeroizing::new(vec![0u8; len]);
    fill_random(&mut s);
    store.save(name, &s)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_derivation_is_deterministic() {
        let kp = StaticKeypair::generate();
        let again = StaticKeypair::from_secret_bytes(*kp.secret_bytes());
        assert_eq!(kp.public(), again.public());
        assert_ne!(kp.public(), StaticKeypair::generate().public());
    }

    #[test]
    fn matches_x25519_rfc7748_vector() {
        // RFC 7748 section 6.1, Alice's keys.
        let sk = data_encoding::HEXLOWER
            .decode(b"77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
            .unwrap();
        let pk = data_encoding::HEXLOWER
            .decode(b"8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
            .unwrap();
        let kp = StaticKeypair::from_secret_slice(&sk).unwrap();
        assert_eq!(kp.public().as_bytes().as_slice(), pk.as_slice());
    }

    #[test]
    fn base64url_roundtrip_and_errors() {
        let kp = StaticKeypair::generate();
        let s = kp.public().to_base64url();
        assert_eq!(s.len(), 43);
        assert_eq!(PublicKey::from_base64url(&s).unwrap(), *kp.public());
        let back = StaticKeypair::from_secret_base64url(&kp.secret_to_base64url()).unwrap();
        assert_eq!(back.public(), kp.public());
        assert!(PublicKey::from_base64url("AAAA").is_err());
        assert!(PublicKey::from_base64url("!!!").is_err());
        assert!(StaticKeypair::from_secret_slice(&[1, 2, 3]).is_err());
    }

    #[test]
    fn serde_uses_base64url() {
        let kp = StaticKeypair::generate();
        let json = serde_json::to_string(kp.public()).unwrap();
        assert_eq!(json, format!("\"{}\"", kp.public().to_base64url()));
        let back: PublicKey = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, kp.public());
        assert!(serde_json::from_str::<PublicKey>("\"abc\"").is_err());
    }

    #[test]
    fn fingerprint_short_form() {
        let fp = PublicKey::from_bytes([0u8; 32]).fingerprint();
        // SHA-256 of 32 zero bytes starts with 66687aadf862bd77.
        assert_eq!(fp.short(), "6668-7aad-f862-bd77");
        assert_eq!(fp.to_hex().len(), 64);
    }

    #[test]
    fn debug_does_not_leak_secret() {
        let kp = StaticKeypair::generate();
        let dbg = format!("{kp:?}");
        assert!(dbg.contains("redacted"));
        assert!(!dbg.contains(kp.secret_to_base64url().as_str()));
    }

    #[test]
    fn memory_store_load_or_generate() {
        let store = MemoryKeyStore::new();
        let a = load_or_generate_keypair(&store, SERVER_STATIC_KEY_NAME).unwrap();
        let b = load_or_generate_keypair(&store, SERVER_STATIC_KEY_NAME).unwrap();
        assert_eq!(a.public(), b.public());
        store.delete(SERVER_STATIC_KEY_NAME).unwrap();
        let c = load_or_generate_keypair(&store, SERVER_STATIC_KEY_NAME).unwrap();
        assert_ne!(a.public(), c.public());
        assert!(store.load("../etc/passwd").is_err());
        assert!(store.save("Upper", b"x").is_err());
    }

    #[test]
    fn file_store_roundtrip_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(dir.path().join("keys"));
        assert!(store.load("device").unwrap().is_none());
        let a = load_or_generate_keypair(&store, "device").unwrap();
        let store2 = FileKeyStore::new(dir.path().join("keys"));
        let b = load_or_generate_keypair(&store2, "device").unwrap();
        assert_eq!(a.public(), b.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("keys/device.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let s1 = load_or_generate_secret(&store, TOKEN_KEY_NAME, 32).unwrap();
        let s2 = load_or_generate_secret(&store2, TOKEN_KEY_NAME, 32).unwrap();
        assert_eq!(*s1, *s2);
        assert!(load_or_generate_secret(&store, TOKEN_KEY_NAME, 16).is_err());
        store.delete("device").unwrap();
        store.delete("device").unwrap();
        assert!(store.load("device").unwrap().is_none());
    }

    /// Needs a real OS keychain (Secret Service on Linux); run manually with
    /// `cargo test -p vca-net --features keychain -- --ignored`.
    #[cfg(feature = "keychain")]
    #[test]
    #[ignore = "requires an OS keychain"]
    fn keychain_store_roundtrip() {
        let store = KeychainKeyStore::new("ai.voicecommand.vca.test");
        let a = load_or_generate_keypair(&store, "test-device").unwrap();
        let b = load_or_generate_keypair(&store, "test-device").unwrap();
        assert_eq!(a.public(), b.public());
        store.delete("test-device").unwrap();
        assert!(store.load("test-device").unwrap().is_none());
    }

    #[test]
    fn file_store_rejects_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.key"), "***").unwrap();
        let store = FileKeyStore::new(dir.path());
        assert!(matches!(store.load("bad"), Err(NetError::Storage(_))));
    }
}
