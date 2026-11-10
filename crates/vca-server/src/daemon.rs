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
use crate::restore::{Restore, RestoreSettings, WindowBackend, live_sessions, platform_backend};
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
    /// Replaces the window backend of session restore (tests use a fake so
    /// that no real window is ever opened or moved).
    pub restore_backend: Option<Arc<dyn WindowBackend>>,
    /// Replaces the OS verifier (tests use a fake so that no real Windows
    /// Hello or Touch ID prompt is ever shown).
    pub verifier: Option<Arc<dyn crate::security::verifier::LocalVerifier>>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("config_file", &self.config_file)
            .field("control", &self.control)
            .field("data_dir", &self.data_dir)
            .field("process_control", &self.process_control.is_some())
            .field("restore_backend", &self.restore_backend.is_some())
            .field("verifier", &self.verifier.is_some())
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
    hooks_watch: Option<tokio::task::JoinHandle<()>>,
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
                allow_generic_launch: sessions.allow_generic_launch,
                workspace_roots: WorkspaceRoots::new(config.resolved_workspace_roots()),
                session_names: Some(paths.data_dir.join(crate::names::NAMES_FILE)),
            },
            Some(Arc::new(broker)),
        );
        let api = Api::new(
            mgr.clone(),
            config.server_name(),
            Duration::from_secs(config.resources.push_interval_secs),
        );
        let discovery = crate::discovery::Discovery::new(
            mgr.clone(),
            vca_adapters::builtin_adapters(),
            crate::discovery::DiscoverySettings {
                enabled: config.discovery.enabled,
                interval: Duration::from_secs(config.discovery.interval_secs),
                detect_terminals: config.discovery.detect_terminals,
                data_dir: Some(paths.data_dir.clone()),
            },
        );
        api.set_discovery(discovery.clone());
        // The pronunciation dictionary of spoken alerts (the PC's voice and
        // every device's), with readings suggested for new session names.
        let pronunciations =
            crate::pronunciations::Pronunciations::open(Some(&paths.data_dir), Some(api.notices()));
        pronunciations.set_llm_from_config(&config.speech, &paths.data_dir);
        api.set_pronunciations(Arc::clone(&pronunciations));
        drop(pronunciations.start(&mgr));
        if let Some(speech) = crate::speech::Speech::from_config(&config.speech, &paths.data_dir)
            .context("starting server-side speech recognition")?
        {
            tracing::info!(engine = speech.engine_name(), "speech recognition on");
            api.set_speech(Arc::new(speech));
        }
        if let Some(tts) = crate::tts::Tts::from_config(&config.tts, &paths.data_dir)
            .context("starting spoken alerts (TTS)")?
        {
            tracing::info!(engines = tts.len(), "spoken alerts from the PC on");
            tts.set_pronunciations(Arc::clone(&pronunciations));
            api.set_tts(Arc::new(tts));
        }
        // Spoken result summaries: devices opt in; without an LLM profile
        // they get a fixed "finished" line.
        api.set_summaries(Arc::new(crate::voice_brief::Summarizer::from_config(
            &config.speech,
            &paths.data_dir,
        )));
        let cloud = crate::cloud::Cloud::open(&config.cloud, &paths.data_dir)
            .map_err(|e| anyhow::anyhow!("invalid [cloud] configuration: {e}"))?;
        api.set_cloud(Arc::new(cloud));
        api.set_attachments(Arc::new(crate::attachments::Attachments::new(
            config.attachments.clone(),
        )));
        api.set_push(Arc::new(crate::push::PushStore::in_data_dir(
            &paths.data_dir,
        )));
        api.set_folders(Arc::new(crate::folders::Folders::new(
            WorkspaceRoots::new(config.resolved_workspace_roots()),
            crate::folders::TranscriptSources::from_env(),
        )));
        api.set_engines(Arc::new(crate::engines::EngineAdmin::new(
            paths.config_file.clone(),
            paths.data_dir.clone(),
            &config,
        )));
        let shells = crate::shell::Shells::new(
            mgr.clone(),
            crate::shell::ShellSettings::from(&config.shell),
            vca_net::system_clock(),
            crate::shell::audit::ShellAudit::new(
                paths.data_dir.join(crate::shell::audit::AUDIT_FILE),
                crate::shell::audit::AUDIT_MAX_BYTES,
            ),
            api.notices(),
        );
        shells.start();
        api.set_shells(shells);
        let live_mgr = mgr.clone();
        let restore = Restore::new(
            RestoreSettings::new(&paths.data_dir, &config.restore, &control_endpoint),
            opts.restore_backend
                .clone()
                .unwrap_or_else(platform_backend),
            Arc::new(move |procs| live_sessions(&live_mgr, procs)),
            WorkspaceRoots::new(config.resolved_workspace_roots()),
            api.notices(),
        );
        restore.start();
        api.set_restore(restore);
        // An older version's sign-in shortcut started vcad itself; point it
        // at the desktop app. Only the real server does this (tests run the
        // server inside their own binary with a fake window backend).
        if opts.restore_backend.is_none() {
            tokio::task::spawn_blocking(migrate_sign_in_shortcut);
        }
        // The conductor (opt-in, off by default); the global kill switch
        // blocks it.
        let kill_shells = api.shells().cloned();
        let conductor = crate::conductor::Conductor::new(
            mgr.clone(),
            api.notices(),
            crate::conductor::ConductorOptions {
                data_dir: paths.data_dir.clone(),
                config_file: paths.config_file.clone(),
                vca_program: None,
                clock: None,
                kill_switch: Some(Arc::new(move || {
                    kill_shells
                        .as_ref()
                        .is_some_and(|s| s.kill_switch_tripped())
                })),
            },
        );
        api.set_conductor(conductor.clone());
        drop(conductor.start(api.clone()));
        // The human task inbox: per PC, shared by every device; audited in
        // the same log as remote shells.
        let tasks = crate::tasks::Tasks::new(
            mgr.clone(),
            api.notices(),
            crate::tasks::TasksOptions {
                data_dir: Some(paths.data_dir.clone()),
                machine: config.server_name(),
                ..Default::default()
            },
        );
        tasks.set_llm_from_config(&config.speech, &paths.data_dir);
        if let Some(shells) = api.shells().cloned() {
            tasks.set_audit(Arc::new(move |actor, event, detail| {
                shells.record_audit(actor, event, detail);
            }));
        }
        drop(tasks.start());
        api.set_tasks(tasks);
        // Before the control socket opens: gated requests are refused
        // while this is missing, so there is no unchecked moment at startup.
        let security = match crate::security::Security::open(
            &paths.data_dir,
            &paths.config_dir,
            config.server_name(),
            opts.verifier.clone(),
        ) {
            Ok(security) => {
                let security = Arc::new(security);
                api.set_security(Arc::clone(&security));
                // Before anything accepts devices: the owner's saved switch
                // holds from the first connection on.
                if let Some(shells) = api.shells().cloned() {
                    security.attach_shells(shells);
                }
                Some(security)
            }
            Err(err) => {
                tracing::error!("security services are off: {err:#}");
                None
            }
        };
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

        if let Some(security) = &security {
            match &remote {
                Some(r) => {
                    security.attach_totp(r.totp_manager(), Some(r.audit_log()));
                    // Pairings and failed sign-ins become notifications.
                    let (watcher, runtime) =
                        (Arc::clone(security), tokio::runtime::Handle::current());
                    r.audit_log().set_observer(Box::new(move |event| {
                        let _runtime = runtime.enter();
                        watcher.on_audit(event);
                    }));
                }
                None => {
                    let file = paths.data_dir.join("remote").join("totp.json");
                    match vca_net::TotpManager::open(file, vca_net::system_clock()) {
                        Ok(totp) => security.attach_totp(Arc::new(totp), None),
                        Err(err) => tracing::warn!(%err, "authenticator store unavailable"),
                    }
                }
            }
        }

        let discovery = discovery.start();
        // Whether the VCA hooks are still in Claude Code's settings (read
        // only). Only the real server reads the user's agent settings; tests
        // run the server with a fake window backend.
        let hooks_watch = opts.restore_backend.is_none().then(|| {
            crate::hooks_watch::spawn(mgr.clone(), vca_adapters::claude::default_config_dir())
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
            hooks_watch,
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
        if let Some(w) = self.hooks_watch.take() {
            w.abort();
        }
        self.api.sessions().stop_cloud_polling();
        // Shells never outlive vcad, whatever the shutdown mode.
        if let Some(shells) = self.api.shells() {
            shells.close_all(crate::shell::CloseReason::Shutdown).await;
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

/// Rewrites VCA's own old sign-in shortcut (target vcad) to the desktop
/// app, logging what was done.
fn migrate_sign_in_shortcut() {
    use crate::restore::shortcut;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if !shortcut::targets_vcad(&exe) {
        return;
    }
    let Some(app) = shortcut::desktop_app_next_to(&exe) else {
        return;
    };
    match shortcut::migrate_sign_in(&app) {
        Ok(true) => tracing::info!(
            target = %app.display(),
            "restore at sign-in: the shortcut now opens the desktop app instead of vcad"
        ),
        Ok(false) => {}
        Err(err) => tracing::warn!("restore at sign-in: cannot update the shortcut: {err}"),
    }
}

/// Set by [`request_shutdown`]; awaited by [`shutdown_signal`].
static SHUTDOWN_REQUESTED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Makes [`shutdown_signal`] resolve, as if Ctrl-C was pressed (a
/// [`LocalRequest::Shutdown`](crate::control::LocalRequest::Shutdown) on the
/// owner-only control channel).
pub fn request_shutdown() {
    tracing::info!("shutdown requested on the control channel");
    SHUTDOWN_REQUESTED.notify_one();
}

/// Resolves when the process is asked to stop (Ctrl-C, SIGTERM on Unix, a
/// console close, log-off or system shutdown on Windows, or
/// [`request_shutdown`]) and says which.
pub async fn shutdown_signal() -> &'static str {
    tokio::select! {
        reason = os_shutdown_signal() => reason,
        _ = SHUTDOWN_REQUESTED.notified() => "a shutdown request on the control channel",
    }
}

/// Waits for one OS signal. A signal that cannot be listened for never
/// resolves (and says so in the log): it must not stop the server.
async fn os_shutdown_signal() -> &'static str {
    async fn ctrl_c() -> &'static str {
        match tokio::signal::ctrl_c().await {
            Ok(()) => "Ctrl-C",
            Err(err) => {
                tracing::warn!(%err, "cannot listen for Ctrl-C");
                std::future::pending().await
            }
        }
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())
            .inspect_err(|err| tracing::warn!(%err, "cannot listen for SIGTERM"))
            .ok();
        let mut hup = signal(SignalKind::hangup())
            .inspect_err(|err| tracing::warn!(%err, "cannot listen for SIGHUP"))
            .ok();
        async fn recv(s: Option<&mut tokio::signal::unix::Signal>) {
            match s {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending().await,
            }
        }
        tokio::select! {
            reason = ctrl_c() => reason,
            _ = recv(term.as_mut()) => "SIGTERM",
            _ = recv(hup.as_mut()) => "SIGHUP",
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        macro_rules! listen {
            ($f:ident, $what:literal) => {
                async {
                    match windows::$f() {
                        Ok(mut s) => {
                            s.recv().await;
                            $what
                        }
                        Err(err) => {
                            tracing::warn!(%err, concat!("cannot listen for ", $what));
                            std::future::pending().await
                        }
                    }
                }
            };
        }
        tokio::select! {
            reason = ctrl_c() => reason,
            reason = listen!(ctrl_break, "Ctrl-Break") => reason,
            reason = listen!(ctrl_close, "the console closing") => reason,
            reason = listen!(ctrl_logoff, "the user signing out") => reason,
            reason = listen!(ctrl_shutdown, "the system shutting down") => reason,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        ctrl_c().await
    }
}

/// Logs panics with a timestamp before the default report (which only
/// goes to stderr) and makes sure they reach the log file.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("unnamed");
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        tracing::error!(
            thread,
            location,
            backtrace = %std::backtrace::Backtrace::force_capture(),
            "panic: {payload}"
        );
        default(info);
        use std::io::Write;
        let _ = std::io::stderr().flush();
    }));
}

/// Logs a line every `every` while the server runs, so that the log shows
/// when a process that was ended from outside (which cannot log) was last
/// alive.
pub fn spawn_heartbeat(every: Duration) -> tokio::task::JoinHandle<()> {
    let started = std::time::Instant::now();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            tracing::info!(
                uptime_mins = started.elapsed().as_secs() / 60,
                pid = std::process::id(),
                "vcad is running"
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requested_shutdown_resolves_the_signal() {
        request_shutdown();
        let reason = tokio::time::timeout(Duration::from_secs(5), shutdown_signal())
            .await
            .expect("shutdown_signal resolves after request_shutdown");
        assert!(reason.contains("control channel"));
    }
}
