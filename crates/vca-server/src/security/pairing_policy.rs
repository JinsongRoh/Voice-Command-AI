//! When a pairing code may be created (TOTP for remote pairing).
//!
//! * A pairing code whose endpoints reach beyond the local network (a
//!   tunnel, a public address, a mesh VPN) needs an enrolled authenticator.
//!   Without one the "connect a phone" flow first walks the owner through
//!   TOTP setup.
//! * A LAN-only code also needs one while `remote.require_totp` is on (the
//!   default): sign-in would fail without it anyway. Only an owner who
//!   turned `require_totp` off may pair over the LAN without TOTP, and the
//!   app then strongly recommends setting it up.

use std::net::IpAddr;

/// What creating a pairing code needs right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRequirement {
    /// Go ahead.
    Allowed,
    /// Go ahead, but recommend 2-step verification (LAN only, TOTP off).
    AllowedRecommendTotp,
    /// Set up 2-step verification first.
    TotpSetupRequired,
}

/// Is `ip` on this computer or the local network? Mesh VPN ranges
/// (100.64.0.0/10) count as remote.
fn local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || (seg0 & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg0 & 0xffc0) == 0xfe80 // link local fe80::/10
        }
    }
}

/// Does `endpoint` (a `ws://` / `wss://` URL or a bare host) stay on the
/// local network?
pub fn is_lan_endpoint(endpoint: &str) -> bool {
    let host = match reqwest::Url::parse(endpoint.trim()) {
        Ok(u) => u.host_str().map(str::to_owned),
        Err(_) => Some(endpoint.trim().to_owned()),
    };
    let Some(host) = host else {
        return false;
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".local") || host.ends_with(".lan") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(local_ip)
}

/// The rule above. An empty endpoint list counts as remote (the provider
/// decides later).
pub fn pairing_requirement(
    endpoints: &[String],
    require_totp: bool,
    totp_enrolled: bool,
) -> PairingRequirement {
    if totp_enrolled {
        return PairingRequirement::Allowed;
    }
    let lan_only = !endpoints.is_empty() && endpoints.iter().all(|e| is_lan_endpoint(e));
    if !lan_only || require_totp {
        PairingRequirement::TotpSetupRequired
    } else {
        PairingRequirement::AllowedRecommendTotp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_endpoints() {
        for lan in [
            "ws://192.168.0.10:47800/v1/ws",
            "ws://10.1.2.3/v1/ws",
            "ws://172.20.0.1/v1/ws",
            "ws://127.0.0.1:47800/v1/ws",
            "ws://[fe80::1]/v1/ws",
            "ws://[fd00::5]/v1/ws",
            "ws://desk.local/v1/ws",
            "169.254.3.4",
        ] {
            assert!(is_lan_endpoint(lan), "{lan}");
        }
        for remote in [
            "wss://vca.example.com/v1/ws",
            "ws://100.101.102.103/v1/ws",
            "wss://desk.tailnet-1234.ts.net/v1/ws",
            "ws://203.0.113.9/v1/ws",
            "ws://[2001:db8::1]/v1/ws",
        ] {
            assert!(!is_lan_endpoint(remote), "{remote}");
        }
    }

    #[test]
    fn totp_is_required_for_remote_pairing() {
        let lan = vec!["ws://192.168.0.10:47800/v1/ws".to_owned()];
        let remote = vec!["wss://vca.example.com/v1/ws".to_owned()];
        let mixed = [lan.clone(), remote.clone()].concat();
        use PairingRequirement::*;
        assert_eq!(pairing_requirement(&remote, false, true), Allowed);
        assert_eq!(
            pairing_requirement(&remote, false, false),
            TotpSetupRequired
        );
        assert_eq!(pairing_requirement(&mixed, false, false), TotpSetupRequired);
        assert_eq!(pairing_requirement(&[], false, false), TotpSetupRequired);
        // LAN only: the default (require_totp) still needs it.
        assert_eq!(pairing_requirement(&lan, true, false), TotpSetupRequired);
        assert_eq!(
            pairing_requirement(&lan, false, false),
            AllowedRecommendTotp
        );
        assert_eq!(pairing_requirement(&lan, true, true), Allowed);
    }
}
