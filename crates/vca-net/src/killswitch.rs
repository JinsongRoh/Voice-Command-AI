//! Kill switch (SEC-09): drops every remote connection and refuses new
//! ones until reset. The tripped state is persisted, so it survives a
//! restart.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;

use crate::error::Result;
use crate::util::write_private_file;

/// Global kill switch. Cheap to clone; all clones share state.
#[derive(Clone, Debug)]
pub struct KillSwitch {
    tx: Arc<watch::Sender<bool>>,
    path: Option<PathBuf>,
}

const TRIPPED_MARKER: &[u8] = b"tripped\n";

impl KillSwitch {
    /// A non-persistent kill switch (starts reset).
    pub fn in_memory() -> Self {
        Self {
            tx: Arc::new(watch::channel(false).0),
            path: None,
        }
    }

    /// A kill switch persisted at `path`: if the file exists the switch
    /// starts tripped.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let tripped = match std::fs::metadata(&path) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            tx: Arc::new(watch::channel(tripped).0),
            path: Some(path),
        })
    }

    pub fn is_tripped(&self) -> bool {
        *self.tx.borrow()
    }

    /// Trips the switch. Live connections observe it immediately. The
    /// in-memory state is always tripped, even if persisting fails (the
    /// error is returned so it can be reported).
    pub fn trip(&self) -> Result<()> {
        self.tx.send_replace(true);
        if let Some(p) = &self.path {
            write_private_file(p, TRIPPED_MARKER)?;
        }
        Ok(())
    }

    /// Resets the switch, allowing connections again.
    pub fn reset(&self) -> Result<()> {
        if let Some(p) = &self.path {
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.tx.send_replace(false);
        Ok(())
    }

    /// Receiver that observes changes.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }

    /// Resolves once the switch is tripped (immediately if it already is).
    pub async fn tripped(&self) {
        let mut rx = self.subscribe();
        // The sender lives as long as `self`, so this cannot fail.
        let _ = rx.wait_for(|t| *t).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn trip_notifies_subscribers() {
        let ks = KillSwitch::in_memory();
        assert!(!ks.is_tripped());
        let ks2 = ks.clone();
        let waiter = tokio::spawn(async move { ks2.tripped().await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        ks.trip().unwrap();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        assert!(ks.is_tripped());
        ks.reset().unwrap();
        assert!(!ks.is_tripped());
    }

    #[test]
    fn state_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("killswitch");
        let ks = KillSwitch::open(&path).unwrap();
        assert!(!ks.is_tripped());
        ks.trip().unwrap();
        assert!(KillSwitch::open(&path).unwrap().is_tripped());
        ks.reset().unwrap();
        assert!(!KillSwitch::open(&path).unwrap().is_tripped());
        ks.reset().unwrap();
    }
}
