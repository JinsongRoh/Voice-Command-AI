//! Starting and stopping the whole server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use vca_broker::contain::{ProcessControl, Scope};
use vca_broker::ipc::IpcEndpoint;
use vca_broker::{Broker, BrokerConfig, Policy};

use crate::api::Api;
use crate::config::{Config, Paths, PauseScope, ShutdownMode};
use crate::control;
use crate::devws::DevWs;
use crate::events::LogLimits;
use crate::local::LocalServer;
use crate::remote::Remote;
use crate::sessions::{ManagerConfig, SessionManager};
use crate::workspace::WorkspaceRoots;

/// Command-line overrides for [`Server::start`].
#[derive(Clone, Default)]
pub struct Options {
    pub config_file: Option<PathBuf>,
    pub control: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    /// Replaces the OS process control of the broker (tests use a fake so
    /// that no real process is ever suspended or terminated).
    pub process_control: Option<Arc<dyn ProcessControl>>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("config_file", &self.config_file)
            .field("control", &self.control)
            .field("data_dir", &self.data_dir)
            .field("process_control", &self.process_control.is_some())
            .finish()
    }
}

/// A running server. Dropping it stops listening; call
/// [`shutdown`](Self::shutdown) to also handle managed sessions.
pub struct Server {
    pub api: Api,
    pub config: Config,
    pub paths: Paths,
    pub control_endpoint: PathBuf,
    pub dev_ws: Option<DevWs>,
    pub remote: Option<Arc<Remote>>,
    local: Option<LocalServer>,
    discovery: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    /// Loads the configuration and starts every component. `config` wins
    /// over the configuration file when given (tests).
    pub async fn start(opts: Options, config: Option<Config>) -> anyhow::Result<Self> {
        let mut paths = Paths::from_env();
        if let Some(file) = &opts.config_file {
            paths.config_file = file.clone();
            if let Some(dir) = file.parent() {
                paths.config_dir = dir.to_owned();
            }
        }
        if let Some(dir) = &opts.data_dir {
            paths.data_dir = dir.clone();
        }
        let config = match config {
            Some(c) => c,
            None => Config::load(&paths.config_file)?,
        };
        let control_endpoint = opts
            .control
            .clone()
            .or_else(|| config.control_socket.clone())
            .map(|p| control::parse_endpoint(&p.to_string_lossy()))
            .unwrap_or_else(control::default_endpoint);

        let policy_path = config
            .broker
            .policy_path
            .clone()
            .unwrap_or_else(|| paths.policy_file());
        let policy = Policy::load(&policy_path)
            .with_context(|| format!("loading {}", policy_path.display()))?;
        let broker_config = BrokerConfig {
            policy,
            terminate_grace: Duration::from_secs(config.shutdown.grace_secs),
            ..BrokerConfig::default()
        };
        let mut broker = match &opts.process_control {
            Some(control) => Broker::start_with_control(broker_config, Arc::clone(control)),
            None => Broker::start(broker_config),
        };
        if let Some(socket) = &config.broker.socket {
            let s = socket.to_string_lossy();
            let endpoint = if cfg!(windows) && !s.contains(['/', '\\']) {
                IpcEndpoint::Named(s.into_owned())
            } else {
                IpcEndpoint::Path(socket.clone())
            };
            broker
                .serve_ipc(endpoint)
                .context("starting the broker IPC endpoint")?;
        }

        let sessions = &config.sessions;
        let mgr = SessionManager::new(
            ManagerConfig {
                limits: LogLimits {
                    max_events: sessions.replay_events,
                    max_bytes: sessions.replay_bytes,
                },
                exited_retention: Duration::from_secs(sessions.exited_retention_secs),
                pause_scope: match sessions.pause_scope {
                    PauseScope::ChildrenOnly => Scope::ChildrenOnly,
                    PauseScope::WholeTree => Scope::WholeTree,
                },
                heuristics_interval: Duration::from_millis(sessions.heuristics_interval_ms),
                control_endpoint: control_endpoint.clone(),
                attach_dir: None,
                workspace_roots: WorkspaceRoots::new(config.resolved_workspace_roots()),
            },
            Some(Arc::new(broker)),
        );
        let api = Api::new(
            mgr.clone(),
            config.server_name(),
            Duration::from_secs(config.resources.push_interval_secs),
        );
        if let Some(speech) = crate::speech::Speech::from_config(&config.speech, &paths.data_dir)
            .context("starting server-side speech recognition")?
        {
            tracing::info!(engine = speech.engine_name(), "speech recognition on");
            api.set_speech(Arc::new(speech));
        }
        let remote_slot: crate::local::RemoteSlot = Arc::default();
        let local = LocalServer::bind(&control_endpoint, api.clone(), Arc::clone(&remote_slot))
            .await
            .with_context(|| format!("listening on {}", control_endpoint.display()))?;

        let remote = if config.remote.enabled {
            let data_dir = &paths.data_dir;
            let remote = Remote::start(api.clone(), &config.remote, config.server_name(), data_dir)
                .await
                .context("starting the remote transport")?;
            let _ = remote_slot.set(Arc::clone(&remote));
            Some(remote)
        } else {
            None
        };

        let discovery = config.discovery.enabled.then(|| {
            tokio::spawn(crate::discovery::discovery_loop(
                mgr.clone(),
                vca_adapters::builtin_adapters(),
                Duration::from_secs(config.discovery.interval_secs),
                config.discovery.detect_terminals,
            ))
        });
        let dev_ws = if config.dev_ws.enabled {
            Some(
                crate::devws::start(api.clone(), config.dev_ws.port)
                    .await
                    .context("starting the development WebSocket")?,
            )
        } else {
            None
        };

        tracing::info!(
            endpoint = %control_endpoint.display(),
            roots = ?config.resolved_workspace_roots(),
            "vcad started"
        );
        Ok(Self {
            api,
            config,
            paths,
            control_endpoint,
            dev_ws,
            remote,
            local: Some(local),
            discovery,
        })
    }

    /// Stops listening and, unless configured to detach, stops managed
    /// sessions within the grace period.
    pub async fn shutdown(mut self) {
        self.local.take();
        self.dev_ws.take();
        if let Some(remote) = self.remote.take() {
            remote.shutdown().await;
        }
        if let Some(d) = self.discovery.take() {
            d.abort();
        }
        if self.config.shutdown.managed_sessions == ShutdownMode::Terminate {
            self.api
                .sessions()
                .stop_all(Duration::from_secs(self.config.shutdown.grace_secs))
                .await;
        }
        tracing::info!("vcad stopped");
    }
}

/// Resolves when the process is asked to stop (Ctrl-C, or SIGTERM on Unix).
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
