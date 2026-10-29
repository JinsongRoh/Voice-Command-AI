//! `machines.json` (non-secret metadata) and key-store naming.
//!
//! Integrity of the pinned server key: each entry stores the server's
//! public key next to its id, and the id must equal the id derived from the
//! key (`vca_net::pairing::server_id`). Secrets in the key store are named
//! after the id, so swapping the key in the file without the matching id
//! leaves the entry unusable instead of redirecting the device.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vca_net::PublicKey;
use vca_net::pairing::server_id;

use crate::MachineInfo;
use crate::error::ClientError;

/// File name of the metadata file inside the hub's data directory.
pub const MACHINES_FILE: &str = "machines.json";

const FILE_VERSION: u32 = 1;
const SERVER_ID_LEN: usize = 16;

/// True when `id` has the form of a server id: 16 lowercase hex characters.
pub fn is_valid_server_id(id: &str) -> bool {
    id.len() == SERVER_ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Key-store name of the device static private key for a server.
/// `server_id` must be valid (see [`is_valid_server_id`]).
pub fn device_key_name(server_id: &str) -> String {
    format!("dev-{server_id}")
}

/// Key-store name of the refresh token for a server.
/// `server_id` must be valid (see [`is_valid_server_id`]).
pub fn refresh_token_name(server_id: &str) -> String {
    format!("rt-{server_id}")
}

/// Removes control characters, limits the length and trims.
pub(crate) fn sanitize_display(s: &str, max_chars: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

/// One entry of `machines.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredMachine {
    #[serde(flatten)]
    pub info: MachineInfo,
    /// Pinned server static key (public, not secret).
    pub server_key: PublicKey,
}

impl StoredMachine {
    fn is_consistent(&self) -> bool {
        is_valid_server_id(&self.info.id) && server_id(&self.server_key) == self.info.id
    }
}

#[derive(Serialize, Deserialize)]
struct MachinesFile {
    version: u32,
    machines: Vec<StoredMachine>,
}

pub(crate) fn machines_path(dir: &Path) -> PathBuf {
    dir.join(MACHINES_FILE)
}

/// Creates the data directory (owner-only on Unix).
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), ClientError> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| ClientError::Storage(format!("cannot create {}: {e}", dir.display())))
}

/// Loads the machines; a missing file is an empty list. Inconsistent
/// entries are skipped with a warning.
pub(crate) fn load(dir: &Path) -> Result<Vec<StoredMachine>, ClientError> {
    let path = machines_path(dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ClientError::Storage(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let file: MachinesFile = serde_json::from_str(&text)
        .map_err(|e| ClientError::Storage(format!("corrupt {}: {e}", path.display())))?;
    if file.version != FILE_VERSION {
        return Err(ClientError::Storage(format!(
            "unsupported {} version {}",
            path.display(),
            file.version
        )));
    }
    let mut out: Vec<StoredMachine> = Vec::with_capacity(file.machines.len());
    for m in file.machines {
        if !m.is_consistent() {
            tracing::warn!(
                id = %m.info.id,
                "ignoring machine entry whose id does not match its server key"
            );
            continue;
        }
        if out.iter().any(|o| o.info.id == m.info.id) {
            continue;
        }
        out.push(m);
    }
    Ok(out)
}

/// Writes the machines atomically (temporary file + rename), owner-only on
/// Unix.
pub(crate) fn save(dir: &Path, machines: &[StoredMachine]) -> Result<(), ClientError> {
    create_private_dir(dir)?;
    let file = MachinesFile {
        version: FILE_VERSION,
        machines: machines.to_vec(),
    };
    let data = serde_json::to_vec_pretty(&file).map_err(ClientError::storage)?;
    let path = machines_path(dir);
    let tmp = dir.join(format!(
        ".{MACHINES_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut f = opts.open(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(ClientError::Storage(format!(
            "cannot write {}: {e}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_net::{Scope, StaticKeypair};

    fn machine(key: &StaticKeypair) -> StoredMachine {
        StoredMachine {
            info: MachineInfo {
                id: server_id(key.public()),
                name: "Desk".into(),
                endpoints: vec!["ws://127.0.0.1:1/v1/ws".into()],
                device_id: "dev1".into(),
                scope: Scope::Command,
                paired_at: 1,
            },
            server_key: *key.public(),
        }
    }

    #[test]
    fn server_id_validation() {
        assert!(is_valid_server_id("0123456789abcdef"));
        assert!(!is_valid_server_id("0123456789ABCDEF"));
        assert!(!is_valid_server_id("0123456789abcde"));
        assert!(!is_valid_server_id("0123456789abcdef0"));
        assert!(!is_valid_server_id("0123456789abcdeg"));
        assert!(!is_valid_server_id("../../etc/passwd"));
        assert!(!is_valid_server_id(""));
        let key = StaticKeypair::generate();
        assert!(is_valid_server_id(&server_id(key.public())));
        assert_eq!(device_key_name("0123456789abcdef"), "dev-0123456789abcdef");
        assert_eq!(
            refresh_token_name("0123456789abcdef"),
            "rt-0123456789abcdef"
        );
    }

    #[test]
    fn sanitize_strips_control_characters() {
        assert_eq!(sanitize_display(" a\u{1b}[31mb\n ", 64), "a[31mb");
        assert_eq!(sanitize_display("abcdef", 3), "abc");
    }

    #[test]
    fn roundtrip_and_inconsistent_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_empty());
        let a = machine(&StaticKeypair::generate());
        let mut forged = machine(&StaticKeypair::generate());
        forged.server_key = *StaticKeypair::generate().public();
        save(dir.path(), &[a.clone(), forged]).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].info, a.info);
        let text = std::fs::read_to_string(machines_path(dir.path())).unwrap();
        assert!(text.contains("\"serverKey\""), "{text}");
        assert!(text.contains("\"deviceId\""), "{text}");
        // No temporary files are left behind.
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(machines_path(dir.path()), b"{not json").unwrap();
        assert!(matches!(load(dir.path()), Err(ClientError::Storage(_))));
    }

    #[cfg(unix)]
    #[test]
    fn file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("data");
        save(&sub, &[machine(&StaticKeypair::generate())]).unwrap();
        let mode = std::fs::metadata(machines_path(&sub))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mode = std::fs::metadata(&sub).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
