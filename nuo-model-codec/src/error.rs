//! Error types for model wire protocol operations.

use thiserror::Error;

pub type Result<T, E = WireError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum WireError {
    #[error("HTTP transport error: {0}")]
    Http(String),

    #[error("API error status {status}: {message}")]
    ApiError { status: u16, message: String },

    #[error("Protocol wire format error: {0}")]
    Protocol(String),

    #[error("Stream decoding error: {0}")]
    Stream(String),

    #[error("In-flight streaming loop detected: {0}")]
    DegenerativeLoop(String),

    #[error("Dynamic credential error: {0}")]
    Credentials(String),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(feature = "reqwest-oracle")]
impl From<reqwest::Error> for WireError {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err.to_string())
    }
}
