//! Smoke test against the agents installed on the current machine.
//! Run with `cargo test -p vca-adapters --test real_machine -- --ignored --nocapture`.

use vca_adapters::{SessionLocator, builtin_adapters, discover_all};

#[tokio::test]
#[ignore = "reads the real ~/.claude and ~/.codex of the current user"]
async fn discover_and_read_real_sessions() {
    let adapters = builtin_adapters();
    let sessions = discover_all(&adapters).await;
    for s in &sessions {
        println!(
            "{:?} id={:?} pid={:?} dir={:?} title={:?}",
            s.agent, s.agent_session_id, s.pid, s.project_dir, s.title
        );
    }
    if let Some(s) = sessions.iter().find(|s| s.transcript_path.is_some()) {
        let adapter = adapters.iter().find(|a| a.kind() == s.agent).unwrap();
        let items = adapter
            .read_history(&SessionLocator::from(s))
            .await
            .unwrap();
        println!("{} history items; last: {:?}", items.len(), items.last());
        assert!(!items.is_empty());
    }
}
