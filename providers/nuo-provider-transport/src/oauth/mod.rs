//! Standard RFC OAuth primitives (PKCE RFC 7636, Device Flow RFC 8628, Loopback redirect).

pub mod browser;
pub mod device;
pub mod pkce;
pub mod presets;
pub mod token;

pub use browser::*;
pub use device::*;
pub use pkce::PkceCodes;
pub use presets::*;
pub use token::TokenResponse;

/// Errors from the auth flows. Surfaced in user-facing CLI and logs.
#[derive(Debug)]
pub enum AuthError {
    Transport(String),
    Authorization(String),
    TokenEndpoint { status: u16, body: String },
    Decode(String),
    DeviceCode(String),
    Cancelled,
    Timeout,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Transport(msg) => write!(f, "network error: {msg}"),
            AuthError::Authorization(msg) => write!(f, "authorization failed: {msg}"),
            AuthError::TokenEndpoint { status, body } => {
                write!(f, "token endpoint returned HTTP {status}: {body}")
            }
            AuthError::Decode(msg) => write!(f, "could not parse response: {msg}"),
            AuthError::DeviceCode(msg) => write!(f, "device authorization: {msg}"),
            AuthError::Cancelled => write!(f, "login was cancelled"),
            AuthError::Timeout => write!(f, "login timed out"),
        }
    }
}

impl std::error::Error for AuthError {}

pub fn token_error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("error")
        .and_then(|error| {
            error.as_str().or_else(|| {
                error
                    .get("code")
                    .or_else(|| error.get("type"))
                    .and_then(serde_json::Value::as_str)
            })
        })
        .or_else(|| value.get("code").and_then(serde_json::Value::as_str))
        .map(|code| code.trim().to_ascii_lowercase())
}

impl AuthError {
    /// Whether this error indicates an invalid/revoked refresh token on the identity provider.
    pub fn is_permanent_grant_error(&self) -> bool {
        match self {
            AuthError::TokenEndpoint { body, .. } => {
                let Some(code) = token_error_code(body) else {
                    return false;
                };
                matches!(
                    code.as_str(),
                    "invalid_grant"
                        | "token_revoked"
                        | "refresh_token_expired"
                        | "refresh_token_reused"
                        | "refresh_token_invalidated"
                )
            }
            _ => false,
        }
    }
}
