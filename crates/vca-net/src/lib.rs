//! Pairing, authentication, end-to-end encryption and transport for VCA.
//!
//! Every connectivity provider (tunnel, mesh VPN, LAN) is treated as an
//! **untrusted transport**: confidentiality, integrity and peer
//! authentication come exclusively from a Noise `XX` channel between the
//! device's static key and the server's static key (see
//! `docs/architecture.md` §8 and `docs/dnspilot-requirements.md` C1).
//!
//! Layering of a connection:
//!
//! 1. HTTP/1.1 WebSocket (plain HTTP on loopback by default; providers
//!    terminate TLS in front of it).
//! 2. [`noise`]: `Noise_XX_25519_ChaChaPoly_SHA256` handshake, then
//!    encrypted transport messages with chunking for large payloads,
//!    periodic rekeying and nonce-exhaustion protection.
//! 3. Authentication inside the Noise channel ([`transport`]): pairing with a
//!    one-time secret from the QR code ([`pairing`]), TOTP sign-in
//!    ([`totp`]), or device-bound tokens ([`tokens`]).
//! 4. MessagePack [`vca_core::Envelope`]s, with an encrypted heartbeat.
//!
//! Supporting pieces: [`keys`] (static keys and key storage),
//! [`registry`] (paired devices), [`ratelimit`], [`resume`] (replay buffers
//! for resumable streams), [`audit`] (append-only audit log) and
//! [`killswitch`].

pub mod audit;
pub mod clock;
pub mod error;
pub mod keys;
pub mod killswitch;
pub mod noise;
pub mod pairing;
pub mod ratelimit;
pub mod registry;
pub mod resume;
pub mod tokens;
pub mod totp;
pub mod transport;
mod util;

pub use audit::{AuditEvent, AuditLog, AuditRecord, AuthMethod, client_ip_from_headers, redact};
pub use clock::{Clock, ManualClock, SharedClock, SystemClock, system_clock};
pub use error::{AuthFailure, CloseReason, NetError, Result};
#[cfg(feature = "keychain")]
pub use keys::KeychainKeyStore;
pub use keys::{
    FileKeyStore, Fingerprint, KeyStore, MemoryKeyStore, PublicKey, StaticKeypair,
    load_or_generate_keypair, load_or_generate_secret,
};
pub use killswitch::KillSwitch;
pub use noise::{NoiseConfig, NoiseSession};
pub use pairing::{PairingManager, PairingPayload, PairingTicket};
pub use ratelimit::{RateLimitConfig, RateLimiter};
pub use registry::{DeviceRecord, DeviceRegistry, FileDeviceRegistry, MemoryDeviceRegistry, Scope};
pub use resume::{ReplayBuffer, ResumeResult, Sequenced};
pub use tokens::{AccessClaims, TokenConfig, TokenError, TokenPair, TokenService};
pub use totp::{PendingEnrollment, TotpError, TotpManager};
pub use transport::{
    AuthenticatedDevice, ClientAuth, ClientConfig, ClientConnection, ClientSession, Connection,
    ConnectionInfo, ConnectionSender, NetServer, NetServerBuilder, ServerConfig, ServerConnection,
    bind,
};

/// Default loopback bind address of the VCA server (`docs/architecture.md` §7).
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:47800";

/// Default WebSocket path.
pub const DEFAULT_WS_PATH: &str = "/v1/ws";
