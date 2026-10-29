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
//! # URLs written into pairing QR codes (default: from the provider below,
//! # else ws://<bind>/v1/ws).
//! # endpoints = ["wss://vca-k3x9q2.example.net/v1/ws"]
//!
//! # How devices reach the port. No provider is a default; pick one of
//! # "lan", "tailscale", "cloudflare_tunnel" or "custom". "lan" and
//! # tailscale "direct" need bind = "0.0.0.0:47800".
//! [remote.provider]
//! kind = "none"
//! # url = "wss://vca.example.net"      # kind = "custom"
//! [remote.provider.tailscale]
//! mode = "serve"                       # or "direct"
//! [remote.provider.cloudflare]
//! mode = "quick"                       # or "named"
//! # tunnel = "vca"; hostname = "vca-k3x9q2.example.com"; token_env = "CF_TUNNEL_TOKEN"
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
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Base URL for `kind = "custom"`.
    pub url: Option<String>,
    pub tailscale: TailscaleSection,
    pub cloudflare: CloudflareSection,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: vca_net::DEFAULT_BIND_ADDR.into(),
            require_totp: true,
            heartbeat_secs: 20,
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
        }
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
    pub dev_ws: DevWsConfig,
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
}
