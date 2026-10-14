//! Direct network construction shared by inference and provider services.

use netune::{Client, ClientConfig, Pool, TcpConnector, TlsConnector};

/// Build a direct client with platform certificate verification. Deadlines
/// belong to callers: catalog requests are bounded, model streams may be long.
pub fn direct_client(config: ClientConfig) -> Result<Client<TlsConnector<TcpConnector>>, String> {
    let connector =
        TlsConnector::platform(TcpConnector::new()).map_err(|error| error.to_string())?;
    Ok(Client::new(connector, Pool::default(), config))
}
