//! The `OAuthProvider` port and its process-wide registry.
//!
//! Each subscription surface implements [`OAuthProvider`] in its own
//! `providers/nuo-provider-*` crate and registers it at the composition root
//! ([`register_oauth_provider`]). The engine holds only `Arc<dyn OAuthProvider>`
//! and therefore never depends on a vendor crate.

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use futures::future::BoxFuture;
use nuo_model_codec::ResolvedAuth;
use nuo_model_codec::provider_auth::OAuthConfig;
use nuo_provider::credentials::{CredentialSession, TokenSet};
use nuo_provider_transport::http::Http;
use crate::oauth::AuthError;
use crate::oauth::token::TokenResponse;

use crate::enricher::{OAuthTokenEnricher, StandardOAuthEnricher};
use crate::session::OAuthLoginPrompt;

/// A begun vendor device login: the prompt to show the user plus the future
/// that completes the grant and returns tokens.
pub struct DeviceLogin {
    pub prompt: OAuthLoginPrompt,
    pub completion: BoxFuture<'static, Result<TokenResponse, AuthError>>,
}

/// One subscription surface's OAuth behaviour. Implemented by the owning
/// provider crate; the engine treats it as an opaque port.
///
/// Every method except [`ids`](Self::ids) and [`config`](Self::config) has a
/// vendor-neutral default, so a plain RFC 8628 / PKCE provider implements only
/// those two.
#[async_trait]
pub trait OAuthProvider: Send + Sync {
    /// Every provider id this surface answers to: the canonical id first, then
    /// accepted aliases (e.g. `["chatgpt", "chatgpt-plan", "openai-subscription"]`).
    fn ids(&self) -> &'static [&'static str];

    /// The static client configuration for this surface.
    fn config(&self) -> OAuthConfig;

    /// Token post-processing policy. Defaults to the RFC no-op.
    fn enricher(&self) -> Box<dyn OAuthTokenEnricher> {
        Box::new(StandardOAuthEnricher)
    }

    /// Vendor-specific device grant, used when the configuration declares
    /// [`DeviceFlowMode::Custom`](nuo_model_codec::provider_auth::DeviceFlowMode::Custom).
    /// `None` means the generic RFC 8628 flow applies.
    fn begin_device_login(
        &self,
        _client: Http,
        _config: OAuthConfig,
        _machine_id: String,
    ) -> Option<BoxFuture<'static, Result<DeviceLogin, AuthError>>> {
        None
    }

    /// Vendor-specific token refresh (e.g. Qoder's `drt-` device-token
    /// rotation). `None` means the RFC refresh on the configured token URL.
    fn refresh_token<'a>(
        &'a self,
        _client: &'a Http,
        _config: &'a OAuthConfig,
        _refresh_token: &'a str,
    ) -> Option<BoxFuture<'a, Result<TokenResponse, AuthError>>> {
        None
    }

    /// Whether a stored credential needs vendor-specific repair before use
    /// (e.g. backfilling Qoder's uid / elected endpoint).
    fn needs_repair(&self, _stored: &TokenSet) -> bool {
        false
    }

    /// Repair a stored credential and persist it through `session`. Default is
    /// the identity function.
    async fn repair(
        &self,
        _session: &mut dyn CredentialSession,
        _connection_id: &str,
        stored: TokenSet,
    ) -> Result<TokenSet, String> {
        Ok(stored)
    }

    /// Project stored token attributes onto typed [`ResolvedAuth`] metadata
    /// (e.g. ChatGPT `account_id`, OpenCode `org_id`, Qoder identity). Default
    /// leaves `auth` untouched.
    fn project_metadata(&self, _tokens: &TokenSet, auth: ResolvedAuth) -> ResolvedAuth {
        auth
    }
}

static PROVIDERS: OnceLock<RwLock<Vec<Arc<dyn OAuthProvider>>>> = OnceLock::new();

fn providers() -> &'static RwLock<Vec<Arc<dyn OAuthProvider>>> {
    PROVIDERS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register one provider surface. Called once per vendor at the composition
/// root's `init()`; re-registration replaces the previous entry for its ids.
pub fn register_oauth_provider(provider: Arc<dyn OAuthProvider>) {
    let mut list = providers().write().unwrap_or_else(|e| e.into_inner());
    let ids = provider.ids();
    list.retain(|existing| !existing.ids().iter().any(|id| ids.contains(id)));
    list.push(provider);
}

/// Resolve the registered provider surface for a provider id or alias.
pub fn oauth_provider(id: &str) -> Option<Arc<dyn OAuthProvider>> {
    providers()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|provider| provider.ids().contains(&id))
        .cloned()
}

/// Resolve the static [`OAuthConfig`] for a provider id or alias.
pub fn oauth_config(id: &str) -> Option<OAuthConfig> {
    oauth_provider(id).map(|provider| provider.config())
}
