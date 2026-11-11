//! Tailscale provider.
//!
//! Detection parses `tailscale status --json` (`BackendState`,
//! `Self.DNSName`, `Self.TailscaleIPs`, `MagicDNSSuffix`,
//! `CurrentTailnet`). Two ways to expose the server are supported; the
//! user picks one:
//!
//! - [`TailscaleMode::Direct`]: devices connect to the machine's tailnet
//!   name or address at the server port (`ws://host.tailnet.ts.net:PORT`).
//!   WireGuard encrypts the tailnet hop, and the server must listen on the
//!   Tailscale interface (not only on loopback).
//! - [`TailscaleMode::Serve`]: `tailscale serve --bg --https=443
//!   http://127.0.0.1:PORT` publishes the loopback port as
//!   `wss://host.tailnet.ts.net` inside the tailnet. This needs MagicDNS and
//!   HTTPS certificates enabled for the tailnet. Teardown runs
//!   `tailscale serve --https=443 off`.
//!
//! In both modes VCA's Noise channel still applies (C1).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::command::{CmdOutput, CommandRunner, TokioRunner};
use crate::error::{ProviderError, Result};
use crate::provider::{
    ConnectivityProvider, Detection, Diagnostic, Endpoint, EndpointKind, ProviderId, ProviderStatus,
};

/// How the server is exposed on the tailnet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailscaleMode {
    Direct,
    Serve,
}

/// The subset of `tailscale status --json` VCA reads. Unknown fields are ignored.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct TailscaleStatus {
    pub version: Option<String>,
    /// `Running`, `NeedsLogin`, `NeedsMachineAuth`, `Stopped`, `Starting`, `NoState`.
    pub backend_state: String,
    #[serde(rename = "Self")]
    pub self_node: Option<TailscaleNode>,
    #[serde(rename = "MagicDNSSuffix")]
    pub magic_dns_suffix: Option<String>,
    pub current_tailnet: Option<TailnetInfo>,
    pub health: Option<Vec<String>>,
}

/// Tailscale prints `null` for empty lists.
fn null_as_default<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct TailscaleNode {
    #[serde(rename = "DNSName")]
    pub dns_name: String,
    pub host_name: String,
    #[serde(rename = "TailscaleIPs", deserialize_with = "null_as_default")]
    pub tailscale_ips: Vec<String>,
    pub online: bool,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct TailnetInfo {
    pub name: String,
    #[serde(rename = "MagicDNSSuffix")]
    pub magic_dns_suffix: String,
    #[serde(rename = "MagicDNSEnabled")]
    pub magic_dns_enabled: bool,
}

impl TailscaleStatus {
    pub fn parse(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    pub fn is_running(&self) -> bool {
        self.backend_state == "Running"
    }

    /// The node's MagicDNS name without the trailing dot.
    pub fn dns_name(&self) -> Option<&str> {
        self.self_node
            .as_ref()
            .map(|n| n.dns_name.trim_end_matches('.'))
            .filter(|n| !n.is_empty())
    }

    /// The node's first IPv4 tailnet address, else the first address.
    pub fn primary_ip(&self) -> Option<&str> {
        let ips = &self.self_node.as_ref()?.tailscale_ips;
        ips.iter()
            .find(|ip| !ip.contains(':'))
            .or_else(|| ips.first())
            .map(String::as_str)
    }

    pub fn magic_dns_enabled(&self) -> bool {
        self.current_tailnet
            .as_ref()
            .map(|t| t.magic_dns_enabled)
            .unwrap_or_else(|| self.dns_name().is_some())
    }
}

/// Command builders (argument lists, without the program name).
pub mod commands {
    pub fn status_json() -> Vec<String> {
        vec!["status".into(), "--json".into()]
    }

    pub fn version() -> Vec<String> {
        vec!["version".into()]
    }

    /// `tailscale serve --bg --https=443 http://127.0.0.1:PORT`
    pub fn serve(local_port: u16) -> Vec<String> {
        vec![
            "serve".into(),
            "--bg".into(),
            "--https=443".into(),
            format!("http://127.0.0.1:{local_port}"),
        ]
    }

    /// `tailscale serve --https=443 off`
    pub fn serve_off() -> Vec<String> {
        vec!["serve".into(), "--https=443".into(), "off".into()]
    }
}

/// Builds the endpoint for a mode from a parsed status.
pub fn endpoint_for(
    status: &TailscaleStatus,
    mode: TailscaleMode,
    local_port: u16,
) -> Result<Endpoint> {
    if !status.is_running() {
        return Err(ProviderError::NotSignedIn(format!(
            "Tailscale backend state is {}",
            status.backend_state
        )));
    }
    match mode {
        TailscaleMode::Direct => {
            let host = match status.dns_name() {
                Some(name) => name.to_owned(),
                None => match status.primary_ip() {
                    Some(ip) if ip.contains(':') => format!("[{ip}]"),
                    Some(ip) => ip.to_owned(),
                    None => return Err(ProviderError::InvalidConfig("no tailnet address".into())),
                },
            };
            Ok(Endpoint::new(
                format!("ws://{host}:{local_port}"),
                EndpointKind::TailnetDirect,
            ))
        }
        TailscaleMode::Serve => {
            let name = status.dns_name().ok_or_else(|| {
                ProviderError::InvalidConfig("tailscale serve needs MagicDNS to be enabled".into())
            })?;
            Ok(Endpoint::new(
                format!("wss://{name}"),
                EndpointKind::TailscaleServe,
            ))
        }
    }
}

/// Default CLI location: `tailscale` on `PATH`; on macOS the App Store and
/// standalone apps ship the CLI inside the app bundle.
fn default_program() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let bundled = PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from("tailscale")
}

/// Tailscale provider.
pub struct TailscaleProvider {
    program: PathBuf,
    mode: TailscaleMode,
    runner: Arc<dyn CommandRunner>,
    state: Mutex<Option<Endpoint>>,
}

impl std::fmt::Debug for TailscaleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TailscaleProvider")
            .field("program", &self.program)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl TailscaleProvider {
    pub fn new(mode: TailscaleMode) -> Self {
        Self::with_runner(mode, default_program(), Arc::new(TokioRunner::default()))
    }

    pub fn with_runner(
        mode: TailscaleMode,
        program: impl Into<PathBuf>,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            program: program.into(),
            mode,
            runner,
            state: Mutex::new(None),
        }
    }

    pub fn mode(&self) -> TailscaleMode {
        self.mode
    }

    async fn run(&self, args: Vec<String>) -> Result<CmdOutput> {
        match self.runner.run(&self.program, &args).await {
            Ok(o) => Ok(o),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(ProviderError::NotInstalled("Tailscale"))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Runs `tailscale status --json` and parses it.
    pub async fn read_status(&self) -> Result<TailscaleStatus> {
        let out = self.run(commands::status_json()).await?;
        // `status --json` prints JSON even when logged out (exit code may be non-zero).
        if out.stdout.trim_start().starts_with('{') {
            return TailscaleStatus::parse(&out.stdout);
        }
        Err(ProviderError::Command(out.stderr.trim().to_owned()))
    }
}

#[async_trait]
impl ConnectivityProvider for TailscaleProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Tailscale
    }

    fn name(&self) -> &'static str {
        "Tailscale"
    }

    async fn detect(&self) -> Result<Detection> {
        let status = match self.read_status().await {
            Ok(s) => s,
            Err(ProviderError::NotInstalled(_)) => return Ok(Detection::default()),
            Err(e) => {
                return Ok(Detection {
                    installed: true,
                    signed_in: None,
                    version: None,
                    detail: Some(e.to_string()),
                });
            }
        };
        Ok(Detection {
            installed: true,
            signed_in: Some(status.is_running()),
            version: status.version.clone(),
            detail: Some(format!("backend state: {}", status.backend_state)),
        })
    }

    async fn status(&self) -> ProviderStatus {
        match &*self.state.lock().await {
            Some(endpoint) => ProviderStatus::Active {
                endpoint: endpoint.clone(),
            },
            None => ProviderStatus::Inactive,
        }
    }

    async fn ensure_endpoint(&self, local_port: u16) -> Result<Endpoint> {
        let status = self.read_status().await?;
        let endpoint = endpoint_for(&status, self.mode, local_port)?;
        if self.mode == TailscaleMode::Serve {
            let out = self.run(commands::serve(local_port)).await?;
            if !out.success() {
                return Err(ProviderError::Command(format!(
                    "tailscale serve failed: {}",
                    out.stderr.trim()
                )));
            }
        }
        *self.state.lock().await = Some(endpoint.clone());
        Ok(endpoint)
    }

    async fn teardown(&self) -> Result<()> {
        let active = self.state.lock().await.take();
        if active.is_some() && self.mode == TailscaleMode::Serve {
            let out = self.run(commands::serve_off()).await?;
            if !out.success() {
                return Err(ProviderError::Command(out.stderr.trim().to_owned()));
            }
        }
        Ok(())
    }

    async fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match self.read_status().await {
            Err(ProviderError::NotInstalled(_)) => {
                out.push(Diagnostic::error("Tailscale is not installed."))
            }
            Err(e) => out.push(Diagnostic::error(format!(
                "Could not read Tailscale status: {e}"
            ))),
            Ok(s) => {
                if !s.is_running() {
                    out.push(Diagnostic::error(format!(
                        "Tailscale is not connected (state {}). Sign in and connect.",
                        s.backend_state
                    )));
                }
                if self.mode == TailscaleMode::Serve && !s.magic_dns_enabled() {
                    out.push(Diagnostic::error(
                        "tailscale serve needs MagicDNS and HTTPS certificates enabled.",
                    ));
                }
                if self.mode == TailscaleMode::Direct {
                    out.push(Diagnostic::info(
                        "Direct mode: the server must listen on the Tailscale interface.",
                    ));
                }
                for h in s.health.iter().flatten() {
                    out.push(Diagnostic::warning(h.clone()));
                }
                out.push(Diagnostic::info(
                    "The phone must be signed in to the same tailnet.",
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::fake::{FakeRunner, ok};

    const RUNNING: &str = r#"{
      "Version": "1.76.1-t1234",
      "TUN": true,
      "BackendState": "Running",
      "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::1"],
      "Self": {
        "ID": "nXYZ", "PublicKey": "nodekey:abc", "HostName": "workstation",
        "DNSName": "workstation.tail1234.ts.net.", "OS": "linux",
        "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::1"],
        "Online": true, "ExitNode": false
      },
      "Health": ["some warning"],
      "MagicDNSSuffix": "tail1234.ts.net",
      "CurrentTailnet": {"Name": "user@example.com", "MagicDNSSuffix": "tail1234.ts.net", "MagicDNSEnabled": true},
      "Peer": {}
    }"#;

    const LOGGED_OUT: &str = r#"{"Version":"1.76.1","BackendState":"NeedsLogin","Self":{"DNSName":"","TailscaleIPs":null},"Health":["You are logged out."],"CurrentTailnet":null}"#;

    #[test]
    fn parses_status_fixture() {
        let s = TailscaleStatus::parse(RUNNING).unwrap();
        assert!(s.is_running());
        assert_eq!(s.dns_name(), Some("workstation.tail1234.ts.net"));
        assert_eq!(s.primary_ip(), Some("100.101.102.103"));
        assert!(s.magic_dns_enabled());
        assert_eq!(
            endpoint_for(&s, TailscaleMode::Direct, 47800).unwrap().url,
            "ws://workstation.tail1234.ts.net:47800"
        );
        let serve = endpoint_for(&s, TailscaleMode::Serve, 47800).unwrap();
        assert_eq!(serve.url, "wss://workstation.tail1234.ts.net");
        assert!(serve.transport_tls());
    }

    #[test]
    fn logged_out_is_rejected() {
        // `"TailscaleIPs": null` must not fail parsing.
        let s = TailscaleStatus::parse(LOGGED_OUT).unwrap();
        assert!(!s.is_running());
        assert!(matches!(
            endpoint_for(&s, TailscaleMode::Direct, 1),
            Err(ProviderError::NotSignedIn(_))
        ));
    }

    #[test]
    fn command_builders() {
        assert_eq!(
            commands::serve(47800).join(" "),
            "serve --bg --https=443 http://127.0.0.1:47800"
        );
        assert_eq!(commands::serve_off().join(" "), "serve --https=443 off");
    }

    #[tokio::test]
    async fn serve_mode_runs_commands() {
        let runner = Arc::new(
            FakeRunner::default()
                .on("status --json", ok(RUNNING))
                .on("serve --bg --https=443 http://127.0.0.1:47800", ok(""))
                .on("serve --https=443 off", ok("")),
        );
        let p = TailscaleProvider::with_runner(TailscaleMode::Serve, "tailscale", runner.clone());
        let d = p.detect().await.unwrap();
        assert_eq!(d.signed_in, Some(true));
        let e = p.ensure_endpoint(47800).await.unwrap();
        assert_eq!(e.kind, EndpointKind::TailscaleServe);
        assert!(matches!(p.status().await, ProviderStatus::Active { .. }));
        p.teardown().await.unwrap();
        assert_eq!(p.status().await, ProviderStatus::Inactive);
        let calls = runner.calls.lock().unwrap().clone();
        assert!(calls.contains(&"serve --https=443 off".to_owned()));
    }

    #[tokio::test]
    async fn not_installed() {
        let runner = Arc::new(FakeRunner::default().not_installed("status --json"));
        let p = TailscaleProvider::with_runner(TailscaleMode::Direct, "tailscale", runner);
        assert!(!p.detect().await.unwrap().installed);
        assert!(matches!(
            p.ensure_endpoint(1).await,
            Err(ProviderError::NotInstalled(_))
        ));
    }
}
