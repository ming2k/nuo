//! Manual authorization response parser for headless / remote (SSH, Docker) environments.
//!
//! When the loopback callback server is not directly reachable by the user's
//! local browser (e.g. running on a remote server or container), the user can
//! simply paste the full redirect URL (or just the code) from their browser address
//! bar back into the CLI. This module parses, validates CSRF state, and extracts
//! the raw authorization code.

use crate::oauth::AuthError;

/// Parse an authorization code from a user input string, which may be:
/// 1. A full redirect URL (e.g. `http://localhost:51121/oauth/callback?code=4%2F0A...&state=XYZ`)
/// 2. A query string (e.g. `code=4%2F0A...&state=XYZ`)
/// 3. A raw authorization code (e.g. `4/0Acv...` or `app_...`)
///
/// If `expected_state` is supplied, structured URL/query input must carry that
/// exact state. A raw code is the explicit headless escape hatch and has no
/// query metadata to validate.
pub fn parse_authorization_response(
    input: &str,
    expected_state: Option<&str>,
) -> Result<String, AuthError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AuthError::Cancelled);
    }

    // Check if input contains query parameters ('?' or '&' or 'code=')
    let query_str = if let Some((_base, query)) = trimmed.split_once('?') {
        query
    } else if trimmed.contains("code=") || trimmed.contains("error=") {
        trimmed
    } else {
        // Plain code entered directly
        return Ok(trimmed.to_string());
    };

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;
    let mut error_description: Option<String> = None;

    let mut seen = std::collections::HashSet::new();
    for pair in query_str.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').ok_or_else(|| {
            AuthError::Decode("malformed authorization response parameter".to_string())
        })?;
        let key = decode_query_component(key)?;
        let value = decode_query_component(value)?;
        if !seen.insert(key.clone()) {
            return Err(AuthError::Decode(format!(
                "duplicate authorization response parameter: {key}"
            )));
        }
        match key.as_str() {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => error = Some(value),
            "error_description" => error_description = Some(value),
            _ => {}
        }
    }

    if let Some(err) = error {
        let msg = error_description.unwrap_or(err);
        return Err(AuthError::Authorization(msg));
    }

    if let Some(expected) = expected_state {
        match state.as_deref() {
            Some(actual) if expected == actual => {}
            Some(_) => {
                return Err(AuthError::Authorization(
                    "CSRF security check failed: authorization state does not match".to_string(),
                ));
            }
            None => {
                return Err(AuthError::Authorization(
                    "CSRF security check failed: authorization state is missing".to_string(),
                ));
            }
        }
    }

    if let Some(c) = code
        && !c.trim().is_empty()
    {
        return Ok(c.trim().to_string());
    }

    Err(AuthError::Decode(
        "no authorization code found in the provided URL or input".to_string(),
    ))
}

fn decode_query_component(value: &str) -> Result<String, AuthError> {
    let input = value.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < input.len() => {
                let encoded = std::str::from_utf8(&input[index + 1..index + 3])
                    .map_err(|_| AuthError::Decode("invalid percent encoding".to_string()))?;
                let byte = u8::from_str_radix(encoded, 16)
                    .map_err(|_| AuthError::Decode("invalid percent encoding".to_string()))?;
                output.push(byte);
                index += 3;
            }
            b'%' => {
                return Err(AuthError::Decode("truncated percent encoding".to_string()));
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output)
        .map_err(|_| AuthError::Decode("authorization response is not UTF-8".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_redirect_url() {
        let url = "http://localhost:51121/oauth/callback?code=4%2F0Acv_test123&state=my_csrf_state";
        let code = parse_authorization_response(url, Some("my_csrf_state")).unwrap();
        assert_eq!(code, "4/0Acv_test123");
    }

    #[test]
    fn rejects_mismatched_state() {
        let url = "http://localhost:51121/oauth/callback?code=test&state=wrong_state";
        let err = parse_authorization_response(url, Some("expected_state")).unwrap_err();
        assert!(matches!(err, AuthError::Authorization(_)));
        assert!(err.to_string().contains("CSRF"));
    }

    #[test]
    fn parses_raw_code_directly() {
        let raw = "4/0Acv_direct_code";
        let code = parse_authorization_response(raw, Some("expected_state")).unwrap();
        assert_eq!(code, "4/0Acv_direct_code");
    }

    #[test]
    fn parses_error_response() {
        let url = "http://localhost:51121/oauth/callback?error=access_denied&error_description=User+cancelled";
        let err = parse_authorization_response(url, None).unwrap_err();
        assert!(err.to_string().contains("User cancelled"));
    }

    #[test]
    fn structured_response_requires_state_and_rejects_duplicates() {
        let missing = parse_authorization_response("code=test", Some("expected")).unwrap_err();
        assert!(matches!(missing, AuthError::Authorization(_)));
        assert!(missing.to_string().contains("state is missing"));

        let duplicate =
            parse_authorization_response("code=first&code=second&state=expected", Some("expected"))
                .unwrap_err();
        assert!(matches!(duplicate, AuthError::Decode(_)));
    }
}
