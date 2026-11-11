//! Transport options of client WebSocket connections: a TLS client
//! certificate (mutual TLS) and an extra bearer header on the upgrade
//! request.
//!
//! Both exist for transports that admit only known clients before any byte
//! reaches the server, such as DNSPilot `app`-mode tunnels: the DNSPilot
//! Agent on the server's PC asks for a client certificate during the TLS
//! handshake and compares the SHA-256 of its SubjectPublicKeyInfo with the
//! tunnel's list of app clients (self-signed certificates are fine; the key
//! is pinned). Optionally a tunnel also accepts `Authorization: Bearer
//! dpt_app_...` tokens. See <https://dns.dnspilot.kr/docs/en/tunnel-app-mode>.
//!
//! These options only decide whether the transport lets the connection
//! through. They never replace VCA's own authentication: the Noise channel
//! with the pinned server key, pairing and device tokens apply unchanged on
//! top of every transport.

use zeroize::Zeroizing;

use crate::error::{NetError, Result};

/// A TLS client certificate and its private key, presented on `wss://`
/// connections when the server asks for one. `Debug` output is redacted.
#[derive(Clone)]
pub struct TlsClientIdentity {
    /// DER certificates, leaf first (a single self-signed certificate is
    /// enough for key pinning).
    pub cert_chain_der: Vec<Vec<u8>>,
    /// PKCS#8 DER private key of the leaf certificate.
    pub private_key_pkcs8_der: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for TlsClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsClientIdentity")
            .field("certificates", &self.cert_chain_der.len())
            .field("private_key", &"<redacted>")
            .finish()
    }
}

/// Transport-level options of one client connection. The default (no client
/// certificate, no bearer header, the public web PKI roots) is what every
/// ordinary endpoint uses.
#[derive(Clone, Default)]
pub struct TransportOptions {
    /// Client certificate for mutual TLS (`wss://` only).
    pub client_identity: Option<TlsClientIdentity>,
    /// Sent as `Authorization: Bearer <value>` on the WebSocket upgrade
    /// request (`wss://` only, never over plain `ws://`).
    pub bearer: Option<Zeroizing<String>>,
    /// Additional trusted root certificates (DER), on top of the public web
    /// PKI roots. For private deployments and tests.
    pub extra_root_certs: Vec<Vec<u8>>,
}

impl std::fmt::Debug for TransportOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportOptions")
            .field("client_identity", &self.client_identity)
            .field("bearer", &self.bearer.as_ref().map(|_| "<redacted>"))
            .field("extra_root_certs", &self.extra_root_certs.len())
            .finish()
    }
}

impl TransportOptions {
    /// True when nothing differs from a plain connection.
    pub fn is_default(&self) -> bool {
        self.client_identity.is_none() && self.bearer.is_none() && self.extra_root_certs.is_empty()
    }
}

/// Builds the WebSocket upgrade request for `url` with `options`.
pub(crate) fn upgrade_request(
    url: &str,
    options: &TransportOptions,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    let secure = url.starts_with("wss://");
    if !secure && !options.is_default() {
        return Err(NetError::InvalidArgument(
            "client certificates and bearer tokens are only sent over wss://".into(),
        ));
    }
    let mut request = url
        .into_client_request()
        .map_err(|e| NetError::InvalidArgument(format!("invalid endpoint URL: {e}")))?;
    if let Some(token) = &options.bearer {
        let mut value = HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
            .map_err(|_| NetError::InvalidArgument("invalid bearer token".into()))?;
        value.set_sensitive(true);
        request.headers_mut().insert("authorization", value);
    }
    Ok(request)
}

/// The TLS connector for `options`, or `None` for the library default.
#[cfg(feature = "client-tls")]
pub(crate) fn connector(
    options: &TransportOptions,
) -> Result<Option<tokio_tungstenite::Connector>> {
    use std::sync::Arc;

    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    if options.client_identity.is_none() && options.extra_root_certs.is_empty() {
        return Ok(None);
    }
    let tls_err = |e: rustls::Error| NetError::InvalidArgument(format!("TLS configuration: {e}"));
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for der in &options.extra_root_certs {
        roots
            .add(CertificateDer::from(der.clone()))
            .map_err(tls_err)?;
    }
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?
        .with_root_certificates(roots);
    let config = match &options.client_identity {
        Some(identity) => {
            if identity.cert_chain_der.is_empty() {
                return Err(NetError::InvalidArgument(
                    "the client identity has no certificate".into(),
                ));
            }
            let chain = identity
                .cert_chain_der
                .iter()
                .map(|c| CertificateDer::from(c.clone()))
                .collect();
            let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                identity.private_key_pkcs8_der.to_vec(),
            ));
            builder.with_client_auth_cert(chain, key).map_err(tls_err)?
        }
        None => builder.with_no_client_auth(),
    };
    Ok(Some(tokio_tungstenite::Connector::Rustls(Arc::new(config))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_is_added_and_redacted() {
        let options = TransportOptions {
            bearer: Some(Zeroizing::new("dpt_app_secret".into())),
            ..TransportOptions::default()
        };
        let req = upgrade_request("wss://vca.example.com/v1/ws", &options).unwrap();
        let value = req.headers().get("authorization").unwrap();
        assert_eq!(value.to_str().unwrap(), "Bearer dpt_app_secret");
        assert!(value.is_sensitive());
        assert!(!format!("{options:?}").contains("secret"));
    }

    #[test]
    fn credentials_are_never_sent_over_plain_ws() {
        let options = TransportOptions {
            bearer: Some(Zeroizing::new("dpt_app_x".into())),
            ..TransportOptions::default()
        };
        assert!(upgrade_request("ws://127.0.0.1:47800/v1/ws", &options).is_err());
        assert!(
            upgrade_request("ws://127.0.0.1:47800/v1/ws", &TransportOptions::default()).is_ok()
        );
    }
}
