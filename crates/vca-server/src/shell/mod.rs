//! Remote shell sessions: admin devices open a plain shell (PowerShell,
//! pwsh, cmd, bash, zsh, sh) on this computer and run commands in it.
//!
//! The server is authoritative for every rule; clients only add friction:
//!
//! - only admin-scope devices (and the owner's local clients) get here; the
//!   scope check happens in [`crate::api::Api`] and in `vca-net`;
//! - every `OpenShell` carries a fresh TOTP code, checked by the transport's
//!   TOTP verifier including its replay protection; a good code starts a
//!   per-device step-up window;
//! - the shell program comes from a fixed per-OS allow-list, never a path
//!   from the request; the project folder must be inside the workspace roots;
//! - shells stop after an idle timeout and an absolute lifetime, on the kill
//!   switch and at server shutdown; they never run elevated (vcad itself is
//!   never elevated and nothing here asks for elevation);
//! - `dangerous` commands ([`danger::classify`]) need the step-up window;
//! - a shell belongs to the device that opened it; another admin device
//!   watches or types into it only after joining it with a TOTP code
//!   ([`Shells::join`]), a grant that ends with the shell or the owner's
//!   switch;
//! - input of two or more lines is never typed before the person confirmed
//!   all of it ([`RunOutcome::Confirm`], [`InputCheck::Confirm`]);
//! - everything is written to an append-only audit log ([`audit`]) and
//!   announced to every client as [`ServerMsg::ShellNotice`].

pub mod audit;
pub mod danger;

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use vca_core::error::ErrorCode;
use vca_core::{
    AgentKind, AuditEntry, DangerLevel, Error, Input, ServerMsg, SessionAction, SessionId,
    SessionInfo, SessionState, ShellInfo, ShellRequest, UnixMillis, validate_session_name,
};
use vca_net::{CodeKind, KillSwitch, SharedClock, TotpError, TotpManager};
use vca_pty::{PtySize, SpawnOptions};

use crate::config::ShellConfig;
use crate::sessions::{SessionManager, Update};
use crate::util::lock;

use self::audit::{ShellAudit, events};

/// Wrong TOTP codes accepted per device within [`FAILURE_WINDOW_MS`] before
/// further attempts are refused.
const MAX_TOTP_FAILURES: usize = 5;
const FAILURE_WINDOW_MS: u64 = 5 * 60 * 1000;
/// Longest command accepted by `ShellRun`.
pub const MAX_COMMAND_BYTES: usize = 8 * 1024;
/// How long `ShellRun` waits for the input to reach the shell.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Environment variables (by prefix) a shell does not inherit from vcad:
/// agent session markers, API keys of the daemon and VCA's own variables.
const STRIPPED_ENV_PREFIXES: &[&str] = &["CLAUDE", "ANTHROPIC", "VCA_"];
/// How long before a shell closes clients get a `ShellClosingSoon` (at
/// most half the idle timeout or lifetime, for short test settings).
pub const CLOSING_SOON: Duration = Duration::from_secs(60);
const SHELL_SIZE: PtySize = PtySize {
    cols: 120,
    rows: 40,
};

/// Durations and switches of remote shells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSettings {
    pub enabled: bool,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    pub step_up_window: Duration,
}

impl From<&ShellConfig> for ShellSettings {
    fn from(cfg: &ShellConfig) -> Self {
        Self {
            enabled: cfg.enabled,
            idle_timeout: cfg.idle_timeout(),
            max_lifetime: cfg.max_lifetime(),
            step_up_window: cfg.step_up_window(),
        }
    }
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self::from(&ShellConfig::default())
    }
}

/// The message of the refusal for a device that has not joined a shell
/// another device opened. Apps recognise `totp_required` with "join".
const JOIN_REQUIRED: &str = "this shell was opened on another device; join it with a code \
     from your authenticator app to watch it or type into it";

/// What `ShellRun` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The command was typed, with the level of its most dangerous line.
    Ran(DangerLevel),
    /// Nothing was typed: the command has `lines` lines and needs an
    /// explicit confirmation (`ShellRun` with `confirm`).
    Confirm { command: String, lines: u32 },
}

/// The verdict on raw input (`SendInput`) into a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputCheck {
    /// It may be typed.
    Pass,
    /// Text of `lines` lines: not typed; it goes through `ShellRun` with
    /// `confirm` after the person saw all of it.
    Confirm { text: String, lines: u32 },
}

/// The lines of one submission: one trailing line break (`\n`, `\r\n` or
/// `\r`) is not a line of its own; every other `\n`, `\r\n` or `\r` starts
/// one. A single command with Enter is one line; a pasted script, a line
/// continuation or a heredoc is several.
pub fn submission_lines(text: &str) -> Vec<&str> {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(text);
    let mut lines = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(['\r', '\n']) {
        lines.push(&rest[..i]);
        let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
        rest = &rest[i + skip..];
    }
    lines.push(rest);
    lines
}

/// The most dangerous level of the whole text and of each of its lines.
fn classify_lines(text: &str, lines: &[&str]) -> DangerLevel {
    lines
        .iter()
        .map(|line| danger::classify(line).level)
        .chain(std::iter::once(danger::classify(text).level))
        .max()
        .unwrap_or(DangerLevel::Safe)
}

/// Who asks: a paired device, or `local` for the owner's local clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub device_id: String,
    pub device_name: String,
}

impl Actor {
    pub fn local() -> Self {
        Self {
            device_id: "local".into(),
            device_name: "local".into(),
        }
    }
}

/// Why a shell was closed (`ShellNotice.reason`, audit `detail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// A client stopped it.
    User,
    Idle,
    Lifetime,
    KillSwitch,
    /// The shell process ended by itself.
    Exit,
    /// vcad is stopping.
    Shutdown,
}

impl CloseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Idle => "idle",
            Self::Lifetime => "lifetime",
            Self::KillSwitch => "kill_switch",
            Self::Exit => "exit",
            Self::Shutdown => "shutdown",
        }
    }
}

/// An allow-listed shell program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProgram {
    /// The allow-list name (`AgentKind::Shell` content).
    pub name: &'static str,
    pub program: PathBuf,
    pub args: Vec<&'static str>,
}

/// No authenticator is enrolled on this computer. The apps recognise the
/// `unsupported` code with "TOTP ... not set up" in the message.
fn not_enrolled() -> Error {
    api_error(
        ErrorCode::Unsupported,
        "TOTP is not set up on this computer; set it up in the VCA app on this PC \
         (Settings > Connect phone > Two-step verification) or run `vca totp enroll`",
    )
}

fn api_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::api(code, message)
}

/// The shell names this OS allows.
pub fn allowed_shells() -> &'static [&'static str] {
    if cfg!(windows) {
        &["powershell", "pwsh", "cmd"]
    } else {
        &["bash", "zsh", "sh"]
    }
}

/// Maps a requested shell name to its program (rule 3). Only the exact
/// allow-list names and `default` are accepted; anything that looks like a
/// path or carries extra characters is refused.
pub fn resolve_shell(requested: &str) -> Result<ShellProgram, Error> {
    let wanted = requested.trim().to_ascii_lowercase();
    let allowed = allowed_shells();
    let name: &'static str = if wanted == "default" {
        default_shell_name()
    } else {
        allowed
            .iter()
            .copied()
            .find(|n| *n == wanted)
            .ok_or_else(|| {
                api_error(
                    ErrorCode::InvalidRequest,
                    format!(
                        "unknown shell {requested:?}; this computer allows {} or default",
                        allowed.join(", ")
                    ),
                )
            })?
    };
    let program = shell_program(name).ok_or_else(|| {
        api_error(
            ErrorCode::Unavailable,
            format!("{name} is not installed on this computer"),
        )
    })?;
    let args = if name == "powershell" || name == "pwsh" {
        vec!["-NoLogo"]
    } else {
        Vec::new()
    };
    Ok(ShellProgram {
        name,
        program,
        args,
    })
}

#[cfg(windows)]
fn default_shell_name() -> &'static str {
    "powershell"
}

#[cfg(not(windows))]
fn default_shell_name() -> &'static str {
    let from_env = std::env::var("SHELL").ok().and_then(|s| {
        let base = s.rsplit('/').next().unwrap_or("").to_owned();
        allowed_shells().iter().copied().find(|n| *n == base)
    });
    from_env.unwrap_or("sh")
}

/// Fixed install locations; `pwsh` (no fixed location) is looked up by
/// name on `PATH`.
#[cfg(windows)]
fn shell_program(name: &str) -> Option<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let system32 = root.join("System32");
    match name {
        "powershell" => Some(
            system32
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        )
        .filter(|p| p.is_file()),
        "cmd" => Some(system32.join("cmd.exe")).filter(|p| p.is_file()),
        "pwsh" => {
            let program_files = std::env::var_os("ProgramFiles")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
            let fixed = program_files.join("PowerShell").join("7").join("pwsh.exe");
            if fixed.is_file() {
                return Some(fixed);
            }
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("pwsh.exe"))
                    .find(|p| p.is_absolute() && p.is_file())
            })
        }
        _ => None,
    }
}

#[cfg(not(windows))]
fn shell_program(name: &str) -> Option<PathBuf> {
    ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
        .iter()
        .map(|dir| PathBuf::from(dir).join(name))
        .find(|p| p.is_file())
}

/// Spawn options for a shell: no inherited agent/VCA variables.
fn spawn_options(program: &ShellProgram, dir: &std::path::Path) -> SpawnOptions {
    let mut opts = SpawnOptions::new(&program.program)
        .args(program.args.iter().copied())
        .cwd(dir)
        .size(SHELL_SIZE);
    for (key, _) in std::env::vars_os() {
        if stripped_env(&key) {
            opts = opts.env_remove(key);
        }
    }
    opts
}

/// `d` in milliseconds, saturating (a configured timeout may be huge).
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `now` plus `d`, saturating: an "endless" timeout must not wrap into the
/// past and close a shell at once.
fn add_millis(now: UnixMillis, d: Duration) -> UnixMillis {
    now.saturating_add(millis(d))
}

fn stripped_env(key: &OsString) -> bool {
    let key = key.to_string_lossy().to_ascii_uppercase();
    STRIPPED_ENV_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// An open shell.
#[derive(Debug, Clone)]
struct OpenShell {
    device_id: String,
    device_name: String,
    idle_deadline: UnixMillis,
    hard_deadline: UnixMillis,
    /// The idle deadline last announced in `SessionInfo.shell`.
    published_idle: UnixMillis,
    /// The deadline a `ShellClosingSoon` was sent for.
    warned_for: Option<UnixMillis>,
    /// Other devices that joined this shell with a code.
    joined: HashSet<String>,
}

#[derive(Default)]
struct State {
    shells: HashMap<SessionId, OpenShell>,
    /// The device that opened each shell, kept after it closed so that
    /// only that device reads an ended shell's output.
    owners: HashMap<SessionId, String>,
    /// Step-up window end per device.
    step_up: HashMap<String, UnixMillis>,
    /// Recent wrong TOTP codes per device.
    failures: HashMap<String, Vec<UnixMillis>>,
}

struct Inner {
    mgr: SessionManager,
    settings: ShellSettings,
    clock: SharedClock,
    audit: ShellAudit,
    totp: OnceLock<Arc<TotpManager>>,
    kill: OnceLock<KillSwitch>,
    notices: broadcast::Sender<ServerMsg>,
    state: Mutex<State>,
    reaper: Mutex<Option<JoinHandle<()>>>,
    /// The owner's switch (`crate::security`); `[shell] enabled` still wins.
    owner_enabled: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(t) = lock(&self.reaper).take() {
            t.abort();
        }
    }
}

/// The remote shell service. Cheap to clone.
#[derive(Clone)]
pub struct Shells {
    inner: Arc<Inner>,
}

impl Shells {
    /// `notices` reaches every connected client.
    pub fn new(
        mgr: SessionManager,
        settings: ShellSettings,
        clock: SharedClock,
        audit: ShellAudit,
        notices: broadcast::Sender<ServerMsg>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                mgr,
                settings,
                clock,
                audit,
                totp: OnceLock::new(),
                kill: OnceLock::new(),
                notices,
                state: Mutex::new(State::default()),
                reaper: Mutex::new(None),
                owner_enabled: AtomicBool::new(true),
            }),
        }
    }

    pub fn settings(&self) -> &ShellSettings {
        &self.inner.settings
    }

    /// The owner's switch for remote shells. Turning it on is gated by
    /// local verification in `crate::security`.
    ///
    /// Turning it off also closes the shells that are open: "off" means
    /// nobody types into a shell on this computer from outside, not only
    /// that no new one opens.
    pub fn set_owner_enabled(&self, enabled: bool) {
        self.inner.owner_enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            // Grants end now, not only when the shells are gone.
            for open in lock(&self.inner.state).shells.values_mut() {
                open.joined.clear();
            }
        }
        if !enabled
            && self.any_open()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            let shells = self.clone();
            rt.spawn(async move { shells.close_all(CloseReason::User).await });
        }
    }

    /// Remote shells are allowed: by the config and by the owner's switch.
    pub fn remote_enabled(&self) -> bool {
        self.inner.settings.enabled && self.inner.owner_enabled.load(Ordering::SeqCst)
    }

    /// The TOTP verifier of the remote transport (shared with sign-in, so
    /// a code is accepted once across both). Without it no shell opens.
    pub fn set_totp(&self, totp: Arc<TotpManager>) {
        let _ = self.inner.totp.set(totp);
    }

    pub fn set_kill_switch(&self, kill: KillSwitch) {
        let _ = self.inner.kill.set(kill);
    }

    /// Stops every shell whenever the kill switch trips.
    pub fn watch_kill_switch(&self) -> Option<JoinHandle<()>> {
        let kill = self.inner.kill.get()?.clone();
        let shells = self.clone();
        Some(tokio::spawn(async move {
            let mut rx = kill.subscribe();
            loop {
                if rx.wait_for(|tripped| *tripped).await.is_err() {
                    return;
                }
                shells.close_all(CloseReason::KillSwitch).await;
                if rx.wait_for(|tripped| !*tripped).await.is_err() {
                    return;
                }
            }
        }))
    }

    /// Starts the task that enforces timeouts and notices shells that
    /// exited by themselves.
    pub fn start(&self) {
        let idle = self.inner.settings.idle_timeout;
        let life = self.inner.settings.max_lifetime;
        let tick = (idle.min(life) / 4).clamp(Duration::from_millis(50), Duration::from_secs(5));
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let mut updates = self.inner.mgr.subscribe_updates();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    update = updates.recv() => match update {
                        Ok(Update::Changed(info)) => {
                            let exited_shell = info.shell.is_some()
                                && matches!(info.state, SessionState::Exited { .. });
                            if !exited_shell {
                                continue;
                            }
                        }
                        Ok(Update::Removed(_)) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return,
                    },
                }
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                Shells { inner }.reap().await;
            }
        });
        if let Some(old) = lock(&self.inner.reaper).replace(task) {
            old.abort();
        }
    }

    fn now(&self) -> UnixMillis {
        self.inner.clock.now_millis()
    }

    fn ensure_enabled(&self) -> Result<(), Error> {
        if self.remote_enabled() {
            Ok(())
        } else {
            Err(api_error(
                ErrorCode::Unsupported,
                "remote shells are off on this computer",
            ))
        }
    }

    fn ensure_not_killed(&self) -> Result<(), Error> {
        if self.inner.kill.get().is_some_and(KillSwitch::is_tripped) {
            Err(api_error(
                ErrorCode::Forbidden,
                "the kill switch is on; reset it on this computer first",
            ))
        } else {
            Ok(())
        }
    }

    fn audit(&self, actor: &Actor, event: &str, session: Option<&SessionId>, detail: &str) {
        self.inner.audit.record(
            self.now(),
            &actor.device_id,
            &actor.device_name,
            event,
            session,
            detail,
        );
    }

    fn notice(&self, session_id: &SessionId, event: &str, device_name: &str, reason: Option<&str>) {
        let _ = self.inner.notices.send(ServerMsg::ShellNotice {
            session_id: session_id.clone(),
            event: event.into(),
            device_name: device_name.into(),
            reason: reason.map(str::to_owned),
        });
    }

    /// Writes an entry about a session (any session, not only shells) to
    /// the audit log.
    pub fn record_session_audit(
        &self,
        actor: &Actor,
        event: &str,
        session: &SessionId,
        detail: &str,
    ) {
        self.audit(actor, event, Some(session), detail);
    }

    /// Drops the step-up window and wrong-code record of a revoked device.
    pub fn forget_device(&self, device_id: &str) {
        let mut state = lock(&self.inner.state);
        state.step_up.remove(device_id);
        state.failures.remove(device_id);
        for open in state.shells.values_mut() {
            open.joined.remove(device_id);
        }
    }

    /// True while the device may run dangerous commands without a code.
    pub fn in_step_up(&self, device_id: &str) -> bool {
        let now = self.now();
        lock(&self.inner.state)
            .step_up
            .get(device_id)
            .is_some_and(|until| now < *until)
    }

    /// The error for a missing code: `totp_required`, or `unsupported`
    /// when no authenticator is enrolled (so the app explains how to set
    /// one up instead of asking for a code that does not exist).
    fn code_required(&self, message: &str) -> Error {
        if !self.inner.totp.get().is_some_and(|t| t.is_enrolled()) {
            return not_enrolled();
        }
        api_error(ErrorCode::TotpRequired, message)
    }

    /// Checks a TOTP code, or a single-use backup code, for `purpose`
    /// (`open_shell` or `shell_run`); a good code starts the device's
    /// step-up window. Codes are never logged.
    fn verify_totp(
        &self,
        actor: &Actor,
        code: Option<&str>,
        purpose: &str,
        session: Option<&SessionId>,
    ) -> Result<(), Error> {
        let Some(code) = code.map(str::trim).filter(|c| !c.is_empty()) else {
            return Err(self.code_required("enter a code from your authenticator app"));
        };
        let now = self.now();
        let Some(totp) = self.inner.totp.get() else {
            return Err(api_error(
                ErrorCode::Unsupported,
                "remote shells need the remote transport and TOTP on this computer",
            ));
        };
        {
            let mut state = lock(&self.inner.state);
            let recent = state.failures.entry(actor.device_id.clone()).or_default();
            recent.retain(|t| now.saturating_sub(*t) < FAILURE_WINDOW_MS);
            if recent.len() >= MAX_TOTP_FAILURES {
                return Err(api_error(
                    ErrorCode::Forbidden,
                    "too many wrong codes; try again in a few minutes",
                ));
            }
            // Counted as a failure until it proves good, so that requests
            // checked at the same time cannot all pass the limit above.
            recent.push(now);
        }
        // An attempt that did not test the code gives its place back.
        let release = || {
            let mut state = lock(&self.inner.state);
            if let Some(recent) = state.failures.get_mut(&actor.device_id)
                && let Some(i) = recent.iter().position(|t| *t == now)
            {
                recent.remove(i);
            }
        };
        let failed = |reason: &str| {
            self.audit(
                actor,
                events::STEP_UP_FAILED,
                session,
                &format!("purpose={purpose} reason={reason}"),
            );
        };
        match totp.verify_code_or_backup(code) {
            Ok(kind) => {
                let until = add_millis(now, self.inner.settings.step_up_window);
                let mut state = lock(&self.inner.state);
                state.step_up.insert(actor.device_id.clone(), until);
                state.failures.remove(&actor.device_id);
                drop(state);
                let method = match kind {
                    CodeKind::Totp => "totp",
                    CodeKind::BackupCode => "backup_code",
                };
                self.audit(
                    actor,
                    events::STEP_UP_OK,
                    session,
                    &format!("purpose={purpose} method={method}"),
                );
                Ok(())
            }
            Err(TotpError::NotEnrolled) => {
                release();
                Err(not_enrolled())
            }
            Err(TotpError::Invalid) => {
                failed("invalid");
                Err(api_error(ErrorCode::TotpInvalid, "the code is not valid"))
            }
            Err(TotpError::Replayed) => {
                failed("replayed");
                Err(api_error(
                    ErrorCode::TotpInvalid,
                    "this code was already used; wait for the next code",
                ))
            }
            Err(TotpError::Storage) => {
                release();
                Err(api_error(
                    ErrorCode::Internal,
                    "cannot record the TOTP code use",
                ))
            }
        }
    }

    /// Writes an entry for another service (session restore) to the audit
    /// log.
    pub fn record_audit(&self, actor: &Actor, event: &str, detail: &str) {
        self.audit(actor, event, None, detail);
    }

    /// The step-up rule of sensitive actions outside shells (session
    /// restore): inside the device's step-up window nothing more is needed,
    /// otherwise a fresh TOTP code, which starts the window. Refused while
    /// the kill switch is on.
    pub fn require_step_up(
        &self,
        actor: &Actor,
        totp: Option<&str>,
        purpose: &str,
    ) -> Result<(), Error> {
        self.ensure_not_killed()?;
        if self.in_step_up(&actor.device_id) {
            return Ok(());
        }
        self.verify_totp(actor, totp, purpose, None)
    }

    /// True while the kill switch is on.
    pub fn kill_switch_tripped(&self) -> bool {
        self.inner.kill.get().is_some_and(KillSwitch::is_tripped)
    }

    /// Opens a shell (`OpenShell`). Cheap checks come first so that a
    /// mistyped request does not use up the code.
    pub async fn open(&self, actor: &Actor, req: ShellRequest) -> Result<SessionInfo, Error> {
        self.ensure_enabled()?;
        self.ensure_not_killed()?;
        let program = resolve_shell(&req.shell)?;
        let dir = self
            .inner
            .mgr
            .workspace_roots()
            .prepare(&req.project_dir, false)?;
        let custom = validate_session_name(req.display_name.as_deref())
            .map_err(|reason| api_error(ErrorCode::InvalidRequest, reason))?;
        self.verify_totp(actor, Some(&req.totp), "open_shell", None)?;

        let agent = AgentKind::Shell(program.name.to_owned());
        let name = custom.unwrap_or_else(|| crate::sessions::default_name(&agent, &dir));
        let now = self.now();
        let hard = add_millis(now, self.inner.settings.max_lifetime);
        let idle = add_millis(now, self.inner.settings.idle_timeout).min(hard);
        let info = ShellInfo {
            idle_timeout_secs: self
                .inner
                .settings
                .idle_timeout
                .as_secs()
                .min(u32::MAX as u64) as u32,
            idle_deadline: idle,
            hard_deadline: hard,
            opened_by: actor.device_name.clone(),
            shell: program.name.to_owned(),
            project_dir: dir.clone(),
        };
        let opts = spawn_options(&program, &dir);
        let session = self
            .inner
            .mgr
            .start_shell(agent, dir.clone(), name, opts, info)?;
        {
            let mut state = lock(&self.inner.state);
            let mgr = &self.inner.mgr;
            state.owners.retain(|id, _| mgr.get(id).is_some());
            state
                .owners
                .insert(session.id.clone(), actor.device_id.clone());
            state.shells.insert(
                session.id.clone(),
                OpenShell {
                    device_id: actor.device_id.clone(),
                    device_name: actor.device_name.clone(),
                    idle_deadline: idle,
                    hard_deadline: hard,
                    published_idle: idle,
                    warned_for: None,
                    joined: HashSet::new(),
                },
            );
        }
        self.audit(
            actor,
            events::SHELL_OPEN,
            Some(&session.id),
            &format!("shell={} dir={}", program.name, dir.display()),
        );
        self.notice(&session.id, "opened", &actor.device_name, None);
        tracing::info!(
            session = %session.id,
            device = %actor.device_id,
            shell = program.name,
            "remote shell opened"
        );
        // The kill switch may have tripped meanwhile.
        if self.ensure_not_killed().is_err() {
            self.close(&session.id, CloseReason::KillSwitch, None).await;
            return Err(api_error(ErrorCode::Forbidden, "the kill switch is on"));
        }
        Ok(session)
    }

    /// Any remote shell is open.
    pub fn any_open(&self) -> bool {
        !lock(&self.inner.state).shells.is_empty()
    }

    /// True for a session that is an open remote shell.
    pub fn is_open(&self, id: &SessionId) -> bool {
        lock(&self.inner.state).shells.contains_key(id)
    }

    /// Checks that `id` is an open shell. `NotFound` for unknown sessions,
    /// `InvalidRequest` for agent sessions, `Conflict` for closed shells.
    fn require_open(&self, id: &SessionId) -> Result<(), Error> {
        let Some(session) = self.inner.mgr.get(id) else {
            return Err(api_error(ErrorCode::NotFound, format!("no session {id}")));
        };
        if !session.is_shell() {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                "commands can only be run in shell sessions",
            ));
        }
        if !self.is_open(id) {
            return Err(api_error(ErrorCode::Conflict, "the shell is closed"));
        }
        Ok(())
    }

    /// True when `actor` may watch and type into shell `id`: it opened it,
    /// or joined it while it is open.
    fn has_access(&self, actor: &Actor, id: &SessionId) -> bool {
        let state = lock(&self.inner.state);
        if let Some(open) = state.shells.get(id) {
            return open.device_id == actor.device_id || open.joined.contains(&actor.device_id);
        }
        state
            .owners
            .get(id)
            .is_some_and(|owner| *owner == actor.device_id)
    }

    /// Refuses a device that neither opened shell `id` nor joined it:
    /// `totp_required` (with "join" in the message), or `unsupported` when
    /// no authenticator is enrolled. Sessions that are not shells pass.
    pub fn check_access(&self, actor: &Actor, id: &SessionId) -> Result<(), Error> {
        let is_shell = self.inner.mgr.get(id).is_some_and(|s| s.is_shell());
        if !is_shell || self.has_access(actor, id) {
            return Ok(());
        }
        Err(self.code_required(JOIN_REQUIRED))
    }

    /// Lets another admin device watch and type into shell `id`
    /// (`ShellJoin`). Always needs a fresh code, also inside the step-up
    /// window; the grant lasts until the shell closes or the owner's switch
    /// goes off. The opener, and a device that already joined, pass
    /// without using the code.
    pub fn join(&self, actor: &Actor, id: &SessionId, totp: &str) -> Result<(), Error> {
        self.ensure_enabled()?;
        self.ensure_not_killed()?;
        self.require_open(id)?;
        if self.has_access(actor, id) {
            return Ok(());
        }
        self.verify_totp(actor, Some(totp), "shell_join", Some(id))?;
        let opened_by = {
            let mut state = lock(&self.inner.state);
            let Some(open) = state.shells.get_mut(id) else {
                return Err(api_error(ErrorCode::Conflict, "the shell is closed"));
            };
            open.joined.insert(actor.device_id.clone());
            open.device_name.clone()
        };
        // The owner may have turned shells off while the code was checked.
        if !self.remote_enabled() {
            if let Some(open) = lock(&self.inner.state).shells.get_mut(id) {
                open.joined.remove(&actor.device_id);
            }
            return Err(api_error(
                ErrorCode::Unsupported,
                "remote shells are off on this computer",
            ));
        }
        self.audit(
            actor,
            events::SHELL_JOIN,
            Some(id),
            &format!("opened_by={opened_by}"),
        );
        self.notice(id, "joined", &actor.device_name, None);
        tracing::info!(session = %id, device = %actor.device_id, "remote shell joined");
        Ok(())
    }

    /// Requires the step-up window for a dangerous command, checking `totp`
    /// when the window is over.
    fn step_up_for(
        &self,
        actor: &Actor,
        level: DangerLevel,
        totp: Option<&str>,
        session: &SessionId,
    ) -> Result<(), Error> {
        if level < DangerLevel::Dangerous || self.in_step_up(&actor.device_id) {
            return Ok(());
        }
        if totp.is_none_or(|c| c.trim().is_empty()) {
            return Err(self.code_required(
                "this command is dangerous; confirm it with a code from your authenticator app",
            ));
        }
        self.verify_totp(actor, totp, "shell_run", Some(session))
    }

    /// Types a command and Enter into a shell (`ShellRun`). A command of
    /// two or more lines is only typed with `confirm`; without it the
    /// answer is [`RunOutcome::Confirm`] and nothing reaches the shell.
    pub async fn run(
        &self,
        actor: &Actor,
        id: &SessionId,
        command: &str,
        totp: Option<&str>,
        confirm: bool,
    ) -> Result<RunOutcome, Error> {
        self.ensure_enabled()?;
        self.ensure_not_killed()?;
        self.require_open(id)?;
        self.check_access(actor, id)?;
        if command.trim().is_empty() {
            return Err(api_error(ErrorCode::InvalidRequest, "the command is empty"));
        }
        if command.len() > MAX_COMMAND_BYTES {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                format!("the command is longer than {MAX_COMMAND_BYTES} bytes"),
            ));
        }
        if command
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
        {
            return Err(api_error(
                ErrorCode::InvalidRequest,
                "the command must not contain control characters",
            ));
        }
        let lines = submission_lines(command);
        if lines.len() > 1 && !confirm {
            return Ok(RunOutcome::Confirm {
                command: command.to_owned(),
                lines: u32::try_from(lines.len()).unwrap_or(u32::MAX),
            });
        }
        let level = classify_lines(command, &lines);
        self.step_up_for(actor, level, totp, id)?;
        let text = lines.join("\n");
        let lines_detail = if lines.len() > 1 {
            format!(" lines={}", lines.len())
        } else {
            String::new()
        };
        self.audit(
            actor,
            events::SHELL_RUN,
            Some(id),
            &format!(
                "danger={}{lines_detail} command={}",
                level_name(level),
                vca_net::redact(&text)
            ),
        );
        let done = self.inner.mgr.send_input(
            id,
            Input::Text {
                text,
                submit: true,
                at_prompt: false,
            },
        )?;
        self.touch(id);
        match tokio::time::timeout(DELIVERY_TIMEOUT, done).await {
            Ok(Ok(Ok(()))) => Ok(RunOutcome::Ran(level)),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) | Err(_) => Err(api_error(
                ErrorCode::Unavailable,
                "the command did not reach the shell",
            )),
        }
    }

    /// Checks raw input (`SendInput`) into a shell from a device that
    /// opened or joined it: keys always pass; text of two or more lines is
    /// held for confirmation ([`InputCheck::Confirm`]); text that
    /// classifies as dangerous needs the step-up window (use `ShellRun`
    /// with a code). Counts as activity when it passes.
    pub fn check_input(
        &self,
        actor: &Actor,
        id: &SessionId,
        input: &Input,
    ) -> Result<InputCheck, Error> {
        self.ensure_enabled()?;
        self.ensure_not_killed()?;
        if !self.is_open(id) {
            return Err(api_error(ErrorCode::Conflict, "the shell is closed"));
        }
        self.check_access(actor, id)?;
        if let Input::Text { text, .. } = input {
            let lines = submission_lines(text);
            if lines.len() > 1 {
                return Ok(InputCheck::Confirm {
                    text: text.clone(),
                    lines: u32::try_from(lines.len()).unwrap_or(u32::MAX),
                });
            }
            let level = danger::classify(text).level;
            if level == DangerLevel::Dangerous && !self.in_step_up(&actor.device_id) {
                return Err(api_error(
                    ErrorCode::TotpRequired,
                    "this text is a dangerous command; send it with shell_run and a code",
                ));
            }
        }
        self.touch(id);
        Ok(InputCheck::Pass)
    }

    /// Records input activity: moves the idle deadline.
    pub fn touch(&self, id: &SessionId) {
        let now = self.now();
        let idle_ms = millis(self.inner.settings.idle_timeout);
        // Announce at most every 5 s (or a tenth of the timeout).
        let threshold = (idle_ms / 10).min(5_000);
        let publish = {
            let mut state = lock(&self.inner.state);
            let Some(open) = state.shells.get_mut(id) else {
                return;
            };
            open.idle_deadline = now.saturating_add(idle_ms).min(open.hard_deadline);
            if open.idle_deadline.saturating_sub(open.published_idle) >= threshold {
                open.published_idle = open.idle_deadline;
                Some(open.idle_deadline)
            } else {
                None
            }
        };
        if let Some(deadline) = publish {
            self.publish_idle(id, deadline);
        }
    }

    /// Announces a new idle deadline in `SessionInfo.shell`.
    fn publish_idle(&self, id: &SessionId, deadline: UnixMillis) {
        let Some(session) = self.inner.mgr.get(id) else {
            return;
        };
        self.inner
            .mgr
            .update_info(&session, |info| match info.shell.as_mut() {
                Some(shell) if shell.idle_deadline != deadline => {
                    shell.idle_deadline = deadline;
                    true
                }
                _ => false,
            });
    }

    /// Keeps an open shell open (`ShellKeepAlive`, the answer to
    /// `ShellClosingSoon`): the idle deadline moves to now plus the idle
    /// timeout, never past the absolute lifetime. Returns the new idle
    /// deadline. A `kept_open` notice follows when a `ShellClosingSoon` was out
    /// and the deadline really moved.
    pub fn keep_alive(&self, actor: &Actor, id: &SessionId) -> Result<UnixMillis, Error> {
        self.ensure_not_killed()?;
        self.require_open(id)?;
        self.check_access(actor, id)?;
        let now = self.now();
        let idle_ms = millis(self.inner.settings.idle_timeout);
        let (deadline, cleared) = {
            let mut state = lock(&self.inner.state);
            let Some(open) = state.shells.get_mut(id) else {
                return Err(api_error(ErrorCode::Conflict, "the shell is closed"));
            };
            open.idle_deadline = now.saturating_add(idle_ms).min(open.hard_deadline);
            open.published_idle = open.idle_deadline;
            let cleared = open.warned_for.is_some_and(|w| open.idle_deadline > w);
            if cleared {
                open.warned_for = None;
            }
            (open.idle_deadline, cleared)
        };
        self.publish_idle(id, deadline);
        if cleared {
            self.notice(id, "kept_open", &actor.device_name, None);
        }
        Ok(deadline)
    }

    /// Closes a shell: audit, notice, and (unless it already ended) stops
    /// the process. `by` is the closing client (`User`); other reasons are
    /// attributed to the device that opened the shell. Returns `false` when
    /// it was not open.
    pub async fn close(&self, id: &SessionId, reason: CloseReason, by: Option<&Actor>) -> bool {
        let Some(open) = lock(&self.inner.state).shells.remove(id) else {
            return false;
        };
        let actor = by.cloned().unwrap_or(Actor {
            device_id: open.device_id.clone(),
            device_name: open.device_name.clone(),
        });
        self.audit(
            &actor,
            events::SHELL_CLOSE,
            Some(id),
            &format!("reason={}", reason.as_str()),
        );
        self.notice(id, "closed", &open.device_name, Some(reason.as_str()));
        tracing::info!(session = %id, reason = reason.as_str(), "remote shell closed");
        if reason != CloseReason::Exit
            && let Err(err) = self.inner.mgr.control(id, SessionAction::Stop).await
        {
            tracing::debug!(session = %id, %err, "stopping the shell");
        }
        true
    }

    /// Closes every shell (kill switch, shutdown). The kill switch also
    /// ends every step-up window.
    pub async fn close_all(&self, reason: CloseReason) {
        let ids: Vec<SessionId> = {
            let mut state = lock(&self.inner.state);
            if reason == CloseReason::KillSwitch {
                state.step_up.clear();
            }
            state.shells.keys().cloned().collect()
        };
        for id in ids {
            self.close(&id, reason, None).await;
        }
    }

    /// How long before a deadline `ShellClosingSoon` goes out.
    fn closing_soon_ms(&self) -> u64 {
        let settings = &self.inner.settings;
        let shortest = settings.idle_timeout.min(settings.max_lifetime) / 2;
        CLOSING_SOON.min(shortest).as_millis() as u64
    }

    /// One pass of timeout enforcement; also notices shells that exited,
    /// and warns once per deadline about a shell that closes soon.
    pub async fn reap(&self) {
        let now = self.now();
        let warn_ms = self.closing_soon_ms();
        let mut soon: Vec<(SessionId, u32, &'static str)> = Vec::new();
        let due: Vec<(SessionId, CloseReason)> = {
            let mut state = lock(&self.inner.state);
            for (id, open) in state.shells.iter_mut() {
                let deadline = open.idle_deadline.min(open.hard_deadline);
                if now >= deadline || open.warned_for == Some(deadline) {
                    continue;
                }
                let left = deadline - now;
                if left <= warn_ms {
                    open.warned_for = Some(deadline);
                    let reason = if open.idle_deadline >= open.hard_deadline {
                        CloseReason::Lifetime
                    } else {
                        CloseReason::Idle
                    };
                    let secs = left.div_ceil(1000).min(u64::from(u32::MAX)) as u32;
                    soon.push((id.clone(), secs, reason.as_str()));
                }
            }
            state
                .shells
                .iter()
                .filter_map(|(id, open)| {
                    let exited = self
                        .inner
                        .mgr
                        .get(id)
                        .is_none_or(|s| matches!(s.info().state, SessionState::Exited { .. }));
                    let reason = if exited {
                        CloseReason::Exit
                    } else if now >= open.hard_deadline {
                        CloseReason::Lifetime
                    } else if now >= open.idle_deadline {
                        CloseReason::Idle
                    } else {
                        return None;
                    };
                    Some((id.clone(), reason))
                })
                .collect()
        };
        for (session_id, seconds_left, reason) in soon {
            if due.iter().any(|(d, _)| *d == session_id) {
                continue;
            }
            let _ = self.inner.notices.send(ServerMsg::ShellClosingSoon {
                session_id,
                seconds_left,
                reason: reason.to_owned(),
            });
        }
        for (id, reason) in due {
            self.close(&id, reason, None).await;
        }
    }

    /// The newest audit entries, newest first.
    pub fn list_audit(&self, limit: u32) -> Vec<AuditEntry> {
        self.inner.audit.recent(limit)
    }
}

fn level_name(level: DangerLevel) -> &'static str {
    match level {
        DangerLevel::Safe => "safe",
        DangerLevel::Caution => "caution",
        DangerLevel::Dangerous => "dangerous",
    }
}

#[cfg(test)]
mod tests;
