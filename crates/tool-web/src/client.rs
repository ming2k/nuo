//! Shared HTTP policy for the web tools: the client handle, the SSRF-guarded
//! GET with an explicit redirect loop, and the untrusted-content framing.
//!
//! The framing itself lives in `tool::untrusted` — one owner for every
//! untrusted region, whatever door the bytes came through (ADR-0228). This crate
//! only needs the HTTP policy.

use http::HeaderMap;

pub const MOZILLA_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub const MAX_REDIRECTS: usize = 5;
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// The final, SSRF-validated response of a [`guarded_get`] call.
#[derive(Debug)]
pub struct GuardedResponse {
    pub final_url: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

const GET_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

fn is_transient_get_status(status: http::StatusCode) -> bool {
    matches!(status.as_u16(), 502..=504)
}

/// GET `url`, following redirects explicitly with an SSRF re-check on every
/// hop, streaming the final body under a single deadline with a hard size cap,
/// and retrying once on transient upstream 502/503/504 or transport glitches
/// (ADR-0224).
///
/// The transport is configured with `max_redirects = 0` on purpose: this loop
/// must see every hop, because the guard validates each one.
pub async fn guarded_get(
    client: &super::http::WebHttp,
    url: &str,
    extra_headers: HeaderMap,
) -> Result<GuardedResponse, String> {
    match guarded_get_once(client, url, &extra_headers).await {
        Ok(response) => Ok(response),
        Err(AttemptFailure::Terminal(err)) => Err(err),
        Err(AttemptFailure::Transient(_)) => {
            client.clear_pool();
            tokio::time::sleep(GET_RETRY_DELAY).await;
            match guarded_get_once(client, url, &extra_headers).await {
                Ok(response) => Ok(response),
                Err(AttemptFailure::Transient(err) | AttemptFailure::Terminal(err)) => Err(err),
            }
        }
    }
}

enum AttemptFailure {
    Transient(String),
    Terminal(String),
}

async fn guarded_get_once(
    client: &super::http::WebHttp,
    url: &str,
    extra_headers: &HeaderMap,
) -> Result<GuardedResponse, AttemptFailure> {
    let deadline = tokio::time::Instant::now() + client.default_timeout();
    let mut current = url.to_string();
    for _hop in 0..=MAX_REDIRECTS {
        // SSRF / DNS refusal is terminal: never retry a blocked target.
        crate::ssrf::assert_public_url(&current)
            .await
            .map_err(AttemptFailure::Terminal)?;
        let request = super::http::WebRequest::get(&current)
            .headers(extra_headers.clone())
            .timeout(client.default_timeout());
        let mut response = client
            .open_until_classified(request, deadline)
            .await
            .map_err(|err| {
                if err.retryable {
                    AttemptFailure::Transient(err.message)
                } else {
                    AttemptFailure::Terminal(err.message)
                }
            })?;
        let status = response.head.status;
        if status.is_redirection() {
            let location = response
                .head
                .headers
                .get(http::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    AttemptFailure::Terminal(format!("HTTP {status} without a Location header"))
                })?;
            current = super::http::resolve_redirect(&current, location).ok_or_else(|| {
                AttemptFailure::Terminal(format!("Invalid redirect target '{location}'"))
            })?;
            continue;
        }
        if !status.is_success() {
            let err = format!("HTTP {status} for {current}");
            return Err(if is_transient_get_status(status) {
                AttemptFailure::Transient(err)
            } else {
                AttemptFailure::Terminal(err)
            });
        }
        let headers = response.head.headers.clone();
        let mut body: Vec<u8> = Vec::new();
        loop {
            let chunk = tokio::time::timeout_at(deadline, response.body.next_chunk())
                .await
                .map_err(|_| {
                    AttemptFailure::Transient(format!(
                        "Request to {current} timed out while reading body"
                    ))
                })?
                .map_err(|error| {
                    let message = format!("Failed to read body: {error}");
                    if error.is_retryable() && !error.is_unreachable() {
                        AttemptFailure::Transient(message)
                    } else {
                        AttemptFailure::Terminal(message)
                    }
                })?;
            let Some(chunk) = chunk else {
                break;
            };
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(AttemptFailure::Terminal(format!(
                    "Response for {url} exceeds the {} MiB fetch limit",
                    MAX_BODY_BYTES / 1024 / 1024
                )));
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(GuardedResponse {
            final_url: current,
            headers,
            body,
        });
    }
    Err(AttemptFailure::Terminal(format!(
        "Too many redirects (more than {MAX_REDIRECTS}) for {url}"
    )))
}
