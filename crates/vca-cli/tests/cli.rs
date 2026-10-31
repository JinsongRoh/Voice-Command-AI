//! End-to-end checks of the `vca` binary that need no running vcad.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// A control endpoint nobody listens on.
fn missing_endpoint() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let name = format!("vca-test-{}-{nanos}", std::process::id());
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\{name}"))
    } else {
        std::env::temp_dir().join(format!("{name}.sock"))
    }
}

fn vca() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vca"));
    cmd.env("VCA_CONTROL", missing_endpoint())
        .env_remove("VCA_SESSION_ID")
        .env_remove("VCA_HOOK_DEBUG");
    cmd
}

fn run_with_stdin(mut cmd: Command, stdin: &[u8]) -> Output {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn vca");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("wait for vca")
}

#[test]
fn claude_hook_fails_open_without_vcad() {
    let mut cmd = vca();
    cmd.args(["hook", "claude", "Stop"]);
    let payload = br#"{"session_id":"s1","hook_event_name":"Stop","cwd":"/tmp"}"#;
    let out = run_with_stdin(cmd, payload);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
}

#[test]
fn claude_hook_fails_open_on_garbage_stdin() {
    let mut cmd = vca();
    cmd.args(["hook", "claude", "Stop"]);
    let out = run_with_stdin(cmd, b"\x00not json{{{");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn blocking_claude_hook_fails_open_without_vcad() {
    let mut cmd = vca();
    cmd.args(["hook", "claude", "pre-tool-use"]);
    let payload =
        br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
    let out = run_with_stdin(cmd, payload);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[test]
fn codex_hook_fails_open_on_garbage_args() {
    for args in [
        &["hook", "codex"][..],
        &["hook", "codex", "not json"],
        &["hook", "codex", "--", "{"],
        &["hook", "codex", "--bogus-flag", "-x"],
        &["hook"],
        &["hook", "nonexistent-agent", "--what"],
    ] {
        let out = vca().args(args).stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn codex_hook_fails_open_without_vcad() {
    let payload =
        r#"{"type":"agent-turn-complete","thread-id":"t1","last-assistant-message":"done"}"#;
    let out = vca()
        .args(["hook", "codex", payload])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[test]
fn ping_quiet_fails_without_vcad() {
    let out = vca().args(["ping", "--quiet"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());

    let endpoint = missing_endpoint();
    let out = vca()
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("ping")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot reach vcad"));
}

#[test]
fn ls_fails_without_vcad() {
    let out = vca().arg("ls").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: "));
}

#[test]
fn shell_init_prints_snippet() {
    let out = vca().args(["shell-init", "bash"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("vca"));
    assert!(text.contains("function claude"));

    let out = vca().args(["shell-init", "tcsh"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn version_prints_versions() {
    let out = vca().arg("version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("vca "), "{text}");
    assert!(text.contains(env!("CARGO_PKG_VERSION")));
    assert!(text.contains("protocol"));
}

#[test]
fn run_inside_managed_session_runs_directly() {
    let (program, args): (&str, &[&str]) = if cfg!(windows) {
        ("cmd", &["/C", "exit 3"])
    } else {
        ("sh", &["-c", "exit 3"])
    };
    let out = vca()
        .env("VCA_SESSION_ID", "outer")
        .arg("run")
        .arg("--")
        .arg(program)
        .args(args)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}
