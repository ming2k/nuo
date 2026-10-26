//! Outbound transport error types and HTTP retry classification.

use std::time::SystemTime;

/// Error arising from outbound network transport, HTTP execution, or streaming.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("HTTP error ({status}): {message}")]
    Http {
        status: http::StatusCode,
        message: String,
        retry_after: Option<u64>,
    },
    #[error("I/O transport error ({label}): {message}")]
    Transport {
        label: &'static str,
        message: String,
        retryable: bool,
    },
    #[error("Request timed out ({label})")]
    Timeout { label: &'static str },
    #[error("Invalid request ({label}): {message}")]
    InvalidRequest {
        label: &'static str,
        message: String,
    },
    #[error("Stream error ({label}): {message}")]
    Stream {
        label: &'static str,
        message: String,
        retryable: bool,
    },
    #[error("Decode error ({label}): {message}")]
    Decode {
        label: &'static str,
        message: String,
    },
}

impl TransportError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { status, .. } => {
                status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
            }
            Self::Transport { retryable, .. } => *retryable,
            Self::Timeout { .. } => true,
            Self::Stream { retryable, .. } => *retryable,
            _ => false,
        }
    }

    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Extract `retry-after` duration from HTTP headers in milliseconds.
pub fn retry_after_ms(headers: &http::header::HeaderMap) -> Option<u64> {
    if let Some(milliseconds) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
    {
        return Some(milliseconds.max(0.0) as u64);
    }
    let value = headers.get(http::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<f64>() {
        return Some((seconds.max(0.0) * 1000.0) as u64);
    }
    let parsed = httpdate::parse_http_date(value).ok()?;
    let now = SystemTime::now();
    Some(
        parsed
            .duration_since(now)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_supports_seconds_and_milliseconds() {
        let mut headers = http::header::HeaderMap::new();
        headers.insert("retry-after", "2.5".parse().unwrap());
        assert_eq!(retry_after_ms(&headers), Some(2_500));

        headers.insert("retry-after-ms", "750".parse().unwrap());
        assert_eq!(retry_after_ms(&headers), Some(750));
    }
}
