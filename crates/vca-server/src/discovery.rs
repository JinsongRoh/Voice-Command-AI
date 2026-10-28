//! Observed sessions: running agents that VCA did not start (SES-01).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use vca_adapters::{AgentAdapter, ProcessTable, SessionLocator};
use vca_core::{ControlTier, SessionId, SessionInfo, SessionOrigin, SessionState, now_millis};

use crate::sessions::{Observed, SessionManager, default_name};
use crate::util::lock;

/// Periodically discovers running agent processes and keeps one observed
/// session per process. Processes that belong to managed sessions are
/// skipped.
pub async fn discovery_loop(
    mgr: SessionManager,
    adapters: Vec<Arc<dyn AgentAdapter>>,
    interval: Duration,
    detect_terminals: bool,
) {
    let mut known: HashMap<u32, SessionId> = HashMap::new();
    let mut tick = tokio::time::interval(interval.max(Duration::from_millis(500)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let found = vca_adapters::discover_all(&adapters).await;
        let procs = tokio::task::spawn_blocking(ProcessTable::snapshot)
            .await
            .unwrap_or_default();

        let mut managed_pids = HashSet::new();
        let mut managed_agent_ids = HashSet::new();
        for s in mgr.all() {
            if s.managed().is_none() {
                continue;
            }
            if let Some(pid) = s.pid() {
                managed_pids.insert(pid);
                managed_pids.extend(procs.descendants(pid));
            }
            if let Some(id) = lock(&s.agent_session_id).clone() {
                managed_agent_ids.insert(id);
            }
        }

        let mut seen = HashSet::new();
        for d in found {
            let Some(pid) = d.pid else { continue };
            if managed_pids.contains(&pid)
                || d.agent_session_id
                    .as_ref()
                    .is_some_and(|id| managed_agent_ids.contains(id))
            {
                continue;
            }
            seen.insert(pid);
            if let Some(id) = known.get(&pid) {
                if let Some(s) = mgr.get(id) {
                    if let Some(at) = d.last_activity {
                        mgr.update_info(&s, |info| {
                            let changed = at > info.last_activity;
                            info.last_activity = info.last_activity.max(at);
                            changed
                        });
                    }
                    continue;
                }
            }
            let terminal = if detect_terminals {
                vca_adapters::terminal::detect(pid, &procs).await
            } else {
                None
            };
            let project_dir = d.project_dir.clone().unwrap_or_else(PathBuf::new);
            let now = now_millis();
            let info = SessionInfo {
                id: SessionId::new(),
                origin: SessionOrigin::Local {
                    agent: d.agent.clone(),
                    project_dir: project_dir.clone(),
                    pid: Some(pid),
                },
                display_name: d
                    .title
                    .clone()
                    .unwrap_or_else(|| default_name(&d.agent, &project_dir)),
                state: SessionState::Running,
                control: if terminal.is_some() {
                    ControlTier::TerminalApi
                } else {
                    ControlTier::ObserveOnly
                },
                profile: None,
                created_at: now,
                last_activity: d.last_activity.unwrap_or(now),
                unread: 0,
                preview: None,
            };
            let session = mgr.insert_observed(
                info,
                Observed {
                    pid: Some(pid),
                    terminal,
                },
            );
            *lock(&session.agent_session_id) = d.agent_session_id.clone();
            if d.transcript_path.is_some() {
                mgr.follow_transcript(&session, SessionLocator::from(&d));
            }
            known.insert(pid, session.id.clone());
        }

        known.retain(|pid, id| {
            if seen.contains(pid) {
                return true;
            }
            if let Some(s) = mgr.get(id) {
                mgr.set_state(&s, SessionState::Exited { code: None });
                mgr.schedule_removal(&s);
            }
            false
        });
    }
}
