//! Direct network construction and process-wide connection pooling.

use std::sync::{Arc, OnceLock};

use netune::{Client, ClientConfig, Pool, TcpConnector, TlsRouterConnector};

/// Build a direct netune client with platform certificate verification.
///
/// The router connector honours a request's named TLS profile
/// ([`RequestBuilder::tls_profile`](super::RequestBuilder::tls_profile)),
/// which is how a provider presents a non-default wire identity.
pub fn direct_client(
    config: ClientConfig,
) -> Result<Client<TlsRouterConnector<TcpConnector>>, String> {
    let connector = TlsRouterConnector::new(TcpConnector::new());
    Ok(Client::new(connector, Pool::default(), config))
}

fn init_shared_client(enable_http2: bool) -> Arc<Client<TlsRouterConnector<TcpConnector>>> {
    let config = ClientConfig {
        enable_http2,
        // The provider transport and control-plane handle both opt into
        // `send_traced` per attempt. Direct callers (MCP, remote skills,
        // web fetch) use `Client::request`: clock-free L0 counters, no
        // event ring, but still a generated trace id and the diagnostic
        // table netune returns on failure.
        observation: netune::Observation::Counters,
        ..ClientConfig::default()
    };
    match direct_client(config) {
        Ok(client) => Arc::new(client),
        Err(error) => {
            tracing::error!(%error, "shared netune client init failed");
            std::process::exit(101);
        }
    }
}

/// The process-wide shared netune client.
///
/// Every model provider, catalog query, and authentication call converges on
/// this single client instance to share socket pools and TLS session caches,
/// avoiding redundant DNS/TCP/TLS handshakes to common provider endpoints.
pub fn shared_client() -> &'static Arc<Client<TlsRouterConnector<TcpConnector>>> {
    static CLIENT: OnceLock<Arc<Client<TlsRouterConnector<TcpConnector>>>> = OnceLock::new();
    CLIENT.get_or_init(|| init_shared_client(true))
}

/// Process-wide shared HTTP/1.1-only netune client (`enable_http2: false`).
///
/// Used for TLS profiles such as `antigravity-cli-unary` that offer only
/// `http/1.1` ALPN and intentionally pair with no HTTP/2 connection profile in
/// netune's registry (`[INV-TLS-10]`).
pub fn shared_http1_client() -> &'static Arc<Client<TlsRouterConnector<TcpConnector>>> {
    static CLIENT: OnceLock<Arc<Client<TlsRouterConnector<TcpConnector>>>> = OnceLock::new();
    CLIENT.get_or_init(|| init_shared_client(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_client_is_a_singleton() {
        let a = shared_client();
        let b = shared_client();
        assert!(Arc::ptr_eq(a, b), "two calls must return the same client");
        let unused = netune::PoolKey::new(
            "__no_such_authority__",
            netune::ConnectionIdentity::default(),
        );
        assert_eq!(a.pool().idle_count(&unused), 0);
    }
}
