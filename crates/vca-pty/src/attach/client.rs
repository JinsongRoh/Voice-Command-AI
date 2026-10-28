use std::io::{self, IsTerminal};
use std::path::Path;

use interprocess::local_socket::tokio::{Stream, prelude::*};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use super::protocol::{
    ATTACH_PROTOCOL_VERSION, AttachStart, ClientFrame, MAX_DATA_PER_FRAME, ServerFrame,
    read_server_frame, write_client_frame,
};
use super::{StreamHalf, endpoint_name, split_stream};
use crate::cpr::{CprReplyFilter, CprScanner};
use crate::session::PtySize;

/// Default detach prefix: Ctrl-] (0x1d), as in telnet.
pub const DETACH_PREFIX: u8 = 0x1d;
/// Key pressed after the prefix to detach.
pub const DETACH_KEY: u8 = b'd';

/// Sequences that put the local terminal back into a sane state after
/// detaching from an application that changed its modes.
const RESET_MODES: &[u8] =
    b"\x1b[?2004l\x1b[?1l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1049l\x1b[?25h\x1b[0m";

/// Options sent in the client's `Hello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachOptions {
    /// Size of the attaching terminal; the session is resized to it.
    pub size: Option<PtySize>,
    pub start: AttachStart,
}

impl Default for AttachOptions {
    fn default() -> Self {
        Self {
            size: None,
            start: AttachStart::Redraw,
        }
    }
}

/// The server's `Welcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Welcome {
    /// Session size after applying the client's size.
    pub size: PtySize,
    /// Sequence number of the first live output chunk.
    pub next_seq: u64,
}

/// A connected attach client. Split it to read and write concurrently.
pub struct AttachClient {
    reader: AttachReader,
    writer: AttachWriter,
    welcome: Welcome,
}

/// Receiving half of an [`AttachClient`].
pub struct AttachReader {
    recv: BufReader<StreamHalf>,
}

/// Sending half of an [`AttachClient`].
pub struct AttachWriter {
    send: StreamHalf,
}

impl AttachClient {
    /// Connects to an [`AttachServer`](super::AttachServer) and performs
    /// the handshake.
    pub async fn connect(endpoint: &Path, opts: AttachOptions) -> io::Result<Self> {
        let stream = Stream::connect(endpoint_name(endpoint)?).await?;
        let (recv, send) = split_stream(stream);
        let mut reader = AttachReader {
            recv: BufReader::new(recv),
        };
        let mut writer = AttachWriter { send };
        write_client_frame(
            &mut writer.send,
            &ClientFrame::Hello {
                version: ATTACH_PROTOCOL_VERSION,
                size: opts.size,
                start: opts.start,
            },
        )
        .await?;
        let welcome = match reader.recv().await? {
            Some(ServerFrame::Welcome { size, next_seq, .. }) => Welcome { size, next_seq },
            Some(ServerFrame::Error(msg)) => return Err(io::Error::other(msg)),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected welcome, got {other:?}"),
                ));
            }
        };
        Ok(Self {
            reader,
            writer,
            welcome,
        })
    }

    pub fn welcome(&self) -> Welcome {
        self.welcome
    }

    pub fn split(self) -> (AttachReader, AttachWriter) {
        (self.reader, self.writer)
    }
}

impl AttachReader {
    /// Next frame from the server; `None` when the server closed the stream.
    /// Not cancel safe: do not use directly in `select!` loops.
    pub async fn recv(&mut self) -> io::Result<Option<ServerFrame>> {
        read_server_frame(&mut self.recv).await
    }
}

impl AttachWriter {
    /// Sends input bytes to the session.
    pub async fn input(&mut self, data: &[u8]) -> io::Result<()> {
        for part in data.chunks(MAX_DATA_PER_FRAME) {
            write_client_frame(&mut self.send, &ClientFrame::Input(part.to_vec())).await?;
        }
        Ok(())
    }

    pub async fn resize(&mut self, size: PtySize) -> io::Result<()> {
        write_client_frame(&mut self.send, &ClientFrame::Resize(size)).await
    }

    /// Tells the server this client is leaving; the session keeps running.
    pub async fn detach(&mut self) -> io::Result<()> {
        write_client_frame(&mut self.send, &ClientFrame::Detach).await
    }
}

/// Detects the detach sequence (prefix, then key) in a stream of input
/// bytes. Prefix twice sends one literal prefix byte; prefix followed by any
/// other byte sends both.
#[derive(Debug, Clone)]
pub struct DetachDetector {
    prefix: u8,
    key: u8,
    armed: bool,
}

impl Default for DetachDetector {
    fn default() -> Self {
        Self::new(DETACH_PREFIX, DETACH_KEY)
    }
}

impl DetachDetector {
    pub fn new(prefix: u8, key: u8) -> Self {
        Self {
            prefix,
            key,
            armed: false,
        }
    }

    /// Returns the bytes to forward and whether the user asked to detach.
    /// Bytes after the detach sequence are dropped.
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.armed {
                self.armed = false;
                if b == self.key || b == self.key.to_ascii_uppercase() {
                    return (out, true);
                }
                out.push(self.prefix);
                if b != self.prefix {
                    out.push(b);
                }
            } else if b == self.prefix {
                self.armed = true;
            } else {
                out.push(b);
            }
        }
        (out, false)
    }
}

/// How an interactive attach ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachOutcome {
    /// The user pressed the detach sequence; the session keeps running.
    Detached,
    /// The session's process exited.
    Exited { code: Option<i32> },
    /// The server closed the connection without an exit status.
    Disconnected,
}

enum LocalEvent {
    Input(Vec<u8>),
    #[cfg_attr(not(windows), allow(dead_code))]
    Resize(PtySize),
    Eof,
}

struct RawModeGuard {
    enabled: bool,
}

impl RawModeGuard {
    fn enable(tty: bool) -> io::Result<Self> {
        if tty {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self { enabled: tty })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

fn terminal_size() -> Option<PtySize> {
    crossterm::terminal::size()
        .ok()
        .filter(|(c, r)| *c > 0 && *r > 0)
        .map(|(cols, rows)| PtySize { cols, rows })
}

/// Turns the calling terminal into an attached view of a session.
///
/// Puts the terminal in raw mode (when stdin is a terminal), relays stdin to
/// the session and its output to stdout, propagates resizes, and restores
/// the terminal on return. Press Ctrl-] then `d` to detach; Ctrl-] twice
/// sends a literal Ctrl-].
///
/// Stdin is read on a detached thread that may consume one more keystroke
/// after this function returns, so call it as the last interactive step of
/// a command. Windows support (console key events, resize events) is
/// unverified.
pub async fn attach_stdio(endpoint: &Path) -> io::Result<AttachOutcome> {
    let tty = io::stdin().is_terminal();
    #[cfg(windows)]
    let _ = crossterm::ansi_support::supports_ansi();
    let client = AttachClient::connect(
        endpoint,
        AttachOptions {
            size: terminal_size(),
            start: AttachStart::Redraw,
        },
    )
    .await?;
    let (mut reader, mut writer) = client.split();

    let (frame_tx, mut frames) = mpsc::channel::<io::Result<ServerFrame>>(64);
    let reader_task = tokio::spawn(async move {
        loop {
            let frame = reader.recv().await.transpose();
            let done = !matches!(frame, Some(Ok(_)));
            if let Some(frame) = frame {
                if frame_tx.send(frame).await.is_err() {
                    break;
                }
            }
            if done {
                break;
            }
        }
    });

    let guard = RawModeGuard::enable(tty)?;
    let (local_tx, mut local) = mpsc::channel::<LocalEvent>(64);
    let app_cursor = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    spawn_input_thread(local_tx, tty, std::sync::Arc::clone(&app_cursor))?;

    #[cfg(unix)]
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    let mut stdout = tokio::io::stdout();
    let mut detector = DetachDetector::default();
    // The session answers cursor position queries itself, so the local
    // terminal's answers to the same queries are dropped.
    let mut cpr_scanner = CprScanner::default();
    let mut cpr_filter = CprReplyFilter::default();
    let mut modes = crate::modes::ModeTracker::new();
    let mut input_open = true;

    let outcome = loop {
        #[cfg(unix)]
        let resized = winch.recv();
        #[cfg(not(unix))]
        let resized = std::future::pending::<Option<()>>();

        tokio::select! {
            frame = frames.recv() => match frame {
                Some(Ok(ServerFrame::Output { data, .. } | ServerFrame::Redraw(data))) => {
                    modes.feed(&data);
                    cpr_filter.expect(cpr_scanner.scan(&data).len());
                    app_cursor.store(modes.modes().application_cursor, std::sync::atomic::Ordering::Relaxed);
                    stdout.write_all(&data).await?;
                    stdout.flush().await?;
                }
                Some(Ok(ServerFrame::Exit { code })) => break AttachOutcome::Exited { code },
                Some(Ok(ServerFrame::Error(msg))) => {
                    drop(guard);
                    return Err(io::Error::other(msg));
                }
                Some(Ok(ServerFrame::Gap { .. } | ServerFrame::Welcome { .. })) => {}
                Some(Err(e)) => {
                    tracing::debug!("attach stream error: {e}");
                    break AttachOutcome::Disconnected;
                }
                None => break AttachOutcome::Disconnected,
            },
            event = local.recv(), if input_open => match event {
                Some(LocalEvent::Input(bytes)) => {
                    let (forward, detach) = detector.feed(&cpr_filter.filter(&bytes));
                    if !forward.is_empty() {
                        writer.input(&forward).await?;
                    }
                    if detach {
                        let _ = writer.detach().await;
                        break AttachOutcome::Detached;
                    }
                }
                Some(LocalEvent::Resize(size)) => writer.resize(size).await?,
                Some(LocalEvent::Eof) | None => {
                    input_open = false;
                    let (rest, _) = detector.feed(&cpr_filter.flush());
                    if !rest.is_empty() {
                        writer.input(&rest).await?;
                    }
                }
            },
            Some(()) = resized => {
                if let Some(size) = terminal_size() {
                    writer.resize(size).await?;
                }
            }
        }
    };
    reader_task.abort();
    if !matches!(outcome, AttachOutcome::Exited { .. }) {
        stdout.write_all(RESET_MODES).await?;
        stdout.flush().await?;
    }
    drop(guard);
    Ok(outcome)
}

/// Reads raw stdin bytes on a detached thread.
fn spawn_byte_reader(tx: mpsc::Sender<LocalEvent>) -> io::Result<()> {
    use std::io::Read;
    std::thread::Builder::new()
        .name("vca-attach-stdin".into())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.blocking_send(LocalEvent::Eof);
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .blocking_send(LocalEvent::Input(buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(not(windows))]
fn spawn_input_thread(
    tx: mpsc::Sender<LocalEvent>,
    _tty: bool,
    _app_cursor: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> io::Result<()> {
    spawn_byte_reader(tx)
}

/// On Windows the console delivers key events rather than VT bytes, so
/// keys are read with crossterm and re-encoded. Unverified on Windows.
#[cfg(windows)]
fn spawn_input_thread(
    tx: mpsc::Sender<LocalEvent>,
    tty: bool,
    app_cursor: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> io::Result<()> {
    use crossterm::event::{Event, read};
    use std::sync::atomic::Ordering;

    if !tty {
        return spawn_byte_reader(tx);
    }
    std::thread::Builder::new()
        .name("vca-attach-console".into())
        .spawn(move || {
            loop {
                let event = match read() {
                    Ok(Event::Key(key)) => {
                        match super::encode_key_event(key, app_cursor.load(Ordering::Relaxed)) {
                            Some(bytes) => LocalEvent::Input(bytes),
                            None => continue,
                        }
                    }
                    Ok(Event::Paste(text)) => LocalEvent::Input(text.into_bytes()),
                    Ok(Event::Resize(cols, rows)) => LocalEvent::Resize(PtySize { cols, rows }),
                    Ok(_) => continue,
                    Err(_) => LocalEvent::Eof,
                };
                let eof = matches!(event, LocalEvent::Eof);
                if tx.blocking_send(event).is_err() || eof {
                    break;
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_sequence() {
        let mut d = DetachDetector::default();
        assert_eq!(d.feed(b"ab\x1ddzz"), (b"ab".to_vec(), true));
    }

    #[test]
    fn detach_sequence_split_across_reads() {
        let mut d = DetachDetector::default();
        assert_eq!(d.feed(b"x\x1d"), (b"x".to_vec(), false));
        assert_eq!(d.feed(b"D"), (Vec::new(), true));
    }

    #[test]
    fn prefix_escapes() {
        let mut d = DetachDetector::default();
        assert_eq!(d.feed(b"\x1d\x1d"), (vec![0x1d], false));
        assert_eq!(d.feed(b"\x1dq"), (vec![0x1d, b'q'], false));
        assert_eq!(d.feed(b"plain"), (b"plain".to_vec(), false));
    }
}
