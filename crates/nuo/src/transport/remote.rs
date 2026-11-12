//! Remote server identity and client target selection.

use super::attach::{AttachError, AttachResult, connect, finish_handshake_with_project};
use super::*;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

static POSTURE_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_posture(posture: nuo_protocol::human_request::HumanChannelPosture) {
    let code = match posture {
        nuo_protocol::human_request::HumanChannelPosture::Interactive => 0,
        nuo_protocol::human_request::HumanChannelPosture::Autonomous => 1,
    };
    POSTURE_OVERRIDE.store(code, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn current_posture() -> nuo_protocol::human_request::HumanChannelPosture {
    match POSTURE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => nuo_protocol::human_request::HumanChannelPosture::Autonomous,
        _ => nuo_protocol::human_request::HumanChannelPosture::Interactive,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteServer {
    pub host: String,
    pub port: u16,
    pub token: String,
    pub secure: bool,
}

#[derive(Debug, Clone)]
pub enum ClientTarget {
    Local(ServerInfo),
    Remote(RemoteServer),
}

impl ClientTarget {
    pub async fn connect(&self, action: AttachAction) -> AttachResult {
        match self {
            Self::Local(info) => connect(info, action).await,
            Self::Remote(remote) => remote.connect(action).await,
        }
    }

    pub fn endpoint_display(&self) -> String {
        match self {
            Self::Local(info) => {
                if info.port > 0 {
                    format!("127.0.0.1:{}", info.port)
                } else {
                    "local UDS".to_string()
                }
            }
            Self::Remote(remote) => remote.endpoint_display(),
        }
    }
}

impl RemoteServer {
    pub fn url(&self) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        format!("{scheme}://{}:{}/", self.host, self.port)
    }

    pub fn endpoint_display(&self) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        if (self.secure && self.port == 443) || (!self.secure && self.port == 80) {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }

    pub fn parse(addr: &str, token: Option<String>) -> Result<Self, String> {
        let trimmed = addr.trim();
        if trimmed.is_empty() {
            return Err("empty remote address".to_string());
        }

        let (without_scheme, secure, scheme_default_port) =
            if let Some(rest) = trimmed.strip_prefix("wss://") {
                (rest, true, Some(443))
            } else if let Some(rest) = trimmed.strip_prefix("https://") {
                (rest, true, Some(443))
            } else if let Some(rest) = trimmed.strip_prefix("ws://") {
                (rest, false, Some(80))
            } else if let Some(rest) = trimmed.strip_prefix("http://") {
                (rest, false, Some(80))
            } else {
                (trimmed, false, None)
            };

        let authority = without_scheme
            .split('/')
            .next()
            .unwrap_or(without_scheme)
            .trim();
        if authority.is_empty() {
            return Err(format!("invalid remote address '{addr}': missing host"));
        }

        let (host, port) = match split_authority(authority)? {
            (host, Some(port)) => (host, port),
            (Some(host), None) => {
                if let Some(def_port) = scheme_default_port {
                    (Some(host), def_port)
                } else {
                    return Err(format!(
                        "'{addr}' is not host:port (must specify port or explicit scheme like wss://)"
                    ));
                }
            }
            (None, None) => {
                return Err(format!("'{addr}' is missing host and port"));
            }
        };

        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());

        let token = token.filter(|t| !t.trim().is_empty()).or_else(|| {
            std::env::var("NUO_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty())
        });

        let Some(token) = token else {
            return Err(
                "--remote needs --token <token> (or NUO_TOKEN env var): every network-exposed server \
                 requires the bearer token from the host's server discovery record"
                    .to_string(),
            );
        };

        Ok(Self {
            host,
            port,
            token,
            secure,
        })
    }

    pub async fn connect(&self, action: AttachAction) -> AttachResult {
        self.connect_with_project(action, None).await
    }

    pub async fn connect_with_project(
        &self,
        action: AttachAction,
        project: Option<std::path::PathBuf>,
    ) -> AttachResult {
        let url = self.url();
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| AttachError::transport(format!("bad ws url {url}: {e}")))?;
        let value = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| AttachError::transport(format!("bad bearer token: {e}")))?;
        request.headers_mut().insert("Authorization", value);
        let (ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| AttachError::transport(format!("ws connect to {url}: {e}")))?;
        let (sink, source) = websocket_split(ws);
        finish_handshake_with_project((sink, source), action, project).await
    }
}

fn split_authority(s: &str) -> Result<(Option<String>, Option<u16>), String> {
    if s.is_empty() {
        return Ok((None, None));
    }
    if s.starts_with('[') {
        if let Some(close_idx) = s.find(']') {
            let host = &s[..=close_idx];
            let rest = &s[close_idx + 1..];
            if rest.is_empty() {
                return Ok((Some(host.to_string()), None));
            } else if let Some(p_str) = rest.strip_prefix(':') {
                let port: u16 = p_str
                    .parse()
                    .map_err(|_| format!("'{p_str}' is not a port number"))?;
                return Ok((Some(host.to_string()), Some(port)));
            } else {
                return Err(format!("invalid IPv6 authority '{s}'"));
            }
        } else {
            return Err(format!("unclosed IPv6 bracket in '{s}'"));
        }
    }
    match s.rsplit_once(':') {
        Some((host, port_str)) => {
            let port: u16 = port_str
                .parse()
                .map_err(|_| format!("'{port_str}' is not a port number"))?;
            let host_opt = (!host.is_empty()).then(|| host.to_string());
            Ok((host_opt, Some(port)))
        }
        None => Ok((Some(s.to_string()), None)),
    }
}
