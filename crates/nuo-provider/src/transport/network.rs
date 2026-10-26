//! Direct network construction and process-wide connection pooling.

use std::sync::{Arc, OnceLock};

use netune::{Client, ClientConfig, Pool, TcpConnector, TlsConnector};

/// Build a direct netune client with platform certificate verification.
pub fn direct_client(config: ClientConfig) -> Result<Client<TlsConnector<TcpConnector>>, String> {
    let connector =
        TlsConnector::platform(TcpConnector::new()).map_err(|error| error.to_string())?;
    Ok(Client::new(connector, Pool::default(), config))
}

/// The process-wide shared netune client.
///
/// Every model provider, catalog query, and authentication call converges on
/// this single client instance to share socket pools and TLS session caches,
/// avoiding redundant DNS/TCP/TLS handshakes to common provider endpoints.
pub fn shared_client() -> &'static Arc<Client<TlsConnector<TcpConnector>>> {
    static CLIENT: OnceLock<Arc<Client<TlsConnector<TcpConnector>>>> = OnceLock::new();
    CLIENT.get_or_init(|| match direct_client(ClientConfig::default()) {
        Ok(client) => Arc::new(client),
        Err(error) => {
            tracing::error!(%error, "shared netune client init failed");
            std::process::exit(101);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_client_is_a_singleton() {
        let a = shared_client();
        let b = shared_client();
        assert!(Arc::ptr_eq(a, b), "two calls must return the same client");
        assert_eq!(a.pool().idle_count("__no_such_authority__"), 0);
    }
}
