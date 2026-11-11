//! Registry of paired devices (SEC-01, SEC-10).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vca_core::{ClientMsg, DeviceScope, PairedDevice, UnixMillis};

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
            | ClientMsg::History { .. }
            | ClientMsg::GetResources
            | ClientMsg::MarkRead { .. }
            | ClientMsg::ListDevices
            | ClientMsg::ListCloudProviders
            | ClientMsg::ListSpeechOptions
            | ClientMsg::RestoreList
            | ClientMsg::ArrangeList
            | ClientMsg::GetSessionOrder => Scope::View,
            // Classifying a command line runs nothing.
            ClientMsg::ClassifyCommand { .. } => Scope::View,
            // Spoken alerts for any device that sees the alerts; the server
            // caps the text and rate-limits each device.
            ClientMsg::Synthesize { .. } | ClientMsg::ListTtsEngines => Scope::View,
            // Spoken briefings and summaries say only what the session list
            // and its events already show this device; summaries also need
            // the device's own opt-in and are rate-limited by the server.
            ClientMsg::Briefing { .. }
            | ClientMsg::SetVoiceSummary { .. }
            | ClientMsg::Summarize { .. } => Scope::View,
            // A phone's own alert registration: a notification only opens
            // a view the device may see anyway. Kept at `View` so the
            // device token never reaches the command audit log.
            ClientMsg::PushRegister { .. } | ClientMsg::PushUnregister => Scope::View,
            // Hands out every phone's token; the server refuses it from any
            // paired device anyway (local clients only).
            ClientMsg::ListPushPhones => Scope::Admin,
            // The kill switch only ever reduces exposure, but a view-only
            // device must not be able to disrupt everyone's sessions.
            ClientMsg::SendInput { .. }
            | ClientMsg::Control { .. }
            | ClientMsg::Launch { .. }
            | ClientMsg::LaunchCloud { .. }
            | ClientMsg::ReorderQueue { .. }
            | ClientMsg::CancelQueued { .. }
            | ClientMsg::RenameSession { .. }
            | ClientMsg::KillSwitch => Scope::Command,
            // Removes an ended session's card early, like the retention
            // timer. With `stop` ("Remove from VCA") a running session VCA
            // started is stopped first: the stricter of Stop (`Control`) and
            // dismiss, which are both `Command`. Audited like every command.
            ClientMsg::DismissSession { .. } => Scope::Command,
            // The card order every client of this computer shows (and the
            // numbers voice commands use), like a rename.
            ClientMsg::SetSessionOrder { .. } | ClientMsg::ResetSessionOrder => Scope::Command,
            // Folder names inside the workspace roots, for choosing where to
            // launch: the same audience as `Launch`.
            ClientMsg::ProjectFolders
            | ClientMsg::ListFolder { .. }
            | ClientMsg::FolderPath { .. } => Scope::Command,
            // A discovery pass only reads the process list and shows what
            // the session list shows anyway; hiding changes that list.
            ClientMsg::ScanSessions => Scope::View,
            ClientMsg::HideObserved { .. } | ClientMsg::UnhideObserved { .. } => Scope::Command,
            // Recognition changes nothing, but server-side engines may spend
            // the owner's speech API quota.
            ClientMsg::Transcribe { .. } => Scope::Command,
            // Changes which engine and LLM the server runs (and bills) for
            // this device.
            ClientMsg::SetSpeechPreferences { .. } => Scope::Command,
            // Attachments write files into a project and may type into the
            // session, like input does.
            ClientMsg::Attach { .. }
            | ClientMsg::AttachBegin { .. }
            | ClientMsg::AttachChunk { .. }
            | ClientMsg::AttachEnd { .. }
            | ClientMsg::AttachCancel { .. }
            | ClientMsg::AttachFolderBegin { .. }
            | ClientMsg::AttachFolderEnd { .. }
            | ClientMsg::AttachBatchEnd { .. } => Scope::Command,
            ClientMsg::RevokeDevice { .. }
            | ClientMsg::RenameDevice { .. }
            | ClientMsg::SetDeviceScope { .. } => Scope::Admin,
            // Remote shells run arbitrary commands with the owner's rights;
            // the server adds TOTP step-up on top.
            ClientMsg::OpenShell { .. }
            | ClientMsg::ShellRun { .. }
            | ClientMsg::ShellJoin { .. }
            | ClientMsg::ShellKeepAlive { .. }
            | ClientMsg::ListAudit { .. } => Scope::Admin,
            // Restore opens terminals running the owner's agents; a run also
            // needs TOTP step-up, enforced by the server.
            ClientMsg::RestoreCapture { .. }
            | ClientMsg::RestoreEdit { .. }
            | ClientMsg::RestoreImport { .. }
            | ClientMsg::RestoreRun { .. } => Scope::Admin,
            // Screen layouts move the owner's windows; moving them also needs
            // TOTP step-up from a paired device, enforced by the server.
            ClientMsg::ArrangeApply { .. }
            | ClientMsg::ArrangeApplyLayout { .. }
            | ClientMsg::ArrangeUndo { .. }
            | ClientMsg::ArrangeEdit { .. } => Scope::Admin,
            // Takes the focus on the computer; the server refuses it from
            // any paired device anyway (local clients only).
            ClientMsg::FocusTerminal { .. } => Scope::Admin,
            // The conductor: watching it is viewing; talking to it, answering
            // its requests and stopping it are commands. Enabling, starting
            // and the allowlist need admin here and are refused from every
            // paired device by the server anyway (this PC only).
            ClientMsg::ConductorGet => Scope::View,
            ClientMsg::ConductorAsk { .. }
            | ClientMsg::ConductorTrustFolder
            | ClientMsg::ConductorDecide { .. }
            | ClientMsg::ConductorStop { .. }
            | ClientMsg::CcPause { .. } => Scope::Command,
            ClientMsg::ConductorConfigure { .. }
            | ClientMsg::ConductorStart
            | ClientMsg::ConductorAllowSession { .. } => Scope::Admin,
            // Human tasks: reading the list is viewing; checking off,
            // snoozing, editing and sending a result (typed into a session)
            // are commands. Opening an administrator shell and the settings
            // (the LLM opt-in) need admin here and are refused from every
            // paired device by the server anyway (this PC only).
            ClientMsg::TaskList | ClientMsg::TaskExport { .. } | ClientMsg::TaskDigest { .. } => {
                Scope::View
            }
            ClientMsg::TaskCreate { .. }
            | ClientMsg::TaskUpdate { .. }
            | ClientMsg::TaskSendResult { .. } => Scope::Command,
            ClientMsg::TaskOpenAdminShell { .. } | ClientMsg::TaskConfigure { .. } => Scope::Admin,
            // The pronunciation dictionary of spoken alerts: reading it is
            // viewing; teaching a pronunciation changes what every device
            // and the PC's voice say.
            ClientMsg::ListPronunciations { .. } => Scope::View,
            ClientMsg::SetPronunciation { .. } | ClientMsg::DeletePronunciation { .. } => {
                Scope::Command
            }
            // The commonly misheard words: reading them is viewing; a saved
            // list rewrites every later dictation on this PC.
            ClientMsg::ListPersonalTerms => Scope::View,
            ClientMsg::SavePersonalTerms { .. } => Scope::Admin,
        }
    }

    /// True when this scope may send `msg`.
    pub fn permits(self, msg: &ClientMsg) -> bool {
        self.allows(Self::required_for(msg))
    }
}

// --- Device management: protocol scope conversion and name rules. ---

impl From<DeviceScope> for Scope {
    fn from(s: DeviceScope) -> Self {
        match s {
            DeviceScope::View => Scope::View,
            DeviceScope::Command => Scope::Command,
            DeviceScope::Admin => Scope::Admin,
        }
    }
}

impl From<Scope> for DeviceScope {
    fn from(s: Scope) -> Self {
        match s {
            Scope::View => DeviceScope::View,
            Scope::Command => DeviceScope::Command,
            Scope::Admin => DeviceScope::Admin,
        }
    }
}

/// Longest device name accepted by a rename, in characters.
pub const DEVICE_NAME_MAX_CHARS: usize = 64;

/// Validates a new device name: trimmed, 1-64 characters, no control
/// characters (rejected rather than stripped, so the owner sees exactly
/// what is stored). Returns the trimmed name.
pub fn validate_device_name(name: &str) -> Result<String> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > DEVICE_NAME_MAX_CHARS {
        return Err(NetError::InvalidArgument(format!(
            "a device name has 1 to {DEVICE_NAME_MAX_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(NetError::InvalidArgument(
            "a device name cannot contain control characters".into(),
        ));
    }
    Ok(name.to_string())
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
    /// Client IP of the most recent connection (display only).
    #[serde(default)]
    pub last_ip: Option<String>,
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
            last_ip: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// Protocol view of the device for `ServerMsg::Devices`.
    /// `current_device_id` is the device that asked. Live fields (`online`,
    /// `connections`, `app_version`) are filled in by the server.
    pub fn to_paired_device(&self, current_device_id: &str) -> PairedDevice {
        PairedDevice {
            id: self.id.clone(),
            name: self.name.clone(),
            platform: self.platform.clone(),
            paired_at: self.paired_at,
            last_seen: self.last_seen.unwrap_or(self.paired_at),
            current: self.id == current_device_id,
            scope: Some(self.scope.into()),
            online: false,
            connections: 0,
            last_ip: self.last_ip.clone(),
            app_version: None,
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
    /// Records that the device connected (and from which client IP).
    fn touch(&self, id: &str, at: UnixMillis, ip: Option<&str>) -> Result<()>;
    /// Renames an active device; returns the previous name, or `None` if it
    /// does not exist or is revoked. `name` must already be validated
    /// ([`validate_device_name`]).
    fn rename(&self, id: &str, name: &str) -> Result<Option<String>>;
    /// Changes the scope of an active device; returns the previous scope,
    /// or `None` if it does not exist or is revoked.
    fn set_scope(&self, id: &str, scope: Scope) -> Result<Option<Scope>>;
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

    fn touch(&mut self, id: &str, at: UnixMillis, ip: Option<&str>) -> bool {
        match self.devices.get_mut(id) {
            Some(d) => {
                d.last_seen = Some(at);
                if let Some(ip) = ip {
                    d.last_ip = Some(ip.to_string());
                }
                true
            }
            None => false,
        }
    }

    fn active_mut(&mut self, id: &str) -> Option<&mut DeviceRecord> {
        self.devices.get_mut(id).filter(|d| d.is_active())
    }

    fn rename(&mut self, id: &str, name: &str) -> Option<String> {
        self.active_mut(id)
            .map(|d| std::mem::replace(&mut d.name, name.to_string()))
    }

    fn set_scope(&mut self, id: &str, scope: Scope) -> Option<Scope> {
        self.active_mut(id)
            .map(|d| std::mem::replace(&mut d.scope, scope))
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

    fn touch(&self, id: &str, at: UnixMillis, ip: Option<&str>) -> Result<()> {
        self.data.lock().expect("poisoned").touch(id, at, ip);
        Ok(())
    }

    fn rename(&self, id: &str, name: &str) -> Result<Option<String>> {
        Ok(self.data.lock().expect("poisoned").rename(id, name))
    }

    fn set_scope(&self, id: &str, scope: Scope) -> Result<Option<Scope>> {
        Ok(self.data.lock().expect("poisoned").set_scope(id, scope))
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

    fn touch(&self, id: &str, at: UnixMillis, ip: Option<&str>) -> Result<()> {
        self.mutate(|d| Ok(d.touch(id, at, ip)))?;
        Ok(())
    }

    fn rename(&self, id: &str, name: &str) -> Result<Option<String>> {
        self.mutate(|d| Ok(d.rename(id, name)))
    }

    fn set_scope(&self, id: &str, scope: Scope) -> Result<Option<Scope>> {
        self.mutate(|d| Ok(d.set_scope(id, scope)))
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
        reg.touch(&id1, 50, Some("100.64.0.7")).unwrap();
        let d = reg.get(&id1).unwrap().unwrap();
        assert_eq!(d.last_seen, Some(50));
        assert_eq!(d.last_ip.as_deref(), Some("100.64.0.7"));
        // Rename and scope changes report the previous value.
        assert_eq!(
            reg.rename(&id1, "Work phone").unwrap().as_deref(),
            Some("Phone")
        );
        assert_eq!(
            reg.set_scope(&id1, Scope::Admin).unwrap(),
            Some(Scope::Command)
        );
        let d = reg.get(&id1).unwrap().unwrap();
        assert_eq!((d.name.as_str(), d.scope), ("Work phone", Scope::Admin));
        assert_eq!(reg.rename("dev_missing", "x").unwrap(), None);
        assert_eq!(reg.set_scope("dev_missing", Scope::View).unwrap(), None);
        assert!(reg.revoke(&id1, 60).unwrap());
        // A revoked device cannot be renamed or re-scoped.
        assert_eq!(reg.rename(&id1, "Again").unwrap(), None);
        assert_eq!(reg.set_scope(&id1, Scope::View).unwrap(), None);
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
    fn device_name_validation() {
        assert_eq!(validate_device_name("  iPad  ").unwrap(), "iPad");
        assert_eq!(
            validate_device_name(&"가".repeat(64))
                .unwrap()
                .chars()
                .count(),
            64
        );
        assert!(validate_device_name("   ").is_err());
        assert!(validate_device_name(&"a".repeat(65)).is_err());
        assert!(validate_device_name("bad\u{1b}[31mname").is_err());
        assert!(validate_device_name("two\nlines").is_err());
    }

    #[test]
    fn old_registry_files_load_without_last_ip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let reg = FileDeviceRegistry::open(&path).unwrap();
        let key = *StaticKeypair::generate().public();
        reg.register(DeviceRecord::new("Phone".into(), key, Scope::View, 1))
            .unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for d in json["devices"].as_object_mut().unwrap().values_mut() {
            d.as_object_mut().unwrap().remove("last_ip");
        }
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let reopened = FileDeviceRegistry::open(&path).unwrap();
        assert_eq!(reopened.list().unwrap()[0].last_ip, None);
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
        // Answering a question form is a decision: command scope, like
        // answering a permission prompt.
        let answer = ClientMsg::SendInput {
            session_id: "s".into(),
            input: vca_core::Input::Answer {
                decision_id: "claude:abc:toolu_1".into(),
                answers: vec![vca_core::QuestionAnswer {
                    options: vec![0],
                    text: None,
                }],
            },
        };
        assert!(!Scope::View.permits(&answer));
        assert!(Scope::Command.permits(&answer));
        assert_eq!(Scope::required_for(&answer), Scope::Command);
        assert!(Scope::View.permits(&ClientMsg::ListSessions));
        assert!(Scope::View.permits(&ClientMsg::ListCloudProviders));
        // Scanning shows; hiding and listing again change the list.
        assert!(Scope::View.permits(&ClientMsg::ScanSessions));
        for msg in [
            ClientMsg::HideObserved {
                session_id: None,
                pid: Some(1),
            },
            ClientMsg::UnhideObserved {
                session_id: None,
                pid: None,
                all: true,
            },
        ] {
            assert_eq!(Scope::required_for(&msg), Scope::Command);
            assert!(!Scope::View.permits(&msg));
        }
        let transcribe = ClientMsg::Transcribe {
            audio: vec![0; 4],
            format: vca_core::AudioEncoding::Wav,
            sample_rate: None,
            language: None,
            session_id: None,
            context: None,
            is_final: false,
        };
        assert!(!Scope::View.permits(&transcribe));
        assert!(Scope::Command.permits(&transcribe));
        // Device management is admin-only.
        for msg in [
            ClientMsg::RevokeDevice {
                device_id: "d".into(),
            },
            ClientMsg::RenameDevice {
                device_id: "d".into(),
                name: "n".into(),
            },
            ClientMsg::SetDeviceScope {
                device_id: "d".into(),
                scope: DeviceScope::Admin,
            },
        ] {
            assert_eq!(Scope::required_for(&msg), Scope::Admin);
            assert!(!Scope::Command.permits(&msg));
            assert!(Scope::Admin.permits(&msg));
        }
        assert_eq!(Scope::from(DeviceScope::Command), Scope::Command);
        assert_eq!(DeviceScope::from(Scope::View), DeviceScope::View);
        assert!(Scope::View.permits(&ClientMsg::ListSpeechOptions));
        assert!(Scope::View.permits(&ClientMsg::ListTtsEngines));
        assert!(Scope::View.permits(&ClientMsg::ListPronunciations { language: None }));
        assert!(Scope::View.permits(&ClientMsg::ListPersonalTerms));
        let save_terms = ClientMsg::SavePersonalTerms { terms: Vec::new() };
        assert!(!Scope::View.permits(&save_terms));
        assert!(!Scope::Command.permits(&save_terms));
        assert!(Scope::Admin.permits(&save_terms));
        for teach in [
            ClientMsg::SetPronunciation {
                term: "SkyHarbor".into(),
                spoken: "스카이하버".into(),
                language: Some("ko".into()),
            },
            ClientMsg::DeletePronunciation {
                term: "SkyHarbor".into(),
                language: None,
            },
        ] {
            assert!(!Scope::View.permits(&teach), "{teach:?}");
            assert!(Scope::Command.permits(&teach), "{teach:?}");
        }
        let say = ClientMsg::Synthesize {
            text: "Session needs a decision".into(),
            language: None,
            engine: None,
            voice: None,
        };
        assert_eq!(Scope::required_for(&say), Scope::View);
        for voice in [
            ClientMsg::Briefing {
                language: Some("ko".into()),
                order: Vec::new(),
            },
            ClientMsg::SetVoiceSummary {
                enabled: true,
                details: false,
            },
            ClientMsg::Summarize {
                session_id: "s1".into(),
                language: None,
                number: Some(3),
            },
        ] {
            assert_eq!(Scope::required_for(&voice), Scope::View, "{voice:?}");
        }
        let prefs = ClientMsg::SetSpeechPreferences {
            engine: Some("whisper".into()),
            llm: Some("none".into()),
        };
        assert_eq!(Scope::required_for(&prefs), Scope::Command);
        assert!(!Scope::View.permits(&prefs));
        assert!(Scope::Command.permits(&prefs));
        assert!(Scope::Admin.permits(&prefs));
        let rename = ClientMsg::RenameSession {
            session_id: "s".into(),
            name: Some("API work".into()),
        };
        assert!(!Scope::View.permits(&rename));
        assert!(Scope::Command.permits(&rename));
        let dismiss = ClientMsg::DismissSession {
            session_id: "s".into(),
            stop: false,
        };
        assert_eq!(Scope::required_for(&dismiss), Scope::Command);
        assert!(!Scope::View.permits(&dismiss));
        // "Remove from VCA" also stops the agent: the stricter of Stop
        // (`Control`) and dismiss, both `Command`.
        let remove = ClientMsg::DismissSession {
            session_id: "s".into(),
            stop: true,
        };
        let stop = ClientMsg::Control {
            session_id: "s".into(),
            action: vca_core::SessionAction::Stop,
        };
        assert_eq!(
            Scope::required_for(&remove),
            Scope::required_for(&stop).max(Scope::required_for(&dismiss))
        );
        assert!(!Scope::View.permits(&remove));
        assert!(Scope::Command.permits(&remove));
        // Keeping a remote shell open is a shell operation: admin only.
        let keep = ClientMsg::ShellKeepAlive {
            session_id: "s".into(),
        };
        assert_eq!(Scope::required_for(&keep), Scope::Admin);
        assert!(!Scope::Command.permits(&keep));
        // Reading the card order is viewing; changing it is a command.
        assert!(Scope::View.permits(&ClientMsg::GetSessionOrder));
        for order in [
            ClientMsg::SetSessionOrder {
                session_ids: vec!["s".into()],
            },
            ClientMsg::ResetSessionOrder,
        ] {
            assert!(!Scope::View.permits(&order), "{order:?}");
            assert!(Scope::Command.permits(&order), "{order:?}");
        }
        let attach = [
            ClientMsg::Attach {
                session_id: "s".into(),
                name: "a.png".into(),
                mime: "image/png".into(),
                data: vec![0; 4],
                note: None,
                submit: false,
            },
            ClientMsg::AttachBegin {
                session_id: "s".into(),
                name: "a.png".into(),
                mime: "image/png".into(),
                size: 4,
                note: None,
                submit: false,
                folder: None,
                batch: Some("b".into()),
            },
            ClientMsg::AttachChunk {
                upload_id: "u".into(),
                offset: 0,
                data: vec![0; 4],
            },
            ClientMsg::AttachEnd {
                upload_id: "u".into(),
            },
            ClientMsg::AttachCancel {
                upload_id: "u".into(),
            },
            ClientMsg::AttachFolderBegin {
                session_id: "s".into(),
                name: "docs".into(),
                files: 1,
                total_bytes: 4,
                note: None,
                submit: false,
            },
            ClientMsg::AttachFolderEnd {
                folder_id: "f".into(),
            },
            ClientMsg::AttachBatchEnd {
                batch_id: "b".into(),
                note: None,
                submit: false,
            },
        ];
        for msg in &attach {
            assert!(!Scope::View.permits(msg), "{msg:?}");
            assert!(Scope::Command.permits(msg), "{msg:?}");
        }
        assert_eq!(
            serde_json::to_string(&Scope::Command).unwrap(),
            "\"command\""
        );
    }

    #[test]
    fn remote_shells_need_admin() {
        let shell = [
            ClientMsg::OpenShell {
                request: vca_core::ShellRequest {
                    shell: "powershell".into(),
                    project_dir: "/code".into(),
                    display_name: None,
                    totp: "123456".into(),
                },
            },
            ClientMsg::ShellRun {
                session_id: "s".into(),
                command: "ls".into(),
                totp: None,
                confirm: false,
            },
            ClientMsg::ShellJoin {
                session_id: "s".into(),
                totp: "123456".into(),
            },
            ClientMsg::ListAudit { limit: 10 },
        ];
        for msg in &shell {
            assert_eq!(Scope::required_for(msg), Scope::Admin);
            assert!(!Scope::Command.permits(msg));
            assert!(Scope::Admin.permits(msg));
        }
        let classify = ClientMsg::ClassifyCommand {
            command: "rm -rf /".into(),
        };
        assert!(Scope::View.permits(&classify));
    }
}
