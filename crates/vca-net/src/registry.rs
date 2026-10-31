//! Registry of paired devices (SEC-01, SEC-10).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vca_core::{ClientMsg, PairedDevice, UnixMillis};

use crate::error::{NetError, Result};
use crate::keys::PublicKey;
use crate::util::{random_hex, write_private_file};

/// Permission scope of a paired device. Ordered: `View < Command < Admin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Read-only: list sessions, watch streams, view resources.
    View,
    /// Everything in `View`, plus sending input and controlling sessions.
    Command,
    /// Everything, including server administration.
    Admin,
}

impl Scope {
    /// True when this scope includes `required`.
    pub fn allows(self, required: Scope) -> bool {
        self >= required
    }

    /// The minimum scope needed for a client message. Step-up
    /// confirmation for sensitive actions (SEC-06) is enforced on top of
    /// this by the server.
    pub fn required_for(msg: &ClientMsg) -> Scope {
        match msg {
            ClientMsg::Hello { .. }
            | ClientMsg::Ping { .. }
            | ClientMsg::ListSessions
            | ClientMsg::Subscribe { .. }
            | ClientMsg::Unsubscribe { .. }
            | ClientMsg::GetResources
            | ClientMsg::MarkRead { .. }
            | ClientMsg::ListDevices
            | ClientMsg::ListCloudProviders => Scope::View,
            // The kill switch only ever reduces exposure, but a view-only
            // device must not be able to disrupt everyone's sessions.
            ClientMsg::SendInput { .. }
            | ClientMsg::Control { .. }
            | ClientMsg::Launch { .. }
            | ClientMsg::LaunchCloud { .. }
            | ClientMsg::ReorderQueue { .. }
            | ClientMsg::CancelQueued { .. }
            | ClientMsg::KillSwitch => Scope::Command,
            // Recognition changes nothing, but server-side engines may spend
            // the owner's speech API quota.
            ClientMsg::Transcribe { .. } => Scope::Command,
            ClientMsg::RevokeDevice { .. } => Scope::Admin,
        }
    }

    /// True when this scope may send `msg`.
    pub fn permits(self, msg: &ClientMsg) -> bool {
        self.allows(Self::required_for(msg))
    }
}

/// A paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    /// Stable random identifier, e.g. `dev_3f2a9c0177deb40e`.
    pub id: String,
    /// User-visible name supplied at pairing (sanitized).
    pub name: String,
    /// Platform reported at pairing, e.g. `ios` or `android` (sanitized,
    /// informational).
    #[serde(default)]
    pub platform: String,
    /// The device's static Noise public key.
    pub public_key: PublicKey,
    pub scope: Scope,
    pub paired_at: UnixMillis,
    pub last_seen: Option<UnixMillis>,
    pub revoked_at: Option<UnixMillis>,
}

impl DeviceRecord {
    /// Creates a record with a fresh random id.
    pub fn new(name: String, public_key: PublicKey, scope: Scope, now: UnixMillis) -> Self {
        Self {
            id: format!("dev_{}", random_hex(8)),
            name,
            platform: String::new(),
            public_key,
            scope,
            paired_at: now,
            last_seen: None,
            revoked_at: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// Protocol view of the device for `ServerMsg::Devices`.
    /// `current_device_id` is the device that asked.
    pub fn to_paired_device(&self, current_device_id: &str) -> PairedDevice {
        PairedDevice {
            id: self.id.clone(),
            name: self.name.clone(),
            platform: self.platform.clone(),
            paired_at: self.paired_at,
            last_seen: self.last_seen.unwrap_or(self.paired_at),
            current: self.id == current_device_id,
        }
    }
}

/// Storage of paired devices. Implementations must be cheap enough to call
/// on every connection attempt.
pub trait DeviceRegistry: Send + Sync {
    /// Looks up a device (active or revoked) by its static key.
    fn get_by_key(&self, key: &PublicKey) -> Result<Option<DeviceRecord>>;
    fn get(&self, id: &str) -> Result<Option<DeviceRecord>>;
    /// Adds a device. Fails if the key or id is already registered (even
    /// if revoked: a revoked device must generate a new key).
    fn register(&self, record: DeviceRecord) -> Result<()>;
    fn list(&self) -> Result<Vec<DeviceRecord>>;
    /// Marks a device revoked. Returns `false` if it does not exist or was
    /// already revoked.
    fn revoke(&self, id: &str, at: UnixMillis) -> Result<bool>;
    /// Records that the device connected.
    fn touch(&self, id: &str, at: UnixMillis) -> Result<()>;
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryData {
    version: u32,
    devices: BTreeMap<String, DeviceRecord>,
}

impl RegistryData {
    fn register(&mut self, record: DeviceRecord) -> Result<()> {
        if self.devices.contains_key(&record.id)
            || self
                .devices
                .values()
                .any(|d| d.public_key == record.public_key)
        {
            return Err(NetError::InvalidArgument(
                "device already registered".into(),
            ));
        }
        self.devices.insert(record.id.clone(), record);
        Ok(())
    }

    fn revoke(&mut self, id: &str, at: UnixMillis) -> bool {
        match self.devices.get_mut(id) {
            Some(d) if d.revoked_at.is_none() => {
                d.revoked_at = Some(at);
                true
            }
            _ => false,
        }
    }

    fn touch(&mut self, id: &str, at: UnixMillis) -> bool {
        match self.devices.get_mut(id) {
            Some(d) => {
                d.last_seen = Some(at);
                true
            }
            None => false,
        }
    }
}

/// In-memory registry.
#[derive(Default)]
pub struct MemoryDeviceRegistry {
    data: Mutex<RegistryData>,
}

impl MemoryDeviceRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

impl DeviceRegistry for MemoryDeviceRegistry {
    fn get_by_key(&self, key: &PublicKey) -> Result<Option<DeviceRecord>> {
        let data = self.data.lock().expect("poisoned");
        Ok(data
            .devices
            .values()
            .find(|d| d.public_key.ct_eq(key))
            .cloned())
    }

    fn get(&self, id: &str) -> Result<Option<DeviceRecord>> {
        Ok(self.data.lock().expect("poisoned").devices.get(id).cloned())
    }

    fn register(&self, record: DeviceRecord) -> Result<()> {
        self.data.lock().expect("poisoned").register(record)
    }

    fn list(&self) -> Result<Vec<DeviceRecord>> {
        Ok(self
            .data
            .lock()
            .expect("poisoned")
            .devices
            .values()
            .cloned()
            .collect())
    }

    fn revoke(&self, id: &str, at: UnixMillis) -> Result<bool> {
        Ok(self.data.lock().expect("poisoned").revoke(id, at))
    }

    fn touch(&self, id: &str, at: UnixMillis) -> Result<()> {
        self.data.lock().expect("poisoned").touch(id, at);
        Ok(())
    }
}

/// JSON-file-backed registry. The file is rewritten atomically with
/// owner-only permissions after each change.
pub struct FileDeviceRegistry {
    path: PathBuf,
    data: Mutex<RegistryData>,
}

const REGISTRY_VERSION: u32 = 1;

impl FileDeviceRegistry {
    /// Opens (or creates on first write) the registry at `path`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let data = match std::fs::read(&path) {
            Ok(bytes) => {
                let data: RegistryData = serde_json::from_slice(&bytes)
                    .map_err(|e| NetError::Storage(format!("corrupt device registry: {e}")))?;
                if data.version != REGISTRY_VERSION {
                    return Err(NetError::Storage(format!(
                        "unsupported device registry version {}",
                        data.version
                    )));
                }
                data
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RegistryData {
                version: REGISTRY_VERSION,
                devices: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn persist(&self, data: &RegistryData) -> Result<()> {
        let json = serde_json::to_vec_pretty(data).map_err(NetError::encoding)?;
        write_private_file(&self.path, &json)
    }

    fn mutate<T>(&self, f: impl FnOnce(&mut RegistryData) -> Result<T>) -> Result<T> {
        let mut data = self.data.lock().expect("poisoned");
        // Apply to a copy first so that a failed write leaves memory and disk consistent.
        let mut next = RegistryData {
            version: data.version,
            devices: data.devices.clone(),
        };
        let out = f(&mut next)?;
        self.persist(&next)?;
        *data = next;
        Ok(out)
    }
}

impl DeviceRegistry for FileDeviceRegistry {
    fn get_by_key(&self, key: &PublicKey) -> Result<Option<DeviceRecord>> {
        let data = self.data.lock().expect("poisoned");
        Ok(data
            .devices
            .values()
            .find(|d| d.public_key.ct_eq(key))
            .cloned())
    }

    fn get(&self, id: &str) -> Result<Option<DeviceRecord>> {
        Ok(self.data.lock().expect("poisoned").devices.get(id).cloned())
    }

    fn register(&self, record: DeviceRecord) -> Result<()> {
        self.mutate(|d| d.register(record))
    }

    fn list(&self) -> Result<Vec<DeviceRecord>> {
        Ok(self
            .data
            .lock()
            .expect("poisoned")
            .devices
            .values()
            .cloned()
            .collect())
    }

    fn revoke(&self, id: &str, at: UnixMillis) -> Result<bool> {
        self.mutate(|d| Ok(d.revoke(id, at)))
    }

    fn touch(&self, id: &str, at: UnixMillis) -> Result<()> {
        self.mutate(|d| Ok(d.touch(id, at)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::StaticKeypair;

    fn exercise(reg: &dyn DeviceRegistry) {
        let k1 = *StaticKeypair::generate().public();
        let k2 = *StaticKeypair::generate().public();
        let d1 = DeviceRecord::new("Phone".into(), k1, Scope::Command, 10);
        let id1 = d1.id.clone();
        reg.register(d1.clone()).unwrap();
        // Same key twice is rejected.
        let dup = DeviceRecord::new("Other".into(), k1, Scope::Admin, 11);
        assert!(reg.register(dup).is_err());
        reg.register(DeviceRecord::new("Tablet".into(), k2, Scope::View, 12))
            .unwrap();
        assert_eq!(reg.list().unwrap().len(), 2);
        assert_eq!(reg.get_by_key(&k1).unwrap().unwrap().id, id1);
        assert!(
            reg.get_by_key(StaticKeypair::generate().public())
                .unwrap()
                .is_none()
        );
        reg.touch(&id1, 50).unwrap();
        assert_eq!(reg.get(&id1).unwrap().unwrap().last_seen, Some(50));
        assert!(reg.revoke(&id1, 60).unwrap());
        assert!(!reg.revoke(&id1, 61).unwrap());
        assert!(!reg.revoke("dev_missing", 61).unwrap());
        let d = reg.get(&id1).unwrap().unwrap();
        assert!(!d.is_active());
        assert_eq!(d.revoked_at, Some(60));
        // A revoked key cannot be re-registered.
        assert!(
            reg.register(DeviceRecord::new("Again".into(), k1, Scope::View, 70))
                .is_err()
        );
    }

    #[test]
    fn memory_registry() {
        exercise(&MemoryDeviceRegistry::new());
    }

    #[test]
    fn file_registry_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let reg = FileDeviceRegistry::open(&path).unwrap();
        exercise(&reg);
        let reopened = FileDeviceRegistry::open(&path).unwrap();
        assert_eq!(reopened.list().unwrap(), reg.list().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn file_registry_rejects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(FileDeviceRegistry::open(&path).is_err());
        std::fs::write(&path, br#"{"version":99,"devices":{}}"#).unwrap();
        assert!(FileDeviceRegistry::open(&path).is_err());
    }

    #[test]
    fn scope_ordering_and_permissions() {
        assert!(Scope::Admin.allows(Scope::Command));
        assert!(Scope::Command.allows(Scope::View));
        assert!(!Scope::View.allows(Scope::Command));
        let input = ClientMsg::SendInput {
            session_id: "s".into(),
            input: vca_core::Input::Key {
                key: vca_core::ControlKey::Interrupt,
            },
        };
        assert!(!Scope::View.permits(&input));
        assert!(Scope::Command.permits(&input));
        assert!(Scope::View.permits(&ClientMsg::ListSessions));
        assert!(Scope::View.permits(&ClientMsg::ListCloudProviders));
        let transcribe = ClientMsg::Transcribe {
            audio: vec![0; 4],
            format: vca_core::AudioEncoding::Wav,
            sample_rate: None,
            language: None,
            session_id: None,
            context: None,
        };
        assert!(!Scope::View.permits(&transcribe));
        assert!(Scope::Command.permits(&transcribe));
        assert_eq!(
            serde_json::to_string(&Scope::Command).unwrap(),
            "\"command\""
        );
    }
}
