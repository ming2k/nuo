//! Pure local-endpoint identity shared by discovery records and the host IPC
//! runtime.
//!
//! The type names where a local server listens; binding, connecting, and
//! probing live in `nuo-host::ipc`, which owns the OS I/O. Keeping the value in
//! the protocol layer lets `Discovery` describe an endpoint without pulling the
//! host's system libraries into every consumer.

use std::fmt;
use std::path::PathBuf;

/// A local control endpoint: a Unix domain socket or a Windows named pipe.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "address", rename_all = "snake_case")]
pub enum LocalEndpoint {
    UnixSocket(PathBuf),
    WindowsNamedPipe(String),
}

impl fmt::Display for LocalEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnixSocket(path) => write!(formatter, "unix://{}", path.display()),
            Self::WindowsNamedPipe(name) => write!(formatter, "npipe://{name}"),
        }
    }
}
