//! Server-Side Request Forgery (SSRF) defense for untrusted model-supplied URLs.
//!
//! When the agent is handed an arbitrary URL (from a prompt, web page, or
//! redirect), a hostile input can point it at internal infrastructure: the
//! cloud instance metadata endpoint `169.254.169.254`, a private service on
//! `10.x`/`172.16.x`/`192.168.x`, loopback, or a link-local address. Resolving
//! and fetching that URL from the host running nuo would leak credentials or
//! poke internal services.
//!
//! [`SsrfGuardedResolver`] enforces this policy directly at `netune`'s DNS
//! resolution seam inside [`crate::http::GuardedWebHttp`], so every initial
//! request and every redirect hop of [`crate::client::guarded_get`] validates
//! the exact addresses passed to TCP connect (eliminating DNS-rebinding TOCTOU).
//! [`assert_public_url`] provides the matching pre-flight check before
//! third-party readers (`read_url` via Jina) receive a target URL. Configured
//! service endpoints (search backends, Jina, self-hosted SearXNG) use
//! unrestricted [`crate::http::WebHttp`] and do **not** go through this guard.
//!
//! Transparent FakeIP proxies (Clash/Mihomo/sing-box/Surge) synthesize
//! `198.18.0.0/15` (RFC 2544 benchmarking space) — and on dual-stack configs,
//! paired IPv6 addresses in `fc00::/18` — for public domain lookups.
//! [`validate_resolved_ips`] permits `198.18.0.0/15` (and paired `fc00::/18`
//! when an IPv4 FakeIP is present in the same answer) **only** when resolved
//! from a genuine domain name. Direct FakeIP literals (standard, IPv4-mapped
//! IPv6, and legacy numeric/hex/octal IPv4 forms), standalone `fc00::/18`, and
//! all private, loopback, link-local, metadata, CGNAT, and reserved addresses
//! remain rejected.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;

pub use netune::is_public_ip;
use netune::{DnsResolver, NetError, SystemResolver};

/// [`DnsResolver`] that enforces [`validate_resolved_ips`] on every connection
/// and every redirect hop immediately before TCP dial.
///
/// Because `netune` dials the exact [`SocketAddr`] list returned by this
/// resolver without re-resolving, placing the SSRF check here closes both
/// redirect-hop bypasses and DNS-rebinding (TOCTOU) gaps in one place.
#[derive(Debug, Clone, Copy, Default)]
pub struct SsrfGuardedResolver;

impl DnsResolver for SsrfGuardedResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, NetError>> + Send + 'a>> {
        Box::pin(async move {
            let lookup = host
                .strip_prefix('[')
                .and_then(|inner| inner.strip_suffix(']'))
                .unwrap_or(host);
            let authority = format!("{lookup}:{port}");

            if !is_safe_lookup_host(lookup) {
                return Err(NetError::Security(format!(
                    "SSRF guard: invalid host '{lookup}' for '{authority}'"
                )));
            }

            if let Ok(ip) = lookup.parse::<IpAddr>() {
                validate_resolved_ips(&authority, lookup, &[ip]).map_err(NetError::Security)?;
                return Ok(vec![SocketAddr::new(ip, port)]);
            }

            let addrs = SystemResolver.resolve(lookup, port).await?;
            let ips: Vec<IpAddr> = addrs.iter().map(SocketAddr::ip).collect();
            validate_resolved_ips(&authority, lookup, &ips).map_err(NetError::Security)?;
            Ok(addrs)
        })
    }
}

/// True for addresses in RFC 2544 (`198.18.0.0/15`) or its IPv4-mapped IPv6
/// form (`::ffff:198.18.0.0/111`), which local transparent proxies allocate as
/// synthetic IPv4 FakeIPs for public domain names.
pub fn is_fake_ip(ip: IpAddr) -> bool {
    let v4 = match ip {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4,
            None => return false,
        },
    };
    let [a, b, _, _] = v4.octets();
    a == 198 && (b == 18 || b == 19)
}

/// True for IPv6 addresses in `fc00::/18` — the unassigned `L=0` slice of
/// RFC 4193 used by dual-stack FakeIP proxies (Mihomo / sing-box) alongside
/// `198.18.0.0/15`. Only permitted when paired with an IPv4 FakeIP in the same
/// domain resolution so standalone internal ULA networks stay blocked.
fn is_fake_ip_v6(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V6(v6) => {
            let [s0, s1, ..] = v6.segments();
            s0 == 0xfc00 && (s1 & 0xc000) == 0
        }
        IpAddr::V4(_) => false,
    }
}

/// Reject empty hosts and characters that invite URL/resolver parser
/// differentials (control chars, whitespace, backslashes, userinfo `@`,
/// percent-encoding / IPv6 zone IDs `%`).
fn is_safe_lookup_host(host: &str) -> bool {
    !host.is_empty()
        && !host
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '\\' | '@' | '%'))
}

/// True when `host` is a literal IP address — standard IPv4/IPv6 or a legacy
/// BSD `inet_aton` numeric/hex/octal IPv4 form (e.g. `3323068619`,
/// `0xc61200cb`, `198.18.203`, `0306.022.0.0313`).
fn is_ip_literal(host: &str) -> bool {
    let trimmed = host.strip_suffix('.').unwrap_or(host);
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.parse::<IpAddr>().is_ok() {
        return true;
    }
    let mut parts = 0usize;
    for part in trimmed.split('.') {
        parts += 1;
        if parts > 4 || !is_numeric_ipv4_part(part) {
            return false;
        }
    }
    parts > 0
}

fn is_numeric_ipv4_part(part: &str) -> bool {
    if part.is_empty() {
        return false;
    }
    if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    part.bytes().all(|b| b.is_ascii_digit())
}

/// Validate a resolved address list for `host` in `url`.
///
/// Public addresses are always allowed. FakeIP (`198.18.0.0/15`, plus paired
/// dual-stack `fc00::/18` when a `198.18.0.0/15` address is present in the
/// same resolution) is allowed only when `host` is a domain name rather than
/// an IP literal. Every resolved address must pass so mixed public/private DNS
/// responses cannot smuggle an internal target through.
pub(crate) fn validate_resolved_ips(url: &str, host: &str, ips: &[IpAddr]) -> Result<(), String> {
    if ips.is_empty() {
        return Err(format!(
            "SSRF guard: '{host}' did not resolve to any address"
        ));
    }
    let literal = is_ip_literal(host);
    let has_paired_fake_v4 = !literal && ips.iter().any(|&ip| is_fake_ip(ip));
    for &ip in ips {
        let allowed = is_public_ip(ip)
            || (!literal && is_fake_ip(ip))
            || (has_paired_fake_v4 && is_fake_ip_v6(ip));
        if !allowed {
            return Err(format!(
                "SSRF guard: refusing to fetch '{url}' — host '{host}' resolves to non-public address {ip}"
            ));
        }
    }
    Ok(())
}

/// Reject untrusted `http(s)` URLs whose host is or resolves to a non-public
/// IP address (with domain-resolved FakeIP `198.18.0.0/15` and paired
/// dual-stack `fc00::/18` permitted for local transparent proxies).
///
/// On any parse ambiguity (unsupported scheme, userinfo `@`, percent/zone `%`,
/// backslash `\`, whitespace, or control chars) the URL is rejected —
/// fail-closed is correct for a guard.
pub async fn assert_public_url(url: &str) -> Result<(), String> {
    let after_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| format!("SSRF guard: refusing unsupported scheme in '{url}'"))?;
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("SSRF guard: could not parse a host from '{url}'"))?;
    if authority.contains('@') {
        return Err(format!("SSRF guard: refusing URL with userinfo in '{url}'"));
    }
    let host = extract_host(url)
        .ok_or_else(|| format!("SSRF guard: could not parse a host from '{url}'"))?;
    // A bracketed IPv6 literal `[::1]` — strip the enclosing brackets.
    let lookup = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(&host);
    if !is_safe_lookup_host(lookup) {
        return Err(format!("SSRF guard: invalid host '{lookup}' in '{url}'"));
    }

    // Standard IP literals need no DNS round-trip and are always checked
    // strictly as literals (never eligible for FakeIP allowance).
    if let Ok(ip) = lookup.parse::<IpAddr>() {
        return validate_resolved_ips(url, lookup, &[ip]);
    }

    let ips = tokio::net::lookup_host(format!("{lookup}:0"))
        .await
        .map_err(|e| format!("SSRF guard: DNS lookup for '{lookup}' failed: {e}"))?
        .map(|sa| sa.ip())
        .collect::<Vec<_>>();
    validate_resolved_ips(url, lookup, &ips)
}

/// Extract the host component from an `http(s)://` URL without a URL crate.
///
/// Handles `[ipv6]:port`, `host:port`, and bare `host`. Returns `None` if no
/// host is present (e.g. `http:///path`).
pub fn extract_host(url: &str) -> Option<String> {
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
        assert!(!is_public_ip("198.18.0.203".parse().unwrap())); // RFC 2544
        assert!(!is_public_ip("198.51.100.1".parse().unwrap())); // TEST-NET-2
        assert!(!is_public_ip("203.0.113.1".parse().unwrap())); // TEST-NET-3
        assert!(!is_public_ip("100.64.0.1".parse().unwrap())); // CGNAT
    }

    #[test]
    fn is_fake_ip_matches_rfc2544_and_v4_mapped_only() {
        assert!(is_fake_ip("198.18.0.0".parse().unwrap()));
        assert!(is_fake_ip("198.18.0.203".parse().unwrap()));
        assert!(is_fake_ip("198.19.255.255".parse().unwrap()));
        assert!(is_fake_ip("::ffff:198.18.0.203".parse().unwrap()));
        assert!(is_fake_ip("::ffff:198.19.0.1".parse().unwrap()));

        assert!(!is_fake_ip("198.17.255.255".parse().unwrap()));
        assert!(!is_fake_ip("198.20.0.0".parse().unwrap()));
        assert!(!is_fake_ip("198.51.100.1".parse().unwrap()));
        assert!(!is_fake_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_fake_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_fake_ip("169.254.169.254".parse().unwrap()));
        assert!(!is_fake_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_fake_ip("fc00::1".parse().unwrap()));
        assert!(!is_fake_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn is_fake_ip_v6_matches_fc00_slash_18_only() {
        assert!(is_fake_ip_v6("fc00::1".parse().unwrap()));
        assert!(is_fake_ip_v6("fc00:3fff:ffff::1".parse().unwrap()));
        assert!(!is_fake_ip_v6("fc00:4000::1".parse().unwrap()));
        assert!(!is_fake_ip_v6("fc01::1".parse().unwrap()));
        assert!(!is_fake_ip_v6("fd00::1".parse().unwrap()));
        assert!(!is_fake_ip_v6("fe80::1".parse().unwrap()));
        assert!(!is_fake_ip_v6("::1".parse().unwrap()));
        assert!(!is_fake_ip_v6("198.18.0.203".parse().unwrap()));
    }

    #[test]
    fn is_ip_literal_catches_standard_and_obfuscated_literals() {
        assert!(is_ip_literal("198.18.0.203"));
        assert!(is_ip_literal("198.18.0.203."));
        assert!(is_ip_literal("::ffff:198.18.0.203"));
        assert!(is_ip_literal("127.0.0.1"));
        assert!(is_ip_literal("::1"));
        assert!(is_ip_literal("3323068619")); // decimal 198.18.0.203
        assert!(is_ip_literal("0xc61200cb")); // hex 198.18.0.203
        assert!(is_ip_literal("0XC6.0x12.0x00.0xcb"));
        assert!(is_ip_literal("198.18.203")); // 3-part IPv4 shorthand
        assert!(is_ip_literal("0306.022.0.0313")); // octal 198.18.0.203

        assert!(!is_ip_literal("mcp.exa.ai"));
        assert!(!is_ip_literal("docs.rs"));
        assert!(!is_ip_literal("123.example.com"));
        assert!(!is_ip_literal("xn--bcher-kva.example"));
        assert!(!is_ip_literal("localhost"));
    }

    #[test]
    fn validate_resolved_ips_allows_domain_fake_ip_but_rejects_literals_and_private() {
        let fake_v4: IpAddr = "198.18.0.203".parse().unwrap();
        let fake_v6_mapped: IpAddr = "::ffff:198.18.0.203".parse().unwrap();
        let fake_v6: IpAddr = "fc00::cb".parse().unwrap();
        let outside_fake_v6: IpAddr = "fc00:4000::1".parse().unwrap();
        let ula_fd: IpAddr = "fd00::1".parse().unwrap();
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let metadata: IpAddr = "169.254.169.254".parse().unwrap();

        // Domain names resolving to public, IPv4 FakeIP, or dual-stack paired
        // IPv4+IPv6 FakeIP are allowed.
        assert!(validate_resolved_ips("https://docs.rs/", "docs.rs", &[public]).is_ok());
        assert!(validate_resolved_ips("https://docs.rs/", "docs.rs", &[fake_v4]).is_ok());
        assert!(validate_resolved_ips("https://docs.rs/", "docs.rs", &[fake_v6_mapped]).is_ok());
        assert!(validate_resolved_ips("https://docs.rs/", "docs.rs", &[fake_v4, fake_v6]).is_ok());

        // Standalone fc00::/18 (without paired IPv4 FakeIP), out-of-range
        // fc00:4000::, fd00:: ULA, or mixed FakeIP + loopback/metadata are refused.
        assert!(validate_resolved_ips("https://lan.example/", "lan.example", &[fake_v6]).is_err());
        assert!(
            validate_resolved_ips(
                "https://evil.example/",
                "evil.example",
                &[fake_v4, outside_fake_v6],
            )
            .is_err()
        );
        assert!(
            validate_resolved_ips("https://evil.example/", "evil.example", &[fake_v4, ula_fd])
                .is_err()
        );
        assert!(
            validate_resolved_ips(
                "https://evil.example/",
                "evil.example",
                &[fake_v4, loopback],
            )
            .is_err()
        );
        assert!(
            validate_resolved_ips("https://evil.example/", "evil.example", &[metadata]).is_err()
        );

        // Direct FakeIP literals (standard, IPv4-mapped, IPv6, and obfuscated) are refused.
        assert!(validate_resolved_ips("http://198.18.0.203/", "198.18.0.203", &[fake_v4]).is_err());
        assert!(validate_resolved_ips("http://[fc00::cb]/", "fc00::cb", &[fake_v6]).is_err());
        assert!(
            validate_resolved_ips(
                "http://[::ffff:198.18.0.203]/",
                "::ffff:198.18.0.203",
                &[fake_v6_mapped],
            )
            .is_err()
        );
        assert!(validate_resolved_ips("http://3323068619/", "3323068619", &[fake_v4]).is_err());
        assert!(validate_resolved_ips("http://0xc61200cb/", "0xc61200cb", &[fake_v4]).is_err());

        // Empty resolution fails closed.
        assert!(validate_resolved_ips("https://docs.rs/", "docs.rs", &[]).is_err());
    }

    #[tokio::test]
    async fn assert_public_url_rejects_bad_schemes_userinfo_zones_and_fake_ip_literals() {
        for blocked in [
            "http://198.18.0.203/",
            "http://198.19.255.254/path",
            "http://[::ffff:198.18.0.203]/",
            "http://[fc00::cb]/",
            "http://[::ffff:198.18.0.203%eth0]/",
            "http://[fe80::1%eth0]/",
            "http://127.0.0.1/",
            "http://[::1]:8080/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "file:///etc/passwd",
            "HTTP://93.184.216.34/",
            "https://127.0.0.1:80@93.184.216.34/",
            "https://user:pass@93.184.216.34/",
            "http://evil.com\\@127.0.0.1/",
        ] {
            let err = assert_public_url(blocked)
                .await
                .expect_err(&format!("expected {blocked} to be rejected"));
            assert!(err.contains("SSRF guard"), "got: {err}");
        }
        assert!(assert_public_url("https://93.184.216.34/").await.is_ok());
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
