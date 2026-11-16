//! Direct network construction and process-wide connection pooling.

use std::sync::{Arc, OnceLock};

use netune::{Client, ClientConfig};

/// Build a direct netune client with platform certificate verification.
///
/// The builder installs the audited rustls platform-verifier path and aligns
/// the ALPN offer with `config.enable_http2`, so a request's named TLS profile
/// ([`RequestBuilder::tls_profile`](super::RequestBuilder::tls_profile)) is
/// carried by the router while plain targets remain on rustls.
pub fn direct_client(config: ClientConfig) -> Client {
    Client::builder().config(config).build()
}

fn init_shared_client(enable_http2: bool) -> Arc<Client> {
    let config = ClientConfig::default()
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
    Arc::new(direct_client(config))
}

/// The process-wide shared netune client.
///
/// Every model provider, catalog query, and authentication call converges on
/// this single client instance to share socket pools and TLS session caches,
/// avoiding redundant DNS/TCP/TLS handshakes to common provider endpoints.
pub fn shared_client() -> &'static Arc<Client> {
    static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();
    CLIENT.get_or_init(|| init_shared_client(true))
}

/// Process-wide shared HTTP/1.1-only netune client (`enable_http2: false`).
///
/// Used for TLS profiles such as `antigravity-cli-unary` that offer only
/// `http/1.1` ALPN and intentionally pair with no HTTP/2 connection profile in
/// netune's registry (`[INV-TLS-10]`).
pub fn shared_http1_client() -> &'static Arc<Client> {
    static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();
    CLIENT.get_or_init(|| init_shared_client(false))
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
    }
}
