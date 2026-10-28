//! Cloudflare Tunnel provider (`cloudflared`).
//!
//! Two modes; the user picks one:
//!
//! - [`CloudflareMode::Quick`]: `cloudflared tunnel --no-autoupdate --url
//!   http://127.0.0.1:PORT` starts a quick tunnel without an account. The
//!   public `https://<random>.trycloudflare.com` URL is read from the
//!   process output. Quick tunnels are meant for testing and have no uptime
//!   guarantee; the URL changes on every start.
//! - [`CloudflareMode::Named`]: a tunnel the user created in their
//!   Cloudflare account, with a public hostname routed to it. VCA runs
//!   `cloudflared tunnel --no-autoupdate run --url http://127.0.0.1:PORT
//!   <TUNNEL>` (locally managed tunnel, needs `cert.pem` / credentials
//!   from `cloudflared tunnel login` and `create`), or, with a tunnel token
//!   from the dashboard, `cloudflared tunnel --no-autoupdate run` with the
//!   token passed in the `TUNNEL_TOKEN` environment variable (never on the
//!   command line, where other local users could read it). For
//!   token-based tunnels, the hostname → `http://127.0.0.1:PORT` route is
//!   configured in the dashboard.
//!
//! Cloudflare terminates TLS at its edge, so the tunnel is an untrusted
//! transport; VCA's Noise channel applies on top (C1). Cloudflare proxies
//! WebSocket over HTTP/1.1, which is all VCA needs (C3).

use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::command::{CommandRunner, TokioRunner};
use crate::error::{ProviderError, Result};
use crate::provider::{
    ConnectivityProvider, Detection, Diagnostic, Endpoint, EndpointKind, ProviderId, ProviderStatus,
};
use crate::secret::Secret;

/// How long to wait for a quick tunnel URL or named tunnel registration.
const START_TIMEOUT: Duration = Duration::from_secs(45);

/// Named tunnel settings.
#[derive(Clone)]
pub struct NamedTunnel {
    /// Tunnel name or UUID (locally managed tunnels). Ignored with a token.
    pub tunnel: Option<String>,
    /// Public hostname routed to the tunnel, for example `vca-k3x9q2.example.com`.
    /// Use a non-identifying label (C6): hostnames appear in public
    /// Certificate Transparency logs.
    pub hostname: String,
    /// Tunnel token from the Cloudflare dashboard (remotely managed tunnels).
    pub token: Option<Secret>,
}

impl fmt::Debug for NamedTunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NamedTunnel")
            .field("tunnel", &self.tunnel)
            .field("hostname", &self.hostname)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum CloudflareMode {
    Quick,
    Named(NamedTunnel),
}

/// Command builders (argument lists, without the program name).
pub mod commands {
    pub fn version() -> Vec<String> {
        vec!["--version".into()]
    }

    /// `cloudflared tunnel --no-autoupdate --url http://127.0.0.1:PORT`
    pub fn quick(local_port: u16) -> Vec<String> {
        vec![
            "tunnel".into(),
            "--no-autoupdate".into(),
            "--url".into(),
            format!("http://127.0.0.1:{local_port}"),
        ]
    }

    /// `cloudflared tunnel --no-autoupdate run --url http://127.0.0.1:PORT TUNNEL`
    pub fn run_named(tunnel: &str, local_port: u16) -> Vec<String> {
        vec![
            "tunnel".into(),
            "--no-autoupdate".into(),
            "run".into(),
            "--url".into(),
            format!("http://127.0.0.1:{local_port}"),
            tunnel.into(),
        ]
    }

    /// `cloudflared tunnel --no-autoupdate run` (token in `TUNNEL_TOKEN`).
    pub fn run_with_token() -> Vec<String> {
        vec!["tunnel".into(), "--no-autoupdate".into(), "run".into()]
    }

    /// `cloudflared tunnel route dns TUNNEL HOSTNAME` (one-time setup).
    pub fn route_dns(tunnel: &str, hostname: &str) -> Vec<String> {
        vec![
            "tunnel".into(),
            "route".into(),
            "dns".into(),
            tunnel.into(),
            hostname.into(),
        ]
    }
}

/// A `config.yml` for a locally managed named tunnel that routes
/// `hostname` to the server's loopback port.
pub fn named_config_yaml(
    tunnel_id: &str,
    credentials_file: &str,
    hostname: &str,
    local_port: u16,
) -> String {
    format!(
        "tunnel: {tunnel_id}\n\
         credentials-file: {credentials_file}\n\
         ingress:\n  \
         - hostname: {hostname}\n    \
         service: http://127.0.0.1:{local_port}\n  \
         - service: http_status:404\n"
    )
}

/// Finds a quick tunnel URL (`https://<label>.trycloudflare.com`) in a line
/// of `cloudflared` output.
pub fn parse_quick_tunnel_url(line: &str) -> Option<String> {
    const SUFFIX: &str = ".trycloudflare.com";
    let start = line.find("https://")?;
    let rest = &line[start + "https://".len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .unwrap_or(rest.len());
    let host = &rest[..end];
    let label = host.strip_suffix(SUFFIX)?;
    if label.is_empty() || label.contains('.') || label == "api" {
        return None;
    }
    Some(format!("https://{host}"))
}

/// True when a line reports a registered tunnel connection.
pub fn is_connection_registered(line: &str) -> bool {
    line.contains("Registered tunnel connection") || line.contains("Connection registered")
}

/// Parses `cloudflared --version` output (`cloudflared version 2024.8.2 (built ...)`).
pub fn parse_version(out: &str) -> Option<String> {
    let rest = out.split("version").nth(1)?.trim();
    rest.split_whitespace().next().map(str::to_owned)
}

/// Converts an `https://host` URL into `wss://host`.
fn to_wss(url: &str) -> String {
    format!(
        "wss://{}",
        url.trim_start_matches("https://").trim_end_matches('/')
    )
}

struct Running {
    child: Child,
    endpoint: Endpoint,
}

/// Cloudflare Tunnel provider.
pub struct CloudflareProvider {
    program: PathBuf,
    mode: CloudflareMode,
    runner: Arc<dyn CommandRunner>,
    running: Mutex<Option<Running>>,
    start_timeout: Duration,
}

impl fmt::Debug for CloudflareProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudflareProvider")
            .field("program", &self.program)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl CloudflareProvider {
    pub fn new(mode: CloudflareMode) -> Self {
        Self {
            program: PathBuf::from("cloudflared"),
            mode,
            runner: Arc::new(TokioRunner::default()),
            running: Mutex::new(None),
            start_timeout: START_TIMEOUT,
        }
    }

    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
    }

    pub fn with_start_timeout(mut self, timeout: Duration) -> Self {
        self.start_timeout = timeout;
        self
    }

    async fn spawn(&self, local_port: u16) -> Result<Running> {
        let mut cmd = tokio::process::Command::new(&self.program);
        let (args, wait_for_url) = match &self.mode {
            CloudflareMode::Quick => (commands::quick(local_port), true),
            CloudflareMode::Named(n) => match (&n.token, &n.tunnel) {
                (Some(token), _) => {
                    cmd.env("TUNNEL_TOKEN", token.expose());
                    (commands::run_with_token(), false)
                }
                (None, Some(tunnel)) => (commands::run_named(tunnel, local_port), false),
                (None, None) => {
                    return Err(ProviderError::InvalidConfig(
                        "named tunnel needs a tunnel name or a token".into(),
                    ));
                }
            },
        };
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotInstalled("cloudflared")
            } else {
                ProviderError::Io(e)
            }
        })?;
        // cloudflared logs to stderr; read both streams line by line.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        if let Some(stderr) = child.stderr.take() {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!(target: "vca_providers::cloudflared", "{l}");
                    let _ = tx.send(l);
                }
            });
        }
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let _ = tx.send(l);
                }
            });
        }
        let wait = async {
            while let Some(line) = rx.recv().await {
                if wait_for_url {
                    if let Some(url) = parse_quick_tunnel_url(&line) {
                        return Some(url);
                    }
                } else if is_connection_registered(&line) {
                    return Some(String::new());
                }
            }
            None
        };
        let found = tokio::time::timeout(self.start_timeout, wait).await;
        let endpoint = match (found, &self.mode) {
            (Ok(Some(url)), CloudflareMode::Quick) => {
                Endpoint::new(to_wss(&url), EndpointKind::CloudflareQuick)
            }
            (Ok(Some(_)), CloudflareMode::Named(n)) => Endpoint::new(
                format!("wss://{}", n.hostname),
                EndpointKind::CloudflareNamed,
            ),
            (Ok(None), _) => {
                let status = child.wait().await.ok().and_then(|s| s.code());
                return Err(ProviderError::Command(format!(
                    "cloudflared exited early (code {status:?})"
                )));
            }
            (Err(_), _) => {
                let _ = child.kill().await;
                return Err(ProviderError::Timeout(
                    "cloudflared did not report a tunnel".into(),
                ));
            }
        };
        // Keep draining output so the pipes never fill up.
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        Ok(Running { child, endpoint })
    }
}

#[async_trait]
impl ConnectivityProvider for CloudflareProvider {
    fn id(&self) -> ProviderId {
        ProviderId::CloudflareTunnel
    }

    fn name(&self) -> &'static str {
        "Cloudflare Tunnel"
    }

    async fn detect(&self) -> Result<Detection> {
        let out = match self.runner.run(&self.program, &commands::version()).await {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Detection::default()),
            Err(e) => return Err(e.into()),
        };
        let version = parse_version(&out.stdout).or_else(|| parse_version(&out.stderr));
        let signed_in = match &self.mode {
            // Quick tunnels need no account.
            CloudflareMode::Quick => None,
            CloudflareMode::Named(n) => Some(n.token.is_some() || cert_present()),
        };
        Ok(Detection {
            installed: out.success() || version.is_some(),
            signed_in,
            version,
            detail: None,
        })
    }

    async fn status(&self) -> ProviderStatus {
        let mut guard = self.running.lock().await;
        if let Some(r) = guard.as_mut() {
            match r.child.try_wait() {
                Ok(None) => ProviderStatus::Active {
                    endpoint: r.endpoint.clone(),
                },
                Ok(Some(status)) => {
                    *guard = None;
                    ProviderStatus::Error {
                        message: format!("cloudflared exited ({status})"),
                    }
                }
                Err(e) => ProviderStatus::Error {
                    message: e.to_string(),
                },
            }
        } else {
            ProviderStatus::Inactive
        }
    }

    async fn ensure_endpoint(&self, local_port: u16) -> Result<Endpoint> {
        let mut guard = self.running.lock().await;
        if let Some(r) = guard.as_mut()
            && matches!(r.child.try_wait(), Ok(None))
        {
            return Ok(r.endpoint.clone());
        }
        let running = self.spawn(local_port).await?;
        let endpoint = running.endpoint.clone();
        *guard = Some(running);
        Ok(endpoint)
    }

    async fn teardown(&self) -> Result<()> {
        if let Some(mut r) = self.running.lock().await.take() {
            let _ = r.child.kill().await;
        }
        Ok(())
    }

    async fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match self.detect().await {
            Ok(d) if !d.installed => out.push(Diagnostic::error("cloudflared is not installed.")),
            Ok(d) => {
                if d.signed_in == Some(false) {
                    out.push(Diagnostic::error(
                        "Named tunnel: run `cloudflared tunnel login` or provide a tunnel token.",
                    ));
                }
            }
            Err(e) => out.push(Diagnostic::error(e.to_string())),
        }
        match &self.mode {
            CloudflareMode::Quick => out.push(Diagnostic::warning(
                "Quick tunnels are for testing: the URL changes on every start and there is no uptime guarantee.",
            )),
            CloudflareMode::Named(n) => out.push(Diagnostic::info(format!(
                "Make sure {} routes to http://127.0.0.1:<server port>.",
                n.hostname
            ))),
        }
        out.push(Diagnostic::info(
            "Cloudflare terminates TLS at its edge; VCA's end-to-end encryption protects the content.",
        ));
        out
    }
}

/// True when `~/.cloudflared/cert.pem` exists (result of `cloudflared tunnel login`).
fn cert_present() -> bool {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(|h| {
        PathBuf::from(h)
            .join(".cloudflared")
            .join("cert.pem")
            .exists()
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Output of a real quick tunnel start (trimmed).
    const QUICK_OUTPUT: &str = "\
2024-08-20T10:00:00Z INF Thank you for trying Cloudflare Tunnel. Doing so, without a Cloudflare account, is a quick way to experiment and try it out. However, be aware that these account-less Tunnels have no uptime guarantee.
2024-08-20T10:00:00Z INF Requesting new quick Tunnel on trycloudflare.com...
2024-08-20T10:00:01Z INF +--------------------------------------------------------------------------------------------+
2024-08-20T10:00:01Z INF |  Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):  |
2024-08-20T10:00:01Z INF |  https://seasonal-deck-organisms-sf.trycloudflare.com                                      |
2024-08-20T10:00:01Z INF +--------------------------------------------------------------------------------------------+
2024-08-20T10:00:02Z INF Registered tunnel connection connIndex=0 connection=abc event=0 ip=198.41.200.13 location=icn01 protocol=quic
";

    #[test]
    fn finds_quick_tunnel_url() {
        let urls: Vec<String> = QUICK_OUTPUT
            .lines()
            .filter_map(parse_quick_tunnel_url)
            .collect();
        assert_eq!(
            urls,
            vec!["https://seasonal-deck-organisms-sf.trycloudflare.com"]
        );
        assert_eq!(
            parse_quick_tunnel_url("see https://api.trycloudflare.com"),
            None
        );
        assert_eq!(parse_quick_tunnel_url("https://example.com"), None);
        assert!(QUICK_OUTPUT.lines().any(is_connection_registered));
        assert_eq!(
            to_wss("https://seasonal-deck-organisms-sf.trycloudflare.com"),
            "wss://seasonal-deck-organisms-sf.trycloudflare.com"
        );
    }

    #[test]
    fn version_and_builders() {
        assert_eq!(
            parse_version("cloudflared version 2024.8.2 (built 2024-08-13-1543 UTC)").as_deref(),
            Some("2024.8.2")
        );
        assert_eq!(
            commands::quick(47800).join(" "),
            "tunnel --no-autoupdate --url http://127.0.0.1:47800"
        );
        assert_eq!(
            commands::run_named("vca", 47800).join(" "),
            "tunnel --no-autoupdate run --url http://127.0.0.1:47800 vca"
        );
        assert_eq!(
            commands::route_dns("vca", "vca-a1.example.com").join(" "),
            "tunnel route dns vca vca-a1.example.com"
        );
        let yaml = named_config_yaml(
            "uuid-1",
            "/home/u/.cloudflared/uuid-1.json",
            "vca-a1.example.com",
            47800,
        );
        assert!(
            yaml.contains(
                "  - hostname: vca-a1.example.com\n    service: http://127.0.0.1:47800\n"
            )
        );
        assert!(yaml.ends_with("  - service: http_status:404\n"));
    }

    #[test]
    fn token_is_redacted_in_debug() {
        let mode = CloudflareMode::Named(NamedTunnel {
            tunnel: None,
            hostname: "h.example.com".into(),
            token: Some(Secret::new("eyJhIjoiYWJjIn0")),
        });
        assert!(!format!("{mode:?}").contains("eyJ"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quick_tunnel_with_fake_cloudflared() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cloudflared");
        let body = format!(
            "#!/bin/sh\n[ \"$4\" = \"http://127.0.0.1:47800\" ] || exit 3\ncat >&2 <<'EOF'\n{QUICK_OUTPUT}EOF\nexec sleep 30\n"
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let p = CloudflareProvider::new(CloudflareMode::Quick)
            .with_program(&script)
            .with_start_timeout(Duration::from_secs(10));
        let e = p.ensure_endpoint(47800).await.unwrap();
        assert_eq!(e.url, "wss://seasonal-deck-organisms-sf.trycloudflare.com");
        assert_eq!(e.kind, EndpointKind::CloudflareQuick);
        assert!(matches!(p.status().await, ProviderStatus::Active { .. }));
        // Idempotent while running.
        assert_eq!(p.ensure_endpoint(47800).await.unwrap(), e);
        p.teardown().await.unwrap();
        assert_eq!(p.status().await, ProviderStatus::Inactive);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn early_exit_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cloudflared");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'ERR failed to request quick Tunnel' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let p = CloudflareProvider::new(CloudflareMode::Quick).with_program(&script);
        assert!(matches!(
            p.ensure_endpoint(1).await,
            Err(ProviderError::Command(_))
        ));
    }
}
