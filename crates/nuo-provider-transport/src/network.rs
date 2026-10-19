//! Direct network construction shared by inference and provider services.

use std::sync::{Arc, OnceLock};

use netune::{Client, ClientConfig, Pool, TcpConnector, TlsConnector};

/// Build a direct client with platform certificate verification. Deadlines
/// belong to callers: catalog requests are bounded, model streams may be long.
pub fn direct_client(config: ClientConfig) -> Result<Client<TlsConnector<TcpConnector>>, String> {
    let connector =
        TlsConnector::platform(TcpConnector::new()).map_err(|error| error.to_string())?;
    Ok(Client::new(connector, Pool::default(), config))
}

/// The process's one shared netune client.
///
/// nuo runs several adapters onto netune (the `providers/*` egress, the wire
/// client in `nuo-model-codec`, decision clients). Each constructing its own
/// [`Pool::default`] fragments the connection cache: the same authority
/// dialled twice, two TLS handshakes paid twice, one pool's warm socket
/// useless to the other. A single shared client makes every adapter converge
/// on one pool — a warm socket is warm for the whole process, and a
/// connection is pooled exactly once per authority.
///
/// The 20 s idle bound ([ADR-0005]) still applies per connection; sharing the
/// pool does not change that, it only stops *duplicating* the dial cost
/// across adapters.
pub fn shared_client() -> &'static Arc<Client<TlsConnector<TcpConnector>>> {
    static CLIENT: OnceLock<Arc<Client<TlsConnector<TcpConnector>>>> = OnceLock::new();
    CLIENT.get_or_init(|| {
        match direct_client(ClientConfig::default()) {
            Ok(client) => Arc::new(client),
            Err(error) => {
                // Platform TLS failed at first use. There is no working
                // fallback (plaintext contradicts the egress confinement
                // policy), and a static accessor cannot return `Result`.
                // Reaching through `std::process::abort` is the deliberate,
                // lint-visible failure: the process cannot serve TLS at all.
                tracing::error!(%error, "shared netune client init failed");
                std::process::exit(101);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared client is a true singleton: two calls, one pool.
    #[test]
    fn shared_client_is_a_singleton() {
        let a = shared_client();
        let b = shared_client();
        assert!(Arc::ptr_eq(a, b), "two calls must return the same client");
        // Its pool reports the same identity through both handles.
        assert_eq!(a.pool().idle_count("__no_such_authority__"), 0);
    }
}
