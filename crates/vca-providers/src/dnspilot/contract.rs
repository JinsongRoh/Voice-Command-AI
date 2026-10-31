//! Request and response types of the DNS Pilot APIs used by VCA.
//!
//! **Field names per DNS Pilot VCA contract; to be confirmed.** The DNS
//! Pilot Agent local REST API and the console OAuth / tunnel APIs are not
//! publicly documented yet (gap G10). Everything that depends on exact
//! paths or JSON field names lives in this one module so that it can be
//! adjusted in one place once the public OpenAPI subset is published.
//! Responses are parsed tolerantly: unknown fields are ignored and most
//! fields are optional.

use serde::{Deserialize, Serialize};

/// Local API paths (Agent, per-user REST API over a Unix socket or named pipe).
pub mod paths {
    pub const TOKEN: &str = "/v1/token";
    pub const STATUS: &str = "/v1/status";
    pub const ACTIVATION: &str = "/v1/activation";
    pub const TUNNELS: &str = "/v1/tunnels";
    pub const KILL: &str = "/v1/kill";
    pub const RESUME: &str = "/v1/resume";
    pub const EVENTS: &str = "/v1/events";

    pub fn tunnel(id: &str) -> String {
        format!("/v1/tunnels/{id}")
    }

    pub fn app_clients(tunnel_id: &str) -> String {
        format!("/v1/tunnels/{tunnel_id}/app-clients")
    }

    pub fn app_client(tunnel_id: &str, client_id: &str) -> String {
        format!("/v1/tunnels/{tunnel_id}/app-clients/{client_id}")
    }

    /// Console OAuth endpoints (RFC 8628 / RFC 6749 / RFC 7009).
    pub const OAUTH_DEVICE_AUTHORIZATION: &str = "/oauth/device_authorization";
    pub const OAUTH_TOKEN: &str = "/oauth/token";
    pub const OAUTH_REVOKE: &str = "/oauth/revoke";

    /// Console tunnel API used with an OAuth access token. To be confirmed.
    pub const CONSOLE_TUNNELS: &str = "/api/v1/tunnels";
}

/// Default Unix socket of the Agent's local API.
pub const UNIX_SOCKET_PATH: &str = "/run/dnspilot/local.sock";
/// Default Windows named pipe of the Agent's local API.
pub const WINDOWS_PIPE_NAME: &str = r"\\.\pipe\dnspilot-local";

/// OAuth client ID registered for VCA.
pub const OAUTH_CLIENT_ID: &str = "vca";

/// Scopes VCA requests. `p2p:write` is not grantable via OAuth.
pub const OAUTH_SCOPES: &[&str] = &[
    "tunnels:read",
    "tunnels:write",
    "p2p:read",
    "push:register",
    "push:send",
    "logs:read",
    "devices:read",
];

/// Default console base URL.
pub const DEFAULT_CONSOLE_URL: &str = "https://dns.dnspilot.kr";

/// Access mode VCA uses: mTLS public-key pinning, no cookies needed.
pub const ACCESS_MODE_APP: &str = "app";

/// `POST /v1/token` response (Unix: the Agent identifies the caller by
/// socket peer credentials and issues a per-user token).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TokenResponse {
    pub token: String,
    pub expires_at: Option<String>,
}

/// `GET /v1/status`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentStatus {
    /// Identifier of the current Agent run; changes when the Agent restarts.
    pub boot_id: Option<String>,
    pub version: Option<String>,
    /// The device has been activated (browser approval done).
    pub activated: bool,
    /// The kill switch is engaged.
    pub killed: bool,
    pub relay_connected: Option<bool>,
    pub region: Option<String>,
}

/// `GET /v1/activation`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Activation {
    /// `activated`, `pending` or `not_activated`.
    pub state: String,
    /// Browser URL where the user approves the device (re-authentication required).
    pub verification_url: Option<String>,
    pub device_id: Option<String>,
}

/// A tunnel as returned by the Agent or the console.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Tunnel {
    pub id: String,
    /// Full public hostname, for example `vca-k3x9q2ab4m.example.dnspilot.net`.
    pub hostname: String,
    pub access_mode: String,
    pub local_scheme: String,
    pub local_host: Option<String>,
    pub local_port: u16,
    pub public_port: Option<u16>,
    pub enabled: bool,
    /// Agent-side state, for example `active`, `starting`, `error`.
    pub state: Option<String>,
}

/// `GET /v1/tunnels`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TunnelList {
    pub tunnels: Vec<Tunnel>,
}

/// `POST /v1/tunnels` (and the console tunnel API).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CreateTunnel {
    /// Random, non-identifying label (C6). The provider picks the domain.
    pub hostname_label: String,
    pub access_mode: String,
    pub local_scheme: String,
    pub local_host: String,
    pub local_port: u16,
    pub public_port: u16,
    /// Console API only: the device that serves the tunnel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

/// `PATCH /v1/tunnels/{id}`.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PatchTunnel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_port: Option<u16>,
}

/// `POST /v1/tunnels/{id}/app-clients`: registers a device's client key
/// for mTLS pinning. Exactly one of the key fields is set.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RegisterAppClient {
    /// Base64 (standard, padded) SHA-256 of the DER SubjectPublicKeyInfo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_sha256: Option<String>,
    /// PEM `PUBLIC KEY` block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_pem: Option<String>,
    /// Non-identifying label.
    pub label: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppClient {
    pub id: String,
    pub label: Option<String>,
    pub public_key_sha256: Option<String>,
}

/// Payload of a local API event. `boot_id` is present on every event.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct EventPayload {
    pub boot_id: Option<String>,
    pub tunnel_id: Option<String>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// SSE event names with special meaning.
pub mod events {
    /// First event of a connection: the requested resume point is valid.
    pub const HELLO: &str = "hello";
    /// First event of a connection: a different Agent run; start from scratch.
    pub const RESET: &str = "reset";
}

/// RFC 8628 device authorization response.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default)]
    pub interval: Option<u64>,
}

/// RFC 6749 token response.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenEndpointResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// RFC 6749 §5.2 error response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OAuthErrorResponse {
    pub error: String,
    pub error_description: Option<String>,
}
