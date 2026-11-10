//! IPC integration tests over a real Unix domain socket.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use vca_broker::admission::{AdmissionController, MachineInfo, SysLiveness};
use vca_broker::clock::{FixedLoad, LoadSample, SystemClock};
use vca_broker::ipc::{IpcClient, IpcEndpoint, IpcRequest, IpcResponse, IpcServer, SlotRequest};
use vca_broker::policy::{MaxJobs, Policy};
use vca_core::AdmissionVerdict;

struct Setup {
    _dir: tempfile::TempDir,
    endpoint: IpcEndpoint,
    admission: Arc<AdmissionController>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn setup(max: u32) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = IpcEndpoint::Path(dir.path().join("run").join("broker.sock"));
    let mut policy = Policy::default();
    policy.admission.max_heavy_jobs = MaxJobs::Fixed(max);
    let admission = Arc::new(AdmissionController::new(
        policy,
        MachineInfo::detect(),
        Arc::new(SystemClock),
        Arc::new(FixedLoad::new(LoadSample {
            cpu_percent: 1.0,
            memory_percent: 10.0,
        })),
        Arc::new(SysLiveness::default()),
    ));
    let server = IpcServer::bind(endpoint.clone(), admission.clone())
        .unwrap()
        .spawn();
    Setup {
        _dir: dir,
        endpoint,
        admission,
        server,
    }
}

fn req(command: &str, session: &str) -> SlotRequest {
    SlotRequest {
        command: command.into(),
        session_id: Some(session.into()),
        ..SlotRequest::default()
    }
}

fn granted(v: &AdmissionVerdict) -> String {
    match v {
        AdmissionVerdict::Grant { slot_id } => slot_id.clone(),
        other => panic!("expected grant, got {other:?}"),
    }
}

async fn eventually(mut cond: impl FnMut() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn queued_client_is_granted_after_release() {
    let s = setup(1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let IpcEndpoint::Path(p) = &s.endpoint else {
            unreachable!()
        };
        let mode = std::fs::metadata(p.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "socket directory is private");
    }

    let mut first = IpcClient::connect(&s.endpoint).await.unwrap();
    first.ping().await.unwrap();
    let slot = granted(
        &first
            .request_slot(req("cargo build", "a"), Duration::from_secs(5))
            .await
            .unwrap(),
    );

    let endpoint = s.endpoint.clone();
    let (pos_tx, mut pos_rx) = tokio::sync::mpsc::unbounded_channel();
    let waiter = tokio::spawn(async move {
        let mut second = IpcClient::connect(&endpoint).await.unwrap();
        second
            .request_slot_with_progress(req("cargo test", "b"), Duration::from_secs(10), |v| {
                let _ = pos_tx.send(v.clone());
            })
            .await
            .unwrap()
    });
    let queued = pos_rx.recv().await.unwrap();
    assert!(matches!(
        queued,
        AdmissionVerdict::Queue { position: 0, .. }
    ));
    assert_eq!(s.admission.queue().len(), 1);

    assert!(first.release(&slot).await.unwrap());
    assert!(!first.release(&slot).await.unwrap());
    let v = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap();
    granted(&v);
    assert_eq!(s.admission.slots()[0].command, "cargo test");
}

#[tokio::test]
async fn light_commands_are_granted_immediately() {
    let s = setup(1);
    let v = vca_broker::ipc::request_slot(&s.endpoint, "git status", None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(granted(&v), vca_broker::admission::LIGHT_SLOT_ID);
    assert!(s.admission.slots().is_empty());
}

#[tokio::test]
async fn held_slot_is_released_on_disconnect() {
    let s = setup(1);
    let mut c = IpcClient::connect(&s.endpoint).await.unwrap();
    let mut r = req("make", "a");
    r.hold = true;
    r.holder_pid = Some(std::process::id());
    granted(&c.request_slot(r, Duration::from_secs(1)).await.unwrap());
    assert_eq!(s.admission.slots().len(), 1);
    drop(c);
    let a = s.admission.clone();
    eventually(move || a.slots().is_empty()).await;
}

#[tokio::test]
async fn disconnect_while_queued_cancels_job() {
    let s = setup(1);
    let mut holder = IpcClient::connect(&s.endpoint).await.unwrap();
    granted(
        &holder
            .request_slot(req("cargo build", "a"), Duration::from_secs(1))
            .await
            .unwrap(),
    );
    let endpoint = s.endpoint.clone();
    let task = tokio::spawn(async move {
        let mut c = IpcClient::connect(&endpoint).await.unwrap();
        c.request_slot(req("cargo test", "b"), Duration::from_secs(60))
            .await
    });
    let a = s.admission.clone();
    eventually(move || a.queue().len() == 1).await;
    task.abort();
    let a = s.admission.clone();
    eventually(move || a.queue().is_empty()).await;
}

#[tokio::test]
async fn timeouts_and_zero_wait() {
    let s = setup(1);
    let mut c = IpcClient::connect(&s.endpoint).await.unwrap();
    granted(
        &c.request_slot(req("cargo build", "a"), Duration::from_secs(1))
            .await
            .unwrap(),
    );
    // wait 0: the queue verdict is final and the job stays queued.
    let v = c
        .request_slot(req("cargo test", "b"), Duration::ZERO)
        .await
        .unwrap();
    let AdmissionVerdict::Queue { job_id, .. } = v else {
        panic!("{v:?}")
    };
    // A short wait times out into a deny and leaves the queue.
    let v = c
        .request_slot(req("cargo test", "c"), Duration::from_millis(100))
        .await
        .unwrap();
    assert!(matches!(v, AdmissionVerdict::Deny { ref reason } if reason.contains("timed out")));
    assert_eq!(s.admission.queue().len(), 1);
    let resp = c
        .call(&IpcRequest::Cancel {
            job_id: job_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(resp, IpcResponse::Ack { found: true });
    assert!(s.admission.queue().is_empty());
}

#[tokio::test]
async fn release_by_key_status_and_classify() {
    let s = setup(2);
    let mut c = IpcClient::connect(&s.endpoint).await.unwrap();
    let mut r = req("npm ci", "a");
    r.key = Some("toolu_1".into());
    let slot = granted(&c.request_slot(r, Duration::from_secs(1)).await.unwrap());
    assert!(c.heartbeat(&slot).await.unwrap());

    match c.call(&IpcRequest::Status).await.unwrap() {
        IpcResponse::Status {
            max_heavy_jobs,
            slots,
            queue,
        } => {
            assert_eq!(max_heavy_jobs, 2);
            assert_eq!(slots[0].key.as_deref(), Some("toolu_1"));
            assert!(queue.is_empty());
        }
        other => panic!("{other:?}"),
    }
    // A different connection (the PostToolUse hook) releases by key.
    let mut post = IpcClient::connect(&s.endpoint).await.unwrap();
    assert_eq!(post.release_key("toolu_1").await.unwrap(), 1);
    assert!(s.admission.slots().is_empty());

    match c
        .call(&IpcRequest::Classify {
            command: "cd web && pnpm build".into(),
        })
        .await
        .unwrap()
    {
        IpcResponse::Classified { class, matched } => {
            assert_eq!(class, vca_core::JobClass::Heavy);
            assert_eq!(matched.as_deref(), Some("pnpm build"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn raw_json_lines_and_errors() {
    let s = setup(1);
    let IpcEndpoint::Path(path) = &s.endpoint else {
        unreachable!()
    };
    let stream = tokio::net::UnixStream::connect(path).await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    w.write_all(b"not json\n{\"type\":\"ping\"}\n")
        .await
        .unwrap();
    let err = lines.next_line().await.unwrap().unwrap();
    assert!(err.starts_with(r#"{"type":"error""#), "{err}");
    assert_eq!(
        lines.next_line().await.unwrap().unwrap(),
        r#"{"type":"pong"}"#
    );
    w.write_all(
        b"{\"type\":\"acquire\",\"command\":\"cargo build\",\"session_id\":\"s\",\"wait_ms\":0}\n",
    )
    .await
    .unwrap();
    let line = lines.next_line().await.unwrap().unwrap();
    assert!(line.contains(r#""verdict":{"type":"grant""#), "{line}");
    assert!(line.ends_with(r#""done":true}"#), "{line}");
}

#[tokio::test]
async fn stale_socket_file_is_replaced() {
    let s = setup(1);
    let endpoint = s.endpoint.clone();
    let admission = s.admission.clone();
    s.server.abort();
    // Simulate a crash: the old listener is gone but the file may remain.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let IpcEndpoint::Path(p) = &endpoint else {
        unreachable!()
    };
    if !p.exists() {
        std::os::unix::net::UnixListener::bind(p).map(drop).unwrap();
    }
    let server = IpcServer::bind(endpoint.clone(), admission)
        .unwrap()
        .spawn();
    IpcClient::connect(&endpoint)
        .await
        .unwrap()
        .ping()
        .await
        .unwrap();
    server.abort();
}
