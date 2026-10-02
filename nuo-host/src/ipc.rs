//! Authenticated-user local IPC: Unix domain sockets on Unix and named pipes
//! with an explicit current-user DACL on Windows.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct LocalEndpointProbe {
    pub exists: bool,
    pub connectable: bool,
}

pub trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}
pub type BoxIo = Box<dyn AsyncIo>;

pub struct LocalListener {
    native: native::LocalListener,
}

impl LocalListener {
    pub fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
        Ok(Self {
            native: native::LocalListener::bind(endpoint)?,
        })
    }

    pub async fn accept(&mut self) -> io::Result<BoxIo> {
        self.native.accept().await
    }
}

pub async fn connect(endpoint: &LocalEndpoint) -> io::Result<BoxIo> {
    native::connect(endpoint).await
}

/// Probe a local endpoint without requiring an async runtime. This is used by
/// diagnostics only; normal clients must use [`connect`].
pub fn probe(endpoint: &LocalEndpoint) -> LocalEndpointProbe {
    native::probe(endpoint)
}

/// A per-user endpoint name safe for discovery records. Unix callers provide
/// the socket path chosen by their path policy; Windows derives a named-pipe
/// namespace from the current user's SID and the instance key.
pub fn endpoint_for_instance(unix_path: PathBuf, instance_key: &str) -> io::Result<LocalEndpoint> {
    native::endpoint_for_instance(unix_path, instance_key)
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    pub(super) struct LocalListener {
        listener: tokio::net::UnixListener,
        path: PathBuf,
    }

    impl LocalListener {
        pub(super) fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
            let LocalEndpoint::UnixSocket(path) = endpoint else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Windows named-pipe endpoint cannot be bound on Unix",
                ));
            };
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && metadata.file_type().is_socket()
            {
                if std::os::unix::net::UnixStream::connect(path).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("local IPC endpoint {} is already live", path.display()),
                    ));
                }
                std::fs::remove_file(path)?;
            }
            let listener = tokio::net::UnixListener::bind(path)?;
            if let Err(error) =
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            {
                drop(listener);
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            Ok(Self {
                listener,
                path: path.clone(),
            })
        }

        pub(super) async fn accept(&mut self) -> io::Result<BoxIo> {
            let (stream, _) = self.listener.accept().await?;
            Ok(Box::new(stream))
        }
    }

    impl Drop for LocalListener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub(super) async fn connect(endpoint: &LocalEndpoint) -> io::Result<BoxIo> {
        let LocalEndpoint::UnixSocket(path) = endpoint else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows named-pipe endpoint cannot be opened on Unix",
            ));
        };
        Ok(Box::new(tokio::net::UnixStream::connect(path).await?))
    }

    pub(super) fn probe(endpoint: &LocalEndpoint) -> LocalEndpointProbe {
        let LocalEndpoint::UnixSocket(path) = endpoint else {
            return LocalEndpointProbe::default();
        };
        let exists = path.exists();
        LocalEndpointProbe {
            exists,
            connectable: exists && std::os::unix::net::UnixStream::connect(path).is_ok(),
        }
    }

    pub(super) fn endpoint_for_instance(
        unix_path: PathBuf,
        _instance_key: &str,
    ) -> io::Result<LocalEndpoint> {
        Ok(LocalEndpoint::UnixSocket(unix_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_endpoint_display_is_an_explicit_uri() {
        let endpoint = LocalEndpoint::UnixSocket(PathBuf::from("/tmp/muta.sock"));
        assert_eq!(endpoint.to_string(), "unix:///tmp/muta.sock");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_bind_replaces_only_stale_socket_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.sock");
        let endpoint = LocalEndpoint::UnixSocket(path.clone());

        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        assert!(
            path.exists(),
            "dropping std listener leaves stale socket state"
        );

        let listener = LocalListener::bind(&endpoint).unwrap();
        assert!(probe(&endpoint).connectable);
        assert!(LocalListener::bind(&endpoint).is_err());
        drop(listener);
        assert!(!path.exists(), "owned listener cleans its endpoint on drop");
    }

    #[cfg(windows)]
    #[test]
    fn windows_endpoint_display_is_an_explicit_uri() {
        let endpoint = LocalEndpoint::WindowsNamedPipe(r"\\.\pipe\muta-test".to_string());
        assert_eq!(endpoint.to_string(), r"npipe://\\.\pipe\muta-test");
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use crate::windows_security::{SecurityDescriptor, current_user_sid};
    use std::ffi::c_void;
    use std::mem::size_of;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    pub(super) struct LocalListener {
        name: String,
        pending: Option<NamedPipeServer>,
    }

    impl LocalListener {
        pub(super) fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
            let LocalEndpoint::WindowsNamedPipe(name) = endpoint else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Unix socket endpoint cannot be bound on Windows",
                ));
            };
            validate_pipe_name(name)?;
            let pending = create_server(name, true)?;
            Ok(Self {
                name: name.clone(),
                pending: Some(pending),
            })
        }

        pub(super) async fn accept(&mut self) -> io::Result<BoxIo> {
            let server = self
                .pending
                .take()
                .ok_or_else(|| io::Error::other("named-pipe listener lost its server instance"))?;
            if let Err(error) = server.connect().await {
                self.pending = create_server(&self.name, false).ok();
                return Err(error);
            }
            // Publish the next listening instance before handing this one to
            // the connection task, avoiding a gap between clients.
            self.pending = Some(create_server(&self.name, false)?);
            Ok(Box::new(server))
        }
    }

    pub(super) async fn connect(endpoint: &LocalEndpoint) -> io::Result<BoxIo> {
        let LocalEndpoint::WindowsNamedPipe(name) = endpoint else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unix socket endpoint cannot be opened on Windows",
            ));
        };
        validate_pipe_name(name)?;
        Ok(Box::new(ClientOptions::new().open(name)?))
    }

    pub(super) fn probe(endpoint: &LocalEndpoint) -> LocalEndpointProbe {
        let LocalEndpoint::WindowsNamedPipe(name) = endpoint else {
            return LocalEndpointProbe::default();
        };
        if validate_pipe_name(name).is_err() {
            return LocalEndpointProbe::default();
        }
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // A successful wait means an instance is available. ERROR_SEM_TIMEOUT
        // means every instance is busy, which still proves a live listener.
        // Other failures (notably FILE_NOT_FOUND) are reported as absent.
        if unsafe { WaitNamedPipeW(wide.as_ptr(), 50) } != 0 {
            return LocalEndpointProbe {
                exists: true,
                connectable: true,
            };
        }
        if io::Error::last_os_error().raw_os_error() == Some(121) {
            LocalEndpointProbe {
                exists: true,
                connectable: true,
            }
        } else {
            LocalEndpointProbe::default()
        }
    }

    pub(super) fn endpoint_for_instance(
        _unix_path: PathBuf,
        instance_key: &str,
    ) -> io::Result<LocalEndpoint> {
        let sid = current_user_sid()?;
        let key: String = instance_key
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .take(64)
            .collect();
        if key.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "named-pipe instance key must not be empty",
            ));
        }
        Ok(LocalEndpoint::WindowsNamedPipe(format!(
            r"\\.\pipe\muta-{sid}-{key}"
        )))
    }

    fn validate_pipe_name(name: &str) -> io::Result<()> {
        if !name.starts_with(r"\\.\pipe\") || name.len() > 240 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "named-pipe endpoint must be a bounded local \\\\.\\pipe\\ path",
            ));
        }
        Ok(())
    }

    fn create_server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let descriptor = SecurityDescriptor::current_user_only()?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_ptr(),
            bInheritHandle: 0,
        };
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        // SAFETY: `attributes` and its descriptor remain alive for the
        // synchronous CreateNamedPipeW call. Windows copies the descriptor.
        unsafe {
            options
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast::<c_void>())
        }
    }
}
