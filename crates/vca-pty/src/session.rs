//! A command running in a pseudo-terminal owned by this process.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, mpsc as std_mpsc};
use std::thread;
use std::time::Duration;

use bytes::Bytes;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, native_pty_system};
use tokio::sync::{broadcast, oneshot, watch};
use vca_core::Input;

use crate::error::PtyError;
use crate::input::{ENTER, encode_input};
use crate::modes::{ModeTracker, TerminalModes};
use crate::ring::{BufferLimits, Gap, OutputChunk, OutputRing};
use crate::screen::{Screen, ScreenOptions, ScreenSnapshot};

const READ_BUF_SIZE: usize = 16 * 1024;
const BROADCAST_CAPACITY: usize = 1024;

/// Terminal size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

impl From<PtySize> for portable_pty::PtySize {
    fn from(s: PtySize) -> Self {
        Self {
            rows: s.rows.max(1),
            cols: s.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// How a session's process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    /// Exit code, or `None` when the process was terminated by a signal.
    pub code: Option<i32>,
    /// Signal description when terminated by a signal (Unix only).
    pub signal: Option<String>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    fn from_portable(status: &portable_pty::ExitStatus) -> Self {
        match status.signal() {
            Some(sig) => Self {
                code: None,
                signal: Some(sig.to_owned()),
            },
            // Windows exit codes are u32 (e.g. 0xC000013A); keep the bit pattern.
            None => Self {
                code: Some(status.exit_code() as i32),
                signal: None,
            },
        }
    }
}

/// What to run and how. Built with [`SpawnOptions::new`] and the builder methods.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Working directory; inherits the host's when `None`.
    pub cwd: Option<PathBuf>,
    /// Variables added to (or overriding) the host environment.
    pub env: Vec<(OsString, OsString)>,
    /// Variables removed from the inherited environment.
    pub env_remove: Vec<OsString>,
    pub size: PtySize,
    pub buffer: BufferLimits,
    /// Track a vt100 screen for snapshots and attach redraws. `None`
    /// disables it (cheaper; attach then falls back to replay).
    pub screen: Option<ScreenOptions>,
    /// Pause between the text and Enter of a submitted [`Input::Text`].
    pub submit_delay: Duration,
}

impl SpawnOptions {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            size: PtySize::default(),
            buffer: BufferLimits::default(),
            screen: Some(ScreenOptions::default()),
            submit_delay: Duration::from_millis(30),
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    pub fn size(mut self, size: PtySize) -> Self {
        self.size = size;
        self
    }

    pub fn buffer(mut self, limits: BufferLimits) -> Self {
        self.buffer = limits;
        self
    }

    pub fn screen(mut self, screen: Option<ScreenOptions>) -> Self {
        self.screen = screen;
        self
    }

    pub fn submit_delay(mut self, delay: Duration) -> Self {
        self.submit_delay = delay;
        self
    }

    fn command(&self) -> CommandBuilder {
        let mut cmd = CommandBuilder::new(&self.program);
        cmd.args(&self.args);
        if let Some(cwd) = &self.cwd {
            cmd.cwd(cwd);
        }
        for key in &self.env_remove {
            cmd.env_remove(key);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        if cmd.get_env("TERM").is_none_or(|t| t.is_empty()) {
            cmd.env("TERM", "xterm-256color");
        }
        cmd
    }
}

/// Item yielded by an [`OutputSubscription`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputEvent {
    Data(OutputChunk),
    /// Some output was evicted before this subscriber could read it.
    Gap(Gap),
}

/// State shared with the reader thread; guarded by one mutex so that
/// subscribing, snapshotting and pushing output are atomic relative to
/// each other.
struct Shared {
    ring: OutputRing,
    screen: Option<Screen>,
    modes: ModeTracker,
    /// `None` once the output side reached EOF.
    tx: Option<broadcast::Sender<OutputChunk>>,
    size: PtySize,
}

struct WriteCmd {
    data: Vec<u8>,
    ack: oneshot::Sender<std::io::Result<()>>,
}

struct Inner {
    shared: Arc<Mutex<Shared>>,
    writer: std_mpsc::Sender<WriteCmd>,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    exit: watch::Receiver<Option<ExitStatus>>,
    pid: Option<u32>,
    submit_delay: Duration,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.exit.borrow().is_none() {
            let _ = lock(&self.killer).kill();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock leaves plain data behind; keep going.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A command running in a pseudo-terminal (ConPTY on Windows, openpty on
/// Unix). Cheap to clone; all clones refer to the same session. The child
/// is killed when the last clone is dropped while it is still running.
///
/// Blocking PTY I/O runs on dedicated threads (reader, writer, waiter), so
/// none of the async methods block the runtime. Input is delivered straight
/// into the PTY and needs no window focus.
#[derive(Clone)]
pub struct PtySession {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("pid", &self.inner.pid)
            .field("exit", &*self.inner.exit.borrow())
            .finish_non_exhaustive()
    }
}

impl PtySession {
    /// Opens a PTY and spawns the command in it. Does not need a Tokio
    /// runtime; the async methods do.
    pub fn spawn(opts: SpawnOptions) -> Result<Self, PtyError> {
        let pair = native_pty_system()
            .openpty(opts.size.into())
            .map_err(|e| PtyError::Spawn(e.to_string()))?;
        let mut child = pair
            .slave
            .spawn_command(opts.command())
            .map_err(|e| PtyError::Spawn(format!("{:?}: {e}", opts.program)))?;
        // Close our copy of the slave so that EOF is seen when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Spawn(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Spawn(e.to_string()))?;
        let killer = child.clone_killer();
        let pid = child.process_id();

        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let shared = Arc::new(Mutex::new(Shared {
            ring: OutputRing::new(opts.buffer),
            screen: opts.screen.map(|s| Screen::new(opts.size, s)),
            modes: ModeTracker::new(),
            tx: Some(tx),
            size: opts.size,
        }));
        let master = Arc::new(Mutex::new(Some(pair.master)));

        let reader_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("vca-pty-reader".into())
            .spawn(move || reader_loop(reader, reader_shared))?;

        let (write_tx, write_rx) = std_mpsc::channel::<WriteCmd>();
        thread::Builder::new()
            .name("vca-pty-writer".into())
            .spawn(move || writer_loop(writer, write_rx))?;

        let (exit_tx, exit_rx) = watch::channel(None);
        let waiter_master = Arc::clone(&master);
        thread::Builder::new()
            .name("vca-pty-waiter".into())
            .spawn(move || {
                let status = match child.wait() {
                    Ok(s) => ExitStatus::from_portable(&s),
                    Err(e) => {
                        tracing::warn!("waiting for pty child failed: {e}");
                        ExitStatus {
                            code: None,
                            signal: None,
                        }
                    }
                };
                // ConPTY keeps the output pipe open until the pseudo console
                // is closed, so the reader would never see EOF. Unverified on
                // Windows in CI here.
                if cfg!(windows) {
                    lock(&waiter_master).take();
                }
                let _ = exit_tx.send(Some(status));
            })?;

        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                writer: write_tx,
                master,
                killer: Mutex::new(killer),
                exit: exit_rx,
                pid,
                submit_delay: opts.submit_delay,
            }),
        })
    }

    /// OS process id of the child, when available.
    pub fn pid(&self) -> Option<u32> {
        self.inner.pid
    }

    /// Writes raw bytes to the PTY input, in order with other writes.
    pub async fn write(&self, data: impl Into<Vec<u8>>) -> Result<(), PtyError> {
        let (ack, done) = oneshot::channel();
        self.inner
            .writer
            .send(WriteCmd {
                data: data.into(),
                ack,
            })
            .map_err(|_| PtyError::Closed)?;
        done.await.map_err(|_| PtyError::Closed)??;
        Ok(())
    }

    /// Delivers structured input, encoded for the session's current modes
    /// (bracketed paste, application cursor keys).
    pub async fn send_input(&self, input: &Input) -> Result<(), PtyError> {
        let encoded = encode_input(input, self.modes())?;
        if !encoded.body.is_empty() {
            self.write(encoded.body.clone()).await?;
        }
        if encoded.submit {
            if !encoded.body.is_empty() && !self.inner.submit_delay.is_zero() {
                tokio::time::sleep(self.inner.submit_delay).await;
            }
            self.write(ENTER).await?;
        }
        Ok(())
    }

    /// Input modes most recently switched by the application.
    pub fn modes(&self) -> TerminalModes {
        lock(&self.inner.shared).modes.modes()
    }

    pub fn size(&self) -> PtySize {
        lock(&self.inner.shared).size
    }

    /// Resizes the PTY (the child receives SIGWINCH on Unix).
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        {
            let master = lock(&self.inner.master);
            let Some(master) = master.as_ref() else {
                return Err(PtyError::Closed);
            };
            master
                .resize(size.into())
                .map_err(|e| PtyError::Resize(e.to_string()))?;
        }
        let mut shared = lock(&self.inner.shared);
        shared.size = size;
        if let Some(screen) = shared.screen.as_mut() {
            screen.resize(size);
        }
        Ok(())
    }

    /// Kills the child process. Use [`wait`](Self::wait) to observe the exit.
    pub fn kill(&self) -> Result<(), PtyError> {
        if self.try_exit_status().is_some() {
            return Ok(());
        }
        lock(&self.inner.killer).kill()?;
        Ok(())
    }

    /// Exit status if the child has exited.
    pub fn try_exit_status(&self) -> Option<ExitStatus> {
        self.inner.exit.borrow().clone()
    }

    /// Waits until the child exits.
    pub async fn wait(&self) -> Result<ExitStatus, PtyError> {
        let mut rx = self.inner.exit.clone();
        let status = rx
            .wait_for(Option::is_some)
            .await
            .map_err(|_| PtyError::Closed)?;
        Ok(status.clone().unwrap_or(ExitStatus {
            code: None,
            signal: None,
        }))
    }

    /// True once the output stream reached EOF.
    pub fn output_closed(&self) -> bool {
        lock(&self.inner.shared).tx.is_none()
    }

    /// Sequence number the next output chunk will get.
    pub fn next_seq(&self) -> u64 {
        lock(&self.inner.shared).ring.next_seq()
    }

    /// Subscribes to output produced from now on.
    pub fn subscribe(&self) -> OutputSubscription {
        let shared = lock(&self.inner.shared);
        let from = shared.ring.next_seq();
        self.subscription(&shared, from)
    }

    /// Subscribes starting at `from`: retained chunks are replayed first. If
    /// `from` was already evicted, the first event is [`OutputEvent::Gap`].
    pub fn subscribe_from(&self, from: u64) -> OutputSubscription {
        let shared = lock(&self.inner.shared);
        self.subscription(&shared, from)
    }

    /// Atomically captures the screen redraw sequence and subscribes to all
    /// output after it. Returns `None` for the redraw when screen tracking is
    /// disabled.
    pub fn subscribe_with_redraw(&self) -> (Option<Vec<u8>>, OutputSubscription) {
        let shared = lock(&self.inner.shared);
        let redraw = shared.screen.as_ref().map(Screen::redraw);
        let from = shared.ring.next_seq();
        (redraw, self.subscription(&shared, from))
    }

    /// Plain-text snapshot of the screen, if screen tracking is enabled.
    pub fn screen_snapshot(&self) -> Option<ScreenSnapshot> {
        lock(&self.inner.shared)
            .screen
            .as_ref()
            .map(Screen::snapshot)
    }

    fn subscription(&self, shared: &Shared, from: u64) -> OutputSubscription {
        let mut sub = OutputSubscription {
            rx: shared.tx.as_ref().map(broadcast::Sender::subscribe),
            shared: Arc::clone(&self.inner.shared),
            next_seq: from,
            pending: VecDeque::new(),
        };
        sub.refill(&shared.ring);
        sub
    }
}

fn reader_loop(mut reader: Box<dyn Read + Send>, shared: Arc<Mutex<Shared>>) {
    let mut buf = vec![0u8; READ_BUF_SIZE];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let data = Bytes::copy_from_slice(&buf[..n]);
                let mut s = lock(&shared);
                s.modes.feed(&data);
                if let Some(screen) = s.screen.as_mut() {
                    screen.process(&data);
                }
                let chunk = s.ring.push(data);
                if let Some(tx) = &s.tx {
                    // No receivers is fine: output stays in the ring.
                    let _ = tx.send(chunk);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // EIO is how Linux reports that the slave side was closed.
            Err(_) => break,
        }
    }
    lock(&shared).tx = None;
}

fn writer_loop(mut writer: Box<dyn Write + Send>, rx: std_mpsc::Receiver<WriteCmd>) {
    while let Ok(cmd) = rx.recv() {
        let result = writer.write_all(&cmd.data).and_then(|()| writer.flush());
        let _ = cmd.ack.send(result);
    }
}

/// Stream of output for one subscriber. Slow subscribers do not block the
/// session: if the live channel overflows, missing chunks are recovered from
/// the ring buffer, and a [`OutputEvent::Gap`] is reported only when they
/// were evicted from it as well.
pub struct OutputSubscription {
    rx: Option<broadcast::Receiver<OutputChunk>>,
    shared: Arc<Mutex<Shared>>,
    next_seq: u64,
    pending: VecDeque<OutputEvent>,
}

impl OutputSubscription {
    /// Sequence number of the next chunk this subscriber expects.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    fn refill(&mut self, ring: &OutputRing) {
        let replay = ring.replay_from(self.next_seq);
        if let Some(gap) = replay.gap {
            self.pending.push_back(OutputEvent::Gap(gap));
            self.next_seq = gap.resumed_at;
        }
        for chunk in replay.chunks {
            self.next_seq = chunk.seq + 1;
            self.pending.push_back(OutputEvent::Data(chunk));
        }
    }

    fn refill_locked(&mut self) {
        let shared = Arc::clone(&self.shared);
        let s = lock(&shared);
        self.refill(&s.ring);
    }

    /// Next output event, or `None` once the session's output reached EOF
    /// and everything was delivered.
    pub async fn recv(&mut self) -> Option<OutputEvent> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Some(ev);
            }
            let Some(rx) = self.rx.as_mut() else {
                self.refill_locked();
                return self.pending.pop_front();
            };
            match rx.recv().await {
                Ok(chunk) if chunk.seq < self.next_seq => continue,
                Ok(chunk) if chunk.seq == self.next_seq => {
                    self.next_seq += 1;
                    return Some(OutputEvent::Data(chunk));
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => self.refill_locked(),
                Err(broadcast::error::RecvError::Closed) => self.rx = None,
            }
        }
    }
}
