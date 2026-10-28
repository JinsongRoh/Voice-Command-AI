use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::{Stream, prelude::*};
use tokio::io::BufReader;
use tokio::task::{JoinHandle, JoinSet};

use super::protocol::{
    ATTACH_PROTOCOL_VERSION, AttachStart, ClientFrame, MAX_DATA_PER_FRAME, ServerFrame,
    read_client_frame, write_server_frame,
};
use super::{StreamHalf, endpoint_name, split_stream};
use crate::session::{OutputEvent, OutputSubscription, PtySession};

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to keep forwarding output after the process exited, in case a
/// grandchild keeps the PTY open.
const EXIT_GRACE: Duration = Duration::from_millis(500);

/// Local IPC server that lets terminals attach to one [`PtySession`].
///
/// Any number of clients may attach at once. All of them see the same
/// output and may all type; the most recent resize wins. Dropping the server
/// (or calling [`shutdown`](Self::shutdown)) stops accepting, disconnects
/// all clients and removes the socket file; it does not stop the session.
///
/// Access control relies on the OS: on Unix a missing parent directory is
/// created with mode 0700 and the socket gets mode 0600. On Windows the named pipe
/// gets a DACL that grants access only to the owner and SYSTEM
/// (unverified on Windows).
pub struct AttachServer {
    endpoint: PathBuf,
    task: JoinHandle<()>,
}

impl AttachServer {
    /// Binds `endpoint` and starts serving `session`. Must be called within
    /// a Tokio runtime.
    pub fn bind(endpoint: impl Into<PathBuf>, session: PtySession) -> io::Result<Self> {
        let endpoint = endpoint.into();
        let listener = listener_options(&endpoint)?.create_tokio()?;
        restrict_socket_file(&endpoint)?;
        let task = tokio::spawn(async move {
            // Dropping the JoinSet (when this task is aborted) aborts clients.
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    conn = listener.accept() => match conn {
                        Ok(stream) => {
                            clients.spawn(serve_client(stream, session.clone()));
                        }
                        Err(e) => {
                            tracing::warn!("attach accept failed: {e}");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    },
                    Some(_) = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        });
        Ok(Self { endpoint, task })
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Stops the server and disconnects all attached clients.
    pub fn shutdown(self) {}
}

impl Drop for AttachServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(unix)]
fn listener_options(endpoint: &Path) -> io::Result<ListenerOptions<'_>> {
    #[cfg(target_os = "linux")]
    use interprocess::os::unix::local_socket::ListenerOptionsExt;
    use std::os::unix::fs::DirBuilderExt;

    if let Some(dir) = endpoint.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
    }
    // A stale socket from a crashed server would make bind fail.
    if std::fs::symlink_metadata(endpoint).is_ok()
        && std::os::unix::net::UnixStream::connect(endpoint).is_err()
    {
        let _ = std::fs::remove_file(endpoint);
    }
    let options = ListenerOptions::new().name(endpoint_name(endpoint)?);
    // Setting the mode before bind is only supported on Linux; other Unix
    // systems get it from `restrict_socket_file` right after binding.
    #[cfg(target_os = "linux")]
    let options = options.mode(0o600);
    Ok(options)
}

/// Makes the socket file owner-only on Unix systems where the mode cannot be
/// set before binding. The parent directory is 0700, so the short window
/// between bind and chmod is not reachable by other users.
#[cfg(all(unix, not(target_os = "linux")))]
fn restrict_socket_file(endpoint: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(all(unix, not(target_os = "linux"))))]
fn restrict_socket_file(_endpoint: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn listener_options(endpoint: &Path) -> io::Result<ListenerOptions<'_>> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;

    // Protected DACL: full access for the owner and SYSTEM only.
    let sddl = U16CString::from_str("D:P(A;;GA;;;OW)(A;;GA;;;SY)")
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let sd = SecurityDescriptor::deserialize(&sddl)?;
    Ok(ListenerOptions::new()
        .name(endpoint_name(endpoint)?)
        .security_descriptor(sd))
}

#[cfg(not(any(unix, windows)))]
fn listener_options(endpoint: &Path) -> io::Result<ListenerOptions<'_>> {
    Ok(ListenerOptions::new().name(endpoint_name(endpoint)?))
}

async fn serve_client(stream: Stream, session: PtySession) {
    let (recv, mut send) = split_stream(stream);
    let recv = BufReader::new(recv);
    if let Err(e) = run_client(recv, &mut send, session).await {
        tracing::debug!("attach client ended: {e}");
    }
}

async fn send_output(send: &mut StreamHalf, seq: u64, data: &[u8]) -> io::Result<()> {
    for part in data.chunks(MAX_DATA_PER_FRAME) {
        write_server_frame(
            send,
            &ServerFrame::Output {
                seq,
                data: part.to_vec(),
            },
        )
        .await?;
    }
    Ok(())
}

async fn reject(send: &mut StreamHalf, msg: String) -> io::Result<()> {
    write_server_frame(send, &ServerFrame::Error(msg.clone())).await?;
    Err(io::Error::new(io::ErrorKind::InvalidData, msg))
}

/// Applies client frames to the session until the client detaches or
/// disconnects. Runs in its own task because frame reads are not cancel safe.
async fn pump_input(mut recv: BufReader<StreamHalf>, session: PtySession) {
    loop {
        match read_client_frame(&mut recv).await {
            Ok(Some(ClientFrame::Input(data))) => {
                if let Err(e) = session.write(data).await {
                    tracing::debug!("attach input write failed: {e}");
                    return;
                }
            }
            Ok(Some(ClientFrame::Resize(size))) => {
                if let Err(e) = session.resize(size) {
                    tracing::debug!("attach resize failed: {e}");
                }
            }
            Ok(Some(ClientFrame::Hello { .. })) => {
                tracing::debug!("attach client sent a second hello; ignoring");
            }
            Ok(Some(ClientFrame::Detach) | None) | Err(_) => return,
        }
    }
}

async fn run_client(
    mut recv: BufReader<StreamHalf>,
    send: &mut StreamHalf,
    session: PtySession,
) -> io::Result<()> {
    let hello = tokio::time::timeout(HELLO_TIMEOUT, read_client_frame(&mut recv))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no hello"))??;
    let (size, start) = match hello {
        Some(ClientFrame::Hello {
            version: ATTACH_PROTOCOL_VERSION,
            size,
            start,
        }) => (size, start),
        Some(ClientFrame::Hello { version, .. }) => {
            return reject(
                send,
                format!("unsupported attach protocol version {version}"),
            )
            .await;
        }
        _ => return reject(send, "expected hello".into()).await,
    };
    if let Some(size) = size {
        if let Err(e) = session.resize(size) {
            tracing::debug!("attach resize failed: {e}");
        }
    }

    let (redraw, mut sub): (Option<Vec<u8>>, OutputSubscription) = match start {
        AttachStart::Redraw => match session.subscribe_with_redraw() {
            (Some(redraw), sub) => (Some(redraw), sub),
            (None, _) => (None, session.subscribe_from(0)),
        },
        AttachStart::ReplayFrom(seq) => (None, session.subscribe_from(seq)),
        AttachStart::Live => (None, session.subscribe()),
    };
    write_server_frame(
        send,
        &ServerFrame::Welcome {
            version: ATTACH_PROTOCOL_VERSION,
            size: session.size(),
            next_seq: sub.next_seq(),
        },
    )
    .await?;
    if let Some(redraw) = redraw {
        write_server_frame(send, &ServerFrame::Redraw(redraw)).await?;
    }

    let input = tokio::spawn(pump_input(recv, session.clone()));
    stream_output(send, &mut sub, &session, input).await
}

/// Forwards output until the process exits (then sends `Exit`) or the
/// client goes away.
async fn stream_output(
    send: &mut StreamHalf,
    sub: &mut OutputSubscription,
    session: &PtySession,
    mut input: JoinHandle<()>,
) -> io::Result<()> {
    let mut exit_deadline: Option<tokio::time::Instant> = None;
    let result = loop {
        tokio::select! {
            event = sub.recv() => match event {
                Some(OutputEvent::Data(chunk)) => {
                    if let Err(e) = send_output(send, chunk.seq, &chunk.data).await {
                        break Err(e);
                    }
                }
                Some(OutputEvent::Gap(gap)) => {
                    let frame = ServerFrame::Gap {
                        requested: gap.requested,
                        resumed_at: gap.resumed_at,
                    };
                    if let Err(e) = write_server_frame(send, &frame).await {
                        break Err(e);
                    }
                }
                None => {
                    // Output closed: the process is gone or about to be.
                    let status = tokio::time::timeout(EXIT_GRACE * 4, session.wait()).await;
                    let code = status.ok().and_then(Result::ok).and_then(|s| s.code);
                    break write_server_frame(send, &ServerFrame::Exit { code }).await;
                }
            },
            status = session.wait(), if exit_deadline.is_none() => {
                if status.is_err() {
                    break Ok(());
                }
                exit_deadline = Some(tokio::time::Instant::now() + EXIT_GRACE);
            }
            () = sleep_until_opt(exit_deadline) => {
                let code = session.try_exit_status().and_then(|s| s.code);
                break write_server_frame(send, &ServerFrame::Exit { code }).await;
            }
            _ = &mut input => break Ok(()),
        }
    };
    input.abort();
    result
}

async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}
