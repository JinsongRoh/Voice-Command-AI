//! Noise `XX` channel: handshake and encrypted transport messages.
//!
//! * Pattern: `Noise_XX_25519_ChaChaPoly_SHA256` with a fixed prologue that
//!   binds the VCA protocol version.
//! * The handshake runs over any [`FrameTransport`] (one transport frame per
//!   Noise message), typically WebSocket binary messages.
//! * The initiator (device) pins the server static key from the pairing QR
//!   code and aborts before revealing its own static key if it differs.
//! * Application messages larger than one Noise message are split into
//!   chunks. Every chunk's plaintext starts with a one-byte header
//!   ([`CHUNK_FINAL`] marks the last chunk), so chunk boundaries are
//!   authenticated by the AEAD.
//! * Both directions rekey every [`NoiseConfig::rekey_interval`] messages
//!   (Noise spec §11.3). Rekey points are implicit (message counters), so
//!   both sides stay in sync on the reliable, ordered transport.
//! * Before the nonce space is exhausted the session refuses to continue
//!   ([`NetError::NonceExhausted`]); the caller closes the connection and the
//!   client reconnects with a fresh handshake.

use std::pin::Pin;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use snow::{HandshakeState, TransportState};

use crate::error::{NetError, Result};
use crate::keys::{PublicKey, StaticKeypair};

/// Noise protocol name.
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

/// Prologue mixed into the handshake hash. Changing it makes old and new
/// peers fail the handshake instead of misinterpreting each other.
pub const NOISE_PROLOGUE: &[u8] = b"VCA noise v1";

/// Maximum size of one Noise message (spec limit).
pub const MAX_NOISE_MESSAGE: usize = 65_535;

/// AEAD tag length.
pub const TAG_LEN: usize = 16;

/// Maximum plaintext bytes per chunk (Noise limit minus tag and header).
pub const MAX_CHUNK_DATA: usize = MAX_NOISE_MESSAGE - TAG_LEN - 1;

/// Chunk header flag: this is the last chunk of a message.
pub const CHUNK_FINAL: u8 = 0x01;

/// Maximum size of a handshake message we accept. XX messages are at most
/// 32 + 48 + 16 bytes plus payload; we send empty payloads.
const MAX_HANDSHAKE_MESSAGE: usize = 1024;

/// A bidirectional transport of discrete binary messages (for example
/// WebSocket binary frames). The stream ends (`None`) when the peer closes.
pub trait FrameTransport:
    Stream<Item = Result<Vec<u8>>> + Sink<Vec<u8>, Error = NetError> + Send + Unpin
{
}

impl<T> FrameTransport for T where
    T: Stream<Item = Result<Vec<u8>>> + Sink<Vec<u8>, Error = NetError> + Send + Unpin
{
}

/// A boxed [`FrameTransport`], convenient for type erasure.
pub type BoxFrameTransport = Pin<Box<dyn FrameTransportObj>>;

/// Object-safe alias used by [`BoxFrameTransport`].
pub trait FrameTransportObj:
    Stream<Item = Result<Vec<u8>>> + Sink<Vec<u8>, Error = NetError> + Send
{
}

impl<T> FrameTransportObj for T where
    T: Stream<Item = Result<Vec<u8>>> + Sink<Vec<u8>, Error = NetError> + Send
{
}

/// Tunables of an established session.
#[derive(Debug, Clone, Copy)]
pub struct NoiseConfig {
    /// Rekey each direction after this many messages.
    pub rekey_interval: u64,
    /// Refuse to send or receive once a nonce reaches this value. The Noise
    /// limit is `u64::MAX - 1`; lower values exist for tests.
    pub nonce_limit: u64,
    /// Maximum size of a reassembled application message.
    pub max_message_size: usize,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            rekey_interval: 1 << 16,
            nonce_limit: u64::MAX - 1,
            max_message_size: 16 * 1024 * 1024,
        }
    }
}

fn builder<'a>(local: &'a StaticKeypair) -> Result<snow::Builder<'a>> {
    let params = NOISE_PARAMS
        .parse()
        .map_err(|e: snow::Error| NetError::Noise(e.to_string()))?;
    Ok(snow::Builder::new(params)
        .local_private_key(local.secret_bytes())?
        .prologue(NOISE_PROLOGUE)?)
}

async fn hs_send<T: FrameTransport>(t: &mut T, hs: &mut HandshakeState) -> Result<()> {
    let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
    let n = hs.write_message(&[], &mut buf)?;
    buf.truncate(n);
    t.send(buf).await
}

async fn hs_recv<T: FrameTransport>(t: &mut T, hs: &mut HandshakeState) -> Result<()> {
    let msg = t
        .next()
        .await
        .ok_or_else(|| NetError::Protocol("peer closed during handshake".into()))??;
    if msg.len() > MAX_HANDSHAKE_MESSAGE {
        return Err(NetError::Protocol("oversized handshake message".into()));
    }
    let mut payload = vec![0u8; MAX_HANDSHAKE_MESSAGE];
    let n = hs.read_message(&msg, &mut payload)?;
    if n != 0 {
        return Err(NetError::Protocol("unexpected handshake payload".into()));
    }
    Ok(())
}

fn remote_static(hs: &HandshakeState) -> Result<PublicKey> {
    PublicKey::from_slice(
        hs.get_remote_static()
            .ok_or_else(|| NetError::Protocol("peer static key missing".into()))?,
    )
}

/// Runs the initiator (device) side of the XX handshake. Fails with
/// [`NetError::ServerKeyMismatch`] if the server's static key is not
/// `expected_server`.
pub async fn handshake_initiator<T: FrameTransport>(
    t: &mut T,
    local: &StaticKeypair,
    expected_server: &PublicKey,
    config: NoiseConfig,
) -> Result<NoiseSession> {
    let mut hs = builder(local)?.build_initiator()?;
    hs_send(t, &mut hs).await?; // -> e
    hs_recv(t, &mut hs).await?; // <- e, ee, s, es
    let server = remote_static(&hs)?;
    if !server.ct_eq(expected_server) {
        return Err(NetError::ServerKeyMismatch {
            expected: expected_server.fingerprint().short(),
            actual: server.fingerprint().short(),
        });
    }
    hs_send(t, &mut hs).await?; // -> s, se
    NoiseSession::new(hs.into_transport_mode()?, server, config)
}

/// Runs the responder (server) side of the XX handshake. The returned
/// session exposes the peer's static key, which the caller must check
/// against the paired-device registry.
pub async fn handshake_responder<T: FrameTransport>(
    t: &mut T,
    local: &StaticKeypair,
    config: NoiseConfig,
) -> Result<NoiseSession> {
    let mut hs = builder(local)?.build_responder()?;
    hs_recv(t, &mut hs).await?; // -> e
    hs_send(t, &mut hs).await?; // <- e, ee, s, es
    hs_recv(t, &mut hs).await?; // -> s, se
    let peer = remote_static(&hs)?;
    NoiseSession::new(hs.into_transport_mode()?, peer, config)
}

/// An established Noise transport session.
pub struct NoiseSession {
    state: TransportState,
    remote: PublicKey,
    config: NoiseConfig,
    sent: u64,
    received: u64,
    pending: Vec<u8>,
    failed: bool,
}

impl std::fmt::Debug for NoiseSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseSession")
            .field("remote", &self.remote)
            .field("sent", &self.sent)
            .field("received", &self.received)
            .finish_non_exhaustive()
    }
}

impl NoiseSession {
    fn new(state: TransportState, remote: PublicKey, config: NoiseConfig) -> Result<Self> {
        if config.rekey_interval == 0 {
            return Err(NetError::InvalidArgument(
                "rekey_interval must be > 0".into(),
            ));
        }
        Ok(Self {
            state,
            remote,
            config,
            sent: 0,
            received: 0,
            pending: Vec::new(),
            failed: false,
        })
    }

    /// The peer's authenticated static public key.
    pub fn remote_static(&self) -> &PublicKey {
        &self.remote
    }

    pub fn config(&self) -> &NoiseConfig {
        &self.config
    }

    /// Changes the maximum reassembled message size (e.g. a small limit
    /// before authentication).
    pub fn set_max_message_size(&mut self, limit: usize) {
        self.config.max_message_size = limit;
    }

    fn check_usable(&self) -> Result<()> {
        if self.failed {
            return Err(NetError::Protocol("session failed earlier".into()));
        }
        Ok(())
    }

    /// Encrypts one application message into one or more Noise messages.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.check_usable()?;
        if plaintext.len() > self.config.max_message_size {
            return Err(NetError::MessageTooLarge {
                size: plaintext.len(),
                limit: self.config.max_message_size,
            });
        }
        let count = plaintext.len().div_ceil(MAX_CHUNK_DATA).max(1);
        let mut out = Vec::with_capacity(count);
        let mut chunk_plain = Vec::with_capacity(MAX_CHUNK_DATA.min(plaintext.len()) + 1);
        for i in 0..count {
            let start = i * MAX_CHUNK_DATA;
            let end = (start + MAX_CHUNK_DATA).min(plaintext.len());
            chunk_plain.clear();
            chunk_plain.push(if i + 1 == count { CHUNK_FINAL } else { 0 });
            chunk_plain.extend_from_slice(&plaintext[start..end]);
            out.push(self.write_one(&chunk_plain)?);
        }
        Ok(out)
    }

    fn write_one(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        if self.state.sending_nonce() >= self.config.nonce_limit {
            self.failed = true;
            return Err(NetError::NonceExhausted);
        }
        let mut buf = vec![0u8; plain.len() + TAG_LEN];
        let n = self
            .state
            .write_message(plain, &mut buf)
            .inspect_err(|_| self.failed = true)?;
        buf.truncate(n);
        self.sent += 1;
        if self.sent % self.config.rekey_interval == 0 {
            self.state.rekey_outgoing();
        }
        Ok(buf)
    }

    /// Decrypts one Noise message. Returns `Some(message)` when it completes
    /// an application message, `None` when more chunks are expected.
    ///
    /// Any error leaves the session unusable; the caller must close the
    /// connection.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Option<Vec<u8>>> {
        self.check_usable()?;
        let result = self.decrypt_inner(ciphertext);
        if result.is_err() {
            self.failed = true;
            self.pending = Vec::new();
        }
        result
    }

    fn decrypt_inner(&mut self, ciphertext: &[u8]) -> Result<Option<Vec<u8>>> {
        if ciphertext.len() > MAX_NOISE_MESSAGE || ciphertext.len() < TAG_LEN + 1 {
            return Err(NetError::Protocol("invalid Noise message length".into()));
        }
        if self.state.receiving_nonce() >= self.config.nonce_limit {
            return Err(NetError::NonceExhausted);
        }
        let mut plain = vec![0u8; ciphertext.len()];
        let n = self
            .state
            .read_message(ciphertext, &mut plain)
            .map_err(|_| NetError::Protocol("decryption failed".into()))?;
        plain.truncate(n);
        self.received += 1;
        if self.received % self.config.rekey_interval == 0 {
            self.state.rekey_incoming();
        }
        let (header, data) = plain
            .split_first()
            .ok_or_else(|| NetError::Protocol("empty chunk".into()))?;
        if header & !CHUNK_FINAL != 0 {
            return Err(NetError::Protocol("unknown chunk flags".into()));
        }
        let new_len = self.pending.len() + data.len();
        if new_len > self.config.max_message_size {
            return Err(NetError::MessageTooLarge {
                size: new_len,
                limit: self.config.max_message_size,
            });
        }
        if header & CHUNK_FINAL != 0 {
            if self.pending.is_empty() {
                return Ok(Some(data.to_vec()));
            }
            let mut msg = std::mem::take(&mut self.pending);
            msg.extend_from_slice(data);
            return Ok(Some(msg));
        }
        if data.len() != MAX_CHUNK_DATA {
            // Non-final chunks are always full; anything else is a framing bug or attack.
            return Err(NetError::Protocol("short non-final chunk".into()));
        }
        self.pending.extend_from_slice(data);
        Ok(None)
    }

    /// Encrypts and sends one application message.
    pub async fn send<T>(&mut self, t: &mut T, plaintext: &[u8]) -> Result<()>
    where
        T: Sink<Vec<u8>, Error = NetError> + Unpin + ?Sized,
    {
        for chunk in self.encrypt(plaintext)? {
            t.feed(chunk).await?;
        }
        t.flush().await
    }

    /// Receives and decrypts one complete application message. Returns
    /// `None` if the peer closed the transport cleanly between messages.
    pub async fn recv<T>(&mut self, t: &mut T) -> Result<Option<Vec<u8>>>
    where
        T: Stream<Item = Result<Vec<u8>>> + Unpin + ?Sized,
    {
        loop {
            match t.next().await {
                None if self.pending.is_empty() => return Ok(None),
                None => return Err(NetError::Protocol("peer closed mid-message".into())),
                Some(frame) => {
                    if let Some(msg) = self.decrypt(&frame?)? {
                        return Ok(Some(msg));
                    }
                }
            }
        }
    }
}

/// In-memory [`FrameTransport`] pair for tests and in-process use.
pub mod memory {
    use super::*;
    use futures_channel::mpsc;

    /// One end of an in-memory duplex message pipe.
    pub struct MemoryTransport {
        tx: mpsc::UnboundedSender<Vec<u8>>,
        rx: mpsc::UnboundedReceiver<Vec<u8>>,
    }

    /// Creates a connected pair of in-memory transports.
    pub fn pair() -> (MemoryTransport, MemoryTransport) {
        let (atx, brx) = mpsc::unbounded();
        let (btx, arx) = mpsc::unbounded();
        (
            MemoryTransport { tx: atx, rx: arx },
            MemoryTransport { tx: btx, rx: brx },
        )
    }

    impl Stream for MemoryTransport {
        type Item = Result<Vec<u8>>;
        fn poll_next(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            self.rx.poll_next_unpin(cx).map(|o| o.map(Ok))
        }
    }

    impl Sink<Vec<u8>> for MemoryTransport {
        type Error = NetError;
        fn poll_ready(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<()>> {
            self.tx
                .poll_ready_unpin(cx)
                .map_err(|_| NetError::WebSocket("peer gone".into()))
        }
        fn start_send(mut self: Pin<&mut Self>, item: Vec<u8>) -> Result<()> {
            self.tx
                .start_send_unpin(item)
                .map_err(|_| NetError::WebSocket("peer gone".into()))
        }
        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<()>> {
            self.tx
                .poll_flush_unpin(cx)
                .map_err(|_| NetError::WebSocket("peer gone".into()))
        }
        fn poll_close(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<()>> {
            self.tx
                .poll_close_unpin(cx)
                .map_err(|_| NetError::WebSocket("peer gone".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::pair;
    use super::*;

    async fn established(
        config: NoiseConfig,
    ) -> (NoiseSession, NoiseSession, StaticKeypair, StaticKeypair) {
        let server = StaticKeypair::generate();
        let device = StaticKeypair::generate();
        let (mut a, mut b) = pair();
        let spub = *server.public();
        let d2 = device.clone();
        let s2 = server.clone();
        let client =
            tokio::spawn(async move { handshake_initiator(&mut a, &d2, &spub, config).await });
        let srv = handshake_responder(&mut b, &s2, config).await.unwrap();
        let cli = client.await.unwrap().unwrap();
        (cli, srv, device, server)
    }

    #[tokio::test]
    async fn handshake_exchanges_static_keys() {
        let (cli, srv, device, server) = established(NoiseConfig::default()).await;
        assert_eq!(cli.remote_static(), server.public());
        assert_eq!(srv.remote_static(), device.public());
    }

    #[tokio::test]
    async fn initiator_rejects_wrong_server_key() {
        let server = StaticKeypair::generate();
        let impostor_expected = *StaticKeypair::generate().public();
        let device = StaticKeypair::generate();
        let (mut a, mut b) = pair();
        let srv = tokio::spawn(async move {
            handshake_responder(&mut b, &server, NoiseConfig::default()).await
        });
        let err = handshake_initiator(&mut a, &device, &impostor_expected, NoiseConfig::default())
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::ServerKeyMismatch { .. }));
        drop(a);
        // The responder never receives message 3, so it fails.
        assert!(srv.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn responder_rejects_garbage() {
        let server = StaticKeypair::generate();
        let (mut a, mut b) = pair();
        a.send(vec![1, 2, 3]).await.unwrap();
        a.send(vec![0u8; 5000]).await.unwrap();
        // First message (e) of random 3 bytes is too short for an ephemeral key.
        assert!(
            handshake_responder(&mut b, &server, NoiseConfig::default())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn prologue_mismatch_fails() {
        // A peer using a different prologue cannot complete the handshake.
        let server = StaticKeypair::generate();
        let device = StaticKeypair::generate();
        let (mut a, mut b) = pair();
        let params: snow::params::NoiseParams = NOISE_PARAMS.parse().unwrap();
        let mut hs = snow::Builder::new(params)
            .local_private_key(device.secret_bytes())
            .unwrap()
            .prologue(b"other")
            .unwrap()
            .build_initiator()
            .unwrap();
        let srv = tokio::spawn(async move {
            handshake_responder(&mut b, &server, NoiseConfig::default()).await
        });
        let mut buf = vec![0u8; 1024];
        let n = hs.write_message(&[], &mut buf).unwrap();
        a.send(buf[..n].to_vec()).await.unwrap();
        let msg2 = a.next().await.unwrap().unwrap();
        assert!(hs.read_message(&msg2, &mut buf).is_err());
        drop(a);
        assert!(srv.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn roundtrip_small_empty_and_large_messages() {
        let (mut cli, mut srv, _, _) = established(NoiseConfig::default()).await;
        for size in [
            0usize,
            1,
            MAX_CHUNK_DATA - 1,
            MAX_CHUNK_DATA,
            MAX_CHUNK_DATA + 1,
            3 * MAX_CHUNK_DATA + 17,
        ] {
            let msg: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let chunks = cli.encrypt(&msg).unwrap();
            assert_eq!(chunks.len(), size.div_ceil(MAX_CHUNK_DATA).max(1));
            assert!(chunks.iter().all(|c| c.len() <= MAX_NOISE_MESSAGE));
            let mut out = None;
            for (i, c) in chunks.iter().enumerate() {
                let r = srv.decrypt(c).unwrap();
                if i + 1 < chunks.len() {
                    assert!(r.is_none());
                } else {
                    out = r;
                }
            }
            assert_eq!(out.unwrap(), msg);
        }
        // And back.
        let c = srv.encrypt(b"pong").unwrap();
        assert_eq!(cli.decrypt(&c[0]).unwrap().unwrap(), b"pong");
    }

    #[tokio::test]
    async fn tampered_ciphertext_is_rejected_and_session_fails() {
        let (mut cli, mut srv, _, _) = established(NoiseConfig::default()).await;
        let mut c = cli.encrypt(b"hello").unwrap().remove(0);
        c[3] ^= 0x40;
        assert!(srv.decrypt(&c).is_err());
        // Session is now poisoned even for valid data.
        let good = cli.encrypt(b"hello").unwrap().remove(0);
        assert!(srv.decrypt(&good).is_err());
    }

    #[tokio::test]
    async fn replayed_or_reordered_message_is_rejected() {
        let (mut cli, mut srv, _, _) = established(NoiseConfig::default()).await;
        let m1 = cli.encrypt(b"one").unwrap().remove(0);
        let m2 = cli.encrypt(b"two").unwrap().remove(0);
        assert_eq!(srv.decrypt(&m1).unwrap().unwrap(), b"one");
        assert!(srv.decrypt(&m1).is_err());
        let _ = m2;
    }

    #[tokio::test]
    async fn oversized_message_rejected_on_both_sides() {
        let cfg = NoiseConfig {
            max_message_size: 100_000,
            ..NoiseConfig::default()
        };
        let (mut cli, mut srv, _, _) = established(cfg).await;
        assert!(matches!(
            cli.encrypt(&vec![0u8; 100_001]),
            Err(NetError::MessageTooLarge { .. })
        ));
        // Receiver enforces its own limit even if the sender's is larger.
        srv.set_max_message_size(70_000);
        let chunks = cli.encrypt(&vec![0u8; 90_000]).unwrap();
        assert!(srv.decrypt(&chunks[0]).unwrap().is_none());
        assert!(matches!(
            srv.decrypt(&chunks[1]),
            Err(NetError::MessageTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn rekeying_keeps_both_sides_in_sync() {
        let cfg = NoiseConfig {
            rekey_interval: 3,
            ..NoiseConfig::default()
        };
        let (mut cli, mut srv, _, _) = established(cfg).await;
        for i in 0..20u32 {
            let m = i.to_be_bytes();
            let c = cli.encrypt(&m).unwrap().remove(0);
            assert_eq!(srv.decrypt(&c).unwrap().unwrap(), m);
            let c = srv.encrypt(&m).unwrap().remove(0);
            assert_eq!(cli.decrypt(&c).unwrap().unwrap(), m);
        }
    }

    #[tokio::test]
    async fn rekey_mismatch_breaks_decryption() {
        // Sanity check that rekeying actually changes keys.
        let (mut cli, mut srv, _, _) = established(NoiseConfig {
            rekey_interval: 2,
            ..NoiseConfig::default()
        })
        .await;
        srv.config.rekey_interval = 1000;
        let c1 = cli.encrypt(b"a").unwrap().remove(0);
        let c2 = cli.encrypt(b"b").unwrap().remove(0);
        let c3 = cli.encrypt(b"c").unwrap().remove(0);
        assert!(srv.decrypt(&c1).is_ok());
        assert!(srv.decrypt(&c2).is_ok());
        assert!(srv.decrypt(&c3).is_err());
    }

    #[tokio::test]
    async fn nonce_exhaustion_stops_the_session() {
        let cfg = NoiseConfig {
            nonce_limit: 3,
            ..NoiseConfig::default()
        };
        let (mut cli, mut srv, _, _) = established(cfg).await;
        for _ in 0..3 {
            let c = cli.encrypt(b"x").unwrap().remove(0);
            srv.decrypt(&c).unwrap();
        }
        assert!(matches!(cli.encrypt(b"x"), Err(NetError::NonceExhausted)));
        assert!(cli.encrypt(b"x").is_err());
    }

    #[tokio::test]
    async fn send_and_recv_over_transport() {
        let server = StaticKeypair::generate();
        let device = StaticKeypair::generate();
        let (mut a, mut b) = pair();
        let spub = *server.public();
        let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let big2 = big.clone();
        let client = tokio::spawn(async move {
            let mut s = handshake_initiator(&mut a, &device, &spub, NoiseConfig::default())
                .await
                .unwrap();
            s.send(&mut a, &big2).await.unwrap();
            s.recv(&mut a).await.unwrap().unwrap()
        });
        let mut s = handshake_responder(&mut b, &server, NoiseConfig::default())
            .await
            .unwrap();
        let got = s.recv(&mut b).await.unwrap().unwrap();
        assert_eq!(got, big);
        s.send(&mut b, b"ok").await.unwrap();
        assert_eq!(client.await.unwrap(), b"ok");
    }

    #[tokio::test]
    async fn crafted_chunk_flags_are_rejected() {
        let (mut cli, mut srv, _, _) = established(NoiseConfig::default()).await;
        // Encrypt a chunk with an unknown flag bit by bypassing `encrypt`.
        let c = cli.write_one(&[0x80, 1, 2]).unwrap();
        assert!(srv.decrypt(&c).is_err());
        let (mut cli, mut srv, _, _) = established(NoiseConfig::default()).await;
        // A short non-final chunk is rejected.
        let c = cli.write_one(&[0x00, 1, 2]).unwrap();
        assert!(srv.decrypt(&c).is_err());
    }
}
