//! Authentication, bearer token validation, and browser drive-by defense for the control plane.
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};
use tokio_tungstenite::tungstenite::http::StatusCode;

use super::options::ServeExpose;

/// Subprotocol carrying a bearer token from clients that cannot set headers
/// (browsers): `Sec-WebSocket-Protocol: bearer.<token>`.
pub const BEARER_SUBPROTOCOL_PREFIX: &str = "bearer.";

/// Generate the bearer token a `--public` listener requires. Two UUIDv4s —
/// 256 bits from the OS CSPRNG (uuid v4 reads `getrandom`), hex-encoded so
/// the token is URL-safe. The previous time^pid hash let a LAN attacker
/// shrink the brute-force space to almost nothing; this does not.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn check_bearer(req: &Request, expected: &str) -> bool {
    let Some(val) = req.headers().get("Authorization") else {
        return false;
    };
    let Ok(s) = val.to_str() else {
        return false;
    };
    let Some(rest) = s.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(rest.trim().as_bytes(), expected.as_bytes())
}

/// Constant-time token comparison. Always scans the full `expected` length
/// and folds every byte into one accumulator, so timing reveals neither the
/// first mismatching position nor the expected length (a shorter `given`
/// reads as 0 bytes, a longer one is caught by the length fold — both still
/// compare every expected byte).
pub fn constant_time_eq(given: &[u8], expected: &[u8]) -> bool {
    let mut diff = given.len() ^ expected.len();
    for (i, &b) in expected.iter().enumerate() {
        diff |= usize::from(given.get(i).copied().unwrap_or(0) ^ b);
    }
    diff == 0
}

pub fn reject_unauthorized() -> ErrorResponse {
    let body = "Unauthorized".to_string();
    tungstenite::handshake::server::Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Bearer")
        .body(Some(body))
        .unwrap_or_default()
}

/// 403 for a handshake that fails the origin policy (distinct from 401: the
/// client presented no credential problem — its *provenance* is disallowed).
#[allow(clippy::result_large_err)]
pub fn reject_forbidden(reason: &str) -> ErrorResponse {
    tungstenite::handshake::server::Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(Some(reason.to_string()))
        .unwrap_or_default()
}

/// The host part of a `Host` header or origin URL, lowercased, port stripped.
pub fn host_part(authority: &str) -> String {
    let authority = authority.trim();
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6: [::1]:9800 → ::1
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.to_ascii_lowercase()
}

pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1") || host.strip_suffix(".localhost").is_some()
}

/// Browser drive-by defense. WebSocket handshakes are not subject
/// to the same-origin policy: any page the user visits can open
/// `ws://127.0.0.1:<port>` and drive the server — *unless* the server checks.
/// Browsers always send `Origin` on a WebSocket upgrade, so on a loopback
/// listener we refuse handshakes whose origin is an http(s) page not itself
/// served from loopback (the panel served by this server, or a local dev
/// server, qualifies). Non-browser clients (TUI, CLI) send no `Origin` and
/// are governed by the bearer token instead. On a `--public` listener the
/// token is mandatory and the origin check is moot — remote origins are
/// legitimate there.
#[allow(clippy::result_large_err)]
pub fn validate_origin(req: &Request, expose: ServeExpose) -> Result<(), ErrorResponse> {
    if expose == ServeExpose::Public {
        return Ok(());
    }
    let Some(origin) = req.headers().get("Origin").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    let without_scheme = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    let allowed = without_scheme
        .map(|rest| is_loopback_host(&host_part(rest.split('/').next().unwrap_or(rest))))
        .unwrap_or(false);
    if allowed {
        Ok(())
    } else {
        tracing::warn!(%origin, "nuo server: refused WebSocket handshake from foreign browser origin");
        Err(reject_forbidden(
            "browser origin not allowed: the nuo control plane only serves pages hosted on loopback",
        ))
    }
}

/// Credential check for the handshake: `Authorization: Bearer` first, then
/// the `bearer.<token>` subprotocol. `Ok(Some(protocol))` means the
/// subprotocol channel was used and the handshake response must echo it.
#[allow(clippy::result_large_err)]
pub fn check_credentials(req: &Request, expected: &str) -> Result<Option<String>, ErrorResponse> {
    if check_bearer(req, expected) {
        return Ok(None);
    }
    if let Some(offers) = req
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
    {
        for offer in offers.split(',').map(str::trim) {
            if let Some(token) = offer.strip_prefix(BEARER_SUBPROTOCOL_PREFIX)
                && constant_time_eq(token.as_bytes(), expected.as_bytes())
            {
                return Ok(Some(offer.to_string()));
            }
        }
    }
    Err(reject_unauthorized())
}
