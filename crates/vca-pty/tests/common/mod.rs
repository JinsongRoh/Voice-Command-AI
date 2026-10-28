//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use vca_pty::{OutputEvent, OutputSubscription, PtySession, SpawnOptions};

pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Builds `vca-fake-agent` once and returns the path of its binary.
pub fn fake_agent() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir.parent().and_then(Path::parent).unwrap();
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "vca-fake-agent"])
            .current_dir(workspace)
            .status()
            .expect("failed to run cargo build for vca-fake-agent");
        assert!(status.success(), "building vca-fake-agent failed");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target"));
        let path = target
            .join("debug")
            .join(format!("vca-fake-agent{}", std::env::consts::EXE_SUFFIX));
        assert!(path.exists(), "missing {}", path.display());
        path
    })
    .clone()
}

pub fn fake_agent_opts() -> SpawnOptions {
    SpawnOptions::new(fake_agent())
}

/// Collects output text from a subscription until `needle` appears.
pub async fn read_until(sub: &mut OutputSubscription, acc: &mut String, needle: &str) {
    let result = tokio::time::timeout(TIMEOUT, async {
        while !acc.contains(needle) {
            match sub.recv().await {
                Some(OutputEvent::Data(chunk)) => {
                    acc.push_str(&String::from_utf8_lossy(&chunk.data))
                }
                Some(OutputEvent::Gap(_)) => {}
                None => break,
            }
        }
    })
    .await;
    assert!(
        result.is_ok() && acc.contains(needle),
        "did not see {needle:?}; output so far: {acc:?}"
    );
}

/// Waits until the session's output contains `needle`, using a fresh
/// subscription that replays from the start of the buffer.
pub async fn wait_for(session: &PtySession, needle: &str) -> String {
    let mut sub = session.subscribe_from(0);
    let mut acc = String::new();
    read_until(&mut sub, &mut acc, needle).await;
    acc
}

/// Unique attach endpoint for a test.
pub fn endpoint(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        vca_pty::attach::default_endpoint(&format!("test-{}-{name}", std::process::id()))
    } else {
        dir.join(format!("{name}.sock"))
    }
}

/// Builds the `attach` example of this crate and returns its path.
pub fn attach_example() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir.parent().and_then(Path::parent).unwrap();
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "vca-pty", "--example", "attach"])
            .current_dir(workspace)
            .status()
            .expect("failed to run cargo build for the attach example");
        assert!(status.success(), "building the attach example failed");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target"));
        target
            .join("debug")
            .join("examples")
            .join(format!("attach{}", std::env::consts::EXE_SUFFIX))
    })
    .clone()
}
