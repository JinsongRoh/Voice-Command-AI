//! Request and response types of the DNSPilot APIs VCA uses, taken from the
//! public documentation:
//!
//! - Agent local API: <https://dns.dnspilot.kr/docs/en/agent-local-api>
//! - OAuth device flow: <https://dns.dnspilot.kr/docs/en/oauth-device-flow>
//! - App-mode tunnels: <https://dns.dnspilot.kr/docs/en/tunnel-app-mode>
//! - Public OpenAPI spec: <https://dns.dnspilot.kr/api/public/openapi.json>
//!
//! plus the production response shapes the DNSPilot team supplied for the
//! responses the OpenAPI spec lists only as `object` (tunnel and device
//! objects, the Agent's `GET /v1/tunnels`, error bodies) and the upcoming
//! `GET /api/tunnels/zones`.
//!
//! Parsing is tolerant: unknown fields are ignored, and every field that is
//! not needed to identify an object is optional.

use serde::{Deserialize, Deserializer, Serialize};

use crate::secret::Secret;

/// Agent local API paths.
pub mod paths {
    pub const STATUS: &str = "/v1/status";
    /// Linux and macOS only (Windows reads a per-user token file).
    pub const TOKEN: &str = "/v1/token";
    pub const ACTIVATION: &str = "/v1/activation";
    pub const TUNNELS: &str = "/v1/tunnels";
    pub const KILL: &str = "/v1/kill";
    pub const RESUME: &str = "/v1/resume";
    pub const EVENTS: &str = "/v1/events";

    pub fn tunnel(id: i64) -> String {
        format!("/v1/tunnels/{id}")
    }

    pub fn app_clients(tunnel_id: i64) -> String {
        format!("/v1/tunnels/{tunnel_id}/app-clients")
    }

    pub fn app_client(tunnel_id: i64, client_id: i64) -> String {
        format!("/v1/tunnels/{tunnel_id}/app-clients/{client_id}")
    }
}

/// Console paths (public API, OAuth bearer token).
pub mod console_paths {
    pub const OAUTH_DEVICE_AUTHORIZATION: &str = "/oauth/device_authorization";
    pub const OAUTH_TOKEN: &str = "/oauth/token";
    pub const OAUTH_REVOKE: &str = "/oauth/revoke";
    pub const AUTH_ME: &str = "/api/auth/me";
    pub const AGENT_DEVICES: &str = "/api/agent-devices";
    pub const TUNNELS: &str = "/api/tunnels";
    /// Zones the person may create tunnels in (`tunnels:read`). Being added
    /// by DNSPilot; 404 until it ships.
    pub const TUNNEL_ZONES: &str = "/api/tunnels/zones";
    pub const PUBLIC_STATUS: &str = "/api/public/status";
    /// Enrolls a phone with an invite code. Public: no authentication.
    pub const PUBLIC_ENROLL: &str = "/api/public/tunnel-app-enroll";

    pub fn tunnel(id: i64) -> String {
        format!("/api/tunnels/{id}")
    }
    pub fn app_clients(tunnel_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/app-clients")
    }
    pub fn app_client(tunnel_id: i64, client_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/app-clients/{client_id}")
    }
    pub fn app_invites(tunnel_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/app-invites")
    }
    pub fn app_invite(tunnel_id: i64, invite_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/app-invites/{invite_id}")
    }
    pub fn app_tokens(tunnel_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/app-tokens")
    }
    pub fn app_token(tunnel_id: i64, token_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/app-tokens/{token_id}")
    }
    pub fn app_token_allowed(tunnel_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/app-token-allowed")
    }
    pub fn enable(tunnel_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/enable")
    }
    pub fn disable(tunnel_id: i64) -> String {
        format!("/api/tunnels/{tunnel_id}/disable")
    }
}

/// Unix socket of the Agent's local API (Linux, macOS).
pub const UNIX_SOCKET_PATH: &str = "/run/dnspilot/local.sock";
/// Named pipe of the Agent's local API (Windows).
pub const WINDOWS_PIPE_NAME: &str = r"\\.\pipe\dnspilot-local";
/// Windows service the named pipe server must belong to.
pub const WINDOWS_SERVICE_NAME: &str = "DNSPilotAgent";
/// Directory (under `%ProgramData%`) of the per-user local API token files
/// `<SID>.token` on Windows.
pub const WINDOWS_TOKEN_DIR: &str = r"DNSPilot\local-api";

/// Public OAuth client id registered for Voice Command AI (public client,
/// no secret).
pub const OAUTH_CLIENT_ID: &str = "vca";

/// Scopes VCA requests: create and manage its own app-mode tunnels and list
/// the Agent devices. Nothing else.
pub const OAUTH_SCOPES: &[&str] = &["tunnels:read", "tunnels:write", "devices:read"];

/// Console base URL.
pub const DEFAULT_CONSOLE_URL: &str = "https://dns.dnspilot.kr";

/// Access mode VCA uses: only clients with a pinned certificate key (or an
/// app token, when allowed) reach the local service.
pub const ACCESS_MODE_APP: &str = "app";

/// Accepts a JSON number or a numeric string.
fn lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

fn lenient_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(lenient_i64(d)?.and_then(|v| u64::try_from(v).ok()))
}

// ---------------------------------------------------------------------------
// Agent local API
// ---------------------------------------------------------------------------

/// `GET /v1/status`: `{version, logged_in, device_id, device_name, relay,
/// tunnels_online, killed, boot_id}`. No token needed, no secrets.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct AgentStatus {
    pub version: Option<String>,
    /// The Agent is signed in (device approved in the console).
    pub logged_in: bool,
    /// The console's id of this Agent device (used as `device_id` when an
    /// app creates a tunnel on it).
    #[serde(deserialize_with = "lenient_i64")]
    pub device_id: Option<i64>,
    pub device_name: Option<String>,
    /// Hostname of the relay the Agent is connected to (empty when none).
    #[serde(deserialize_with = "lenient_string")]
    pub relay: Option<String>,
    #[serde(deserialize_with = "lenient_u64")]
    pub tunnels_online: Option<u64>,
    /// The kill switch is engaged.
    pub killed: bool,
    /// Changes on every Agent restart (16 hex characters).
    pub boot_id: Option<String>,
}

impl AgentStatus {
    /// The relay hostname, when connected to one.
    pub fn relay_summary(&self) -> Option<String> {
        self.relay.clone().filter(|r| !r.trim().is_empty())
    }
}

/// Accepts a JSON string (or a scalar, shown as text); `null` is `None`.
fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string()),
    })
}

/// Error body of the Agent local API: `{"error": "<code>", "detail":
/// "<text>"}`. Console errors are passed through with their status.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentErrorResponse {
    pub error: Option<String>,
    pub detail: Option<serde_json::Value>,
}

/// A clear, user-facing explanation of an Agent error code.
pub fn agent_error_text(code: &str) -> Option<&'static str> {
    Some(match code {
        "not_logged_in" => "the DNSPilot Agent on this PC is not signed in",
        "killed" => "the DNSPilot kill switch is engaged",
        "already_logged_in" => "the DNSPilot Agent is already signed in",
        "unauthorized" => "the DNSPilot local API token was not accepted",
        "peer_denied" => "this user may not use the DNSPilot Agent's local API",
        "bad_request" => "the DNSPilot Agent rejected the request",
        "bad_key" => "the DNSPilot Agent rejected the client key",
        "not_implemented" => "the DNSPilot Agent does not support this yet",
        "login_failed" => "the DNSPilot Agent could not sign in",
        "agent_error" => "the DNSPilot Agent could not reach the console",
        "token_error" => "the DNSPilot Agent could not create a local API token",
        "kill_persist_failed" => "the DNSPilot Agent could not save the kill switch",
        "resume_failed" => "the DNSPilot Agent could not lift the kill switch",
        "not_kill_owner" => {
            "only the user who pressed the kill switch, or an administrator, can lift it"
        }
        _ => return None,
    })
}

/// One tunnel in the Agent's `GET /v1/tunnels`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LocalTunnel {
    pub id: i64,
    pub hostname: String,
    /// Omitted means 443.
    pub public_port: Option<u16>,
    pub local_port: Option<u16>,
    pub local_scheme: Option<String>,
    pub access_mode: Option<String>,
    pub server_status: Option<String>,
    pub suspend_reason: Option<String>,
    pub paused: Option<bool>,
    /// `ready`, `waiting` or `error`.
    pub cert: Option<String>,
    pub serving: Option<bool>,
    /// `serving`, `paused`, `suspended`, `disabled`, `waiting`,
    /// `connecting` or `error`.
    pub state: Option<String>,
    pub message: Option<String>,
}

impl LocalTunnel {
    pub fn public_port(&self) -> u16 {
        self.public_port.unwrap_or(443)
    }

    pub fn is_app_mode(&self) -> bool {
        self.access_mode.as_deref() == Some(ACCESS_MODE_APP)
    }

    /// For example `serving (certificate ready)`.
    pub fn state_summary(&self) -> String {
        let mut s = self.state.clone().unwrap_or_else(|| "unknown".into());
        if let Some(cert) = &self.cert {
            s.push_str(&format!(" (certificate {cert})"));
        }
        if let Some(m) = self.message.as_deref().filter(|m| !m.is_empty()) {
            s.push_str(&format!(": {m}"));
        }
        s
    }
}

/// `GET /v1/tunnels`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LocalTunnelList {
    pub tunnels: Vec<LocalTunnel>,
}

/// `PATCH /v1/tunnels/{id}` response.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LocalPatchResponse {
    pub id: i64,
    pub enabled: bool,
}

/// `POST /v1/tunnels/{id}/app-clients` response (201).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LocalAppClient {
    pub id: i64,
    pub spki_sha256: Option<String>,
    pub label: Option<String>,
}

/// `GET /v1/token` (Linux, macOS).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LocalToken {
    pub token: String,
}

/// `POST /v1/activation`: starts device sign-in.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Activation {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
}

/// `PATCH /v1/tunnels/{id}`: share or stop sharing a tunnel from this PC.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct PatchTunnel {
    pub enabled: bool,
}

/// App-client registration, for both `POST /v1/tunnels/{id}/app-clients`
/// (Agent) and `POST /api/tunnels/{id}/app-clients` (console). Exactly one
/// of the key fields is set.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RegisterAppClient {
    /// 64 hex characters: SHA-256 of the DER SubjectPublicKeyInfo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spki_sha256: Option<String>,
    /// `PUBLIC KEY` or `CERTIFICATE` PEM; the server computes the hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_pem: Option<String>,
    /// At most 64 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Body of one Agent event: `{id, boot_id, kind, at, data}`. The control
/// events `hello {boot_id, last_id}` and `reset {boot_id, last_id, reason}`
/// carry no `id`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct AgentEventBody {
    #[serde(deserialize_with = "lenient_u64")]
    pub id: Option<u64>,
    pub boot_id: Option<String>,
    pub kind: Option<String>,
    pub at: Option<String>,
    pub data: serde_json::Value,
    /// `hello` / `reset`: the last id the Agent knows for this boot.
    #[serde(deserialize_with = "lenient_u64")]
    pub last_id: Option<u64>,
    /// `reset`: `boot_changed` or `id_ahead`.
    pub reason: Option<String>,
}

/// Event kinds of the Agent's event stream.
pub mod event_kinds {
    pub const HELLO: &str = "hello";
    pub const RESET: &str = "reset";
    pub const TUNNEL_UP: &str = "tunnel.up";
    pub const TUNNEL_DOWN: &str = "tunnel.down";
    pub const TUNNEL_RECONNECTING: &str = "tunnel.reconnecting";
    pub const CONN_ALLOW: &str = "conn.allow";
    pub const CONN_DENY: &str = "conn.deny";
    pub const CONFIG_CHANGED: &str = "config.changed";
    pub const UPDATE_SCHEDULED: &str = "update.scheduled";
    pub const KILLED: &str = "killed";
    pub const RESUMED: &str = "resumed";
}

// ---------------------------------------------------------------------------
// Console (public API)
// ---------------------------------------------------------------------------

/// RFC 8628 device authorization response.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

/// `POST /oauth/token` success.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenEndpointResponse {
    /// `dpo_at_...`, valid for one hour.
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    /// `dpo_rt_...`, valid for 30 days, rotated on every use.
    pub refresh_token: String,
    pub scope: String,
}

/// RFC 6749 / RFC 8628 error body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OAuthErrorResponse {
    pub error: String,
    pub error_description: Option<String>,
}

/// Error body of `/api/*`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiErrorResponse {
    pub detail: Option<serde_json::Value>,
}

/// `POST /api/tunnels` (OAuth apps may only create `app`-mode tunnels, on
/// the devices picked when the app was approved).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CreateTunnel {
    /// Agent device that serves the tunnel.
    pub device_id: i64,
    /// Zone (domain) the tunnel name is created in.
    pub zone_id: i64,
    /// Host label: `vca` becomes `vca.<zone>`.
    pub name: String,
    pub local_port: u16,
    /// Always `"app"`.
    pub access_mode: String,
    /// `"http"` or `"https"`.
    pub local_scheme: String,
    /// 443 or 9443.
    pub public_port: u16,
}

/// A tunnel object (`POST /api/tunnels` 201, `GET /api/tunnels/{id}`, items
/// of `GET /api/tunnels`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Tunnel {
    pub id: i64,
    /// `name.zone`.
    pub hostname: String,
    #[serde(deserialize_with = "lenient_i64")]
    pub zone_id: Option<i64>,
    pub name: Option<String>,
    #[serde(deserialize_with = "lenient_i64")]
    pub device_id: Option<i64>,
    /// The serving device (its name, or an object; shown only).
    pub device: Option<serde_json::Value>,
    pub device_status: Option<String>,
    pub local_port: Option<u16>,
    pub local_scheme: Option<String>,
    pub access_mode: Option<String>,
    pub public_port: Option<u16>,
    /// `active`, `suspended` or `disabled`.
    pub status: Option<String>,
    pub suspend_reason: Option<String>,
    pub url: Option<String>,
    pub app_token_allowed: Option<bool>,
    pub created_by: Option<String>,
    pub created_at: Option<String>,
    pub last_conn_at: Option<String>,
}

impl Tunnel {
    pub fn is_app_mode(&self) -> bool {
        self.access_mode.as_deref() == Some(ACCESS_MODE_APP)
    }

    pub fn public_port(&self) -> u16 {
        self.public_port.unwrap_or(443)
    }

    /// The serving device's display name, when the tunnel names one.
    pub fn device_name(&self) -> Option<String> {
        match self.device.as_ref()? {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => o.get("name")?.as_str().map(str::to_owned),
            _ => None,
        }
    }
}

/// `GET /api/tunnels`: `{items, paid, public_allowed, limit, used, quota,
/// public_ports, relay_ready}` (only the fields VCA shows are kept). With an
/// OAuth token the console adds `oauth_allowed_device_ids`: the devices the
/// person picked on the approval screen, the only ones tunnels may use.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TunnelList {
    pub items: Vec<Tunnel>,
    pub paid: Option<bool>,
    #[serde(deserialize_with = "lenient_i64")]
    pub limit: Option<i64>,
    #[serde(deserialize_with = "lenient_i64")]
    pub used: Option<i64>,
    pub relay_ready: Option<bool>,
    pub oauth_allowed_device_ids: Option<Vec<i64>>,
}

impl TunnelList {
    /// Whether the OAuth approval allows tunnels on `device_id`; `true`
    /// when the console does not report the list (older consoles, which
    /// then answer the creation with 403).
    pub fn device_allowed(&self, device_id: i64) -> bool {
        self.oauth_allowed_device_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&device_id))
    }
}

/// An Agent device in `GET /api/agent-devices`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentDevice {
    pub id: i64,
    pub name: Option<String>,
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub agent_version: Option<String>,
    pub flavor: Option<String>,
    /// `active`, `revoked`, ...
    pub status: Option<String>,
    pub online: Option<bool>,
    pub last_seen_at: Option<String>,
    pub tunnel_create_allowed: Option<bool>,
}

impl AgentDevice {
    pub fn is_active(&self) -> bool {
        self.status.as_deref() == Some("active")
    }
}

/// `GET /api/agent-devices`: `{items, limit, used, flavors}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentDeviceList {
    pub items: Vec<AgentDevice>,
}

/// A zone in `GET /api/tunnels/zones`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Zone {
    pub id: i64,
    /// The domain, for example `example.com`.
    pub name: String,
}

/// `GET /api/tunnels/zones`: a list of zones (an `{items}` wrapper is
/// accepted too).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ZoneList {
    Plain(Vec<Zone>),
    Wrapped { items: Vec<Zone> },
}

impl ZoneList {
    pub fn into_vec(self) -> Vec<Zone> {
        match self {
            Self::Plain(v) | Self::Wrapped { items: v } => v,
        }
    }
}

/// An app client (pinned certificate key) of an app-mode tunnel.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppClient {
    pub id: i64,
    pub tunnel_id: Option<i64>,
    pub label: Option<String>,
    pub spki_sha256: Option<String>,
    /// `active` or `revoked`.
    pub status: Option<String>,
    /// `local`, `console`, `oauth` or `invite`.
    pub source: Option<String>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
    pub last_seen_at: Option<String>,
    pub revoked_at: Option<String>,
}

impl AppClient {
    pub fn is_active(&self) -> bool {
        self.status.as_deref() != Some("revoked") && self.revoked_at.is_none()
    }
}

/// `GET /api/tunnels/{id}/app-clients`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppClientList {
    pub items: Vec<AppClient>,
}

/// Stable `detail.code` values of the invite endpoints. Match on these, not
/// on the (translated) message.
pub mod invite_errors {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const INVITE_INVALID: &str = "invite_invalid";
    pub const ENROLL_BAD_SIGNATURE: &str = "enroll_bad_signature";
    pub const ENROLL_BAD_CERT: &str = "enroll_bad_cert";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const TUNNEL_NOT_APP_MODE: &str = "tunnel_not_app_mode";
    pub const CLIENT_LIMIT: &str = "client_limit";
    pub const INVITE_LIMIT: &str = "invite_limit";

    /// Every documented code.
    pub const ALL: &[&str] = &[
        BAD_REQUEST,
        INVITE_INVALID,
        ENROLL_BAD_SIGNATURE,
        ENROLL_BAD_CERT,
        RATE_LIMITED,
        TUNNEL_NOT_APP_MODE,
        CLIENT_LIMIT,
        INVITE_LIMIT,
    ];
}

/// Lifetime of an invite code: DNSPilot's default and limits, in seconds.
pub const INVITE_TTL_DEFAULT_SECS: u32 = 600;
pub const INVITE_TTL_MIN_SECS: u32 = 60;
pub const INVITE_TTL_MAX_SECS: u32 = 3600;

/// `POST /api/tunnels/{id}/app-invites` request.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CreateAppInvite {
    /// At most 64 characters; becomes the app client's label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 60 to 3600 (server default 600).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_sec: Option<u32>,
}

/// An invite as listed by the console. The code itself is never listed.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppInvite {
    pub id: i64,
    pub tunnel_id: Option<i64>,
    pub label: Option<String>,
    /// `active`, `used`, `expired` or `revoked`.
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub used_at: Option<String>,
    /// App client registered with this code.
    pub used_client_id: Option<i64>,
}

impl AppInvite {
    /// Unused and unexpired.
    pub fn is_active(&self) -> bool {
        self.status.as_deref() == Some("active")
    }
}

/// `POST /api/tunnels/{id}/app-invites` response (201). Holds the only copy
/// of the code: `Debug` is redacted and it is not `Serialize`.
#[derive(Debug, Clone, Deserialize)]
pub struct AppInviteCreated {
    #[serde(flatten)]
    pub invite: AppInvite,
    /// `dpt_inv_...`: single use, shown once.
    pub code: Secret,
    pub hostname: String,
    /// Seconds until `expires_at`.
    pub expires_in: u64,
    /// `<console>/enroll#v=1&h=...&t=...&c=...`; contains the code.
    pub enroll_url: Secret,
    #[serde(default)]
    pub note: Option<String>,
}

/// `GET /api/tunnels/{id}/app-invites`: newest first, up to 100, all states.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppInviteList {
    pub items: Vec<AppInvite>,
    pub access_mode: Option<String>,
}

/// `POST /api/public/tunnel-app-enroll` request.
#[derive(Clone, Serialize)]
pub struct AppEnrollRequest {
    /// `dpt_inv_...` from the QR code.
    pub code: Secret,
    /// The client certificate later used for mTLS.
    pub cert_pem: String,
    /// Base64 signature by the certificate's key over
    /// `"dpt-app-enroll:v1:" + code`.
    pub sig: String,
}

impl std::fmt::Debug for AppEnrollRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppEnrollRequest")
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

/// How long the Agent may take to learn a newly registered key, when the
/// console does not say (console 1.27.0 has no `ready_within_sec`).
pub const ENROLL_READY_DEFAULT_SECS: u32 = 30;
/// Upper bound on a wait announced by the console.
pub const ENROLL_READY_MAX_SECS: u32 = 120;

/// `POST /api/public/tunnel-app-enroll` response.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppEnrollResult {
    pub tunnel_id: i64,
    pub hostname: String,
    pub client_id: i64,
    pub spki_sha256: String,
    /// Seconds until the Agent accepts the key: 30 for a new key, 0 when it
    /// was already registered. Absent on older consoles.
    #[serde(default)]
    pub ready_within_sec: Option<u32>,
}

impl AppEnrollResult {
    /// The announced wait, or [`ENROLL_READY_DEFAULT_SECS`].
    pub fn ready_within(&self) -> u32 {
        self.ready_within_sec
            .unwrap_or(ENROLL_READY_DEFAULT_SECS)
            .min(ENROLL_READY_MAX_SECS)
    }
}

/// `PUT /api/tunnels/{id}/app-token-allowed` request.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AppTokenAllowed {
    pub allowed: bool,
}

/// `PUT /api/tunnels/{id}/app-token-allowed` response.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct AppTokenAllowedResponse {
    pub token_allowed: bool,
}

/// `POST /api/tunnels/{id}/app-tokens` request.
#[derive(Debug, Clone, Serialize)]
pub struct IssueAppToken {
    /// 1 to 365 (server default 90).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_days: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// An app token (`dpt_app_...`). `token` is only present, once, in the
/// response that issued it.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppToken {
    pub id: i64,
    pub tunnel_id: Option<i64>,
    pub label: Option<String>,
    pub prefix: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    pub token: Option<Secret>,
}

/// `GET /api/tunnels/{id}/app-tokens`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppTokenList {
    pub items: Vec<AppToken>,
}

/// One relay in `GET /api/public/status` (addresses are never included).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RelayStatus {
    pub host: Option<String>,
    pub region: Option<String>,
    pub role: Option<String>,
    pub status: Option<String>,
    pub active: Option<bool>,
    pub last_check: Option<String>,
}

/// `GET /api/public/status` (no authentication; cached for 60 seconds).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct PublicStatus {
    /// For example `operational`, `degraded` or `unknown`.
    pub tunnel_status: Option<String>,
    pub relays: Vec<RelayStatus>,
    pub updated_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parses_documented_fields_leniently() {
        let s: AgentStatus = serde_json::from_value(serde_json::json!({
            "version": "1.4.0", "logged_in": true, "device_id": 3, "device_name": "Desk",
            "relay": "kr-1", "tunnels_online": 1, "killed": false, "boot_id": "0123456789abcdef",
            "extra": [1]
        }))
        .unwrap();
        assert!(s.logged_in);
        assert_eq!(s.device_id, Some(3));
        assert_eq!(s.tunnels_online, Some(1));
        assert_eq!(s.relay_summary().as_deref(), Some("kr-1"));
        let s: AgentStatus =
            serde_json::from_value(serde_json::json!({"device_id": "7", "relay": ""})).unwrap();
        assert_eq!(s.device_id, Some(7));
        assert_eq!(s.relay_summary(), None);
    }

    #[test]
    fn production_shapes_parse() {
        let t: Tunnel = serde_json::from_value(serde_json::json!({
            "id": 12, "hostname": "vca-a.example.com", "zone_id": 7, "name": "vca-a",
            "device_id": 3, "device": "Desk", "device_status": "online", "local_port": 47800,
            "local_scheme": "http", "access_mode": "app", "public_port": 443, "status": "active",
            "suspend_reason": null, "suspended_by": null, "suspended_at": null,
            "certificate_id": 4, "key_mode": "agent", "url": "https://vca-a.example.com",
            "session_hours": 12, "session_hours_set": false, "app_token_allowed": false,
            "free_host_id": null, "created_by": "me", "created_at": "2026-10-30T00:00:00Z",
            "last_conn_at": null, "month_bytes": 0, "month_conns": 0
        }))
        .unwrap();
        assert!(t.is_app_mode());
        assert_eq!(t.device_name().as_deref(), Some("Desk"));
        let l: LocalTunnelList = serde_json::from_value(serde_json::json!({"tunnels": [
            {"id": 12, "hostname": "vca-a.example.com", "local_port": 47800, "local_scheme": "http",
             "access_mode": "app", "server_status": "active", "paused": false, "cert": "ready",
             "serving": true, "state": "serving", "message": ""}
        ]}))
        .unwrap();
        assert_eq!(l.tunnels[0].public_port(), 443);
        assert_eq!(l.tunnels[0].state_summary(), "serving (certificate ready)");
        let z: ZoneList =
            serde_json::from_value(serde_json::json!([{"id": 7, "name": "example.com"}])).unwrap();
        assert_eq!(z.into_vec()[0].name, "example.com");
        assert!(agent_error_text("killed").is_some());
        assert!(agent_error_text("nope").is_none());
    }

    #[test]
    fn invite_shapes_follow_the_openapi_spec() {
        let code = format!("dpt_inv_{}", "a".repeat(43));
        let created: AppInviteCreated = serde_json::from_value(serde_json::json!({
            "id": 9, "tunnel_id": 12, "label": "VCA phone", "status": "active",
            "created_at": "2026-10-31T00:00:00Z", "created_by": "owner@example.com",
            "expires_at": "2026-10-31T00:10:00Z", "revoked_at": null, "used_at": null,
            "used_client_id": null, "code": code, "hostname": "vca.example.com",
            "expires_in": 600, "note": "shown once",
            "enroll_url": format!("https://dns.dnspilot.kr/enroll#v=1&h=vca.example.com&t=12&c={code}")
        }))
        .unwrap();
        assert_eq!(created.invite.id, 9);
        assert!(created.invite.is_active());
        assert_eq!(created.code.expose(), code);
        let debug = format!("{created:?}");
        assert!(!debug.contains(&code), "{debug}");

        let list: AppInviteList = serde_json::from_value(serde_json::json!({
            "access_mode": "app", "items": [
                {"id": 1, "status": "used", "used_client_id": 41, "tunnel_id": 12, "label": "x"}
            ]
        }))
        .unwrap();
        assert_eq!(list.items[0].used_client_id, Some(41));
        assert!(!list.items[0].is_active());

        let request = AppEnrollRequest {
            code: code.as_str().into(),
            cert_pem: "pem".into(),
            sig: "c2ln".into(),
        };
        assert!(!format!("{request:?}").contains(&code));
        let body = serde_json::to_value(&request).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"code": code, "cert_pem": "pem", "sig": "c2ln"})
        );

        assert_eq!(
            serde_json::to_value(CreateAppInvite {
                label: Some("VCA phone".into()),
                ttl_sec: Some(600)
            })
            .unwrap(),
            serde_json::json!({"label": "VCA phone", "ttl_sec": 600})
        );
    }

    #[test]
    fn create_tunnel_uses_documented_names() {
        let v = serde_json::to_value(CreateTunnel {
            device_id: 3,
            zone_id: 7,
            name: "vca".into(),
            local_port: 47800,
            access_mode: ACCESS_MODE_APP.into(),
            local_scheme: "http".into(),
            public_port: 443,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"device_id": 3, "zone_id": 7, "name": "vca", "local_port": 47800,
                "access_mode": "app", "local_scheme": "http", "public_port": 443})
        );
    }
}
