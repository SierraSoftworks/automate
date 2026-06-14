//! Shared helpers for interpreting incoming HTTP requests.
//!
//! These honour the `web.trust_proxy` and `web.base_url` configuration so that
//! the scheme and host we derive from a request are only influenced by proxy
//! forwarding headers when we have been explicitly told to trust them. They are
//! used by both the OIDC admin authentication flow and the OAuth client flow.

use std::net::SocketAddr;

use actix_web::http::header::HeaderMap;

use crate::prelude::Services;

/// Returns the value of the named header as a string, if present and valid.
pub fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Determines the client IP address to evaluate the admin ACL against.
///
/// When the deployment trusts its reverse proxy, the left-most `X-Forwarded-For`
/// entry (the original client as recorded by the proxy) is used; the left-most
/// entry mirrors how [`is_https`] reads `X-Forwarded-Proto`. When the proxy is
/// **not** trusted the header is ignored entirely and the direct socket peer is
/// used, so a client cannot spoof its address by sending `X-Forwarded-For`.
///
/// Because the left-most value is taken, `trust_proxy` must only be enabled
/// behind a proxy that overwrites (or otherwise sanitises) any inbound
/// `X-Forwarded-For` from the client, exactly as its configuration documentation
/// requires.
pub fn client_ip(
    trust_proxy: bool,
    headers: &HeaderMap,
    peer_addr: Option<SocketAddr>,
) -> Option<String> {
    if trust_proxy
        && let Some(client) = header_str(headers, "x-forwarded-for")
            .and_then(|forwarded| forwarded.split(',').next())
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
    {
        return Some(client.to_string());
    }

    peer_addr.map(|addr| addr.ip().to_string())
}

/// Determines whether the original request reached us over HTTPS. The
/// `x-forwarded-proto` header (set by reverse proxies) is only consulted when
/// the deployment is configured to trust its proxy.
pub fn is_https(trust_proxy: bool, headers: &HeaderMap, uri_scheme: Option<&str>) -> bool {
    if trust_proxy && let Some(proto) = header_str(headers, "x-forwarded-proto") {
        // The header may contain a comma-separated list when multiple proxies
        // are chained; the left-most entry is the closest to the client.
        return proto
            .split(',')
            .next()
            .map(|p| p.trim().eq_ignore_ascii_case("https"))
            .unwrap_or(false);
    }

    uri_scheme == Some("https")
}

/// Computes the externally visible base URL of the service, preferring the
/// explicitly configured `web.base_url` and otherwise reconstructing it from
/// the request's host and scheme. Forwarding headers (`x-forwarded-host`,
/// `x-forwarded-proto`) are only trusted when `web.trust_proxy` is enabled.
pub fn base_url<S: Services>(
    services: &S,
    headers: &HeaderMap,
    uri_scheme: Option<&str>,
) -> Option<String> {
    if let Some(base_url) = &services.config().web.base_url {
        return Some(base_url.trim_end_matches('/').to_string());
    }

    let trust_proxy = services.config().web.trust_proxy;
    let host = if trust_proxy {
        header_str(headers, "x-forwarded-host").or_else(|| header_str(headers, "host"))
    } else {
        header_str(headers, "host")
    }?;
    let scheme = if is_https(trust_proxy, headers, uri_scheme) {
        "https"
    } else {
        "http"
    };

    Some(format!("{scheme}://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::header::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn is_https_prefers_forwarded_proto_when_trusted() {
        assert!(is_https(
            true,
            &headers(&[("x-forwarded-proto", "https")]),
            None
        ));
        assert!(is_https(
            true,
            &headers(&[("x-forwarded-proto", "https, http")]),
            None
        ));
        assert!(!is_https(
            true,
            &headers(&[("x-forwarded-proto", "http")]),
            None
        ));
        assert!(!is_https(true, &headers(&[]), Some("http")));
        assert!(is_https(true, &headers(&[]), Some("https")));
    }

    #[test]
    fn is_https_ignores_forwarded_proto_when_proxy_untrusted() {
        // A spoofed header must not be able to convince us the request was
        // secure when we are not configured to trust a proxy.
        assert!(!is_https(
            false,
            &headers(&[("x-forwarded-proto", "https")]),
            Some("http")
        ));
        assert!(is_https(false, &headers(&[]), Some("https")));
    }

    fn peer(addr: &str) -> Option<SocketAddr> {
        Some(addr.parse().unwrap())
    }

    #[test]
    fn client_ip_uses_forwarded_for_when_trusted() {
        // The left-most entry is the original client as seen by the trusted proxy.
        assert_eq!(
            client_ip(
                true,
                &headers(&[("x-forwarded-for", "203.0.113.5, 10.0.0.1")]),
                peer("10.0.0.1:443")
            ),
            Some("203.0.113.5".to_string())
        );
        // Falls back to the socket peer when no forwarding header is present.
        assert_eq!(
            client_ip(true, &headers(&[]), peer("198.51.100.7:1234")),
            Some("198.51.100.7".to_string())
        );
    }

    #[test]
    fn client_ip_ignores_forwarded_for_when_proxy_untrusted() {
        // A spoofed X-Forwarded-For must not override the real socket peer when we
        // are not configured to trust a proxy — otherwise an attacker could forge
        // an allow-listed client_ip in the admin ACL.
        assert_eq!(
            client_ip(
                false,
                &headers(&[("x-forwarded-for", "127.0.0.1")]),
                peer("203.0.113.9:5555")
            ),
            Some("203.0.113.9".to_string())
        );
        assert_eq!(client_ip(false, &headers(&[]), None), None);
    }
}
