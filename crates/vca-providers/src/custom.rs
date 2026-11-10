//! Custom provider (CON-05): any user-supplied tunnel or VPN that forwards
//! to the server's local port. VCA only validates the URL; the user is
//! responsible for securing the tunnel, and VCA's authentication and
//! end-to-end encryption still apply.

use std::net::IpAddr;

use async_trait::async_trait;
use url::{Host, Url};

use crate::error::{ProviderError, Result};
use crate::provider::{
    ConnectivityProvider, Detection, Diagnostic, Endpoint, EndpointKind, ProviderId, ProviderStatus,
};

/// Validates and normalizes a user-supplied public URL.
///
/// Accepts `ws`, `wss`, `http` and `https` (HTTP schemes are converted to
/// their WebSocket equivalents). Rejects missing hosts, embedded
/// credentials, queries and fragments. A path is kept.
pub fn validate_url(input: &str) -> Result<Endpoint> {
    let url = Url::parse(input.trim())
        .map_err(|e| ProviderError::InvalidConfig(format!("invalid URL: {e}")))?;
    let scheme = match url.scheme() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        other => {
            return Err(ProviderError::InvalidConfig(format!(
                "unsupported scheme {other:?}; use wss://, ws://, https:// or http://"
            )));
        }
    };
    let host = url
        .host()
        .ok_or_else(|| ProviderError::InvalidConfig("URL has no host".into()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProviderError::InvalidConfig(
            "URL must not contain credentials".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ProviderError::InvalidConfig(
            "URL must not contain a query or fragment".into(),
        ));
    }
    let host = match host {
        Host::Domain(d) => d.to_ascii_lowercase(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => format!("[{ip}]"),
    };
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let path = url.path().trim_end_matches('/');
    Ok(Endpoint::new(
        format!("{scheme}://{host}{port}{path}"),
        EndpointKind::Custom,
    ))
}

fn is_private_host(endpoint: &Endpoint) -> bool {
    let Ok(url) = Url::parse(&endpoint.url) else {
        return false;
    };
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        Some(Host::Ipv6(ip)) => {
            ip.is_loopback()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || IpAddr::V6(ip).is_unspecified()
        }
        Some(Host::Domain(d)) => d == "localhost" || d.ends_with(".local") || d.ends_with(".lan"),
        None => false,
    }
}

/// Custom provider with a fixed, validated URL.
#[derive(Debug, Clone)]
pub struct CustomProvider {
    endpoint: Endpoint,
}

impl CustomProvider {
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            endpoint: validate_url(url)?,
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

#[async_trait]
impl ConnectivityProvider for CustomProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Custom
    }

    fn name(&self) -> &'static str {
        "Custom"
    }

    async fn detect(&self) -> Result<Detection> {
        Ok(Detection {
            installed: true,
            signed_in: None,
            version: None,
            detail: Some(self.endpoint.url.clone()),
        })
    }

    async fn status(&self) -> ProviderStatus {
        ProviderStatus::Active {
            endpoint: self.endpoint.clone(),
        }
    }

    /// Returns the configured URL; the user's tunnel must forward to
    /// `127.0.0.1:local_port` itself.
    async fn ensure_endpoint(&self, _local_port: u16) -> Result<Endpoint> {
        Ok(self.endpoint.clone())
    }

    async fn teardown(&self) -> Result<()> {
        Ok(())
    }

    async fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = vec![Diagnostic::info(
            "You are responsible for securing this tunnel; VCA's pairing and end-to-end encryption still apply.",
        )];
        if !self.endpoint.transport_tls() && !is_private_host(&self.endpoint) {
            out.push(Diagnostic::warning(
                "This public URL has no transport TLS. Content stays encrypted end to end, but metadata is visible and some networks block plain WebSocket.",
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            validate_url("https://Vca.Example.com/").unwrap().url,
            "wss://vca.example.com"
        );
        assert_eq!(
            validate_url("http://10.0.0.5:47800").unwrap().url,
            "ws://10.0.0.5:47800"
        );
        assert_eq!(
            validate_url("wss://h.example.com:8443/vca/").unwrap().url,
            "wss://h.example.com:8443/vca"
        );
        assert_eq!(
            validate_url("ws://[fd00::1]:1").unwrap().url,
            "ws://[fd00::1]:1"
        );
    }

    #[test]
    fn rejects_bad_urls() {
        for bad in [
            "ftp://x.example.com",
            "not a url",
            "https://user:pw@x.example.com",
            "https://x.example.com/?token=1",
            "https://x.example.com/#frag",
            "file:///etc/passwd",
        ] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn diagnostics_warn_on_public_plaintext() {
        let p = CustomProvider::new("http://vca.example.com").unwrap();
        assert!(
            p.diagnostics()
                .await
                .iter()
                .any(|d| d.level == crate::provider::DiagnosticLevel::Warning)
        );
        let p = CustomProvider::new("http://192.168.1.2:47800").unwrap();
        assert!(
            p.diagnostics()
                .await
                .iter()
                .all(|d| d.level != crate::provider::DiagnosticLevel::Warning)
        );
        assert_eq!(
            p.ensure_endpoint(1).await.unwrap().url,
            "ws://192.168.1.2:47800"
        );
    }
}
