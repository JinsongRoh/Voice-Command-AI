//! Codex cloud adapter against a fake `codex` script (Unix only).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use vca_cloud::providers::CodexCloudAdapter;
use vca_cloud::{CloudAgentAdapter, CloudError, CloudLaunch, Credential};
use vca_core::SessionState;

const FAKE_CODEX: &str = r#"#!/bin/sh
# Fake `codex` CLI implementing the `codex cloud` subcommands used by VCA.
[ "$1" = "cloud" ] || { echo "unexpected: $*" >&2; exit 2; }
shift
case "$1" in
  exec)
    # Expect: exec --env ENV [--branch B] -- PROMPT
    [ "$2" = "--env" ] && [ "$3" = "env_1" ] || { echo "bad env: $*" >&2; exit 2; }
    echo "https://chatgpt.com/codex/tasks/task_e_42"
    ;;
  list)
    cat <<'EOF'
{
  "tasks": [
    {"id": "task_e_42", "url": "https://chatgpt.com/codex/tasks/task_e_42", "title": "Fix bug",
     "status": "ready", "updated_at": "2025-01-01T00:00:00Z", "environment_id": "env_1",
     "environment_label": "octo/hello", "summary": {"files_changed": 1, "lines_added": 3, "lines_removed": 1},
     "is_review": false, "attempt_total": 1}
  ],
  "cursor": null
}
EOF
    ;;
  status)
    case "$2" in
      task_e_42) printf '[READY] Fix bug\n  octo/hello  •  1m ago\n  +3/-1 • 1 file\n' ;;
      task_e_pending) printf '[PENDING] Still going\n  env_1  •  just now\n  no diff\n'; exit 1 ;;
      *) echo "Error: task not found" >&2; exit 1 ;;
    esac
    ;;
  diff)
    printf 'diff --git a/x b/x\n+fixed\n'
    ;;
  *) echo "unknown subcommand" >&2; exit 2 ;;
esac
"#;

fn fake_codex(dir: &tempfile::TempDir) -> PathBuf {
    let p = dir.path().join("codex");
    std::fs::write(&p, FAKE_CODEX).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[tokio::test]
async fn codex_cli_flow() {
    let dir = tempfile::tempdir().unwrap();
    let codex = CodexCloudAdapter::new()
        .with_program(fake_codex(&dir))
        .with_environment("env_1");
    let cred = Credential::none();

    let s = codex
        .create_session(
            &cred,
            &CloudLaunch {
                prompt: "-fix the bug".into(),
                branch: Some("main".into()),
                ..CloudLaunch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(s.id.0, "task_e_42");

    let list = codex.list_sessions(&cred).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].state, SessionState::Exited { code: Some(0) });

    let ready = codex.get_session(&cred, &"task_e_42".into()).await.unwrap();
    assert_eq!(ready.title.as_deref(), Some("Fix bug"));
    let pending = codex
        .get_session(&cred, &"task_e_pending".into())
        .await
        .unwrap();
    assert_eq!(pending.state, SessionState::Running);

    let err = codex
        .get_session(&cred, &"task_missing".into())
        .await
        .unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)), "{err:?}");

    let page = codex
        .activity(&cred, &"task_e_42".into(), None)
        .await
        .unwrap();
    assert_eq!(page.activities.len(), 2);

    let diff = codex.diff(&"task_e_42".into()).await.unwrap();
    assert!(diff.contains("+fixed"));
    assert!(matches!(
        codex
            .send_message(&cred, &"task_e_42".into(), "x")
            .await
            .unwrap_err(),
        CloudError::Unsupported(_)
    ));
}

#[tokio::test]
async fn codex_missing_binary_is_unsupported() {
    let codex = CodexCloudAdapter::new().with_program("/nonexistent/codex-binary");
    let err = codex.list_sessions(&Credential::none()).await.unwrap_err();
    assert!(matches!(err, CloudError::Unsupported(_)));
}
