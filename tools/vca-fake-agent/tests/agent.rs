//! Runs the fake agent with piped stdio (no PTY needed).

use std::io::Write;
use std::process::{Command, Stdio};

fn run(args: &[&str], input: &str) -> (String, Option<i32>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vca-fake-agent"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8(out.stdout).unwrap(), out.status.code())
}

#[test]
fn default_rules_and_exit_code() {
    let (out, code) = run(&[], "hello\nrun tests\nn\n/exit 7\n");
    assert!(
        out.starts_with("Fake Agent (vca-fake-agent) ready.\n> "),
        "{out}"
    );
    assert!(out.contains("Echo: hello\nDone.\n> "), "{out}");
    assert!(
        out.contains("Allow command `cargo test`? [y/n] Skipped `cargo test`.\nDone."),
        "{out}"
    );
    assert!(out.ends_with("Bye.\n"), "{out}");
    assert_eq!(code, Some(7));
}

#[test]
fn eof_exits_cleanly() {
    let (out, code) = run(&["--prompt", "$ "], "think about it\n");
    assert!(
        out.contains("Thinking...\nThought about: think about it\nDone.\n$ "),
        "{out}"
    );
    assert_eq!(code, Some(0));
}

#[test]
fn script_rules_take_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.json");
    std::fs::write(
        &script,
        r#"{ "banner": "Scripted", "prompt": "? ",
             "rules": [
               { "pattern": "hello", "mode": "exact",
                 "actions": [ { "type": "say", "text": "Hi there ({input})" } ] },
               { "pattern": "deploy", "mode": "prefix",
                 "actions": [ { "type": "permission", "command": "make deploy",
                                "yes": [ { "type": "say", "text": "deployed" }, { "type": "exit", "code": 4 } ] } ] }
             ] }"#,
    )
    .unwrap();
    let (out, code) = run(
        &["--script", script.to_str().unwrap()],
        "hello\ndeploy now\nyes\n",
    );
    assert!(
        out.starts_with("Scripted\n? Hi there (hello)\nDone.\n? "),
        "{out}"
    );
    assert!(
        out.contains("Allow command `make deploy`? [y/n] deployed\n"),
        "{out}"
    );
    assert_eq!(code, Some(4));
}

#[test]
fn invalid_script_fails() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("bad.json");
    std::fs::write(&script, "{ nope").unwrap();
    let (_, code) = run(&["--script", script.to_str().unwrap()], "");
    assert_eq!(code, Some(2));
}

#[test]
fn bracketed_paste_is_one_message() {
    let (out, _) = run(&["--bracketed-paste"], "\x1b[200~a\nb\x1b[201~\n/exit\n");
    assert!(out.starts_with("\x1b[?2004h"), "{out:?}");
    assert!(out.contains("Echo: a\nb\nDone."), "{out:?}");
    assert_eq!(out.matches("Echo:").count(), 1);
}

#[cfg(unix)]
#[test]
fn emits_claude_style_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("hooks.log");
    let hook = format!(
        "printf '%s ' \"$VCA_HOOK_EVENT\" >> '{0}'; cat >> '{0}'; echo >> '{0}'",
        log.display()
    );
    let (_, code) = run(
        &["--hook-cmd", &hook, "--session-id", "s-1"],
        "run tests\ny\n/exit\n",
    );
    assert_eq!(code, Some(0));
    let text = std::fs::read_to_string(&log).unwrap();
    let events: Vec<(&str, serde_json::Value)> = text
        .lines()
        .map(|l| {
            let (name, json) = l.split_once(' ').unwrap();
            (name, serde_json::from_str(json).unwrap())
        })
        .collect();
    let names: Vec<&str> = events.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        [
            "SessionStart",
            "UserPromptSubmit",
            "Notification",
            "PreToolUse",
            "Stop",
            "UserPromptSubmit",
            "SessionEnd"
        ]
    );
    for (name, payload) in &events {
        assert_eq!(payload["hook_event_name"], *name);
        assert_eq!(payload["session_id"], "s-1");
    }
    assert_eq!(events[1].1["prompt"], "run tests");
    assert_eq!(events[2].1["notification_type"], "permission_prompt");
    assert_eq!(events[3].1["tool_input"]["command"], "cargo test");
}
