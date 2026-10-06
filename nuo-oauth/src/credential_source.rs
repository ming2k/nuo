//! Transactional OAuth credential resolution for provider connections.
//!
//! Vendor-specific repair and typed-metadata projection are delegated to the
//! registered [`OAuthProvider`](crate::provider::OAuthProvider); this module
//! holds only the store locking, single-flight refresh, and rejection
//! recovery that is common to every surface.

use crate::provider::oauth_config;
use crate::session::{ACCESS_TOKEN_REFRESH_SKEW_MS, OAuth, access_token_is_expiring};
use futures::future::BoxFuture;
use nuo_model_codec::{ConnectionAuth, CredentialSource, ResolvedAuth, SecretString};
use nuo_provider::credentials::CredentialHost;
use nuo_provider::credentials::{CredentialSession, CredentialStore, TokenSet};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// State shared by every channel, catalog request, and runtime activation for
/// one connection. The store's cross-process lock is the actual refresh gate;
/// rejected-token identity is carried by each request rather than inferred
/// from mutable global state.
struct ConnectionOAuth {
    oauth: OAuth,
}

fn registry() -> &'static Mutex<HashMap<String, Weak<ConnectionOAuth>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Weak<ConnectionOAuth>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn shared_oauth(
    host: &CredentialHost,
    connection_id: &str,
    auth: &ConnectionAuth,
) -> Option<Arc<ConnectionOAuth>> {
    let integration_id = auth.oauth_provider_id()?;
    let registry_key = format!("{connection_id}\0{integration_id}");
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(existing) = entries.get(&registry_key).and_then(Weak::upgrade) {
        return Some(existing);
    }
    let config = oauth_config(integration_id)?;
    let state = Arc::new(ConnectionOAuth {
        oauth: OAuth::new(config, host.clone()),
    });
    entries.retain(|_, value| value.strong_count() > 0);
    entries.insert(registry_key, Arc::downgrade(&state));
    Some(state)
}

/// Dynamic OAuth token source for one exact provider connection.
pub struct OAuthCredentialSource {
    pub connection_id: String,
    pub auth: ConnectionAuth,
    state: Option<Arc<ConnectionOAuth>>,
    /// Where the tokens this source reads and rotates live.
    store: Arc<dyn CredentialStore>,
}

impl OAuthCredentialSource {
    pub fn new(
        host: &CredentialHost,
        connection_id: impl Into<String>,
        auth: ConnectionAuth,
    ) -> Self {
        let connection_id = connection_id.into();
        let state = shared_oauth(host, &connection_id, &auth);
        Self {
            connection_id,
            auth,
            state,
            store: Arc::clone(host.store()),
        }
    }

    fn store(&self) -> &dyn CredentialStore {
        self.store.as_ref()
    }

    /// The registered vendor surface for this connection, if any.
    fn provider(&self) -> Option<&Arc<dyn crate::provider::OAuthProvider>> {
        self.state.as_ref().and_then(|state| state.oauth.provider())
    }

    fn resolved(&self, tokens: &TokenSet) -> ResolvedAuth {
        let mut auth = ResolvedAuth::new(tokens.access.clone());
        if let Some(email) = &tokens.user_email {
            auth = auth.with_user_email(email.clone());
        }
        // Typed metadata projection (ChatGPT `account_id`, OpenCode `org_id`,
        // Google `project_id`, Qoder identity) is the vendor's policy.
        if let Some(provider) = self.provider() {
            auth = provider.project_metadata(tokens, auth);
        }
        auth
    }

    /// Apply the vendor's credential repair when it declares the stored
    /// credential incomplete, persisting through `session`.
    async fn repair_if_needed(
        &self,
        session: &mut dyn CredentialSession,
        stored: TokenSet,
    ) -> Result<TokenSet, String> {
        match self.provider() {
            Some(provider) if provider.needs_repair(&stored) => {
                provider
                    .repair(session, &self.connection_id, stored)
                    .await
            }
            _ => Ok(stored),
        }
    }

    /// The `resolve_auth` fast path holds no lock until repair is actually
    /// needed, so a live token costs zero locking.
    async fn repair_on_fast_path(&self, stored: TokenSet) -> Result<TokenSet, String> {
        let needs_repair = self
            .provider()
            .is_some_and(|provider| provider.needs_repair(&stored));
        if !needs_repair {
            return Ok(stored);
        }
        let mut session = self
            .store()
            .lock()
            .await
            .map_err(|error| error.to_string())?;
        self.repair_if_needed(session.as_mut(), stored).await
    }

    async fn refresh_locked(
        &self,
        force: bool,
        rejected_access: Option<&SecretString>,
    ) -> Result<ResolvedAuth, String> {
        let Some(state) = &self.state else {
            return Err(format!(
                "OAuth configuration not found for auth variant {:?}",
                self.auth
            ));
        };
        let mut session = self
            .store()
            .lock()
            .await
            .map_err(|error| error.to_string())?;
        let stored = exact_tokens(session.as_ref(), &self.connection_id, &self.auth)?;
        // Complete any vendor-declared repair (e.g. Qoder's uid backfill and
        // endpoint election) before touching the token.
        let stored = self.repair_if_needed(session.as_mut(), stored).await?;

        // A force-refresh is normally a reaction to a 401. If another request
        // or process already replaced the token that this caller used, retry
        // with the replacement instead of rotating the refresh token again.
        if force
            && token_rotated_since_rejection(rejected_access, &stored)
            && token_is_live(&stored)
        {
            return Ok(self.resolved(&stored));
        }
        if !force && token_is_live(&stored) {
            return Ok(self.resolved(&stored));
        }

        match state.oauth.force_resolve_access_token(stored).await {
            Ok((_access, tokens)) => {
                session.set(&self.connection_id, tokens.clone());
                // Never hand out a newly rotated token unless its replacement
                // refresh token is durable. Otherwise the next process could
                // reuse the old refresh token and invalidate the login.
                session.commit().await.map_err(|error| error.to_string())?;
                Ok(self.resolved(&tokens))
            }
            Err(error) => {
                if error.is_permanent_grant_error() {
                    tracing::error!(
                        provider = %self.connection_id,
                        error = %error,
                        "OAuth refresh token is permanently invalid; removing exact connection credential"
                    );
                    session.remove(&self.connection_id);
                    session.commit().await.map_err(|save_error| {
                        format!("{error}; additionally failed to remove invalid credential: {save_error}")
                    })?;
                }
                Err(format!(
                    "OAuth token resolution failed for '{}': {error}",
                    self.connection_id
                ))
            }
        }
    }
}

impl CredentialSource for OAuthCredentialSource {
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            if self.state.is_none() {
                return Err(format!(
                    "OAuth configuration not found for auth variant {:?}",
                    self.auth
                ));
            }
            let stored = self
                .store()
                .read(&self.connection_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    format!(
                        "No OAuth credentials stored for connection '{}' ({:?}); reconnect it",
                        self.connection_id, self.auth
                    )
                })?;
            // Repair on both branches: a live token would otherwise return
            // before the refresh path's repair ever runs.
            let stored = self.repair_on_fast_path(stored).await?;
            if token_is_live(&stored) {
                return Ok(self.resolved(&stored));
            }
            self.refresh_locked(false, None).await
        })
    }

    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move { self.refresh_locked(true, None).await })
    }

    fn force_refresh_after_rejection<'a>(
        &'a self,
        rejected_access: &'a SecretString,
    ) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move { self.refresh_locked(true, Some(rejected_access)).await })
    }

    fn is_oauth(&self) -> bool {
        true
    }
}

fn exact_tokens(
    session: &dyn CredentialSession,
    connection_id: &str,
    auth: &ConnectionAuth,
) -> Result<TokenSet, String> {
    session.get(connection_id).ok_or_else(|| {
        format!(
            "No OAuth credentials stored for connection '{}' ({auth:?}); reconnect it",
            connection_id
        )
    })
}

fn token_is_live(tokens: &TokenSet) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    !tokens.access.is_empty()
        && !access_token_is_expiring(
            Some(tokens.access.expose_secret()),
            ACCESS_TOKEN_REFRESH_SKEW_MS,
            now,
        )
        && tokens.expires_ms > now + ACCESS_TOKEN_REFRESH_SKEW_MS
}

fn token_rotated_since_rejection(
    rejected_access: Option<&SecretString>,
    stored: &TokenSet,
) -> bool {
    rejected_access.is_some_and(|rejected| rejected != &stored.access)
}

impl fmt::Debug for OAuthCredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredentialSource")
            .field("connection_id", &self.connection_id)
            .field("auth", &self.auth)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use nuo_model_codec::provider_auth::OAuthConfig;

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access: access.into(),
            refresh: "refresh".into(),
            expires_ms: i64::MAX,
            id_token: None,
            token_type: None,
            scope: None,
            user_email: None,
            attributes: serde_json::Map::new(),
        }
    }

    /// A minimal vendor surface used to exercise the engine's plumbing without
    /// naming a real vendor (the engine crate has no vendor dependency).
    struct TestProvider;

    #[async_trait]
    impl crate::provider::OAuthProvider for TestProvider {
        fn ids(&self) -> &'static [&'static str] {
            &["test-provider"]
        }
        fn config(&self) -> OAuthConfig {
            let mut config = OAuthConfig::builder("test-provider").build();
            config.provider_id = std::borrow::Cow::Borrowed("test-provider");
            config
        }
        fn project_metadata(
            &self,
            _tokens: &TokenSet,
            mut auth: ResolvedAuth,
        ) -> ResolvedAuth {
            auth = auth.with_extension(nuo_model_codec::GoogleAuthMetadata {
                project_id: "proj-1".to_string(),
            });
            auth
        }
    }

    fn register_test_provider() {
        crate::provider::register_oauth_provider(std::sync::Arc::new(TestProvider));
    }

    #[test]
    fn rejected_token_identity_prevents_duplicate_rotation() {
        let stored = tokens("new-access");
        let old: SecretString = "old-access".into();
        let current: SecretString = "new-access".into();
        assert!(token_rotated_since_rejection(Some(&old), &stored));
        assert!(!token_rotated_since_rejection(Some(&current), &stored));
        assert!(!token_rotated_since_rejection(None, &stored));
    }

    #[test]
    fn registered_provider_resolves_and_projects_metadata() {
        register_test_provider();
        let source = OAuthCredentialSource::new(
            &CredentialHost::none(),
            "test-conn",
            ConnectionAuth::subscription("test-provider"),
        );
        assert!(source.state.is_some(), "registered provider resolves");
        let resolved = source.resolved(&tokens("access"));
        assert_eq!(
            resolved
                .extension::<nuo_model_codec::GoogleAuthMetadata>()
                .map(|m| m.project_id.as_str()),
            Some("proj-1"),
            "the engine must invoke the provider's projection hook"
        );
    }

    #[test]
    fn unknown_provider_yields_no_state() {
        let source = OAuthCredentialSource::new(
            &CredentialHost::none(),
            "unknown-conn",
            ConnectionAuth::subscription("no-such-provider"),
        );
        assert!(source.state.is_none());
    }

    #[test]
    fn independent_credential_sources_for_same_connection_share_underlying_state() {
        register_test_provider();
        let source1 = OAuthCredentialSource::new(
            &CredentialHost::none(),
            "conn-shared-1",
            ConnectionAuth::subscription("test-provider"),
        );
        let source2 = OAuthCredentialSource::new(
            &CredentialHost::none(),
            "conn-shared-1",
            ConnectionAuth::subscription("test-provider"),
        );
        assert!(Arc::ptr_eq(
            &source1.state.clone().unwrap(),
            &source2.state.clone().unwrap()
        ));
    }
}
