//! Post-login and post-refresh credential enrichment port (ADR-0267).
//!
//! The engine defines only the port and the RFC-standard no-op. Each vendor's
//! enricher lives in its owning provider crate and is returned by
//! [`crate::provider::OAuthProvider::enricher`].

use async_trait::async_trait;
use nuo_provider::credentials::TokenSet;
use nuo_provider_transport::http::Http;
use crate::oauth::AuthError;
use crate::oauth::token::{TokenResponse, access_token_expiry_ms};

/// Lifecycle hook for post-login and post-refresh provider-specific metadata enrichment.
#[async_trait]
pub trait OAuthTokenEnricher: Send + Sync {
    /// Enrich a newly minted [`TokenSet`] following a successful authorization
    /// flow.
    ///
    /// `previous` is whatever the credential store held for this connection
    /// before the login — `None` on a first login. It exists so an enricher can
    /// preserve durable identity material across a re-login without reaching for
    /// a store of its own: the caller owns the store, the enricher owns the
    /// policy.
    async fn on_login_success(
        &self,
        client: &Http,
        connection_label: &str,
        tokens: &TokenResponse,
        previous: Option<&TokenSet>,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError>;

    /// Enrich a refreshed [`TokenSet`], preserving durable identity material.
    async fn on_refresh_success(
        &self,
        client: &Http,
        stored: &TokenSet,
        refreshed: &TokenResponse,
        token_set: &mut TokenSet,
    ) -> Result<(), AuthError>;

    /// Human-facing error formatting for provider-specific failure codes.
    fn format_login_error(&self, error: &AuthError) -> String {
        error.to_string()
    }
}

/// Standard enricher for generic RFC 6749 / RFC 8628 OAuth providers (e.g. xAI,
/// Copilot): no post-processing.
#[derive(Debug, Clone, Default)]
pub struct StandardOAuthEnricher;

#[async_trait]
impl OAuthTokenEnricher for StandardOAuthEnricher {
    async fn on_login_success(
        &self,
        _client: &Http,
        _connection_label: &str,
        _tokens: &TokenResponse,
        _previous: Option<&TokenSet>,
        _token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        Ok(())
    }

    async fn on_refresh_success(
        &self,
        _client: &Http,
        _stored: &TokenSet,
        _refreshed: &TokenResponse,
        _token_set: &mut TokenSet,
    ) -> Result<(), AuthError> {
        Ok(())
    }
}

/// Build a complete token set, applying `enricher`'s post-login policy.
pub async fn build_token_set_with_enricher(
    client: &Http,
    connection_label: &str,
    tokens: TokenResponse,
    previous: Option<TokenSet>,
    now_ms: i64,
    enricher: &dyn OAuthTokenEnricher,
) -> Result<TokenSet, AuthError> {
    let expires_ms = access_token_expiry_ms(
        tokens.access_token.expose_secret(),
        tokens.expires_in,
        now_ms,
    );

    let mut token_set = TokenSet {
        access: tokens.access_token.clone(),
        refresh: tokens.refresh_token.clone().unwrap_or_default(),
        expires_ms,
        id_token: tokens.id_token.clone(),
        token_type: tokens.token_type.clone(),
        scope: tokens.scope.clone(),
        user_email: None,
        attributes: serde_json::Map::new(),
    };

    enricher
        .on_login_success(
            client,
            connection_label,
            &tokens,
            previous.as_ref(),
            &mut token_set,
        )
        .await?;

    Ok(token_set)
}
