//! TypeSafe System One (Jev) wire protocol implementation.

use crate::decision::endpoint::DecisionEndpoint;
use crate::decision::types::{DecisionRequest, DecisionResponse};
use crate::error::{Result, WireError};
use http::HeaderMap;

/// Builds HTTP URL, headers, and JSON body for TypeSafe System One evaluation endpoint.
pub fn build_request(
    endpoint: &DecisionEndpoint,
    token: &str,
    request: &DecisionRequest,
) -> (String, HeaderMap, serde_json::Value) {
    let mut url = endpoint.base_url.clone();
    if !url.ends_with("/systemone") && !url.ends_with("/decide") {
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str("systemone");
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    // Track the crate version from one place; a static literal here would drift
    // on every version bump.
    const USER_AGENT: &str =
        concat!("nuo-model-codec/", env!("CARGO_PKG_VERSION"), " (SystemOne; Jev)");
    headers.insert(
        http::header::USER_AGENT,
        http::HeaderValue::from_static(USER_AGENT),
    );

    if !token.is_empty() {
        let auth_val = format!("Bearer {token}");
        if let Ok(hv) = http::HeaderValue::from_str(&auth_val) {
            headers.insert(http::header::AUTHORIZATION, hv);
        }
    }

    for (k, v) in &endpoint.custom_headers {
        if let (Ok(hk), Ok(hv)) = (
            http::header::HeaderName::from_bytes(k.as_bytes()),
            http::HeaderValue::from_str(v),
        ) {
            headers.insert(hk, hv);
        }
    }

    let body = serde_json::to_value(request).unwrap_or(serde_json::Value::Null);

    (url, headers, body)
}

/// Parses vendor response into canonical [`DecisionResponse`].
pub fn parse_response(json_val: &serde_json::Value) -> Result<DecisionResponse> {
    if let Some(err_obj) = json_val.get("error") {
        let message = err_obj
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error from TypeSafe System One API");
        return Err(WireError::ApiError {
            status: 400,
            message: message.to_string(),
        });
    }

    let resp: DecisionResponse = serde_json::from_value(json_val.clone())
        .map_err(|err| WireError::Protocol(format!("failed to deserialize DecisionResponse: {err}")))?;

    // Quality assertion on probabilities and bounds
    for (id, answer) in &resp.answers {
        match answer {
            crate::decision::types::DecisionAnswer::Noul(n) => {
                if !(0.0..=1.0001).contains(&n.noul) {
                    return Err(WireError::Protocol(format!(
                        "Noul probability for question `{id}` out of [0, 1] range: {}",
                        n.noul
                    )));
                }
            }
            crate::decision::types::DecisionAnswer::Choice(c) => {
                if !(0.0..=1.0001).contains(&c.confidence) {
                    return Err(WireError::Protocol(format!(
                        "Choice confidence for question `{id}` out of [0, 1] range: {}",
                        c.confidence
                    )));
                }
            }
            crate::decision::types::DecisionAnswer::Score(s) => {
                if !(0.0..=1.0001).contains(&s.confidence) {
                    return Err(WireError::Protocol(format!(
                        "Score confidence for question `{id}` out of [0, 1] range: {}",
                        s.confidence
                    )));
                }
            }
        }
    }

    Ok(resp)
}
