//! Server-Side Request Forgery (SSRF) defense for the web tools.
//!
//! When the agent is handed an arbitrary URL (from a user or from a prompt), a
//! hostile input can point it at internal infrastructure: the cloud instance
//! metadata endpoint `169.254.169.254`, a private service on `10.x`/`192.168.x`,
//! loopback, or a link-local address. Resolving and fetching that URL from the
//! host running muta would leak credentials or poke internal services.
//!
//! [`assert_public_url`] resolves the host and rejects any address that is not
//! globally routable *before* the request is issued. Per-hop coverage is
//! provided by disabling reqwest's automatic redirects (the shared client is
//! built with `Policy::none()`) and following them explicitly via
//! [`crate::tools::web::client::guarded_get`], which re-runs this guard on every hop —
//! so a public URL bouncing to an internal address mid-flight is refused.
//! Remaining exotic variants (DNS rebinding between the check and the connect)
//! would require resolve-and-pin; this module closes the direct and redirect
//! vectors.

/// Reject URLs whose host resolves to a non-public IP address.
///
/// `url` must already be validated as `http(s)`. The host is extracted with the
/// `url` crate-free approach (no extra dep): we walk the authority section. On
/// any parse ambiguity the URL is rejected — fail-closed is correct for a guard.
pub(crate) async fn assert_public_url(url: &str) -> Result<(), String> {
    let host = extract_host(url)
        .ok_or_else(|| format!("SSRF guard: could not parse a host from '{url}'"))?;
    // A bracketed IPv6 literal `[::1]` — strip the brackets and parse.
    let lookup = host.trim_start_matches('[').trim_end_matches(']');

    let ips = tokio::net::lookup_host(format!("{lookup}:0"))
        .await
        .map_err(|e| format!("SSRF guard: DNS lookup for '{lookup}' failed: {e}"))?
        .map(|sa| sa.ip())
        .collect::<Vec<_>>();
    if ips.is_empty() {
        return Err(format!(
            "SSRF guard: '{lookup}' did not resolve to any address"
        ));
    }
    for ip in &ips {
        if !is_public_ip(*ip) {
            return Err(format!(
                "SSRF guard: refusing to fetch '{url}' — host '{lookup}' resolves to non-public address {ip}"
            ));
        }
    }
    Ok(())
}

pub(crate) use netune::is_public_ip;

/// Extract the host component from an `http(s)://` URL without a URL crate.
///
/// Handles `[ipv6]:port`, `host:port`, and bare `host`. Returns `None` if no
/// host is present (e.g. `http:///path`).
pub(crate) fn extract_host(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    // The authority ends at the first `/`, `?`, or `#`.
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    // Strip credentials: `user:pass@host`.
    let authority = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    // Strip the port, but keep IPv6 literals intact (`[::1]:8080`).
    if authority.starts_with('[') {
        if let Some(end) = authority.find(']') {
            return Some(authority[..=end].to_string());
        }
        return None;
    }
    let host = authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_public_classifies_each_range() {
        // Public addresses pass.
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("93.184.216.34".parse().unwrap()));
        // Private / loopback / link-local / metadata are blocked.
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("172.16.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(!is_public_ip("169.254.0.1".parse().unwrap()));
        assert!(!is_public_ip("0.0.0.0".parse().unwrap()));
        // IPv6.
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(!is_public_ip("::".parse().unwrap()));
        assert!(!is_public_ip("fd00::1".parse().unwrap()));
        assert!(!is_public_ip("fe80::1".parse().unwrap()));
        assert!(!is_public_ip("::ffff:10.0.0.1".parse().unwrap()));
        // Reserved / special-purpose v4 the basic checks used to miss.
        assert!(!is_public_ip("240.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("255.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.0.2.1".parse().unwrap())); // TEST-NET-1
        assert!(!is_public_ip("198.51.100.1".parse().unwrap())); // TEST-NET-2
        assert!(!is_public_ip("203.0.113.1".parse().unwrap())); // TEST-NET-3
        assert!(!is_public_ip("100.64.0.1".parse().unwrap())); // CGNAT
    }

    #[test]
    fn extract_host_handles_ports_credentials_and_ipv6() {
        assert_eq!(
            extract_host("https://example.com/path"),
            Some("example.com".into())
        );
        assert_eq!(
            extract_host("http://example.com:8080/p?q=1"),
            Some("example.com".into())
        );
        assert_eq!(
            extract_host("https://user:pass@host.example/x"),
            Some("host.example".into())
        );
        assert_eq!(extract_host("http://[::1]:9000/x"), Some("[::1]".into()));
        assert_eq!(extract_host("http:///path"), None);
        assert_eq!(extract_host("not a url"), None);
    }
}
