//! Wire format of the attach protocol.
//!
//! The protocol runs over a local stream socket (a Unix domain socket, or a
//! named pipe on Windows). Every message is one frame:
//!
//! ```text
//! +----------------------+---------+----------------------+
//! | length: u32 (BE)     | type: u8| payload: length-1 B  |
//! +----------------------+---------+----------------------+
//! ```
//!
//! `length` counts the type byte plus the payload and must be between 1 and
//! [`MAX_FRAME_LEN`]. All integers are big-endian.
//!
//! Client to server:
//!
//! | Type | Name   | Payload                                                  |
//! |------|--------|----------------------------------------------------------|
//! | 0x01 | Hello  | version u16, cols u16, rows u16 (0/0 = keep size), start u8 (0 = redraw, 1 = replay from seq, 2 = live), seq u64 |
//! | 0x02 | Input  | raw bytes for the PTY                                    |
//! | 0x03 | Resize | cols u16, rows u16                                       |
//! | 0x04 | Detach | empty                                                    |
//!
//! Server to client:
//!
//! | Type | Name    | Payload                                                 |
//! |------|---------|---------------------------------------------------------|
//! | 0x81 | Welcome | version u16, cols u16, rows u16, next_seq u64           |
//! | 0x82 | Output  | seq u64, raw bytes                                      |
//! | 0x83 | Gap     | requested u64, resumed_at u64                           |
//! | 0x84 | Redraw  | escape sequences that repaint the current screen        |
//! | 0x85 | Exit    | has_code u8, code i32                                   |
//! | 0x86 | Error   | UTF-8 message; the server closes the connection after it |
//!
//! A session starts with the client's `Hello` and the server's `Welcome`.
//! The server then sends either a `Redraw` (start = redraw, when screen
//! tracking is enabled), the replay from the requested sequence number
//! (possibly preceded by a `Gap`), or nothing (live), followed by live
//! `Output`. When the process exits, the server sends the remaining output,
//! then `Exit`, and closes the stream.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::session::PtySize;

/// Protocol version sent in `Hello` and `Welcome`.
pub const ATTACH_PROTOCOL_VERSION: u16 = 1;
/// Maximum value of the length prefix.
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// Where the server starts streaming output for a new client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachStart {
    /// Repaint the current screen, then stream live output. Falls back to
    /// replaying the ring buffer when the session has no screen tracking.
    Redraw,
    /// Replay output from this sequence number, then stream live output.
    ReplayFrom(u64),
    /// Only stream output produced after attaching.
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFrame {
    Hello {
        version: u16,
        size: Option<PtySize>,
        start: AttachStart,
    },
    Input(Vec<u8>),
    Resize(PtySize),
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerFrame {
    Welcome {
        version: u16,
        size: PtySize,
        next_seq: u64,
    },
    Output {
        seq: u64,
        data: Vec<u8>,
    },
    Gap {
        requested: u64,
        resumed_at: u64,
    },
    Redraw(Vec<u8>),
    Exit {
        code: Option<i32>,
    },
    Error(String),
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        if self.0.len() < N {
            return Err(invalid("truncated frame"));
        }
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        Ok(head.try_into().expect("length checked"))
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take()?))
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_be_bytes(self.take()?))
    }
    fn rest(self) -> Vec<u8> {
        self.0.to_vec()
    }
    fn end(self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("trailing bytes in frame"))
        }
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

fn size_bytes(out: &mut Vec<u8>, size: PtySize) {
    out.extend_from_slice(&size.cols.to_be_bytes());
    out.extend_from_slice(&size.rows.to_be_bytes());
}

impl ClientFrame {
    fn encode(&self) -> (u8, Vec<u8>) {
        let mut p = Vec::new();
        let ty = match self {
            Self::Hello {
                version,
                size,
                start,
            } => {
                p.extend_from_slice(&version.to_be_bytes());
                size_bytes(&mut p, size.unwrap_or(PtySize { cols: 0, rows: 0 }));
                let (kind, seq) = match start {
                    AttachStart::Redraw => (0u8, 0u64),
                    AttachStart::ReplayFrom(seq) => (1, *seq),
                    AttachStart::Live => (2, 0),
                };
                p.push(kind);
                p.extend_from_slice(&seq.to_be_bytes());
                0x01
            }
            Self::Input(data) => {
                p.extend_from_slice(data);
                0x02
            }
            Self::Resize(size) => {
                size_bytes(&mut p, *size);
                0x03
            }
            Self::Detach => 0x04,
        };
        (ty, p)
    }

    fn decode(ty: u8, payload: &[u8]) -> io::Result<Self> {
        let mut c = Cursor(payload);
        let frame = match ty {
            0x01 => {
                let version = c.u16()?;
                let cols = c.u16()?;
                let rows = c.u16()?;
                let kind = c.u8()?;
                let seq = c.u64()?;
                let start = match kind {
                    0 => AttachStart::Redraw,
                    1 => AttachStart::ReplayFrom(seq),
                    2 => AttachStart::Live,
                    _ => return Err(invalid("unknown attach start")),
                };
                let size = (cols > 0 && rows > 0).then_some(PtySize { cols, rows });
                Self::Hello {
                    version,
                    size,
                    start,
                }
            }
            0x02 => return Ok(Self::Input(c.rest())),
            0x03 => Self::Resize(PtySize {
                cols: c.u16()?,
                rows: c.u16()?,
            }),
            0x04 => Self::Detach,
            _ => return Err(invalid("unknown client frame type")),
        };
        c.end()?;
        Ok(frame)
    }
}

impl ServerFrame {
    fn encode(&self) -> (u8, Vec<u8>) {
        let mut p = Vec::new();
        let ty = match self {
            Self::Welcome {
                version,
                size,
                next_seq,
            } => {
                p.extend_from_slice(&version.to_be_bytes());
                size_bytes(&mut p, *size);
                p.extend_from_slice(&next_seq.to_be_bytes());
                0x81
            }
            Self::Output { seq, data } => {
                p.extend_from_slice(&seq.to_be_bytes());
                p.extend_from_slice(data);
                0x82
            }
            Self::Gap {
                requested,
                resumed_at,
            } => {
                p.extend_from_slice(&requested.to_be_bytes());
                p.extend_from_slice(&resumed_at.to_be_bytes());
                0x83
            }
            Self::Redraw(data) => {
                p.extend_from_slice(data);
                0x84
            }
            Self::Exit { code } => {
                p.push(u8::from(code.is_some()));
                p.extend_from_slice(&code.unwrap_or(0).to_be_bytes());
                0x85
            }
            Self::Error(msg) => {
                p.extend_from_slice(msg.as_bytes());
                0x86
            }
        };
        (ty, p)
    }

    fn decode(ty: u8, payload: &[u8]) -> io::Result<Self> {
        let mut c = Cursor(payload);
        let frame = match ty {
            0x81 => Self::Welcome {
                version: c.u16()?,
                size: PtySize {
                    cols: c.u16()?,
                    rows: c.u16()?,
                },
                next_seq: c.u64()?,
            },
            0x82 => {
                let seq = c.u64()?;
                return Ok(Self::Output {
                    seq,
                    data: c.rest(),
                });
            }
            0x83 => Self::Gap {
                requested: c.u64()?,
                resumed_at: c.u64()?,
            },
            0x84 => return Ok(Self::Redraw(c.rest())),
            0x85 => {
                let has = c.u8()? != 0;
                let code = c.i32()?;
                Self::Exit {
                    code: has.then_some(code),
                }
            }
            0x86 => return Ok(Self::Error(String::from_utf8_lossy(&c.rest()).into_owned())),
            _ => return Err(invalid("unknown server frame type")),
        };
        c.end()?;
        Ok(frame)
    }
}

async fn write_raw<W: AsyncWrite + Unpin>(w: &mut W, ty: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len() + 1)
        .ok()
        .filter(|l| *l <= MAX_FRAME_LEN)
        .ok_or_else(|| invalid("frame too large"))?;
    let mut buf = Vec::with_capacity(payload.len() + 5);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.push(ty);
    buf.extend_from_slice(payload);
    w.write_all(&buf).await?;
    w.flush().await
}

/// Reads one frame; `Ok(None)` on a clean EOF at a frame boundary.
async fn read_raw<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len);
    if len == 0 || len > MAX_FRAME_LEN {
        return Err(invalid("bad frame length"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    let ty = buf[0];
    buf.remove(0);
    Ok(Some((ty, buf)))
}

/// Largest data payload that fits in one `Output`/`Input` frame.
pub(crate) const MAX_DATA_PER_FRAME: usize = (MAX_FRAME_LEN as usize) - 16;

pub async fn write_client_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &ClientFrame,
) -> io::Result<()> {
    let (ty, p) = frame.encode();
    write_raw(w, ty, &p).await
}

pub async fn read_client_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<ClientFrame>> {
    match read_raw(r).await? {
        Some((ty, p)) => ClientFrame::decode(ty, &p).map(Some),
        None => Ok(None),
    }
}

pub async fn write_server_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &ServerFrame,
) -> io::Result<()> {
    let (ty, p) = frame.encode();
    write_raw(w, ty, &p).await
}

pub async fn read_server_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<ServerFrame>> {
    match read_raw(r).await? {
        Some((ty, p)) => ServerFrame::decode(ty, &p).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_frames_roundtrip() {
        let frames = vec![
            ClientFrame::Hello {
                version: 1,
                size: Some(PtySize {
                    cols: 120,
                    rows: 40,
                }),
                start: AttachStart::ReplayFrom(42),
            },
            ClientFrame::Hello {
                version: 1,
                size: None,
                start: AttachStart::Redraw,
            },
            ClientFrame::Hello {
                version: 1,
                size: None,
                start: AttachStart::Live,
            },
            ClientFrame::Input(b"ls -la\r".to_vec()),
            ClientFrame::Input(Vec::new()),
            ClientFrame::Resize(PtySize { cols: 80, rows: 24 }),
            ClientFrame::Detach,
        ];
        let mut buf = Vec::new();
        for f in &frames {
            write_client_frame(&mut buf, f).await.unwrap();
        }
        let mut r = buf.as_slice();
        for f in &frames {
            assert_eq!(read_client_frame(&mut r).await.unwrap().as_ref(), Some(f));
        }
        assert_eq!(read_client_frame(&mut r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn server_frames_roundtrip() {
        let frames = vec![
            ServerFrame::Welcome {
                version: 1,
                size: PtySize { cols: 1, rows: 2 },
                next_seq: u64::MAX,
            },
            ServerFrame::Output {
                seq: 7,
                data: b"\x1b[31mhi".to_vec(),
            },
            ServerFrame::Gap {
                requested: 1,
                resumed_at: 5,
            },
            ServerFrame::Redraw(b"\x1b[H".to_vec()),
            ServerFrame::Exit { code: Some(-3) },
            ServerFrame::Exit { code: None },
            ServerFrame::Error("nope".into()),
        ];
        let mut buf = Vec::new();
        for f in &frames {
            write_server_frame(&mut buf, f).await.unwrap();
        }
        let mut r = buf.as_slice();
        for f in &frames {
            assert_eq!(read_server_frame(&mut r).await.unwrap().as_ref(), Some(f));
        }
        assert_eq!(read_server_frame(&mut r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_bad_frames() {
        // Zero length.
        let mut r: &[u8] = &[0, 0, 0, 0];
        assert!(read_client_frame(&mut r).await.is_err());
        // Oversized length.
        let mut r: &[u8] = &[0xff, 0xff, 0xff, 0xff, 1];
        assert!(read_client_frame(&mut r).await.is_err());
        // Unknown type.
        let mut r: &[u8] = &[0, 0, 0, 1, 0x7f];
        assert!(read_client_frame(&mut r).await.is_err());
        // Truncated resize payload.
        let mut r: &[u8] = &[0, 0, 0, 2, 0x03, 0];
        assert!(read_client_frame(&mut r).await.is_err());
        // Trailing bytes.
        let mut r: &[u8] = &[0, 0, 0, 2, 0x04, 0];
        assert!(read_client_frame(&mut r).await.is_err());
        // Truncated stream inside a frame.
        let mut r: &[u8] = &[0, 0, 0, 9, 0x02, 1];
        assert!(read_client_frame(&mut r).await.is_err());
    }
}
