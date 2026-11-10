//! Attaching terminals to a [`PtySession`](crate::PtySession) over local IPC.
//!
//! An [`AttachServer`] listens on a local endpoint (a Unix domain socket
//! path, or a named pipe such as `\\.\pipe\vca-attach-<id>` on Windows) and
//! lets any number of clients attach to one session at the same time, like
//! `tmux attach`. [`attach_stdio`] turns the calling terminal into such a
//! view; [`AttachClient`] is the programmatic client. The wire format is
//! documented in [`protocol`].

mod client;
mod keys;
pub mod protocol;
mod server;

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::{GenericFilePath, Name, ToFsName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub use client::{
    AttachClient, AttachOptions, AttachOutcome, AttachReader, AttachWriter, DetachDetector,
    Welcome, attach_stdio,
};
pub use keys::encode_key_event;
pub use protocol::{AttachStart, ClientFrame, ServerFrame};
pub use server::AttachServer;

/// Default endpoint for a session's attach server.
///
/// * Unix: `$XDG_RUNTIME_DIR/vca/attach-<id>.sock`, or
///   `/tmp/vca-<user>/attach-<id>.sock` when `XDG_RUNTIME_DIR` is unset
///   (kept short because socket paths are limited to about 104 bytes).
/// * Windows: `\\.\pipe\vca-attach-<user>-<id>`.
///
/// `id` should be a short, filesystem-safe identifier such as a session id.
pub fn default_endpoint(id: &str) -> PathBuf {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into());
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\vca-attach-{user}-{id}"))
    } else {
        let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) if !d.is_empty() => PathBuf::from(d).join("vca"),
            _ => PathBuf::from(format!("/tmp/vca-{user}")),
        };
        dir.join(format!("attach-{id}.sock"))
    }
}

pub(crate) fn endpoint_name(endpoint: &Path) -> io::Result<Name<'_>> {
    endpoint.to_fs_name::<GenericFilePath>()
}

/// One direction of a local socket stream shared through an `Arc`
/// (`&Stream` implements the Tokio I/O traits). The connection closes when
/// both halves are dropped.
pub(crate) struct StreamHalf(Arc<Stream>);

pub(crate) fn split_stream(stream: Stream) -> (StreamHalf, StreamHalf) {
    let stream = Arc::new(stream);
    (StreamHalf(Arc::clone(&stream)), StreamHalf(stream))
}

impl AsyncRead for StreamHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for StreamHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &*self.0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_shutdown(cx)
    }
}
