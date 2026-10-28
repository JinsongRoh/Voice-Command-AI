//! Delivery tier 3 against a real tmux server (skipped when tmux is absent).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use vca_adapters::ProcessTable;
use vca_adapters::terminal::{self, TerminalTarget, tmux};

struct Server {
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .status();
    }
}

fn start_server() -> Option<Server> {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux not installed; skipping");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sock");
    // `cat` echoes what it receives, so the pane shows injected text twice
    // (terminal echo, then cat's output after Enter).
    let status = Command::new("tmux")
        .arg("-S")
        .arg(&socket)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "vca-test",
            "-x",
            "120",
            "-y",
            "20",
            "cat",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "tmux failed to start");
    Some(Server { socket, _dir: dir })
}

async fn wait_for(socket: &Path, pane: &str, needle: &str, times: usize) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen = tmux::capture_pane(Some(socket), pane).await.unwrap();
        if screen.matches(needle).count() >= times || Instant::now() > deadline {
            return screen;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn detects_pane_and_injects_text_without_focus() {
    let Some(server) = start_server() else { return };
    let panes = tmux::list_panes(Some(&server.socket)).await.unwrap();
    assert_eq!(panes.len(), 1);
    let pane = panes[0].clone();
    assert_eq!(pane.session_name, "vca-test");

    // Detection from the process environment (TMUX / TMUX_PANE).
    let procs = tokio::task::spawn_blocking(ProcessTable::snapshot)
        .await
        .unwrap();
    let detected = terminal::detect(pane.pane_pid, &procs)
        .await
        .expect("pane detected");
    assert_eq!(
        detected,
        TerminalTarget::Tmux {
            pane: pane.pane_id.clone(),
            socket: Some(server.socket.clone()),
        }
    );

    // Literal text: a leading dash, a key name and a trailing semicolon
    // must arrive unchanged.
    let text = "-n Enter echo 'hi' $HOME;";
    terminal::inject(&detected, text, true).await.unwrap();
    let screen = wait_for(&server.socket, &pane.pane_id, text, 2).await;
    assert_eq!(screen.matches(text).count(), 2, "screen was:\n{screen}");

    // Without submit, nothing is echoed back by cat yet.
    terminal::inject(&detected, "pending input", false)
        .await
        .unwrap();
    let screen = wait_for(&server.socket, &pane.pane_id, "pending input", 1).await;
    assert_eq!(
        screen.matches("pending input").count(),
        1,
        "screen was:\n{screen}"
    );
}
