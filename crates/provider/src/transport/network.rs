//! Direct network construction and process-wide connection pooling.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use netune::{Client, ClientConfig, SocketConfig};

/// TCP keepalive idle interval for outbound provider sockets (ADR-0224).
///
/// Surfaces NAT- or proxy-culled half-open connections via the OS stack rather
/// than waiting passively for the 60 s `body_idle_timeout`.
pub const TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(30);

static SHARED_H2_CLIENT: OnceLock<Arc<Client>> = OnceLock::new();
static SHARED_H1_CLIENT: OnceLock<Arc<Client>> = OnceLock::new();

fn default_socket_config() -> SocketConfig {
    let config = SocketConfig::default();
    #[cfg(unix)]
    let config = config.with_keepalive(TCP_KEEPALIVE_IDLE);
    config
}

/// Build a direct netune client with platform certificate verification.
///
/// The builder installs the audited rustls platform-verifier path and aligns
/// the ALPN offer with `config.enable_http2`, so a request's named TLS profile
/// ([`RequestBuilder::tls_profile`](super::RequestBuilder::tls_profile)) is
/// carried by the router while plain targets remain on rustls.
pub fn direct_client(config: ClientConfig) -> Client {
    Client::builder()
        .config(config)
        .socket_config(default_socket_config())
        .build()
}

fn init_shared_client(enable_http2: bool) -> Arc<Client> {
    let mut config = ClientConfig::default()
        .with_enable_http2(enable_http2)
        // Nuo owns the total budget: non-streaming model calls pass their
        // `CHAT_REQUEST_TIMEOUT`, and a streaming turn deliberately has no
        // overall deadline.
        .with_request_timeout(None)
        // netune's 30 s default head bound is shorter than a non-streaming
        // model answer may take: the server sends the response head only once
        // the answer is ready. Align it with the whole-request budget so the
        // 300 s deadline is the effective limit; the 60 s body-idle bound
        // remains the streaming liveness check.
        .with_response_head_timeout(Some(super::client::CHAT_REQUEST_TIMEOUT))
        // The provider transport and control-plane handle both opt into
        // `send_traced` per attempt. Direct callers (MCP, remote skills,
        // web fetch) use `Client::request`: clock-free L0 counters, no
        // event ring, but still a generated trace id and the diagnostic
        // table netune returns on failure.
        .with_observation(netune::Observation::Counters);
    // The HTTP/1.1 client exists for the Antigravity lanes, whose real
    // client is a Go `net/http` transport: it offers `accept-encoding: gzip`
    // on every request and decodes what it offered (live `agy` 1.5.12
    // capture, 2026-11-19). The h2-capable shared client serves every other
    // provider and keeps its own quieter identity.
    if !enable_http2 {
        config = config.with_accept_encoding_gzip(true);
    }
    Arc::new(direct_client(config))
}

/// The process-wide shared netune client.
///
/// Every model provider, catalog query, and authentication call converges on
/// this single client instance to share socket pools and TLS session caches,
/// avoiding redundant DNS/TCP/TLS handshakes to common provider endpoints.
pub fn shared_client() -> &'static Arc<Client> {
    SHARED_H2_CLIENT.get_or_init(|| init_shared_client(true))
}

/// Process-wide shared HTTP/1.1-only netune client (`enable_http2: false`).
///
/// Used for TLS profiles such as `antigravity-cli-unary` that offer only
/// `http/1.1` ALPN and intentionally pair with no HTTP/2 connection profile in
/// netune's registry (`[INV-TLS-10]`).
pub fn shared_http1_client() -> &'static Arc<Client> {
    SHARED_H1_CLIENT.get_or_init(|| init_shared_client(false))
}

/// Immediately evict all idle and shared connections from the process-wide
/// provider pools without initializing unused clients or disturbing in-flight
/// streams (ADR-0224).
///
/// Called on transport-level retry backoff so a Wi-Fi or VPN transition drops
/// stranded sockets before the next attempt dials.
pub fn clear_shared_pools() -> usize {
    let h2 = SHARED_H2_CLIENT.get().map_or(0, |c| c.clear_pool());
    let h1 = SHARED_H1_CLIENT.get().map_or(0, |c| c.clear_pool());
    h2 + h1
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn shared_client_is_a_singleton() {
        let a = shared_client();
        let b = shared_client();
        assert!(Arc::ptr_eq(a, b), "two calls must return the same client");
        assert!(a.config().enable_http2, "the model path negotiates HTTP/2");
        assert_eq!(
            a.config().request_timeout,
            None,
            "Nuo owns the whole-attempt budget: streaming turns have no overall deadline"
        );
        assert_eq!(
            a.config().response_head_timeout,
            Some(Duration::from_secs(300)),
            "the head bound must not cut off a non-streaming answer before its 300 s budget"
        );
        assert_eq!(
            a.config().body_idle_timeout,
            Some(Duration::from_secs(60)),
            "the 60 s body-idle bound is the streaming liveness check"
        );
        assert!(
            !shared_http1_client().config().enable_http2,
            "the unary fallback must offer HTTP/1.1 only"
        );
        assert!(
            shared_http1_client().config().accept_encoding_gzip,
            "the Antigravity HTTP/1.1 lanes adopt the Go transport's gzip offer"
        );
        assert!(
            !shared_client().config().accept_encoding_gzip,
            "the shared h2 client keeps its own quieter identity"
        );
        #[cfg(unix)]
        assert_eq!(default_socket_config().keepalive, Some(TCP_KEEPALIVE_IDLE));
        // Flushing empty shared pools is a safe zero-eviction operation.
        assert_eq!(clear_shared_pools(), 0);
    }
}
