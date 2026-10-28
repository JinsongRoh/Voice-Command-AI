//! Integration tests for `PtySession` using the fake agent.

mod common;

use common::{TIMEOUT, fake_agent_opts, read_until, wait_for};
use vca_core::{ControlKey, Input};
use vca_pty::{BufferLimits, OutputEvent, PtySession, PtySize};

fn text(s: &str) -> Input {
    Input::Text {
        text: s.into(),
        submit: true,
    }
}

async fn wait_exit(session: &PtySession) -> vca_pty::ExitStatus {
    tokio::time::timeout(TIMEOUT, session.wait())
        .await
        .expect("process did not exit")
        .unwrap()
}

#[tokio::test]
async fn delivers_input_without_focus_and_reports_exit_code() {
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    assert!(session.pid().is_some());
    let mut sub = session.subscribe_from(0);
    let mut out = String::new();
    read_until(&mut sub, &mut out, "> ").await;

    session.send_input(&text("hello pty")).await.unwrap();
    read_until(&mut sub, &mut out, "Echo: hello pty").await;
    read_until(&mut sub, &mut out, "Done.").await;

    let snap = session
        .screen_snapshot()
        .expect("screen tracking is on by default");
    assert!(snap.text().contains("Echo: hello pty"), "{}", snap.text());

    session.send_input(&text("/exit 42")).await.unwrap();
    let status = wait_exit(&session).await;
    assert_eq!(status.code, Some(42));
    assert!(!status.success());

    // The subscription ends after the remaining output.
    tokio::time::timeout(TIMEOUT, async { while sub.recv().await.is_some() {} })
        .await
        .expect("output did not close");
    assert!(session.output_closed());
}

#[tokio::test]
async fn permission_prompt_flow() {
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    let mut sub = session.subscribe_from(0);
    let mut out = String::new();
    read_until(&mut sub, &mut out, "> ").await;
    session.send_input(&text("please run tests")).await.unwrap();
    read_until(&mut sub, &mut out, "Allow command `cargo test`? [y/n]").await;
    session.send_input(&text("y")).await.unwrap();
    read_until(&mut sub, &mut out, "Running `cargo test`... ok").await;
    session.kill().unwrap();
    wait_exit(&session).await;
}

#[tokio::test]
async fn control_keys_reach_the_process() {
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    wait_for(&session, "> ").await;
    // Type a partial line, erase it with Backspace, then type a full message.
    session
        .send_input(&Input::Text {
            text: "xy".into(),
            submit: false,
        })
        .await
        .unwrap();
    session
        .send_input(&Input::Key {
            key: ControlKey::Backspace,
        })
        .await
        .unwrap();
    session
        .send_input(&Input::Key {
            key: ControlKey::Enter,
        })
        .await
        .unwrap();
    wait_for(&session, "Echo: x\r").await;
    session.kill().unwrap();
    wait_exit(&session).await;
}

#[tokio::test]
async fn resize_is_visible_to_the_child() {
    let session =
        PtySession::spawn(fake_agent_opts().size(PtySize { cols: 90, rows: 30 })).unwrap();
    wait_for(&session, "> ").await;
    session.send_input(&text("/size")).await.unwrap();
    wait_for(&session, "size 90x30").await;

    session
        .resize(PtySize {
            cols: 120,
            rows: 40,
        })
        .unwrap();
    assert_eq!(
        session.size(),
        PtySize {
            cols: 120,
            rows: 40
        }
    );
    assert_eq!(
        session.screen_snapshot().unwrap().size,
        PtySize {
            cols: 120,
            rows: 40
        }
    );
    session.send_input(&text("/size")).await.unwrap();
    wait_for(&session, "size 120x40").await;
    session.kill().unwrap();
    wait_exit(&session).await;
}

#[tokio::test]
async fn env_and_cwd_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let session = PtySession::spawn(
        fake_agent_opts()
            .cwd(dir.path())
            .env("VCA_TEST_VALUE", "a b c"),
    )
    .unwrap();
    wait_for(&session, "> ").await;
    session
        .send_input(&text("/env VCA_TEST_VALUE"))
        .await
        .unwrap();
    wait_for(&session, "env VCA_TEST_VALUE=a b c").await;
    session.send_input(&text("/cwd")).await.unwrap();
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let out = wait_for(&session, &name).await;
    assert!(out.contains(&name), "{out}");
    session.kill().unwrap();
    wait_exit(&session).await;
}

#[tokio::test]
async fn replay_from_seq_and_fan_out() {
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    let mut a = session.subscribe();
    let mut b = session.subscribe();
    wait_for(&session, "> ").await;
    let mark = session.next_seq();
    session.send_input(&text("first")).await.unwrap();
    let mut out_a = String::new();
    let mut out_b = String::new();
    read_until(&mut a, &mut out_a, "Echo: first").await;
    read_until(&mut b, &mut out_b, "Echo: first").await;

    // A late subscriber replays from `mark` and sees the response, but not
    // the banner printed before it.
    let mut late = session.subscribe_from(mark);
    assert_eq!(late.next_seq(), session.next_seq());
    let mut replay = String::new();
    read_until(&mut late, &mut replay, "Echo: first").await;
    assert!(!replay.contains("Fake Agent"), "{replay}");

    // Replaying from 0 includes everything.
    let all = wait_for(&session, "Echo: first").await;
    assert!(all.contains("Fake Agent"));
    session.kill().unwrap();
    wait_exit(&session).await;
}

#[tokio::test]
async fn evicted_output_is_reported_as_gap() {
    let session = PtySession::spawn(fake_agent_opts().buffer(BufferLimits {
        max_bytes: 64,
        max_chunks: 2,
    }))
    .unwrap();
    let mut live = session.subscribe_from(0);
    let mut out = String::new();
    read_until(&mut live, &mut out, "> ").await;
    for i in 0..4 {
        let msg = format!("message number {i}");
        session.send_input(&text(&msg)).await.unwrap();
        read_until(&mut live, &mut out, &format!("Echo: {msg}")).await;
    }
    let mut sub = session.subscribe_from(0);
    match sub.recv().await {
        Some(OutputEvent::Gap(gap)) => {
            assert_eq!(gap.requested, 0);
            assert!(gap.resumed_at > 0);
            assert!(gap.missed() > 0);
        }
        other => panic!("expected gap, got {other:?}"),
    }
    match sub.recv().await {
        Some(OutputEvent::Data(chunk)) => assert!(chunk.seq > 0),
        other => panic!("expected data, got {other:?}"),
    }
    session.kill().unwrap();
    wait_exit(&session).await;
}

#[tokio::test]
async fn multi_line_text_uses_bracketed_paste() {
    let session = PtySession::spawn(fake_agent_opts().arg("--bracketed-paste")).unwrap();
    let mut sub = session.subscribe_from(0);
    let mut out = String::new();
    read_until(&mut sub, &mut out, "> ").await;
    assert!(session.modes().bracketed_paste);

    session
        .send_input(&text("line one\nline two"))
        .await
        .unwrap();
    read_until(&mut sub, &mut out, "Echo: line one").await;
    read_until(&mut sub, &mut out, "Done.").await;
    // One message, not two.
    assert_eq!(out.matches("Echo:").count(), 1, "{out}");
    assert!(out.contains("Echo: line one\r\nline two"), "{out}");

    session.send_input(&text("/exit")).await.unwrap();
    assert_eq!(wait_exit(&session).await.code, Some(0));
    assert!(!session.modes().bracketed_paste);
}

#[tokio::test]
async fn kill_terminates_the_process() {
    let session = PtySession::spawn(fake_agent_opts()).unwrap();
    wait_for(&session, "> ").await;
    session.kill().unwrap();
    let status = wait_exit(&session).await;
    assert!(!status.success());
    // Killing again after exit is a no-op.
    session.kill().unwrap();
}

#[tokio::test]
async fn spawn_failure_is_an_error() {
    let err =
        PtySession::spawn(vca_pty::SpawnOptions::new("/definitely/not/a/program")).unwrap_err();
    assert!(matches!(err, vca_pty::PtyError::Spawn(_)), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn shell_command_exit_code_and_output() {
    let session = PtySession::spawn(
        vca_pty::SpawnOptions::new("/bin/sh")
            .args(["-c", "stty size; printf 'from sh\\n'; exit 5"])
            .size(PtySize { cols: 77, rows: 22 }),
    )
    .unwrap();
    let out = wait_for(&session, "from sh").await;
    assert!(out.contains("22 77"), "{out}");
    assert_eq!(wait_exit(&session).await.code, Some(5));
}

#[cfg(unix)]
#[tokio::test]
async fn write_after_exit_does_not_hang() {
    let session =
        PtySession::spawn(vca_pty::SpawnOptions::new("/bin/sh").args(["-c", "exit 0"])).unwrap();
    wait_exit(&session).await;
    // The write may succeed (kernel buffer) or fail; it must not hang.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.write(b"x".to_vec()),
    )
    .await
    .expect("write hung");
}
