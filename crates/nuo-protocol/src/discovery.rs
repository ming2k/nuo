//! Discovery definitions: how clients find a live session server's endpoint.

use std::path::PathBuf;

/// The discovery record, written once the bound port is known.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Discovery {
    /// The serving process's id (staleness probe for readers).
    pub pid: u32,
    /// OS process creation token paired with `pid`, preventing a stale record
    /// from targeting an unrelated process after PID reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_birth_token: Option<u64>,
    /// The bound TCP port the WebSocket listener serves.
    pub port: u16,
    /// The bearer token clients must present, when auth is active.
    pub token: Option<String>,
    /// The project root the host serves.
    pub project_root: String,
    /// Unix seconds at startup.
    pub started_at: u64,
    /// Unix domain socket the control plane also listens on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uds_path: Option<PathBuf>,
    /// Native local control endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_endpoint: Option<crate::endpoint::LocalEndpoint>,
    /// The server build's version string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The server's configured graceful-drain budget, seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_secs: Option<u64>,
    /// The wire protocol number this server speaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    /// Bounded content digest (lowercase hex) of the server's own executable
    /// image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    /// Byte length of the server's executable image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_len: Option<u64>,
}

impl Discovery {
    pub fn effective_local_endpoint(&self) -> Option<crate::endpoint::LocalEndpoint> {
        self.local_endpoint.clone().or_else(|| {
            self.uds_path
                .clone()
                .map(crate::endpoint::LocalEndpoint::UnixSocket)
        })
    }
}

/// Canonical alias for session server discovery information.
pub type ServerInfo = Discovery;
