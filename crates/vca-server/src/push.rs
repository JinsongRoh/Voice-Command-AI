//! Phones registered for approval alerts (push).
//!
//! A paired phone hands its APNs or FCM token and its X25519 public key to
//! vcad with [`ClientMsg::PushRegister`](vca_core::ClientMsg::PushRegister).
//! vcad only keeps them: it holds no DNSPilot login and never sends. The
//! desktop app of this computer reads them over the owner-only local channel
//! ([`ClientMsg::ListPushPhones`](vca_core::ClientMsg::ListPushPhones)) and
//! is the only sender (`vca_client::push`).
//!
//! * One registration per paired device, keyed by the device id of the
//!   authenticated connection; a client never names the phone.
//! * The file is private (`0600` on Unix) and written atomically.
//! * Tokens are never logged: [`PushToken`]'s `Debug` hides them and no
//!   error here repeats one.
//! * A revoked device's registration is deleted with the revocation and,
//!   as a backstop, whenever the desktop app lists the phones.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vca_core::error::ErrorCode;
use vca_core::{Error, PushPhone, PushPlatform, PushToken};

use crate::util::lock;

/// Directory below the data directory.
pub const PUSH_DIR: &str = "push";
/// The registrations, below [`PUSH_DIR`].
pub const PHONES_FILE: &str = "phones.json";

const FILE_VERSION: u32 = 1;
/// Longest device token accepted. APNs tokens are 64 hex characters and FCM
/// tokens a few hundred; anything near this is not a token.
pub const MAX_TOKEN_CHARS: usize = 4096;
/// Longest machine id accepted.
pub const MAX_MACHINE_CHARS: usize = 128;
/// DNSPilot takes a locale of at most 8 characters.
const MAX_LOCALE_CHARS: usize = 8;

/// What a phone sends, before the phone id is attached.
#[derive(Debug, Clone)]
pub struct PushRegistration {
    pub machine: String,
    pub platform: PushPlatform,
    pub token: PushToken,
    pub e2e_pubkey: String,
    pub locale: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PhonesFile {
    version: u32,
    #[serde(default)]
    phones: Vec<PushPhone>,
}

/// The registered phones. Without a path they live in memory only (tests).
pub struct PushStore {
    path: Option<PathBuf>,
    phones: Mutex<BTreeMap<String, PushPhone>>,
}

fn invalid(message: &str) -> Error {
    Error::api(ErrorCode::InvalidRequest, message)
}

/// The primary language subtag when the whole tag is too long; `None` for
/// anything that is not a plain language tag.
fn normalize_locale(locale: Option<String>) -> Option<String> {
    let locale = locale?;
    let locale = locale.trim();
    if locale.is_empty()
        || !locale
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    if locale.len() <= MAX_LOCALE_CHARS {
        return Some(locale.to_owned());
    }
    let primary = locale.split(['-', '_']).next().unwrap_or_default();
    (!primary.is_empty() && primary.len() <= MAX_LOCALE_CHARS).then(|| primary.to_owned())
}

/// Checks a registration. Error texts never quote the token.
fn validate(reg: PushRegistration) -> Result<PushRegistration, Error> {
    let machine = reg.machine.trim();
    if machine.is_empty()
        || machine.chars().count() > MAX_MACHINE_CHARS
        || !machine.chars().all(|c| c.is_ascii_graphic())
    {
        return Err(invalid("the machine id is not valid"));
    }
    let token = reg.token.expose().trim();
    if token.is_empty()
        || token.len() > MAX_TOKEN_CHARS
        || !token.chars().all(|c| c.is_ascii_graphic())
    {
        return Err(invalid("the device token is not valid"));
    }
    let key = reg.e2e_pubkey.trim();
    if vca_net::PublicKey::from_base64url(key).is_err() {
        return Err(invalid("the push key is not a valid public key"));
    }
    Ok(PushRegistration {
        machine: machine.to_owned(),
        platform: reg.platform,
        token: PushToken(token.to_owned()),
        e2e_pubkey: key.to_owned(),
        locale: normalize_locale(reg.locale),
    })
}

fn load(path: &Path) -> BTreeMap<String, PushPhone> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<PhonesFile>(&bytes) {
        Ok(file) if file.version == FILE_VERSION => file
            .phones
            .into_iter()
            .map(|p| (p.phone.clone(), p))
            .collect(),
        // The error text could quote the file, so it is not logged.
        _ => {
            tracing::warn!("ignoring an unreadable {PHONES_FILE}; phones register again");
            BTreeMap::new()
        }
    }
}

/// Temporary file, then rename: a crash leaves the old or the new list.
fn save(path: &Path, phones: &BTreeMap<String, PushPhone>) -> std::io::Result<()> {
    let file = PhonesFile {
        version: FILE_VERSION,
        phones: phones.values().cloned().collect(),
    };
    let data = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{PHONES_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

impl PushStore {
    /// `data_dir/push/phones.json`.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::open(data_dir.join(PUSH_DIR).join(PHONES_FILE))
    }

    pub fn open(path: PathBuf) -> Self {
        let phones = Mutex::new(load(&path));
        Self {
            path: Some(path),
            phones,
        }
    }

    pub fn in_memory() -> Self {
        Self {
            path: None,
            phones: Mutex::new(BTreeMap::new()),
        }
    }

    /// Applies `f` to a copy and keeps it once it is on disk.
    fn update<T>(&self, f: impl FnOnce(&mut BTreeMap<String, PushPhone>) -> T) -> Result<T, Error> {
        let mut phones = lock(&self.phones);
        let mut next = phones.clone();
        let out = f(&mut next);
        if let Some(path) = &self.path {
            save(path, &next).map_err(|e| {
                tracing::warn!("cannot store the alert phones: {}", e.kind());
                Error::api(ErrorCode::Internal, "cannot store the alert registration")
            })?;
        }
        *phones = next;
        Ok(out)
    }

    /// Registers (or replaces) the alert registration of `device_id`, the
    /// authenticated device of the connection.
    pub fn register(&self, device_id: &str, reg: PushRegistration) -> Result<(), Error> {
        let reg = validate(reg)?;
        let phone = PushPhone {
            phone: device_id.to_owned(),
            machine: reg.machine,
            platform: reg.platform,
            token: reg.token,
            e2e_pubkey: reg.e2e_pubkey,
            locale: reg.locale,
        };
        if lock(&self.phones).get(device_id) == Some(&phone) {
            return Ok(());
        }
        self.update(|p| {
            p.insert(device_id.to_owned(), phone);
        })?;
        tracing::info!(device = device_id, "phone registered for approval alerts");
        Ok(())
    }

    /// Removes the registration of `device_id`. `false` when there was none.
    pub fn remove(&self, device_id: &str) -> Result<bool, Error> {
        if !lock(&self.phones).contains_key(device_id) {
            return Ok(false);
        }
        self.update(|p| p.remove(device_id).is_some())
    }

    /// Keeps the phones `keep` accepts (the active paired devices); returns
    /// how many were deleted.
    pub fn retain(&self, keep: impl Fn(&str) -> bool) -> Result<usize, Error> {
        let gone: Vec<String> = lock(&self.phones)
            .keys()
            .filter(|id| !keep(id))
            .cloned()
            .collect();
        if gone.is_empty() {
            return Ok(0);
        }
        self.update(|p| {
            for id in &gone {
                p.remove(id);
            }
        })?;
        Ok(gone.len())
    }

    pub fn list(&self) -> Vec<PushPhone> {
        lock(&self.phones).values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        vca_net::StaticKeypair::generate().public().to_base64url()
    }

    fn reg(token: &str) -> PushRegistration {
        PushRegistration {
            machine: "0123456789abcdef".into(),
            platform: PushPlatform::Ios,
            token: PushToken(token.into()),
            e2e_pubkey: key(),
            locale: Some("ko-KR".into()),
        }
    }

    #[test]
    fn one_registration_per_device_and_replace_on_resend() {
        let store = PushStore::in_memory();
        store.register("dev_a", reg("tok-a1")).unwrap();
        store.register("dev_a", reg("tok-a2")).unwrap();
        store.register("dev_b", reg("tok-b")).unwrap();
        let phones = store.list();
        assert_eq!(phones.len(), 2);
        let a = phones.iter().find(|p| p.phone == "dev_a").unwrap();
        assert_eq!(a.token.expose(), "tok-a2");
        assert_eq!(a.locale.as_deref(), Some("ko-KR"));
        assert!(store.remove("dev_a").unwrap());
        assert!(!store.remove("dev_a").unwrap());
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn bad_registrations_are_refused_without_quoting_the_token() {
        let store = PushStore::in_memory();
        let mut bad_key = reg("secret-token");
        bad_key.e2e_pubkey = "short".into();
        let e = store.register("dev_a", bad_key).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidRequest);
        assert!(!e.to_string().contains("secret-token"));
        let e = store.register("dev_a", reg("has space")).unwrap_err();
        assert!(!e.to_string().contains("has space"));
        let long = "x".repeat(MAX_TOKEN_CHARS + 1);
        assert!(store.register("dev_a", reg(&long)).is_err());
        let mut bad_machine = reg("tok");
        bad_machine.machine = " ".into();
        assert!(store.register("dev_a", bad_machine).is_err());
        assert!(store.list().is_empty());
    }

    #[test]
    fn locales_are_cut_to_what_dnspilot_takes() {
        assert_eq!(normalize_locale(Some("en".into())).as_deref(), Some("en"));
        assert_eq!(
            normalize_locale(Some("zh-Hant-TW".into())).as_deref(),
            Some("zh")
        );
        assert_eq!(normalize_locale(Some("../x".into())), None);
        assert_eq!(normalize_locale(None), None);
    }

    #[test]
    fn retain_drops_revoked_devices() {
        let store = PushStore::in_memory();
        store.register("dev_a", reg("tok-a")).unwrap();
        store.register("dev_b", reg("tok-b")).unwrap();
        assert_eq!(store.retain(|id| id == "dev_b").unwrap(), 1);
        let phones = store.list();
        assert_eq!(phones.len(), 1);
        assert_eq!(phones[0].phone, "dev_b");
        assert_eq!(store.retain(|_| true).unwrap(), 0);
    }

    #[test]
    fn the_file_persists_privately_and_bad_files_mean_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = PushStore::in_data_dir(dir.path());
        store.register("dev_a", reg("tok-a")).unwrap();
        let path = dir.path().join(PUSH_DIR).join(PHONES_FILE);
        let again = PushStore::in_data_dir(dir.path());
        assert_eq!(again.list(), store.list());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(again.remove("dev_a").unwrap());
        assert!(PushStore::in_data_dir(dir.path()).list().is_empty());
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(PushStore::in_data_dir(dir.path()).list().is_empty());
        // No temporary files are left behind.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(PHONES_FILE)]);
    }

    #[test]
    fn debug_output_hides_tokens() {
        let store = PushStore::in_memory();
        store.register("dev_a", reg("very-secret-token")).unwrap();
        assert!(!format!("{:?}", store.list()).contains("very-secret-token"));
        assert!(!format!("{:?}", reg("very-secret-token")).contains("very-secret-token"));
    }
}

/// The push messages through [`Api::handle`](crate::api::Api::handle).
#[cfg(test)]
mod api_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::mpsc;
    use vca_core::{ClientMsg, Envelope, PairedDevice, ServerMsg};
    use vca_net::Scope;

    use super::*;
    use crate::api::{Api, Client, DeviceAdmin, DeviceIdentity};
    use crate::events::LogLimits;
    use crate::sessions::{ManagerConfig, SessionManager};
    use crate::workspace::WorkspaceRoots;

    /// Paired devices in memory.
    #[derive(Default)]
    struct Devices(Mutex<HashMap<String, Scope>>);

    impl DeviceAdmin for Devices {
        fn list_devices(&self, _: Option<&str>) -> Result<Vec<PairedDevice>, Error> {
            Ok(Vec::new())
        }
        fn revoke_device(&self, device_id: &str) -> Result<bool, Error> {
            Ok(lock(&self.0).remove(device_id).is_some())
        }
        fn trip_kill_switch(&self, _: &str) -> Result<(), Error> {
            Ok(())
        }
        fn rename_device(&self, _: &str, _: &str, _: &str) -> Result<bool, Error> {
            Ok(false)
        }
        fn set_device_scope(&self, _: &str, _: Scope, _: &str) -> Result<bool, Error> {
            Ok(false)
        }
        fn device_scope(&self, device_id: &str) -> Result<Option<Scope>, Error> {
            Ok(lock(&self.0).get(device_id).copied())
        }
    }

    struct Fixture {
        tmp: tempfile::TempDir,
        api: Api,
        devices: Arc<Devices>,
        push: Arc<PushStore>,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(
            ManagerConfig {
                limits: LogLimits {
                    max_events: 100,
                    max_bytes: 1 << 20,
                },
                exited_retention: Duration::from_secs(60),
                pause_scope: vca_broker::contain::Scope::ChildrenOnly,
                heuristics_interval: Duration::from_millis(500),
                control_endpoint: tmp.path().join("ctl"),
                attach_dir: None,
                workspace_roots: WorkspaceRoots::new([tmp.path().to_owned()]),
                session_names: None,
                allow_generic_launch: false,
            },
            None,
        );
        let api = Api::new(mgr, "test".into(), Duration::from_secs(60));
        let devices = Arc::new(Devices::default());
        for (id, scope) in [
            ("dev_view", Scope::View),
            ("dev_b", Scope::Command),
            ("dev_admin", Scope::Admin),
        ] {
            lock(&devices.0).insert(id.into(), scope);
        }
        api.set_device_admin(Arc::clone(&devices) as Arc<dyn DeviceAdmin>);
        let push = Arc::new(PushStore::in_data_dir(tmp.path()));
        api.set_push(Arc::clone(&push));
        Fixture {
            tmp,
            api,
            devices,
            push,
        }
    }

    type Conn = (Arc<Client>, mpsc::Receiver<Envelope<ServerMsg>>);

    fn device(f: &Fixture, id: &str) -> Conn {
        let scope = lock(&f.devices.0)[id];
        f.api.connect_as(
            "remote",
            Some(DeviceIdentity {
                device_id: id.into(),
                scope,
            }),
        )
    }

    async fn call(f: &Fixture, conn: &mut Conn, body: ClientMsg) -> ServerMsg {
        f.api.handle(&conn.0, Envelope { id: 7, body }).await;
        loop {
            let env = tokio::time::timeout(Duration::from_secs(5), conn.1.recv())
                .await
                .expect("no answer")
                .expect("closed");
            if env.id == 7 {
                return env.body;
            }
        }
    }

    fn register(token: &str) -> ClientMsg {
        ClientMsg::PushRegister {
            machine: "0123456789abcdef".into(),
            platform: PushPlatform::Android,
            token: PushToken(token.into()),
            e2e_pubkey: vca_net::StaticKeypair::generate().public().to_base64url(),
            locale: Some("en".into()),
        }
    }

    fn error_code(msg: &ServerMsg) -> Option<ErrorCode> {
        match msg {
            ServerMsg::Error { code, .. } => Some(*code),
            _ => None,
        }
    }

    async fn phones(f: &Fixture) -> Vec<PushPhone> {
        let mut local = f.api.connect("local");
        match call(f, &mut local, ClientMsg::ListPushPhones).await {
            ServerMsg::PushPhones { phones } => phones,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn registrations_are_scoped_to_the_connections_device() {
        let f = fixture();
        let mut view = device(&f, "dev_view");
        let mut b = device(&f, "dev_b");
        // A view-only phone may get alerts too.
        assert_eq!(
            call(&f, &mut view, register("tok-view")).await,
            ServerMsg::Ok
        );
        assert_eq!(call(&f, &mut b, register("tok-b")).await, ServerMsg::Ok);
        let list = phones(&f).await;
        let ids: Vec<_> = list.iter().map(|p| p.phone.as_str()).collect();
        assert_eq!(ids, ["dev_b", "dev_view"]);
        assert_eq!(list[0].token.expose(), "tok-b");

        // Unregistering removes only the asking device's registration.
        assert_eq!(
            call(&f, &mut b, ClientMsg::PushUnregister).await,
            ServerMsg::Ok
        );
        let ids: Vec<_> = phones(&f).await.into_iter().map(|p| p.phone).collect();
        assert_eq!(ids, ["dev_view"]);
        // Again: still fine.
        assert_eq!(
            call(&f, &mut b, ClientMsg::PushUnregister).await,
            ServerMsg::Ok
        );

        // The owner's local channel is not a phone.
        let mut local = f.api.connect("local");
        let reply = call(&f, &mut local, register("tok-local")).await;
        assert_eq!(error_code(&reply), Some(ErrorCode::InvalidRequest));
        // A bad key is refused without quoting the token.
        let mut bad = register("tok-secret");
        if let ClientMsg::PushRegister { e2e_pubkey, .. } = &mut bad {
            *e2e_pubkey = "nope".into();
        }
        let reply = call(&f, &mut b, bad).await;
        assert_eq!(error_code(&reply), Some(ErrorCode::InvalidRequest));
        assert!(!format!("{reply:?}").contains("tok-secret"));
    }

    #[tokio::test]
    async fn only_the_local_channel_lists_the_phones() {
        let f = fixture();
        let mut b = device(&f, "dev_b");
        assert_eq!(call(&f, &mut b, register("tok-b")).await, ServerMsg::Ok);
        // Not even an admin device reads the tokens.
        for id in ["dev_admin", "dev_b", "dev_view"] {
            let mut conn = device(&f, id);
            let reply = call(&f, &mut conn, ClientMsg::ListPushPhones).await;
            assert_eq!(error_code(&reply), Some(ErrorCode::Forbidden), "{id}");
            assert!(!format!("{reply:?}").contains("tok-b"));
        }
        // Nor the development socket.
        let mut dev = f.api.connect("dev-ws");
        let reply = call(&f, &mut dev, ClientMsg::ListPushPhones).await;
        assert_eq!(error_code(&reply), Some(ErrorCode::Forbidden));
        assert_eq!(phones(&f).await.len(), 1);
    }

    #[tokio::test]
    async fn revoking_a_device_deletes_its_registration() {
        let f = fixture();
        let mut view = device(&f, "dev_view");
        let mut b = device(&f, "dev_b");
        assert_eq!(
            call(&f, &mut view, register("tok-view")).await,
            ServerMsg::Ok
        );
        assert_eq!(call(&f, &mut b, register("tok-b")).await, ServerMsg::Ok);

        let mut local = f.api.connect("local");
        let revoke = ClientMsg::RevokeDevice {
            device_id: "dev_b".into(),
        };
        assert_eq!(call(&f, &mut local, revoke).await, ServerMsg::Ok);
        assert!(f.push.list().iter().all(|p| p.phone != "dev_b"));
        // Gone from disk too.
        let on_disk = PushStore::in_data_dir(f.tmp.path()).list();
        assert!(on_disk.iter().all(|p| p.phone != "dev_b"));
        assert!(
            !std::fs::read_to_string(f.tmp.path().join(PUSH_DIR).join(PHONES_FILE))
                .unwrap()
                .contains("tok-b")
        );

        // A revocation that bypassed the API is caught when listing.
        lock(&f.devices.0).remove("dev_view");
        assert!(phones(&f).await.is_empty());
        assert!(f.push.list().is_empty());
    }
}
