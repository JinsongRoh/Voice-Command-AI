//! Connectivity providers (`vca-providers`, requirements §3.8): the
//! user-chosen way paired devices reach the remote transport. The provider
//! only carries bytes to the listen address; pairing, TOTP and the Noise
//! channel apply on top of it (C1), so no provider is trusted.
//!
//! No provider is a default (CON-02). Without one, pairing codes list the
//! configured `remote.endpoints`, or the listen address itself.

use std::net::SocketAddr;

use anyhow::bail;
use tokio::sync::Mutex;
use vca_providers::{
    CloudflareMode, CloudflareProvider, ConnectivityProvider, CustomProvider, Diagnostic,
    DiagnosticLevel, Endpoint, LanProvider, NamedTunnel, ProviderStatus, Secret, TailscaleMode,
    TailscaleProvider,
};

use crate::config::{CloudflareKind, ProviderConfig, ProviderKind, TailscaleKind};

/// The configured provider and the endpoint it set up.
pub struct Connectivity {
    provider: Box<dyn ConnectivityProvider>,
    listen: SocketAddr,
    endpoint: Mutex<Option<Endpoint>>,
}

impl std::fmt::Debug for Connectivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connectivity")
            .field("provider", &self.provider.id())
            .field("listen", &self.listen)
            .finish_non_exhaustive()
    }
}

/// What `vca remote` shows about the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityStatus {
    pub provider: String,
    /// `inactive`, `starting`, `active` or `error: ...`.
    pub state: String,
    pub diagnostics: Vec<String>,
}

/// Reads the secret named by `var`.
fn env_secret(var: Option<&str>) -> Option<Secret> {
    let var = var.filter(|v| !v.is_empty())?;
    std::env::var(var)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(Secret::new)
}

impl Connectivity {
    /// Builds the configured provider; `None` when none is chosen.
    ///
    /// Providers that connect devices directly to this machine (LAN,
    /// Tailscale direct) need the listener on a non-loopback address, which
    /// the user must configure explicitly: the default stays loopback-only.
    pub fn from_config(cfg: &ProviderConfig, listen: SocketAddr) -> anyhow::Result<Option<Self>> {
        let direct = |name: &str| -> anyhow::Result<()> {
            if listen.ip().is_loopback() {
                bail!(
                    "the {name} provider connects devices straight to this machine, but the \
                     remote transport only listens on {listen}; set remote.bind to a \
                     reachable address (for example \"0.0.0.0:{port}\") to use it",
                    port = listen.port()
                );
            }
            Ok(())
        };
        let provider: Box<dyn ConnectivityProvider> = match cfg.kind {
            ProviderKind::None => return Ok(None),
            ProviderKind::Lan => {
                direct("LAN")?;
                Box::new(LanProvider::new())
            }
            ProviderKind::Tailscale => match cfg.tailscale.mode {
                TailscaleKind::Direct => {
                    direct("Tailscale (direct)")?;
                    Box::new(TailscaleProvider::new(TailscaleMode::Direct))
                }
                TailscaleKind::Serve => Box::new(TailscaleProvider::new(TailscaleMode::Serve)),
            },
            ProviderKind::CloudflareTunnel => {
                let cf = &cfg.cloudflare;
                let mode = match cf.mode {
                    CloudflareKind::Quick => CloudflareMode::Quick,
                    CloudflareKind::Named => {
                        let Some(hostname) = cf.hostname.clone().filter(|h| !h.is_empty()) else {
                            bail!(
                                "a named Cloudflare tunnel needs remote.provider.cloudflare.hostname"
                            );
                        };
                        let token = env_secret(cf.token_env.as_deref());
                        if token.is_none() && cf.tunnel.is_none() {
                            bail!(
                                "a named Cloudflare tunnel needs a tunnel name or a token \
                                 (remote.provider.cloudflare.tunnel or token_env)"
                            );
                        }
                        CloudflareMode::Named(NamedTunnel {
                            tunnel: cf.tunnel.clone(),
                            hostname,
                            token,
                        })
                    }
                };
                let mut p = CloudflareProvider::new(mode);
                if let Some(program) = &cf.program {
                    p = p.with_program(program.clone());
                }
                Box::new(p)
            }
            // The DNS Pilot APIs are not public yet (vca-providers'
            // `dnspilot/contract.rs` holds placeholder field names), and VCA
            // never calls undocumented provider APIs.
            ProviderKind::Dnspilot => bail!(
                "the DNS Pilot provider is not available yet: its API contract is still \
                 unconfirmed (see docs/handoff.md)"
            ),
            ProviderKind::Custom => {
                let Some(url) = cfg.url.as_deref().filter(|u| !u.is_empty()) else {
                    bail!("the custom provider needs remote.provider.url");
                };
                Box::new(CustomProvider::new(url)?)
            }
        };
        Ok(Some(Self::with_provider(provider, listen)))
    }

    pub fn with_provider(provider: Box<dyn ConnectivityProvider>, listen: SocketAddr) -> Self {
        Self {
            provider,
            listen,
            endpoint: Mutex::new(None),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// Sets up (or returns) the endpoint and the WebSocket URLs for pairing
    /// codes. Idempotent.
    pub async fn ensure_urls(&self) -> Result<Vec<String>, vca_providers::ProviderError> {
        let mut current = self.endpoint.lock().await;
        let endpoint = match &*current {
            Some(e) => e.clone(),
            None => {
                let e = self.provider.ensure_endpoint(self.listen.port()).await?;
                tracing::info!(provider = self.provider.name(), url = %e.url, "endpoint ready");
                *current = Some(e.clone());
                e
            }
        };
        let mut urls = vec![ws_url(&endpoint.url)];
        // LAN pairing codes carry every local address, best first, so the
        // phone can try the next one when it is on another subnet.
        if self.provider.id() == vca_providers::ProviderId::Lan
            && let Ok(all) = LanProvider::endpoints(self.listen.port())
        {
            for e in all {
                let url = ws_url(&e.url);
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
        }
        Ok(urls)
    }

    /// Removes what [`ensure_urls`](Self::ensure_urls) set up.
    pub async fn teardown(&self) {
        if self.endpoint.lock().await.take().is_some()
            && let Err(err) = self.provider.teardown().await
        {
            tracing::warn!(provider = self.provider.name(), %err, "teardown failed");
        }
    }

    pub async fn status(&self) -> ConnectivityStatus {
        let state = match self.provider.status().await {
            ProviderStatus::Inactive => "inactive".to_owned(),
            ProviderStatus::Starting => "starting".to_owned(),
            ProviderStatus::Active { endpoint } => format!("active ({})", endpoint.url),
            ProviderStatus::Error { message } => format!("error: {message}"),
        };
        let diagnostics = self
            .provider
            .diagnostics()
            .await
            .into_iter()
            .map(diagnostic_line)
            .collect();
        ConnectivityStatus {
            provider: self.provider.name().to_owned(),
            state,
            diagnostics,
        }
    }
}

fn diagnostic_line(d: Diagnostic) -> String {
    let level = match d.level {
        DiagnosticLevel::Info => "info",
        DiagnosticLevel::Warning => "warning",
        DiagnosticLevel::Error => "error",
    };
    format!("{level}: {}", d.message)
}

/// Appends the server's WebSocket path to a provider's base URL, unless the
/// URL already names a path (custom URLs may).
pub fn ws_url(base: &str) -> String {
    let after_scheme = base.split_once("://").map_or(base, |(_, rest)| rest);
    if after_scheme.contains('/') {
        base.to_owned()
    } else {
        format!("{base}{}", vca_net::DEFAULT_WS_PATH)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use vca_providers::{Detection, EndpointKind, ProviderId, Result as PResult};

    use super::*;
    use crate::config::ProviderConfig;

    fn loopback() -> SocketAddr {
        "127.0.0.1:47800".parse().unwrap()
    }

    #[test]
    fn ws_paths() {
        assert_eq!(ws_url("wss://h.example"), "wss://h.example/v1/ws");
        assert_eq!(ws_url("ws://10.0.0.2:47800"), "ws://10.0.0.2:47800/v1/ws");
        assert_eq!(ws_url("wss://h.example/vca/ws"), "wss://h.example/vca/ws");
    }

    #[test]
    fn none_is_the_default() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.kind, ProviderKind::None);
        assert!(
            Connectivity::from_config(&cfg, loopback())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn direct_providers_need_an_explicit_non_loopback_bind() {
        let mut cfg = ProviderConfig {
            kind: ProviderKind::Lan,
            ..ProviderConfig::default()
        };
        let err = Connectivity::from_config(&cfg, loopback()).unwrap_err();
        assert!(err.to_string().contains("remote.bind"), "{err}");
        assert!(
            Connectivity::from_config(&cfg, "0.0.0.0:47800".parse().unwrap())
                .unwrap()
                .is_some()
        );
        cfg.kind = ProviderKind::Tailscale;
        cfg.tailscale.mode = TailscaleKind::Direct;
        assert!(Connectivity::from_config(&cfg, loopback()).is_err());
        // `tailscale serve` forwards to loopback itself.
        cfg.tailscale.mode = TailscaleKind::Serve;
        assert!(
            Connectivity::from_config(&cfg, loopback())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn dnspilot_is_refused_until_its_contract_is_public() {
        let cfg = ProviderConfig {
            kind: ProviderKind::Dnspilot,
            ..ProviderConfig::default()
        };
        let err = Connectivity::from_config(&cfg, loopback()).unwrap_err();
        assert!(err.to_string().contains("DNS Pilot"), "{err}");
    }

    #[test]
    fn named_cloudflare_tunnels_need_a_hostname_and_a_tunnel() {
        let mut cfg = ProviderConfig {
            kind: ProviderKind::CloudflareTunnel,
            ..ProviderConfig::default()
        };
        cfg.cloudflare.mode = CloudflareKind::Named;
        assert!(Connectivity::from_config(&cfg, loopback()).is_err());
        cfg.cloudflare.hostname = Some("vca-k3x9q2.example.com".into());
        assert!(Connectivity::from_config(&cfg, loopback()).is_err());
        cfg.cloudflare.tunnel = Some("vca".into());
        assert!(
            Connectivity::from_config(&cfg, loopback())
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn custom_urls_are_validated_and_used_for_pairing() {
        let mut cfg = ProviderConfig {
            kind: ProviderKind::Custom,
            ..ProviderConfig::default()
        };
        assert!(Connectivity::from_config(&cfg, loopback()).is_err());
        cfg.url = Some("https://user:pw@vca.example.net".into());
        assert!(Connectivity::from_config(&cfg, loopback()).is_err());
        cfg.url = Some("https://vca.example.net".into());
        let c = Connectivity::from_config(&cfg, loopback())
            .unwrap()
            .unwrap();
        assert_eq!(
            c.ensure_urls().await.unwrap(),
            vec!["wss://vca.example.net/v1/ws".to_owned()]
        );
    }

    #[derive(Default)]
    struct Counts {
        ensures: AtomicUsize,
        teardowns: AtomicUsize,
    }

    /// Counts calls to check idempotence and teardown.
    struct Counting(Arc<Counts>);

    #[async_trait]
    impl ConnectivityProvider for Counting {
        fn id(&self) -> ProviderId {
            ProviderId::Custom
        }
        fn name(&self) -> &'static str {
            "counting"
        }
        async fn detect(&self) -> PResult<Detection> {
            Ok(Detection::default())
        }
        async fn status(&self) -> ProviderStatus {
            ProviderStatus::Inactive
        }
        async fn ensure_endpoint(&self, port: u16) -> PResult<Endpoint> {
            self.0.ensures.fetch_add(1, Ordering::SeqCst);
            Ok(Endpoint::new(
                format!("wss://tunnel.example:{port}"),
                EndpointKind::Custom,
            ))
        }
        async fn teardown(&self) -> PResult<()> {
            self.0.teardowns.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn diagnostics(&self) -> Vec<Diagnostic> {
            vec![Diagnostic::warning("check the firewall")]
        }
    }

    #[tokio::test]
    async fn endpoint_is_set_up_once_and_torn_down_once() {
        let counting = Arc::new(Counts::default());
        let c = Connectivity::with_provider(Box::new(Counting(Arc::clone(&counting))), loopback());
        let urls = c.ensure_urls().await.unwrap();
        assert_eq!(urls, vec!["wss://tunnel.example:47800/v1/ws".to_owned()]);
        c.ensure_urls().await.unwrap();
        assert_eq!(counting.ensures.load(Ordering::SeqCst), 1);
        let status = c.status().await;
        assert_eq!(status.diagnostics, vec!["warning: check the firewall"]);
        c.teardown().await;
        c.teardown().await;
        assert_eq!(counting.teardowns.load(Ordering::SeqCst), 1);
    }
}
