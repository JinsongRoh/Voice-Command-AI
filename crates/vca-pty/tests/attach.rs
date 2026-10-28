//! Attach server and client roundtrip tests (client in-process, no raw mode).

mod common;

use common::{TIMEOUT, endpoint, fake_agent_opts, wait_for};
use vca_pty::attach::{AttachReader, ServerFrame};
use vca_pty::{AttachClient, AttachOptions, AttachServer, AttachStart, PtySession, PtySize};

/// Reads frames until the accumulated output contains `needle`. Returns
/// all frames seen.
async fn read_frames_until(
    reader: &mut AttachReader,
    acc: &mut String,
    needle: &str,
) -> Vec<ServerFrame> {
    let mut frames = Vec::new();
    let res = tokio::time::timeout(TIMEOUT, async {
        while !acc.contains(needle) {
            match reader.recv().await.unwrap() {
                Some(frame) => {
                    if let ServerFrame::Output { data, .. } | ServerFrame::Redraw(data) = &frame {
                        acc.push_str(&String::from_utf8_lossy(data));
                    }
                    frames.push(frame);
                }
                None => break,
            }
        }
    })
    .await;
    assert!(
        res.is_ok() && acc.contains(needle),
        "did not see {needle:?} in {acc:?}"
    );
    frames
}

async fn read_exit(reader: &mut AttachReader) -> Option<i32> {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match reader.recv().await.unwrap() {
                Some(ServerFrame::Exit { code }) => return code,
                Some(_) => {}
                None => panic!("stream closed without exit"),
            }
        }
    })
    .await
    .expect("no exit frame")
}

#[tokio::test]
async fn multiple_clients_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    wait_for(&session, "> ").await;
    let ep = endpoint(dir.path(), "multi");
    let server = AttachServer::bind(&ep, session.clone()).unwrap();
    assert_eq!(server.endpoint(), ep.as_path());

    let c1 = AttachClient::connect(
        &ep,
        AttachOptions {
            size: Some(PtySize {
                cols: 100,
                rows: 30,
            }),
            start: AttachStart::Redraw,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        c1.welcome().size,
        PtySize {
            cols: 100,
            rows: 30
        }
    );
    assert_eq!(
        session.size(),
        PtySize {
            cols: 100,
            rows: 30
        }
    );
    let c2 = AttachClient::connect(&ep, AttachOptions::default())
        .await
        .unwrap();
    let (mut r1, mut w1) = c1.split();
    let (mut r2, mut w2) = c2.split();

    // The redraw shows the existing screen (banner) to both clients.
    let mut out1 = String::new();
    let mut out2 = String::new();
    let frames = read_frames_until(&mut r1, &mut out1, "Fake Agent").await;
    assert!(matches!(frames[0], ServerFrame::Redraw(_)), "{frames:?}");
    read_frames_until(&mut r2, &mut out2, "Fake Agent").await;

    // Input from client 1 is seen by both clients.
    w1.input(b"hi from one\r").await.unwrap();
    read_frames_until(&mut r1, &mut out1, "Echo: hi from one").await;
    read_frames_until(&mut r2, &mut out2, "Echo: hi from one").await;

    // Client 2 resizes; last resize wins.
    w2.resize(PtySize { cols: 70, rows: 20 }).await.unwrap();
    w2.input(b"/size\r").await.unwrap();
    read_frames_until(&mut r2, &mut out2, "size 70x20").await;
    assert_eq!(session.size(), PtySize { cols: 70, rows: 20 });

    // Client 1 detaches; the session keeps running for client 2.
    w1.detach().await.unwrap();
    let closed = tokio::time::timeout(TIMEOUT, async {
        while let Ok(Some(_)) = r1.recv().await {}
    })
    .await;
    assert!(closed.is_ok(), "detached client stream did not close");
    assert!(session.try_exit_status().is_none());

    w2.input(b"/exit 3\r").await.unwrap();
    assert_eq!(read_exit(&mut r2).await, Some(3));
    drop(server);
}

#[tokio::test]
async fn replay_from_seq_over_attach() {
    let dir = tempfile::tempdir().unwrap();
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    wait_for(&session, "> ").await;
    let _server = AttachServer::bind(endpoint(dir.path(), "replay"), session.clone()).unwrap();
    let ep = endpoint(dir.path(), "replay");

    let client = AttachClient::connect(
        &ep,
        AttachOptions {
            size: None,
            start: AttachStart::ReplayFrom(0),
        },
    )
    .await
    .unwrap();
    let (mut reader, mut writer) = client.split();
    let mut out = String::new();
    let frames = read_frames_until(&mut reader, &mut out, "Fake Agent").await;
    assert!(
        matches!(frames[0], ServerFrame::Output { seq: 0, .. }),
        "{frames:?}"
    );

    // Live mode does not replay the banner.
    let live = AttachClient::connect(
        &ep,
        AttachOptions {
            size: None,
            start: AttachStart::Live,
        },
    )
    .await
    .unwrap();
    let (mut live_reader, _live_writer) = live.split();
    writer.input(b"ping\r").await.unwrap();
    let mut live_out = String::new();
    read_frames_until(&mut live_reader, &mut live_out, "Echo: ping").await;
    assert!(!live_out.contains("Fake Agent"), "{live_out}");
    session.kill().unwrap();
}

#[tokio::test]
async fn attach_after_exit_gets_output_and_exit() {
    let dir = tempfile::tempdir().unwrap();
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    wait_for(&session, "> ").await;
    session.write(b"/exit 9\r".to_vec()).await.unwrap();
    tokio::time::timeout(TIMEOUT, session.wait())
        .await
        .unwrap()
        .unwrap();
    let ep = endpoint(dir.path(), "late");
    let _server = AttachServer::bind(&ep, session.clone()).unwrap();
    let client = AttachClient::connect(&ep, AttachOptions::default())
        .await
        .unwrap();
    let (mut reader, _writer) = client.split();
    assert_eq!(read_exit(&mut reader).await, Some(9));
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_unknown_protocol_version() {
    use vca_pty::attach::protocol::{ClientFrame, read_server_frame, write_client_frame};

    let dir = tempfile::tempdir().unwrap();
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    let ep = endpoint(dir.path(), "version");
    let _server = AttachServer::bind(&ep, session.clone()).unwrap();

    {
        let mut stream = tokio::net::UnixStream::connect(&ep).await.unwrap();
        write_client_frame(
            &mut stream,
            &ClientFrame::Hello {
                version: 999,
                size: None,
                start: AttachStart::Live,
            },
        )
        .await
        .unwrap();
        let frame = read_server_frame(&mut stream).await.unwrap();
        assert!(matches!(frame, Some(ServerFrame::Error(_))), "{frame:?}");
    }
    session.kill().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn socket_is_private_and_removed_on_drop() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    let ep = dir.path().join("nested").join("p.sock");
    let server = AttachServer::bind(&ep, session.clone()).unwrap();
    let dir_mode = std::fs::metadata(ep.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700);
    let sock_mode = std::fs::metadata(&ep).unwrap().permissions().mode();
    assert_eq!(sock_mode & 0o077, 0, "socket mode {sock_mode:o}");
    drop(server);
    // The listener is dropped asynchronously with the aborted task.
    for _ in 0..100 {
        if !ep.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!ep.exists());

    // Rebinding after a crash-like leftover works.
    std::fs::write(&ep, b"").unwrap();
    let _server = AttachServer::bind(&ep, session.clone()).unwrap();
    session.kill().unwrap();
}

/// Runs `attach_stdio` (via the `attach` example) inside an outer PTY, so
/// raw mode, stdin relay, SIGWINCH propagation and the detach key are
/// exercised against a real terminal device.
#[cfg(unix)]
#[tokio::test]
async fn attach_stdio_in_a_real_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let inner = PtySession::spawn(fake_agent_opts()).unwrap();
    wait_for(&inner, "> ").await;
    let ep = endpoint(dir.path(), "stdio");
    let _server = AttachServer::bind(&ep, inner.clone()).unwrap();

    let outer = PtySession::spawn(
        vca_pty::SpawnOptions::new(common::attach_example())
            .arg(&ep)
            .size(PtySize { cols: 81, rows: 25 }),
    )
    .unwrap();
    wait_for(&outer, "Fake Agent").await;
    assert_eq!(inner.size(), PtySize { cols: 81, rows: 25 });

    outer.write(b"via outer\r".to_vec()).await.unwrap();
    wait_for(&outer, "Echo: via outer").await;
    wait_for(&inner, "Echo: via outer").await;

    // Resizing the outer terminal propagates to the inner session.
    outer.resize(PtySize { cols: 99, rows: 33 }).unwrap();
    let resized = tokio::time::timeout(TIMEOUT, async {
        while inner.size() != (PtySize { cols: 99, rows: 33 }) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(resized.is_ok(), "inner size is {:?}", inner.size());

    // Ctrl-] d detaches; the inner session keeps running.
    outer.write(b"\x1dd".to_vec()).await.unwrap();
    wait_for(&outer, "outcome: Detached").await;
    let status = tokio::time::timeout(TIMEOUT, outer.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code, Some(0));
    assert!(inner.try_exit_status().is_none());

    // Re-attach; when the agent exits, the view exits with its code.
    let outer =
        PtySession::spawn(vca_pty::SpawnOptions::new(common::attach_example()).arg(&ep)).unwrap();
    wait_for(&outer, "Echo: via outer").await;
    outer.write(b"/exit 4\r".to_vec()).await.unwrap();
    wait_for(&outer, "outcome: Exited { code: Some(4) }").await;
    let status = tokio::time::timeout(TIMEOUT, outer.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code, Some(4));
}
