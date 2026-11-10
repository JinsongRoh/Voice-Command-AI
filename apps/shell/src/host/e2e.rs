//! End-to-end check against a real vcad, run by hand (it starts a process):
//!
//! ```text
//! cargo build -p vca-server --bin vcad                       # repository root
//! VCA_E2E_VCAD=<abs path to vcad> VCA_CONFIG=<scratch>/config.toml \
//!   VCA_DATA_DIR=<scratch>/data \
//!   cargo test --manifest-path apps/Cargo.toml -p vca-shell e2e -- --ignored
//! ```
//!
//! `VCA_CONFIG` / `VCA_DATA_DIR` must point at scratch locations (they are
//! inherited by the started vcad, which must not touch the real ones); the
//! config should use `bind = "127.0.0.1:0"`, `require_totp = true` and
//! `[discovery] enabled = false`.
//!
//! Gated calls go through the real local verification: on a computer with
//! Windows Hello or Touch ID a person answers each prompt: there is no way
//! around them, by design. Without an OS verifier the test confirms the first enrollment
//! like the desktop app does, and uses the authenticator code afterwards.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;
use vca_client::LinkState;
use vca_core::{Envelope, ServerMsg};

use super::*;

struct TestSink {
    status: mpsc::UnboundedSender<LinkStatus>,
    message: Mutex<mpsc::UnboundedSender<Vec<u8>>>,
}

impl LinkSink for TestSink {
    fn status(&self, status: LinkStatus) {
        let _ = self.status.send(status);
    }
    fn message(&self, msgpack: Vec<u8>) {
        let _ = lock(&self.message).send(msgpack);
    }
}

async fn next_reply(rx: &mut mpsc::UnboundedReceiver<Vec<u8>>, id: u64) -> ServerMsg {
    loop {
        let bytes = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("reply in time")
            .expect("sink open");
        let env: Envelope<ServerMsg> = rmp_serde::from_slice(&bytes).unwrap();
        if env.id == id {
            return env.body;
        }
    }
}

fn totp_now(secret_base32: &str) -> String {
    let secret = data_encoding::BASE32_NOPAD
        .decode(secret_base32.trim_end_matches('=').as_bytes())
        .expect("base32 secret");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    vca_net::totp::totp_code_at(&secret, now)
}

/// Answers the challenge of the gated call that just failed.
async fn verify(host: &Host, action: &str, secret: Option<&str>) {
    let endpoint = host.endpoint.clone();
    let totp = secret.map(totp_now);
    let confirmed = secret.is_none();
    security::answer_challenge(
        &endpoint,
        action,
        None,
        totp,
        confirmed,
        Some("en".to_owned()),
    )
    .await
    .expect("verification");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "starts a real vcad; see the module docs"]
async fn e2e_with_a_real_vcad() {
    let vcad = PathBuf::from(std::env::var_os("VCA_E2E_VCAD").expect("VCA_E2E_VCAD"));
    assert!(
        std::env::var_os("VCA_CONFIG").is_some() && std::env::var_os("VCA_DATA_DIR").is_some(),
        "set VCA_CONFIG and VCA_DATA_DIR to scratch locations"
    );
    let dir = tempfile::tempdir().unwrap();
    let endpoint = if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\vca-e2e-{}", std::process::id()))
    } else {
        dir.path().join("control.sock")
    };
    let host = Host::with(dir.path().to_owned(), endpoint.clone(), Some(vcad.clone()));

    let s = host.status().await;
    assert!(!s.running && s.can_start && !s.managed, "{s:?}");

    // Start the bundled server.
    let s = host.start().await.expect("start");
    assert!(s.running && s.managed, "{s:?}");
    let remote = s.remote.clone().expect("remote status");
    assert!(remote.enabled && remote.require_totp && !remote.totp_enrolled);
    assert!(s.version.is_some() && s.server_name.is_some());

    // Pairing waits for TOTP enrollment.
    let err = host.pair(PairScope::Command, None).await.unwrap_err();
    assert_eq!(err.code, "totp_not_enrolled");
    // Nothing runs before the local verification.
    let err = host.totp_begin(false).await.unwrap_err();
    assert_eq!(err.code, "step_up_required");
    verify(&host, "totp_setup", None).await;
    let enrollment = host.totp_begin(false).await.expect("totp begin");
    assert!(enrollment.otpauth_uri.starts_with("otpauth://totp/"));
    assert!(enrollment.qr_svg.contains("<svg"));
    let err = host.totp_confirm("000000x", None).await.unwrap_err();
    assert_eq!(err.code, "totp_invalid");
    let done = host
        .totp_confirm(&totp_now(&enrollment.secret), None)
        .await
        .expect("totp confirm");
    assert!(!done.backup_codes.is_empty());
    assert_eq!(
        host.totp_begin(false).await.unwrap_err().code,
        "totp_enrolled"
    );
    // Setting up again is explicit; the status shows the enrollment.
    let err = host.totp_begin(true).await.unwrap_err();
    assert_eq!(err.code, "step_up_required");
    tokio::time::sleep(Duration::from_secs(31)).await;
    verify(&host, "totp_reset", Some(&enrollment.secret)).await;
    let again = host.totp_begin(true).await.expect("totp begin again");
    assert_ne!(again.secret, enrollment.secret);
    let remote = host.status().await.remote.expect("remote status");
    assert!(remote.totp_enrolled_at.is_some());
    assert_eq!(remote.backup_codes_remaining, done.backup_codes.len());
    let err = host.totp_backup_codes(None).await.unwrap_err();
    assert_eq!(err.code, "step_up_required");
    tokio::time::sleep(Duration::from_secs(31)).await;
    verify(&host, "backup_codes_export", Some(&enrollment.secret)).await;
    // One approval is not enough: new codes need the authenticator too.
    let err = host.totp_backup_codes(None).await.unwrap_err();
    assert_eq!(err.code, "current_code_required");
    let fresh = host
        .totp_backup_codes(Some(done.backup_codes[0].as_str()))
        .await
        .expect("new backup codes");
    assert_eq!(fresh.backup_codes.len(), done.backup_codes.len());

    // A pairing code for a phone, with the code to compare on it.
    let err = host.pair(PairScope::Command, None).await.unwrap_err();
    assert_eq!(err.code, "step_up_required");
    tokio::time::sleep(Duration::from_secs(31)).await;
    verify(&host, "pairing_create", Some(&enrollment.secret)).await;
    let p = host.pair(PairScope::Command, None).await.expect("pair");
    assert!(p.uri.starts_with("vca://pair?"));
    assert_eq!(p.confirm_code.len(), 19);
    // A token is good for one request.
    let err = host.pair(PairScope::Command, None).await.unwrap_err();
    assert_eq!(err.code, "step_up_required");
    assert!(p.loopback_only, "bound to 127.0.0.1: {:?}", p.endpoints);
    assert!(p.qr_svg.contains("<svg"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(p.expires_at > now && p.expires_at <= now + 301);

    // "This PC" over the local channel.
    host.ready.send_replace(true);
    let machine = host.local_machine().await.expect("listed once seen");
    assert_eq!(machine.id, LOCAL_MACHINE_ID);
    let (status_tx, mut status_rx) = mpsc::unbounded_channel();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel();
    host.connect_link(Arc::new(TestSink {
        status: status_tx,
        message: Mutex::new(msg_tx),
    }));
    loop {
        let st = tokio::time::timeout(Duration::from_secs(10), status_rx.recv())
            .await
            .expect("status in time")
            .unwrap();
        if st.state == LinkState::Connected {
            break;
        }
    }
    let hello = rmp_serde::to_vec_named(&Envelope {
        id: 41,
        body: ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: "e2e".into(),
        },
    })
    .unwrap();
    host.link_send(&hello).await.expect("send hello");
    assert!(matches!(
        next_reply(&mut msg_rx, 41).await,
        ServerMsg::Welcome { .. }
    ));
    let list = rmp_serde::to_vec_named(&Envelope {
        id: 42,
        body: ClientMsg::ListSessions,
    })
    .unwrap();
    host.link_send(&list).await.expect("send list");
    assert!(matches!(
        next_reply(&mut msg_rx, 42).await,
        ServerMsg::Sessions { .. }
    ));
    host.link_disconnect();

    // After an app restart the record still proves ownership.
    let again = Host::with(dir.path().to_owned(), endpoint.clone(), Some(vcad.clone()));
    let s = again.status().await;
    assert!(s.running && s.managed, "{s:?}");
    // A host without the bundled binary never claims it.
    let other = Host::with(dir.path().to_owned(), endpoint.clone(), None);
    assert!(!other.status().await.managed);
    assert_eq!(other.stop().await.unwrap_err().code, "not_owned");

    // Graceful stop through the control channel.
    let started = std::time::Instant::now();
    let s = again.stop().await.expect("stop");
    assert!(!s.running && !s.managed, "{s:?}");
    assert!(
        started.elapsed() < STOP_TIMEOUT,
        "stopped without the kill fallback"
    );
    let log = std::fs::read_to_string(dir.path().join("vcad.log")).unwrap_or_default();
    assert!(log.contains("shutdown requested"), "{log}");
    assert!(host.sidecar.owned_pid().is_none());
}
