//! Direct LAN provider.
//!
//! Lists the machine's non-loopback interface addresses and produces
//! `ws://` endpoints for the pairing QR code. These endpoints have **no
//! TLS** at the transport level; confidentiality and authentication come
//! from VCA's Noise channel and device pairing keys, as with every provider
//! (C1). The server must listen on the LAN interface (not only on
//! loopback) for this provider to work.

use std::net::IpAddr;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::error::{ProviderError, Result};
use crate::provider::{
    ConnectivityProvider, Detection, Diagnostic, Endpoint, EndpointKind, ProviderId, ProviderStatus,
};

/// One usable local address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanAddress {
    pub interface: String,
    pub ip: IpAddr,
}

/// Lists usable interface addresses: no loopback, no link-local, no
/// unspecified addresses. Private IPv4 addresses come first.
pub fn local_addresses() -> std::io::Result<Vec<LanAddress>> {
    let ifaces = if_addrs::get_if_addrs()?;
    let addrs = ifaces
        .into_iter()
        .map(|i| LanAddress {
            ip: i.ip(),
            interface: i.name,
        })
        .collect();
    Ok(filter_and_sort(addrs))
}

fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast())
        }
        IpAddr::V6(v6) => {
            // Exclude loopback, unspecified and link-local (fe80::/10),
            // which needs a zone ID that cannot go into a URL portably.
            !(v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

fn rank(ip: &IpAddr) -> u8 {
    match ip {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(_) => 1,
        // Unique local fc00::/7.
        IpAddr::V6(v6) if (v6.segments()[0] & 0xfe00) == 0xfc00 => 2,
        IpAddr::V6(_) => 3,
    }
}

fn filter_and_sort(addrs: Vec<LanAddress>) -> Vec<LanAddress> {
    let mut v: Vec<LanAddress> = addrs.into_iter().filter(|a| usable(&a.ip)).collect();
    v.sort_by_key(|a| (rank(&a.ip), a.interface.clone()));
    v.dedup_by(|a, b| a.ip == b.ip);
    v
}

/// Formats a `ws://` URL for an address and port.
pub fn ws_url(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("ws://{v4}:{port}"),
        IpAddr::V6(v6) => format!("ws://[{v6}]:{port}"),
    }
}

/// Direct LAN provider.
#[derive(Debug, Default)]
pub struct LanProvider {
    state: Mutex<Option<Endpoint>>,
}

impl LanProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// All LAN endpoints for `port`, best first (for a QR code that lists
    /// several candidates).
    pub fn endpoints(port: u16) -> Result<Vec<Endpoint>> {
        Ok(local_addresses()?
            .into_iter()
            .map(|a| Endpoint::new(ws_url(a.ip, port), EndpointKind::Lan))
            .collect())
    }
}

#[async_trait]
impl ConnectivityProvider for LanProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Lan
    }

    fn name(&self) -> &'static str {
        "Local network"
    }

    async fn detect(&self) -> Result<Detection> {
        let addrs = local_addresses()?;
        Ok(Detection {
            installed: true,
            signed_in: None,
            version: None,
            detail: Some(if addrs.is_empty() {
                "no usable network interface".into()
            } else {
                format!("{} usable address(es)", addrs.len())
            }),
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
        let endpoint = Self::endpoints(local_port)?
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::InvalidConfig("no usable LAN address".into()))?;
        *self.state.lock().await = Some(endpoint.clone());
        Ok(endpoint)
    }

    async fn teardown(&self) -> Result<()> {
        *self.state.lock().await = None;
        Ok(())
    }

    async fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = vec![Diagnostic::info(
            "LAN endpoints have no transport TLS; traffic is protected by VCA's end-to-end encryption.",
        )];
        match local_addresses() {
            Ok(a) if a.is_empty() => {
                out.push(Diagnostic::error("No usable network interface found."))
            }
            Ok(a) => out.push(Diagnostic::info(format!(
                "Addresses: {}",
                a.iter()
                    .map(|x| x.ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            Err(e) => out.push(Diagnostic::error(format!("Could not list interfaces: {e}"))),
        }
        out.push(Diagnostic::info(
            "The phone must be on the same network, and the firewall must allow the server port.",
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(name: &str, ip: &str) -> LanAddress {
        LanAddress {
            interface: name.into(),
            ip: ip.parse().unwrap(),
        }
    }

    #[test]
    fn filters_and_orders() {
        let v = filter_and_sort(vec![
            a("lo", "127.0.0.1"),
            a("eth0", "fe80::1"),
            a("eth0", "2001:db8::5"),
            a("wlan0", "203.0.113.9"),
            a("eth0", "192.168.1.10"),
            a("tun0", "fd12::1"),
            a("lo", "::1"),
            a("x", "169.254.3.3"),
        ]);
        let ips: Vec<String> = v.iter().map(|x| x.ip.to_string()).collect();
        assert_eq!(
            ips,
            vec!["192.168.1.10", "203.0.113.9", "fd12::1", "2001:db8::5"]
        );
    }

    #[test]
    fn formats_urls() {
        assert_eq!(
            ws_url("10.0.0.2".parse().unwrap(), 47800),
            "ws://10.0.0.2:47800"
        );
        assert_eq!(ws_url("fd12::1".parse().unwrap(), 1), "ws://[fd12::1]:1");
    }

    #[tokio::test]
    async fn real_interfaces_do_not_fail() {
        let p = LanProvider::new();
        assert!(p.detect().await.unwrap().installed);
        for e in LanProvider::endpoints(47800).unwrap() {
            assert!(e.url.starts_with("ws://"));
            assert!(!e.url.contains("127.0.0.1"));
        }
    }
}
