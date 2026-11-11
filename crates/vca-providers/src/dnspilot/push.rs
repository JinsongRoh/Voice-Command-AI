//! DNSPilot's end-to-end encrypted push relay (the `push` tag of the public
//! OpenAPI spec, version 1.1.0).
//!
//! * `POST /api/v1/push/devices` (`push:register`): registers an APNs or FCM
//!   token. Required: `app_id`, `platform`, `token`.
//! * `DELETE /api/v1/push/devices/{device_id}` (`push:register`).
//! * `POST /api/v1/push/send` (`push:send`, editor role or higher): only
//!   `ciphertext` is required, at most [`MAX_CIPHERTEXT_BYTES`] decoded. The
//!   server never stores or logs it, and the visible notification text is
//!   fixed by the server. `ttl` defaults to 3600 and `priority` to `high`;
//!   the spec gives no bounds, so VCA leaves both to the documented defaults.
//!
//! The spec has no call that lists devices or reads `invalid_at` later, so
//! the caller keeps the ids it registered.
//!
//! Errors from these calls drop the server's text: a registration carries a
//! device token, and nothing here may repeat it into a log.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::console::ConsoleClient;
use crate::error::{ProviderError, Result};
use crate::secret::Secret;

/// Scopes of the OAuth grant that sends pushes.
pub const PUSH_SCOPES: &[&str] = &["push:register", "push:send"];

/// Largest decoded ciphertext `POST /api/v1/push/send` accepts.
pub const MAX_CIPHERTEXT_BYTES: usize = 3072;

pub mod paths {
    pub const DEVICES: &str = "/api/v1/push/devices";
    pub const SEND: &str = "/api/v1/push/send";

    pub fn device(id: i64) -> String {
        format!("{DEVICES}/{id}")
    }
}

/// `platform` of a push device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushPlatform {
    Ios,
    Android,
}

/// Body of `POST /api/v1/push/devices`. `Debug` hides the token; `Serialize`
/// writes it, for the request only.
#[derive(Debug, Clone, Serialize)]
pub struct RegisterPushDevice {
    pub app_id: String,
    pub platform: PushPlatform,
    pub token: Secret,
    /// Binds the phone to one Agent PC of the same organization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_device_id: Option<i64>,
    /// The public key the sender encrypts payloads with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e2e_pubkey: Option<String>,
    /// Language of the fixed notification text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

/// A registered device. No token is returned.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PushDevice {
    pub id: i64,
    #[serde(default)]
    pub invalid_at: Option<String>,
}

/// Opaque ciphertext, checked against [`MAX_CIPHERTEXT_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushCiphertext(String);

impl PushCiphertext {
    pub fn new(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_CIPHERTEXT_BYTES {
            return Err(ProviderError::InvalidConfig(format!(
                "a push payload is 1 to {MAX_CIPHERTEXT_BYTES} bytes, not {}",
                bytes.len()
            )));
        }
        Ok(Self(encode_base64(bytes)))
    }
}

/// Body of `POST /api/v1/push/send`. `device_id` names one phone; each phone
/// has its own key, so one request goes out per phone. `ttl` and `priority`
/// stay at the documented defaults (one hour, high).
#[derive(Debug, Clone, Serialize)]
pub struct PushSend {
    ciphertext: String,
    pub device_id: i64,
    /// Replaces a queued push with the same key.
    pub collapse_key: String,
}

impl PushSend {
    pub fn new(ciphertext: PushCiphertext, device_id: i64, collapse_key: String) -> Self {
        Self {
            ciphertext: ciphertext.0,
            device_id,
            collapse_key,
        }
    }
}

/// `202` answer of `POST /api/v1/push/send`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PushQueued {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub queued: i64,
    #[serde(default)]
    pub message_ids: Vec<i64>,
}

pub fn encode_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text.trim()).ok()
}

/// Keeps the kind of failure and drops the server's text.
fn quiet(e: ProviderError) -> ProviderError {
    match e {
        ProviderError::Unauthorized(_) => {
            ProviderError::Unauthorized("the push authorization was refused".into())
        }
        ProviderError::Forbidden(_) => ProviderError::Forbidden(
            "push needs the granted scope and an editor role or higher".into(),
        ),
        ProviderError::NotFound(_) => ProviderError::NotFound("push device".into()),
        ProviderError::Conflict(_) => ProviderError::Conflict("push device".into()),
        ProviderError::Api { status, .. } => ProviderError::Api {
            status,
            message: "the push request was refused".into(),
        },
        // Was a plain `Forbidden` before DNSPilot gave it a code.
        ProviderError::Coded { ref code, .. }
            if code == super::contract::invite_errors::OAUTH_TUNNEL_NOT_OWNED =>
        {
            ProviderError::Forbidden(
                "push needs the granted scope and an editor role or higher".into(),
            )
        }
        ProviderError::Coded { status, .. } => ProviderError::Api {
            status,
            message: "the push request was refused".into(),
        },
        other => other,
    }
}

impl ConsoleClient {
    /// `POST /api/v1/push/devices`.
    pub async fn push_register(&self, req: &RegisterPushDevice) -> Result<PushDevice> {
        self.call(Method::POST, paths::DEVICES, Some(req))
            .await
            .map_err(quiet)
    }

    /// `DELETE /api/v1/push/devices/{device_id}`.
    pub async fn push_unregister(&self, device_id: i64) -> Result<()> {
        self.send(Method::DELETE, &paths::device(device_id), None)
            .await
            .map(|_| ())
            .map_err(quiet)
    }

    /// `POST /api/v1/push/send`.
    pub async fn push_send(&self, req: &PushSend) -> Result<PushQueued> {
        self.call(Method::POST, paths::SEND, Some(req))
            .await
            .map_err(quiet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciphertext_is_limited_to_the_documented_size() {
        assert!(PushCiphertext::new(&[0; MAX_CIPHERTEXT_BYTES]).is_ok());
        assert!(PushCiphertext::new(&[0; MAX_CIPHERTEXT_BYTES + 1]).is_err());
        assert!(PushCiphertext::new(&[]).is_err());
    }

    #[test]
    fn debug_output_hides_the_device_token() {
        let req = RegisterPushDevice {
            app_id: "app".into(),
            platform: PushPlatform::Ios,
            token: Secret::new("apns-token-value"),
            agent_device_id: None,
            e2e_pubkey: None,
            locale: None,
        };
        assert!(!format!("{req:?}").contains("apns-token-value"));
    }

    #[test]
    fn server_text_is_dropped_from_errors() {
        let e = quiet(ProviderError::Api {
            status: 400,
            message: "bad token apns-token-value".into(),
        });
        assert!(!e.to_string().contains("apns-token-value"));
        let e = quiet(ProviderError::Forbidden("apns-token-value".into()));
        assert!(!e.to_string().contains("apns-token-value"));
        assert!(matches!(
            quiet(ProviderError::RateLimited { retry_after: None }),
            ProviderError::RateLimited { .. }
        ));
    }
}
