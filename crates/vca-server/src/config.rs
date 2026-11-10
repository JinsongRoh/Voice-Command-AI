//! Server configuration (`config.toml`) and per-user directories.
//!
//! The configuration file lives in the OS configuration directory
//! (`~/.config/vca/config.toml` on Linux, `~/Library/Application
//! Support/dev.vca.vca/config.toml` on macOS, `%APPDATA%\vca\vca\config\
//! config.toml` on Windows). Every key has a default, so a missing file is
//! valid. Example:
//!
//! ```toml
//! server_name = "workstation"
//! # Launch requests (from the app or remote clients) must target a folder
//! # below one of these roots. Defaults to the home directory.
//! workspace_roots = ["~/code", "~/work"]
//!
//! [shutdown]
//! # "terminate" stops managed agents when vcad exits; "detach" leaves them.
//! managed_sessions = "terminate"
//! grace_secs = 5
//!
//! [discovery]
//! enabled = true
//! interval_secs = 5
//!
//! [sessions]
//! replay_events = 5000
//! replay_bytes = 8388608
//! exited_retention_secs = 600
//! pause_scope = "children_only"   # or "whole_tree"
//! # Development/testing only: allow client `Launch` of arbitrary programs
//! # (AgentKind::Generic). Default false; real programs start via `vca run`.
//! allow_generic_launch = false
//!
//! [resources]
//! push_interval_secs = 2
//!
//! [broker]
//! # policy_path = "/path/to/policies.toml"  (default: <config dir>/policies.toml)
//!
//! # Development only: an unauthenticated-by-device, token-protected
//! # Paired devices: Noise-encrypted WebSocket. Providers (tunnel, VPN, LAN
//! # forwarding) expose this port; it binds loopback by default.
//! [remote]
//! enabled = true
//! bind = "127.0.0.1:47800"
//! require_totp = true
//! # Seconds a pairing QR code stays valid (single use; 10 to 600).
//! # pairing_ttl_secs = 60
//! # URLs written into pairing QR codes (default: from the provider below,
//! # else ws://<bind>/v1/ws).
//! # endpoints = ["wss://vca-k3x9q2.example.net/v1/ws"]
//!
//! # How devices reach the port. No provider is a default; pick one of
//! # "lan", "tailscale", "cloudflare_tunnel", "dnspilot" or "custom".
//! # "lan" and tailscale "direct" need bind = "0.0.0.0:47800"; "dnspilot"
//! # needs the loopback bind.
//! [remote.provider]
//! kind = "none"
//! # url = "wss://vca.example.net"      # kind = "custom"
//! [remote.provider.tailscale]
//! mode = "serve"                       # or "direct"
//! [remote.provider.cloudflare]
//! mode = "quick"                       # or "named"
//! # tunnel = "vca"; hostname = "vca-k3x9q2.example.com"; token_env = "CF_TUNNEL_TOKEN"
//! [remote.provider.dnspilot]
//! # Address of the app-mode tunnel to 127.0.0.1:47800 (created by the VCA
//! # app's "Connect DNSPilot"). Optional: by default vcad finds it in the Agent's tunnel list.
//! # hostname = "vca-k3x9q2ab4m.example.com"
//! public_port = 443                    # or 9443
//! # local_api = "/run/dnspilot/local.sock"   # default per platform
//!
//! # Server-side speech recognition (off by default). Keys come from the
//! # environment variables named here, never from this file.
//! [speech]
//! engine = "off"                 # "openai" or "google"
//! language = "en"                # primary language; omit to auto-detect
//! secondary_languages = ["ko"]
//! [speech.openai]
//! base_url = "https://api.openai.com/v1"   # any OpenAI-compatible server
//! model = "whisper-1"
//! api_key_env = "OPENAI_API_KEY"
//! [speech.google]
//! project_id = "my-project"
//! api_key_env = "GOOGLE_SPEECH_API_KEY"   # or access_token_env
//! # Optional LLM pass (off by default) over any OpenAI-compatible chat
//! # completions endpoint (POST {base_url}/chat/completions). It fixes
//! # misheard words in FINAL dictation chunks only (never in interim
//! # previews) and detects the dictation topic, whose terms are added to the
//! # recognizer's prompt hints. Any failure or timeout keeps the vocabulary-
//! # corrected text.
//! [speech.llm]
//! enabled = false
//! base_url = "https://api.openai.com/v1"
//! model = "gpt-4o-mini"
//! api_key_env = "OPENAI_API_KEY"   # optional for self-hosted servers
//! timeout_ms = 4000
//! max_concurrent = 2
//! topic_every_n_finals = 3       # refresh the topic every N final chunks
//!
//! # Several engines and correction LLMs can be configured as profiles; each
//! # device picks one in the app (Settings > Speech recognition), else the
//! # defaults apply. The single [speech] engine and [speech.llm] above are
//! # still accepted and become profiles "openai"/"google" and "llm"; the
//! # timeout_ms, max_concurrent and topic_every_n_finals of [speech.llm]
//! # apply to every LLM profile. Profile ids use a-z, 0-9, "-" and "_".
//! # Keys: api_key_env, or `vca speech login <id>` (stored on this PC under
//! # the id, or under `credential` to share one key between profiles).
//! default_engine = "whisper-local"
//! default_llm = "none"           # or a profile id
//! [[speech.engines]]
//! id = "whisper-local"
//! label = "Whisper (this PC)"
//! kind = "openai_compatible"     # or "openai", "google"
//! base_url = "http://127.0.0.1:8000/v1"
//! model = "whisper-1"
//! [[speech.engines]]
//! id = "openai"
//! kind = "openai"                # https://api.openai.com/v1 by default
//! model = "gpt-4o-transcribe"
//! api_key_env = "OPENAI_API_KEY"
//! # Behind a gateway or reverse proxy that authenticates with its own
//! # bearer token (OpenAI-compatible engines, LLMs and TTS engines): the
//! # key moves to its own header and the gateway token, stored with
//! # `vca speech gateway-login <id>` (or `vca tts gateway-login <name>`) or
//! # named by gateway_token_env, is sent as `Authorization: Bearer`.
//! [[speech.engines]]
//! id = "team-stt"
//! base_url = "https://engine.example.com/v1"
//! api_key_header = "X-VCA-Key"
//! # gateway_token_env = "TEAM_GATEWAY_TOKEN"
//! [[speech.llms]]
//! id = "local-llm"
//! label = "Local LLM"
//! kind = "openai_compatible"
//! base_url = "http://127.0.0.1:8080/v1"
//! model = "qwen2.5-7b-instruct"
//! [[speech.llms]]
//! id = "claude"
//! kind = "anthropic"             # Messages API, claude-haiku-5-5 by default
//! # api_key_env = "ANTHROPIC_API_KEY", or `vca speech login claude`, or the
//! # user's Anthropic (Console) account through the ant CLI:
//! ant_profile = "default"
//!
//! # Spoken alerts through the owner's own TTS server (off without
//! # engines). VCA ships no voices: point it at any server offering the
//! # OpenAI-compatible POST {base_url}/audio/speech. Devices choose "PC
//! # server" in Settings > Notifications; the device voice is the default.
//! # Keys: api_key_env, or `vca tts login <name>` (stored on this PC).
//! [tts]
//! default_engine = "local-tts"
//! max_audio_bytes = 2097152
//! requests_per_minute = 20      # per device
//! [[tts.engines]]
//! name = "local-tts"
//! kind = "openai_compatible"
//! base_url = "http://127.0.0.1:8880/v1"
//! model = "tts-1"
//! voice = "default"
//! format = "mp3"                 # opus, aac, flac, wav
//! [tts.engines.voices]           # optional voice per language
//! ko = "ko-voice"
//!
//! # Cloud coding agents (see `cloud.rs` for every account setting).
//! # Secrets never go here: use `credential_env` or `vca cloud login`.
//! [[cloud.accounts]]
//! provider = "cursor_cloud"
//! account = "default"
//! credential_env = "CURSOR_API_KEY"
//!
//! # Files and folders sent from the app into a session's project folder
//! # (saved in `.vca/attachments/`, which is added to `.git/info/exclude`).
//! [attachments]
//! enabled = true
//! max_bytes = 26214400           # per file (25 MiB)
//! max_folder_bytes = 104857600   # per folder upload (100 MiB)
//! max_folder_files = 500
//! git_exclude = true
//!
//! # Remote shells: admin devices open a plain shell (PowerShell, cmd,
//! # bash, ...) with a TOTP code. Never elevated.
//! [shell]
//! enabled = true
//! idle_timeout_mins = 15         # no input from any client
//! max_lifetime_mins = 60         # absolute
//! step_up_window_mins = 5        # after a TOTP code, dangerous commands need no new code
//!
//! # Session restore: a snapshot of the AI CLI windows, written only when
//! # something changed. `vca restore` and the app's Windows menu use it.
//! [restore]
//! enabled = true
//! snapshot_interval_secs = 300   # at least 30
//! auto_register = true           # list agent sessions with a terminal window
//! launch_mode = "direct"         # "direct": the window runs the agent itself;
//!                                # "managed": it runs `vca run` (mirrored in the
//!                                # app, but it ends when vcad stops or updates)
//!
//! # WebSocket on 127.0.0.1 for UI development, without device pairing.
//! # Never enable this on a shared machine.
//! [dev_ws]
//! enabled = false
//! port = 47801
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Environment variable overriding the configuration file path.
pub const CONFIG_ENV: &str = "VCA_CONFIG";
/// Environment variable overriding the data directory.
pub const DATA_DIR_ENV: &str = "VCA_DATA_DIR";

/// What happens to managed agents when vcad shuts down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode {
    /// Terminate managed agents (SIGTERM, then SIGKILL after the grace period).
    #[default]
    Terminate,
    /// Leave managed agents running. Their PTY is owned by vcad, so on Unix
    /// they receive SIGHUP when vcad exits unless they ignore it; surviving
    /// a restart needs a separate PTY holder process (not implemented yet).
    Detach,
}

/// Which processes a user-requested pause suspends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseScope {
    /// Only the agent's child processes (builds, tests); avoids API timeouts
    /// of the agent itself (RES-07 default).
    #[default]
    ChildrenOnly,
    WholeTree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShutdownConfig {
    pub managed_sessions: ShutdownMode,
    pub grace_secs: u64,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            managed_sessions: ShutdownMode::Terminate,
            grace_secs: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    /// Detect the hosting terminal (tmux, WezTerm, kitty, iTerm2, Windows
    /// console) of observed sessions to enable tier-3 input.
    pub detect_terminals: bool,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 5,
            detect_terminals: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionsConfig {
    /// Maximum number of events kept per session for replay.
    pub replay_events: usize,
    /// Maximum bytes of terminal output kept per session for replay.
    pub replay_bytes: usize,
    /// How long exited sessions stay listed.
    pub exited_retention_secs: u64,
    pub pause_scope: PauseScope,
    /// Interval of the output-heuristics state check, in milliseconds.
    pub heuristics_interval_ms: u64,
    /// Development/testing only: let client `Launch` requests start
    /// arbitrary programs (`AgentKind::Generic`). Off by default because it
    /// gives a `command`-scoped device a shell without the TOTP step-up.
    pub allow_generic_launch: bool,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            allow_generic_launch: false,
            replay_events: 5000,
            replay_bytes: 8 * 1024 * 1024,
            exited_retention_secs: 600,
            pause_scope: PauseScope::ChildrenOnly,
            heuristics_interval_ms: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourcesConfig {
    pub push_interval_secs: u64,
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        Self {
            push_interval_secs: 2,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrokerSection {
    /// `policies.toml`; defaults to `<config dir>/policies.toml`.
    pub policy_path: Option<PathBuf>,
    /// Broker IPC endpoint for third-party wrappers (socket path, or pipe
    /// name on Windows). Off when unset: VCA's own hooks reach the broker
    /// through vcad's control channel.
    pub socket: Option<PathBuf>,
}

/// The remote transport for paired devices (`vca-net`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    pub enabled: bool,
    /// Listen address. Loopback by default: connectivity providers forward
    /// to it, and every connection is authenticated inside Noise anyway.
    pub bind: String,
    /// Require a TOTP code (or backup code) to sign in after pairing.
    pub require_totp: bool,
    /// Heartbeat interval in seconds (at most 30).
    pub heartbeat_secs: u64,
    /// Seconds a pairing QR code (one-time secret) stays valid. Clamped to
    /// 10..=600; default 60.
    pub pairing_ttl_secs: u64,
    /// Endpoints written into pairing QR codes. Empty: the provider's
    /// endpoint, else `ws://<bind>/v1/ws`.
    pub endpoints: Vec<String>,
    /// How devices reach the listen address.
    pub provider: ProviderConfig,
}

/// Connectivity provider kinds (requirements §3.8). None is a default
/// (CON-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    None,
    Lan,
    Tailscale,
    CloudflareTunnel,
    Dnspilot,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TailscaleKind {
    /// `tailscale serve` publishes the loopback port inside the tailnet.
    #[default]
    Serve,
    /// Devices connect to the tailnet address (needs a non-loopback bind).
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudflareKind {
    /// A temporary `*.trycloudflare.com` tunnel.
    #[default]
    Quick,
    /// A named tunnel with the user's hostname.
    Named,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TailscaleSection {
    pub mode: TailscaleKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CloudflareSection {
    pub mode: CloudflareKind,
    /// Tunnel name or UUID (locally managed named tunnels).
    pub tunnel: Option<String>,
    /// Public hostname of a named tunnel. Use a non-identifying label:
    /// hostnames appear in Certificate Transparency logs.
    pub hostname: Option<String>,
    /// Environment variable holding a tunnel token (remotely managed).
    pub token_env: Option<String>,
    /// `cloudflared` location; `PATH` by default.
    pub program: Option<PathBuf>,
}

/// DNSPilot (`kind = "dnspilot"`): an app-mode tunnel served by the
/// DNSPilot Agent on this PC forwards to the loopback listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DnspilotSection {
    /// Public hostname of the app-mode tunnel (`name.zone`). Optional: when
    /// unset, vcad uses the Agent's only app-mode tunnel to its port. Use a random,
    /// non-identifying name: hostnames appear in Certificate Transparency
    /// logs.
    pub hostname: Option<String>,
    /// Public port of the tunnel: 443 or 9443.
    pub public_port: u16,
    /// Agent local API location (a Unix socket path or a named pipe name).
    /// Default: `/run/dnspilot/local.sock`, or the `dnspilot-local` named
    /// pipe on Windows.
    pub local_api: Option<String>,
}

impl Default for DnspilotSection {
    fn default() -> Self {
        Self {
            hostname: None,
            public_port: 443,
            local_api: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Base URL for `kind = "custom"`.
    pub url: Option<String>,
    pub tailscale: TailscaleSection,
    pub cloudflare: CloudflareSection,
    pub dnspilot: DnspilotSection,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: vca_net::DEFAULT_BIND_ADDR.into(),
            require_totp: true,
            heartbeat_secs: 20,
            pairing_ttl_secs: vca_net::DEFAULT_PAIRING_TTL.as_secs(),
            endpoints: Vec::new(),
            provider: ProviderConfig::default(),
        }
    }
}

/// Which speech-to-text engine the server runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechEngineKind {
    /// No server-side recognition; devices transcribe on their own.
    #[default]
    Off,
    /// An OpenAI-compatible transcription API (OpenAI or self-hosted).
    Openai,
    /// Google Cloud Speech-to-Text v2.
    Google,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiSpeechConfig {
    pub base_url: String,
    pub model: String,
    /// Environment variable holding the API key (optional for self-hosted
    /// servers).
    pub api_key_env: Option<String>,
    pub timeout_secs: u64,
}

impl Default for OpenAiSpeechConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            model: "whisper-1".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            timeout_secs: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoogleSpeechConfig {
    pub project_id: String,
    pub location: String,
    pub model: String,
    /// API root override (regional endpoints).
    pub base_url: Option<String>,
    /// Environment variable holding an API key.
    pub api_key_env: Option<String>,
    /// Environment variable holding an OAuth access token (used when no API
    /// key is set).
    pub access_token_env: Option<String>,
}

impl Default for GoogleSpeechConfig {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            location: "global".into(),
            model: "long".into(),
            base_url: None,
            api_key_env: Some("GOOGLE_SPEECH_API_KEY".into()),
            access_token_env: None,
        }
    }
}

/// Optional LLM pass over transcripts (`[speech.llm]`): contextual
/// correction of misheard words in final chunks and topic detection, through
/// any OpenAI-compatible chat completions endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmSpeechConfig {
    /// Off by default.
    pub enabled: bool,
    /// API root; requests go to `{base_url}/chat/completions`.
    pub base_url: String,
    pub model: String,
    /// Environment variable holding the API key (optional for self-hosted
    /// servers).
    pub api_key_env: Option<String>,
    /// Longest wait for one LLM answer; the vocabulary-corrected text is
    /// returned when it runs out.
    pub timeout_ms: u64,
    /// LLM requests running at the same time.
    pub max_concurrent: usize,
    /// Refresh the topic every this many final chunks (and whenever the
    /// dictation has grown a lot since the last refresh).
    pub topic_every_n_finals: u32,
}

impl Default for LlmSpeechConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            timeout_ms: 4000,
            max_concurrent: 2,
            topic_every_n_finals: 3,
        }
    }
}

/// API of a speech engine profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineProfileKind {
    /// Any OpenAI-compatible transcription server; the key is optional.
    #[default]
    OpenaiCompatible,
    /// The OpenAI API (needs a key).
    Openai,
    /// Google Cloud Speech-to-Text v2.
    Google,
}

impl EngineProfileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai_compatible",
            Self::Openai => "openai",
            Self::Google => "google",
        }
    }
}

/// One `[[speech.engines]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpeechEngineProfile {
    /// Unique among engines and LLMs: `a-z`, `0-9`, `-`, `_`.
    pub id: String,
    /// Shown in the app; the id when unset.
    pub label: Option<String>,
    pub kind: EngineProfileKind,
    /// API root. Required for `openai_compatible`.
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Environment variable holding the API key.
    pub api_key_env: Option<String>,
    /// Credential-store entry (`vca speech login <name>`); the id when unset.
    pub credential: Option<String>,
    pub timeout_secs: Option<u64>,
    /// Google only.
    pub project_id: Option<String>,
    /// Google only (`global` by default).
    pub location: Option<String>,
    /// Google only: environment variable holding an OAuth access token.
    pub access_token_env: Option<String>,
    /// Header carrying the API key, sent raw, instead of `Authorization:
    /// Bearer <key>` (e.g. `X-VCA-Key`). Needed behind a gateway that
    /// authenticates with its own bearer token. OpenAI-compatible APIs only.
    pub api_key_header: Option<String>,
    /// Environment variable holding the gateway token, sent as
    /// `Authorization: Bearer <token>`; else the token stored with
    /// `vca speech gateway-login`. Needs `api_key_header`.
    pub gateway_token_env: Option<String>,
    /// The key may go over plain http to this private network address. Set
    /// in the app, which asks for local verification.
    pub allow_plain_http: bool,
}

/// API of a correction LLM profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProfileKind {
    /// Any OpenAI-compatible chat completions endpoint.
    #[default]
    OpenaiCompatible,
    /// The Anthropic Messages API.
    Anthropic,
}

impl LlmProfileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai_compatible",
            Self::Anthropic => "anthropic",
        }
    }
}

/// One `[[speech.llms]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmProfile {
    /// Unique among engines and LLMs: `a-z`, `0-9`, `-`, `_`.
    pub id: String,
    pub label: Option<String>,
    pub kind: LlmProfileKind,
    /// API root: `{base_url}/chat/completions` (OpenAI-compatible, OpenAI's
    /// API by default) or `{base_url}/v1/messages` (Anthropic,
    /// `https://api.anthropic.com` by default).
    pub base_url: Option<String>,
    /// `gpt-4o-mini` (OpenAI-compatible) or `claude-haiku-5-5` (Anthropic)
    /// by default.
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    /// Credential-store entry (`vca speech login <name>`); the id when unset.
    pub credential: Option<String>,
    /// Anthropic only: use the user's Anthropic (Console) account through
    /// `ant auth print-credentials` (`default`, or an `ant` profile name).
    /// Used when no API key is found.
    pub ant_profile: Option<String>,
    /// Overrides `[speech.llm] timeout_ms`.
    pub timeout_ms: Option<u64>,
    /// Header carrying the API key, sent raw, instead of `Authorization:
    /// Bearer <key>` (e.g. `X-VCA-Key`). Needed behind a gateway that
    /// authenticates with its own bearer token. OpenAI-compatible APIs only.
    pub api_key_header: Option<String>,
    /// Environment variable holding the gateway token, sent as
    /// `Authorization: Bearer <token>`; else the token stored with
    /// `vca speech gateway-login`. Needs `api_key_header`.
    pub gateway_token_env: Option<String>,
    /// The key may go over plain http to this private network address. Set
    /// in the app, which asks for local verification.
    pub allow_plain_http: bool,
}

/// Default and legacy profile ids.
pub const LEGACY_LLM_ID: &str = "llm";

/// Profile ids: what the credential store accepts as names.
pub fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id != vca_core::SPEECH_LLM_NONE
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Checks the key header and gateway token settings of one engine.
/// `what`: "speech engine \"x\"" etc.; `openai_like`: the engine speaks an
/// OpenAI-compatible API (the only kind these settings apply to).
pub fn check_gateway_settings(
    what: &str,
    openai_like: bool,
    api_key_header: Option<&str>,
    gateway_token_env: Option<&str>,
) -> Result<(), String> {
    let header = api_key_header.map(str::trim).filter(|h| !h.is_empty());
    let gateway = gateway_token_env.map(str::trim).filter(|g| !g.is_empty());
    if !openai_like && (header.is_some() || gateway.is_some()) {
        return Err(format!(
            "{what}: api_key_header and gateway tokens apply to OpenAI-compatible engines only"
        ));
    }
    if let Some(h) = header {
        vca_speech::auth::check_key_header(h).map_err(|e| format!("{what}: {e}"))?;
    }
    if gateway.is_some() && header.is_none_or(vca_speech::auth::is_authorization) {
        return Err(format!(
            "{what}: {}",
            vca_speech::auth::GATEWAY_NEEDS_KEY_HEADER
        ));
    }
    Ok(())
}

/// Server-side speech recognition (VOC-01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpeechConfig {
    pub engine: SpeechEngineKind,
    /// Primary language (BCP-47); `None` lets the engine detect it.
    pub language: Option<String>,
    pub secondary_languages: Vec<String>,
    /// Base lexicon directory; the built-in lexicon when unset.
    pub lexicon_dir: Option<PathBuf>,
    /// Scan the selected session's folder for project terms.
    pub project_vocabulary: bool,
    pub remove_fillers: bool,
    /// Largest accepted utterance.
    pub max_audio_bytes: usize,
    /// Recognitions running at the same time.
    pub max_concurrent: usize,
    pub openai: OpenAiSpeechConfig,
    pub google: GoogleSpeechConfig,
    /// Optional LLM correction and topic detection.
    pub llm: LlmSpeechConfig,
    /// Engine profiles.
    pub engines: Vec<SpeechEngineProfile>,
    /// Correction LLM profiles.
    pub llms: Vec<LlmProfile>,
    /// Engine used by devices without a preference; the first profile when
    /// unset.
    pub default_engine: Option<String>,
    /// LLM used by devices without a preference; `"none"` (or unset without
    /// an enabled `[speech.llm]`) means no correction.
    pub default_llm: Option<String>,
}

impl SpeechConfig {
    /// Every engine profile: the legacy `[speech] engine` first (id
    /// `openai` or `google`), then `[[speech.engines]]`.
    pub fn engine_profiles(&self) -> Vec<SpeechEngineProfile> {
        let legacy = match self.engine {
            SpeechEngineKind::Off => None,
            SpeechEngineKind::Openai => Some(SpeechEngineProfile {
                id: "openai".into(),
                label: Some("OpenAI-compatible".into()),
                kind: EngineProfileKind::OpenaiCompatible,
                base_url: Some(self.openai.base_url.clone()),
                model: Some(self.openai.model.clone()),
                api_key_env: self.openai.api_key_env.clone(),
                timeout_secs: Some(self.openai.timeout_secs),
                ..SpeechEngineProfile::default()
            }),
            SpeechEngineKind::Google => Some(SpeechEngineProfile {
                id: "google".into(),
                label: Some("Google Cloud".into()),
                kind: EngineProfileKind::Google,
                base_url: self.google.base_url.clone(),
                model: Some(self.google.model.clone()),
                api_key_env: self.google.api_key_env.clone(),
                project_id: Some(self.google.project_id.clone()),
                location: Some(self.google.location.clone()),
                access_token_env: self.google.access_token_env.clone(),
                ..SpeechEngineProfile::default()
            }),
        };
        legacy
            .into_iter()
            .chain(self.engines.iter().cloned())
            .collect()
    }

    /// Every LLM profile: the legacy `[speech.llm]` first (id `llm`, when
    /// enabled), then `[[speech.llms]]`.
    pub fn llm_profiles(&self) -> Vec<LlmProfile> {
        let legacy = self.llm.enabled.then(|| LlmProfile {
            id: LEGACY_LLM_ID.into(),
            label: Some("LLM".into()),
            kind: LlmProfileKind::OpenaiCompatible,
            base_url: Some(self.llm.base_url.clone()),
            model: Some(self.llm.model.clone()),
            api_key_env: self.llm.api_key_env.clone(),
            ..LlmProfile::default()
        });
        legacy
            .into_iter()
            .chain(self.llms.iter().cloned())
            .collect()
    }

    /// The default engine profile id.
    pub fn default_engine_id(&self) -> Option<String> {
        self.default_engine
            .clone()
            .filter(|d| d != vca_core::SPEECH_LLM_NONE)
            .or_else(|| self.engine_profiles().first().map(|p| p.id.clone()))
    }

    /// The default LLM profile id; `None` means no correction.
    pub fn default_llm_id(&self) -> Option<String> {
        match self.default_llm.as_deref() {
            Some(vca_core::SPEECH_LLM_NONE) => None,
            Some(id) => Some(id.to_owned()),
            None => self.llm.enabled.then(|| LEGACY_LLM_ID.to_owned()),
        }
    }

    /// Checks profile ids and defaults.
    pub fn validate(&self) -> Result<(), String> {
        let engines = self.engine_profiles();
        let llms = self.llm_profiles();
        let mut seen: Vec<&str> = Vec::new();
        let ids = engines
            .iter()
            .map(|p| (p.id.as_str(), p.credential.as_deref()))
            .chain(
                llms.iter()
                    .map(|p| (p.id.as_str(), p.credential.as_deref())),
            );
        for (id, credential) in ids {
            if !valid_profile_id(id) {
                return Err(format!(
                    "speech profile id {id:?} must be 1-64 characters of a-z, 0-9, - and _ \
                     (and not \"none\")"
                ));
            }
            if seen.contains(&id) {
                return Err(format!(
                    "speech profile id {id:?} is used twice (ids are shared by engines and LLMs; \
                     the legacy [speech] engine is \"openai\"/\"google\" and [speech.llm] is \"llm\")"
                ));
            }
            if let Some(c) = credential
                && !valid_profile_id(c)
            {
                return Err(format!(
                    "speech profile {id:?}: invalid credential name {c:?}"
                ));
            }
            seen.push(id);
        }
        for p in &engines {
            if p.kind == EngineProfileKind::OpenaiCompatible
                && p.base_url.as_deref().is_none_or(|u| u.trim().is_empty())
            {
                return Err(format!("speech engine {:?} needs a base_url", p.id));
            }
            check_gateway_settings(
                &format!("speech engine {:?}", p.id),
                p.kind != EngineProfileKind::Google,
                p.api_key_header.as_deref(),
                p.gateway_token_env.as_deref(),
            )?;
        }
        for p in &llms {
            check_gateway_settings(
                &format!("speech LLM {:?}", p.id),
                p.kind == LlmProfileKind::OpenaiCompatible,
                p.api_key_header.as_deref(),
                p.gateway_token_env.as_deref(),
            )?;
        }
        if let Some(d) = self.default_engine.as_deref()
            && d != vca_core::SPEECH_LLM_NONE
            && !engines.iter().any(|p| p.id == d)
        {
            return Err(format!(
                "default_engine {d:?} is not a [[speech.engines]] id"
            ));
        }
        if let Some(d) = self.default_llm.as_deref()
            && d != vca_core::SPEECH_LLM_NONE
            && !llms.iter().any(|p| p.id == d)
        {
            return Err(format!("default_llm {d:?} is not a [[speech.llms]] id"));
        }
        Ok(())
    }
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            engine: SpeechEngineKind::Off,
            language: None,
            secondary_languages: Vec::new(),
            lexicon_dir: None,
            project_vocabulary: true,
            remove_fillers: true,
            max_audio_bytes: 8 * 1024 * 1024,
            max_concurrent: 2,
            openai: OpenAiSpeechConfig::default(),
            google: GoogleSpeechConfig::default(),
            llm: LlmSpeechConfig::default(),
            engines: Vec::new(),
            llms: Vec::new(),
            default_engine: None,
            default_llm: None,
        }
    }
}

/// API of a TTS engine profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsEngineKind {
    /// `POST {base_url}/audio/speech` with an OpenAI-style JSON body.
    #[default]
    OpenaiCompatible,
}

impl TtsEngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai_compatible",
        }
    }
}

/// One `[[tts.engines]]` entry: the owner's own TTS server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsEngineProfile {
    /// Unique among TTS engines: `a-z`, `0-9`, `-`, `_`.
    pub name: String,
    pub kind: TtsEngineKind,
    /// API root, e.g. `http://127.0.0.1:8880/v1`. Required.
    pub base_url: Option<String>,
    /// `tts-1` when unset.
    pub model: Option<String>,
    /// Default voice; `alloy` when unset.
    pub voice: Option<String>,
    /// Voice per language (`ko = "..."`, `en-US = "..."`); a request's
    /// language, then its base language, then `voice`.
    pub voices: std::collections::BTreeMap<String, String>,
    /// Speakers a user may pick (`id` is sent as the engine's `voice`).
    /// Without any, clients get no picker and `voice` / `voices` apply.
    pub speakers: Vec<vca_core::TtsSpeaker>,
    /// Shown to users where the engine is offered, for engines whose licence
    /// requires telling people the audio is machine-generated.
    pub disclosure: Option<String>,
    /// `mp3` (default), `opus`, `aac`, `flac` or `wav`.
    pub format: Option<String>,
    /// Environment variable holding the API key.
    pub api_key_env: Option<String>,
    /// Credential-store entry (`vca tts login <name>`); the name when unset.
    pub credential: Option<String>,
    pub timeout_secs: Option<u64>,
    /// Header carrying the API key, sent raw, instead of `Authorization:
    /// Bearer <key>` (e.g. `X-VCA-Key`). Needed behind a gateway that
    /// authenticates with its own bearer token. OpenAI-compatible APIs only.
    pub api_key_header: Option<String>,
    /// Environment variable holding the gateway token, sent as
    /// `Authorization: Bearer <token>`; else the token stored with
    /// `vca tts gateway-login`. Needs `api_key_header`.
    pub gateway_token_env: Option<String>,
    /// The key may go over plain http to this private network address. Set
    /// in the app, which asks for local verification.
    pub allow_plain_http: bool,
}

/// Spoken alerts synthesized on this PC (`[tts]`). Off without engines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    /// Engine used when a request names none; the first engine when unset.
    pub default_engine: Option<String>,
    /// Largest audio answer accepted from an engine.
    pub max_audio_bytes: usize,
    /// Requests per device per minute.
    pub requests_per_minute: u32,
    /// Syntheses running at the same time.
    pub max_concurrent: usize,
    pub engines: Vec<TtsEngineProfile>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            default_engine: None,
            max_audio_bytes: 2 * 1024 * 1024,
            requests_per_minute: 20,
            max_concurrent: 2,
            engines: Vec::new(),
        }
    }
}

impl TtsConfig {
    /// The default engine name.
    pub fn default_engine_name(&self) -> Option<String> {
        self.default_engine
            .clone()
            .or_else(|| self.engines.first().map(|e| e.name.clone()))
    }

    /// Checks names, URLs, formats and the default.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for e in &self.engines {
            let name = e.name.as_str();
            if !valid_profile_id(name) {
                return Err(format!(
                    "tts engine name {name:?} must be 1-64 characters of a-z, 0-9, - and _ \
                     (and not \"none\")"
                ));
            }
            if seen.contains(&name) {
                return Err(format!("tts engine name {name:?} is used twice"));
            }
            seen.push(name);
            if let Some(c) = e.credential.as_deref()
                && !valid_profile_id(c)
            {
                return Err(format!(
                    "tts engine {name:?}: invalid credential name {c:?}"
                ));
            }
            check_gateway_settings(
                &format!("tts engine {name:?}"),
                true,
                e.api_key_header.as_deref(),
                e.gateway_token_env.as_deref(),
            )?;
            let url = e.base_url.as_deref().map(str::trim).unwrap_or_default();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(format!(
                    "tts engine {name:?} needs a base_url starting with http:// or https://"
                ));
            }
            let mut ids: Vec<&str> = Vec::new();
            for sp in &e.speakers {
                let id = sp.id.as_str();
                if id.is_empty()
                    || id.chars().count() > 64
                    || !id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
                {
                    return Err(format!(
                        "tts engine {name:?}: speaker id {id:?} must be 1-64 characters of \
                         a-z, A-Z, 0-9, -, _, . and :"
                    ));
                }
                if ids.contains(&id) {
                    return Err(format!(
                        "tts engine {name:?}: speaker id {id:?} is used twice"
                    ));
                }
                ids.push(id);
                let label = sp.label.trim();
                if label.is_empty()
                    || label.chars().count() > 64
                    || label.chars().any(char::is_control)
                {
                    return Err(format!(
                        "tts engine {name:?}: speaker {id:?} needs a label of 1-64 characters"
                    ));
                }
            }
            if let Some(d) = e.disclosure.as_deref()
                && (d.trim().is_empty() || d.chars().count() > 300)
            {
                return Err(format!(
                    "tts engine {name:?}: disclosure must be 1-300 characters"
                ));
            }
            if let Some(f) = e.format.as_deref()
                && vca_speech::TtsFormat::parse(f).is_none()
            {
                return Err(format!(
                    "tts engine {name:?}: format {f:?} is not one of mp3, opus, aac, flac, wav"
                ));
            }
        }
        if let Some(d) = self.default_engine.as_deref()
            && !self.engines.iter().any(|e| e.name == d)
        {
            return Err(format!(
                "[tts] default_engine {d:?} is not a [[tts.engines]] name"
            ));
        }
        if self.requests_per_minute == 0 {
            return Err("[tts] requests_per_minute must be at least 1".into());
        }
        Ok(())
    }
}

/// Session restore (`[restore]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RestoreConfig {
    /// `false` stops the timed snapshot; the list still works.
    pub enabled: bool,
    pub snapshot_interval_secs: u64,
    /// List running agent sessions with a terminal window on their own.
    pub auto_register: bool,
    /// What a reopened window runs.
    pub launch_mode: RestoreLaunchMode,
}

/// What a window opened by restore runs (`[restore] launch_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreLaunchMode {
    /// `vca run --name <title> -- <agent> <args>`: a session managed by
    /// vcad, attached to the window, which the app can mirror and type into
    /// without console injection. Falls back to `direct` when `vca` is not
    /// installed next to vcad. The session ends when vcad stops, so an
    /// update or restart of VCA would end every such agent; opt in only.
    Managed,
    /// The agent itself; VCA only observes it. The default: agents keep
    /// running across VCA restarts and updates.
    #[default]
    Direct,
}

/// Shortest time between two restore snapshots.
pub const MIN_RESTORE_SNAPSHOT_SECS: u64 = 30;

impl Default for RestoreConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            snapshot_interval_secs: 300,
            auto_register: true,
            launch_mode: RestoreLaunchMode::Direct,
        }
    }
}

impl RestoreConfig {
    pub fn snapshot_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.snapshot_interval_secs.max(MIN_RESTORE_SNAPSHOT_SECS))
    }
}

/// Remote shell sessions (`[shell]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShellConfig {
    /// `false` refuses `OpenShell` and `ShellRun`.
    pub enabled: bool,
    /// A shell without input from any client for this long is stopped.
    pub idle_timeout_mins: u64,
    /// A shell is stopped this long after it was opened.
    pub max_lifetime_mins: u64,
    /// After a successful TOTP code, `dangerous` commands of the same device
    /// need no new code for this long.
    pub step_up_window_mins: u64,
    /// Second-precision overrides of the three durations above (testing).
    pub idle_timeout_secs: Option<u64>,
    pub max_lifetime_secs: Option<u64>,
    pub step_up_window_secs: Option<u64>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_timeout_mins: 15,
            max_lifetime_mins: 60,
            step_up_window_mins: 5,
            idle_timeout_secs: None,
            max_lifetime_secs: None,
            step_up_window_secs: None,
        }
    }
}

impl ShellConfig {
    fn secs(over: Option<u64>, mins: u64) -> std::time::Duration {
        std::time::Duration::from_secs(over.unwrap_or(mins.saturating_mul(60)).max(1))
    }

    pub fn idle_timeout(&self) -> std::time::Duration {
        Self::secs(self.idle_timeout_secs, self.idle_timeout_mins)
    }

    pub fn max_lifetime(&self) -> std::time::Duration {
        Self::secs(self.max_lifetime_secs, self.max_lifetime_mins)
    }

    pub fn step_up_window(&self) -> std::time::Duration {
        Self::secs(self.step_up_window_secs, self.step_up_window_mins)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevWsConfig {
    /// Off by default. Development only.
    pub enabled: bool,
    /// Port on 127.0.0.1; `0` picks a free port.
    pub port: u16,
}

impl Default for DevWsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 47801,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Name shown to clients; defaults to the host name.
    pub server_name: Option<String>,
    /// Allowed project roots for `Launch` (SEC-05). `~` is expanded.
    /// Empty means the home directory.
    pub workspace_roots: Vec<PathBuf>,
    /// Local control endpoint override (socket path, or pipe name on Windows).
    pub control_socket: Option<PathBuf>,
    pub shutdown: ShutdownConfig,
    pub discovery: DiscoveryConfig,
    pub sessions: SessionsConfig,
    pub resources: ResourcesConfig,
    pub broker: BrokerSection,
    pub remote: RemoteConfig,
    pub speech: SpeechConfig,
    /// Spoken alerts synthesized by the owner's own TTS engines.
    pub tts: TtsConfig,
    pub dev_ws: DevWsConfig,
    /// Cloud coding agent accounts.
    pub cloud: crate::cloud::CloudConfig,
    /// Files and folders sent from clients into sessions.
    pub attachments: crate::attachments::AttachmentsConfig,
    /// Remote shell sessions.
    pub shell: ShellConfig,
    /// Session restore.
    pub restore: RestoreConfig,
    /// Log filter in `tracing_subscriber::EnvFilter` syntax; `RUST_LOG` wins.
    pub log: Option<String>,
}

/// Errors from loading the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration in {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

impl Config {
    /// Parses TOML text.
    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Loads `path`; a missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml_str(&text).map_err(|e| ConfigError::Parse {
                path: path.to_owned(),
                message: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// The server name: configured, or the host name, or `vca`.
    pub fn server_name(&self) -> String {
        self.server_name
            .clone()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|s| s.trim().to_owned())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "vca".into())
    }

    /// Workspace roots with `~` expanded; the home directory when empty.
    pub fn resolved_workspace_roots(&self) -> Vec<PathBuf> {
        if self.workspace_roots.is_empty() {
            return home_dir().into_iter().collect();
        }
        self.workspace_roots
            .iter()
            .map(|p| expand_tilde(p))
            .collect()
    }
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_owned())
}

/// Expands a leading `~` to the home directory.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_owned();
    };
    match home_dir() {
        Some(home) => home.join(rest),
        None => path.to_owned(),
    }
}

/// Per-user directories used by vcad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Directory of `config.toml` and `policies.toml`.
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    /// Persistent data (profiles, history).
    pub data_dir: PathBuf,
    /// State that may be lost (logs).
    pub state_dir: PathBuf,
}

impl Paths {
    /// The OS default directories, honouring `VCA_CONFIG` and `VCA_DATA_DIR`.
    pub fn from_env() -> Self {
        let dirs = directories::ProjectDirs::from("dev", "vca", "vca");
        let (config_dir, data_dir, state_dir) = match &dirs {
            Some(d) => (
                d.config_dir().to_owned(),
                d.data_dir().to_owned(),
                d.state_dir()
                    .map(Path::to_owned)
                    .unwrap_or_else(|| d.data_local_dir().join("state")),
            ),
            None => {
                let base = std::env::temp_dir().join("vca");
                (base.join("config"), base.join("data"), base.join("state"))
            }
        };
        let config_file = std::env::var_os(CONFIG_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir.join("config.toml"));
        let config_dir = config_file
            .parent()
            .map(Path::to_owned)
            .unwrap_or(config_dir);
        let data_dir = std::env::var_os(DATA_DIR_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or(data_dir);
        Self {
            config_dir,
            config_file,
            data_dir,
            state_dir,
        }
    }

    /// All directories below one base directory (for tests and portable use).
    pub fn under(base: &Path) -> Self {
        Self {
            config_dir: base.join("config"),
            config_file: base.join("config").join("config.toml"),
            data_dir: base.join("data"),
            state_dir: base.join("state"),
        }
    }

    pub fn policy_file(&self) -> PathBuf {
        self.config_dir.join("policies.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg, Config::default());
        assert!(!cfg.dev_ws.enabled);
        assert_eq!(cfg.shutdown.managed_sessions, ShutdownMode::Terminate);
    }

    #[test]
    fn parses_all_sections() {
        let cfg = Config::from_toml_str(
            r#"
            server_name = "box"
            workspace_roots = ["/srv/code"]
            [shutdown]
            managed_sessions = "detach"
            [sessions]
            pause_scope = "whole_tree"
            replay_events = 10
            [dev_ws]
            enabled = true
            port = 0
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server_name(), "box");
        assert_eq!(cfg.shutdown.managed_sessions, ShutdownMode::Detach);
        assert_eq!(cfg.sessions.pause_scope, PauseScope::WholeTree);
        assert_eq!(cfg.sessions.replay_events, 10);
        assert!(cfg.dev_ws.enabled);
        assert_eq!(
            cfg.resolved_workspace_roots(),
            vec![PathBuf::from("/srv/code")]
        );
    }

    #[test]
    fn shell_section_defaults_and_overrides() {
        use std::time::Duration;
        let cfg = Config::default();
        assert!(cfg.shell.enabled);
        assert_eq!(cfg.shell.idle_timeout(), Duration::from_secs(15 * 60));
        assert_eq!(cfg.shell.max_lifetime(), Duration::from_secs(60 * 60));
        assert_eq!(cfg.shell.step_up_window(), Duration::from_secs(5 * 60));
        let cfg = Config::from_toml_str(
            r#"
            [shell]
            enabled = false
            idle_timeout_mins = 5
            max_lifetime_mins = 30
            step_up_window_secs = 2
            "#,
        )
        .unwrap();
        assert!(!cfg.shell.enabled);
        assert_eq!(cfg.shell.idle_timeout(), Duration::from_secs(300));
        assert_eq!(cfg.shell.max_lifetime(), Duration::from_secs(1800));
        assert_eq!(cfg.shell.step_up_window(), Duration::from_secs(2));
        assert!(
            Config::from_toml_str(
                "[shell]
unknown = 1"
            )
            .is_err()
        );
    }

    #[test]
    fn tts_engines_parse_and_are_validated() {
        let cfg = Config::default();
        assert!(cfg.tts.engines.is_empty());
        assert_eq!(cfg.tts.default_engine_name(), None);
        let cfg = Config::from_toml_str(
            r#"
            [tts]
            default_engine = "kokoro"
            [[tts.engines]]
            name = "melo"
            base_url = "http://127.0.0.1:8888/v1"
            [[tts.engines]]
            name = "kokoro"
            kind = "openai_compatible"
            base_url = "http://127.0.0.1:8880/v1"
            model = "kokoro"
            voice = "af_sky"
            format = "opus"
            credential = "shared-tts"
            disclosure = "Machine-generated voice"
            [[tts.engines.speakers]]
            id = "F1"
            label = "Female 1"
            gender = "female"
            [[tts.engines.speakers]]
            id = "M1"
            label = "Male 1"
            gender = "male"
            [tts.engines.voices]
            ko = "ko_voice"
            "#,
        )
        .unwrap();
        cfg.tts.validate().unwrap();
        assert!(cfg.tts.engines[0].speakers.is_empty());
        assert_eq!(cfg.tts.engines[1].speakers.len(), 2);
        assert_eq!(
            cfg.tts.engines[1].speakers[1].gender,
            vca_core::TtsGender::Male
        );
        assert_eq!(
            cfg.tts.engines[1].disclosure.as_deref(),
            Some("Machine-generated voice")
        );
        assert_eq!(cfg.tts.default_engine_name().as_deref(), Some("kokoro"));
        assert_eq!(cfg.tts.engines[1].voices["ko"], "ko_voice");
        assert_eq!(cfg.tts.requests_per_minute, 20);

        let bad = |edit: fn(&mut TtsConfig)| {
            let mut t = cfg.tts.clone();
            edit(&mut t);
            t.validate().unwrap_err()
        };
        assert!(bad(|t| t.engines[0].name = "Bad Name".into()).contains("name"));
        assert!(bad(|t| t.engines[1].name = "melo".into()).contains("twice"));
        assert!(bad(|t| t.engines[0].base_url = None).contains("base_url"));
        assert!(bad(|t| t.engines[0].format = Some("pcm".into())).contains("format"));
        assert!(bad(|t| t.default_engine = Some("x".into())).contains("default_engine"));
        assert!(bad(|t| t.engines[1].credential = Some("A B".into())).contains("credential"));
        assert!(bad(|t| t.engines[1].speakers[1].id = "F1".into()).contains("twice"));
        assert!(bad(|t| t.engines[1].speakers[0].id = "a b".into()).contains("speaker id"));
        assert!(bad(|t| t.engines[1].speakers[0].label = " ".into()).contains("label"));
        assert!(bad(|t| t.engines[1].disclosure = Some(" ".into())).contains("disclosure"));
        assert!(
            Config::from_toml_str(
                "[[tts.engines]]\nname = \"x\"\nbase_url = \"http://h/v1\"\n\
                 [[tts.engines.speakers]]\nid = \"F1\"\nlabel = \"F\"\ngender = \"robot\""
            )
            .is_err()
        );
        // Keys never go in the file.
        assert!(
            Config::from_toml_str(
                "[[tts.engines]]\nname = \"x\"\nbase_url = \"http://h/v1\"\napi_key = \"sk\""
            )
            .is_err()
        );
    }

    #[test]
    fn speech_llm_is_off_by_default_and_parses() {
        assert!(!Config::default().speech.llm.enabled);
        let cfg = Config::from_toml_str(
            r#"
            [speech]
            engine = "openai"
            [speech.llm]
            enabled = true
            base_url = "http://localhost:8000/v1"
            model = "small"
            api_key_env = "VCA_LLM_KEY"
            timeout_ms = 2500
            max_concurrent = 1
            topic_every_n_finals = 5
            "#,
        )
        .unwrap();
        let llm = &cfg.speech.llm;
        assert!(llm.enabled);
        assert_eq!(llm.base_url, "http://localhost:8000/v1");
        assert_eq!(llm.model, "small");
        assert_eq!(llm.api_key_env.as_deref(), Some("VCA_LLM_KEY"));
        assert_eq!(
            (llm.timeout_ms, llm.max_concurrent, llm.topic_every_n_finals),
            (2500, 1, 5)
        );
        assert!(Config::from_toml_str("[speech.llm]\nkey = \"x\"").is_err());
    }

    #[test]
    fn speech_profiles_with_legacy_entries() {
        let cfg = Config::from_toml_str(
            r#"
            [speech]
            engine = "openai"
            default_llm = "claude"
            [speech.openai]
            base_url = "http://127.0.0.1:9000/v1"
            [speech.llm]
            enabled = true
            [[speech.engines]]
            id = "cloud-stt"
            kind = "openai"
            [[speech.llms]]
            id = "claude"
            kind = "anthropic"
            ant_profile = "default"
            "#,
        )
        .unwrap();
        let s = &cfg.speech;
        s.validate().unwrap();
        let engines: Vec<String> = s.engine_profiles().into_iter().map(|p| p.id).collect();
        assert_eq!(engines, ["openai", "cloud-stt"]);
        let llms: Vec<String> = s.llm_profiles().into_iter().map(|p| p.id).collect();
        assert_eq!(llms, ["llm", "claude"]);
        assert_eq!(s.default_engine_id().as_deref(), Some("openai"));
        assert_eq!(s.default_llm_id().as_deref(), Some("claude"));
        assert_eq!(
            s.engine_profiles()[0].base_url.as_deref(),
            Some("http://127.0.0.1:9000/v1")
        );
        assert_eq!(s.llm_profiles()[1].kind, LlmProfileKind::Anthropic);

        // Legacy LLM without default_llm is the default; "none" turns it off.
        let mut s2 = s.clone();
        s2.default_llm = None;
        assert_eq!(s2.default_llm_id().as_deref(), Some("llm"));
        s2.default_llm = Some("none".into());
        assert_eq!(s2.default_llm_id(), None);
        assert_eq!(SpeechConfig::default().default_llm_id(), None);
        assert_eq!(SpeechConfig::default().default_engine_id(), None);
    }

    #[test]
    fn speech_profiles_are_validated() {
        let bad = |toml: &str| {
            Config::from_toml_str(toml)
                .unwrap()
                .speech
                .validate()
                .is_err()
        };
        assert!(bad(
            "[[speech.engines]]\nid = \"Bad Id\"\nbase_url = \"http://x\""
        ));
        assert!(bad(
            "[[speech.engines]]\nid = \"none\"\nbase_url = \"http://x\""
        ));
        assert!(
            bad("[[speech.engines]]\nid = \"a\""),
            "base_url is required"
        );
        assert!(bad(
            "[[speech.engines]]\nid = \"a\"\nbase_url = \"http://x\"\n[[speech.llms]]\nid = \"a\""
        ));
        assert!(bad(
            "[speech]\nengine = \"openai\"\n[[speech.engines]]\nid = \"openai\"\nkind = \"openai\""
        ));
        assert!(bad("[speech]\ndefault_engine = \"missing\""));
        assert!(bad("[speech]\ndefault_llm = \"missing\""));
        assert!(!bad("[speech]\ndefault_llm = \"none\""));
        assert!(Config::from_toml_str("[[speech.llms]]\nid = \"a\"\nsecret = \"x\"").is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::from_toml_str("nope = 1").is_err());
        assert!(Config::from_toml_str("[dev_ws]\nopen = true").is_err());
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&dir.path().join("missing.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn tilde_expansion() {
        let p = expand_tilde(Path::new("~/x"));
        assert!(!p.starts_with("~"));
        assert_eq!(expand_tilde(Path::new("/a/b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn gateway_settings_are_checked() {
        let cfg = Config::from_toml_str(
            r#"
            [[speech.engines]]
            id = "team"
            base_url = "http://h/v1"
            api_key_header = "X-VCA-Key"
            gateway_token_env = "TEAM_GATEWAY_TOKEN"
            [[speech.llms]]
            id = "team-llm"
            base_url = "http://h/v1"
            api_key_header = "X-VCA-Key"
            [[tts.engines]]
            name = "team-tts"
            base_url = "http://h/v1"
            api_key_header = "X-VCA-Key"
            gateway_token_env = "TEAM_GATEWAY_TOKEN"
            "#,
        )
        .unwrap();
        cfg.speech.validate().unwrap();
        cfg.tts.validate().unwrap();
        assert_eq!(
            cfg.speech.engines[0].api_key_header.as_deref(),
            Some("X-VCA-Key")
        );

        // A gateway token needs a key header other than Authorization.
        for header in [None, Some("Authorization")] {
            let mut s = cfg.speech.clone();
            s.engines[0].api_key_header = header.map(str::to_owned);
            let err = s.validate().unwrap_err();
            assert!(err.contains("api_key_header"), "{err}");
            let mut t = cfg.tts.clone();
            t.engines[0].api_key_header = header.map(str::to_owned);
            assert!(t.validate().unwrap_err().contains("api_key_header"));
        }
        // Without a gateway token, Authorization (raw key) is allowed.
        let mut t = cfg.tts.clone();
        t.engines[0].gateway_token_env = None;
        t.engines[0].api_key_header = Some("Authorization".into());
        t.validate().unwrap();
        // Invalid header names.
        let mut s = cfg.speech.clone();
        s.llms[0].api_key_header = Some("X VCA Key".into());
        assert!(s.validate().is_err());
        // Only OpenAI-compatible APIs.
        let mut s = cfg.speech.clone();
        s.llms[0].kind = LlmProfileKind::Anthropic;
        assert!(s.validate().unwrap_err().contains("OpenAI-compatible"));
        let mut s = cfg.speech.clone();
        s.engines[0].kind = EngineProfileKind::Google;
        s.engines[0].project_id = Some("p".into());
        assert!(s.validate().unwrap_err().contains("OpenAI-compatible"));
    }
}
