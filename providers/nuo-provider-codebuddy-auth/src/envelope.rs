//! The wrapped credential envelope CodeBuddy auth responses arrive in.
//!
//! Field names are quoted exactly as the server emits them; the lowercase
//! serde renames align them with the RFC-facing names `TokenResponse` uses.

use nuo_host::SecretString;
use nuo_oauth::oauth::AuthError;
use nuo_oauth::oauth::token::TokenResponse;

/// The wrapped credential payload CodeBuddy returns from state-login and
/// refresh.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CodeBuddyAuthEnvelope {
    #[serde(rename = "accessToken")]
    pub access_token: SecretString,
    #[serde(rename = "refreshToken", default)]
    pub refresh_token: Option<SecretString>,
    #[serde(rename = "tokenType", default)]
    pub token_type: Option<String>,
    #[serde(rename = "expiresIn", default)]
    pub expires_in: Option<u64>,
    #[serde(rename = "refreshExpiresIn", default)]
    pub refresh_expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(rename = "sessionState", default)]
    pub session_state: Option<String>,
    #[serde(rename = "notBeforePolicy", default)]
    pub not_before_policy: Option<u64>,
    /// The account line the credential belongs to (e.g. `www.codebuddy.cn`
    /// for a domestic login, `codebuddy.ai` for an international one). This
    /// is the realm-boundary fact channel crates gate on
    /// ([INV-PROV-REALM-01]).
    #[serde(default)]
    pub domain: Option<String>,
}

/// The `data: { data: … }` / `data: { authUrl, state, data: … }` wrapper
/// CodeBuddy's auth responses arrive in.
#[derive(Debug, serde::Deserialize)]
#[serde(bound(deserialize = "T: serde::de::DeserializeOwned"))]
pub struct CodeBuddyEnvelope<T> {
    #[serde(rename = "data")]
    pub data: Option<CodeBuddyData<T>>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(bound(deserialize = "T: serde::de::DeserializeOwned"))]
pub struct CodeBuddyData<T> {
    /// `authUrl` / `state` sit at the *outer* `data` level for `/auth/state`.
    #[serde(rename = "authUrl", default)]
    pub auth_url: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// The wrapped credential pair sits one level deeper.
    #[serde(default)]
    pub data: Option<T>,
}

impl CodeBuddyAuthEnvelope {
    /// Lift the wrapped payload into the RFC `TokenResponse` the engine hands
    /// to the enricher.
    pub fn into_token_response(self) -> Result<TokenResponse, AuthError> {
        if self.access_token.expose_secret().trim().is_empty() {
            return Err(AuthError::Decode(
                "codebuddy auth returned an empty access_token".into(),
            ));
        }
        Ok(TokenResponse {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            id_token: None,
            token_type: self.token_type,
            expires_in: self.expires_in,
            scope: self.scope,
            qoder_uid: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_envelope_parses_the_full_shape() {
        let sample = r#"{
            "code": 0,
            "data": { "data": {
                "accessToken": "eyJtest-access",
                "refreshToken": "eyJtest-refresh",
                "tokenType": "Bearer",
                "expiresIn": 5184000,
                "refreshExpiresIn": 7776000,
                "scope": "openid profile offline_access email",
                "sessionState": "abc",
                "notBeforePolicy": 1724292326,
                "domain": "www.codebuddy.cn"
            }}
        }"#;
        let wrapped: CodeBuddyEnvelope<CodeBuddyAuthEnvelope> =
            serde_json::from_str(sample).expect("parses");
        let inner = wrapped.data.expect("data").data.expect("inner");
        // Mirrors the live `Tencent-Cloud.coding-copilot.info` capture:
        // ~60-day access / ~90-day refresh.
        assert_eq!(inner.expires_in, Some(5_184_000));
        assert_eq!(inner.refresh_expires_in, Some(7_776_000));
        assert_eq!(inner.token_type.as_deref(), Some("Bearer"));
        assert_eq!(
            inner.scope.as_deref(),
            Some("openid profile offline_access email")
        );
        assert_eq!(inner.domain.as_deref(), Some("www.codebuddy.cn"));
        let response = inner.into_token_response().expect("lifts");
        assert_eq!(response.access_token.expose_secret(), "eyJtest-access");
        assert_eq!(
            response
                .refresh_token
                .as_ref()
                .map(|r| r.expose_secret())
                .unwrap_or_default(),
            "eyJtest-refresh"
        );
    }

    #[test]
    fn pending_poll_without_token_pair_decodes_to_none() {
        // While the web sign-in is unfinished the server returns
        // `code: 0, data: { data: null }` — the pending semantic.
        let sample = r#"{ "code": 0, "data": { "data": null } }"#;
        let wrapped: CodeBuddyEnvelope<CodeBuddyAuthEnvelope> =
            serde_json::from_str(sample).expect("parses");
        assert!(wrapped.data.unwrap().data.is_none(), "still pending");
    }

    #[test]
    fn empty_access_token_is_rejected_by_the_lift() {
        let sample = r#"{ "data": { "data": {
            "accessToken": "  ", "tokenType": "Bearer"
        }}}"#;
        let wrapped: CodeBuddyEnvelope<CodeBuddyAuthEnvelope> =
            serde_json::from_str(sample).expect("parses");
        let inner = wrapped.data.unwrap().data.unwrap();
        assert!(inner.into_token_response().is_err());
    }
}
