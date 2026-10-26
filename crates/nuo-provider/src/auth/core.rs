//! Dynamic authentication credential sources (API key, OAuth token, etc.)
//!
//! Provides a uniform [`CredentialSource`] abstraction for resolving live
//! authentication credentials and associated account metadata just-in-time
//! before sending requests to upstream LLMs.

use crate::connection_auth::WirePlacement;
pub use futures::future::BoxFuture;
use nuo_host::SecretString;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Type-safe extensible property carrier for runtime provider authentication metadata.
#[derive(Clone, Default)]
pub struct ExtensionMap {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ExtensionMap {
    /// Create an empty extension map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a typed metadata value into the map.
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.map.insert(TypeId::of::<T>(), Arc::new(val));
    }

    /// Retrieve a reference to a typed metadata value.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Remove a typed metadata value from the map.
    pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<Arc<T>> {
        self.map
            .remove(&TypeId::of::<T>())
            .and_then(|b| b.downcast::<T>().ok())
    }

    /// Whether this map contains an extension for the given type ID.
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<T>())
    }

    /// Number of typed extensions currently stored.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether this map is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl fmt::Debug for ExtensionMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtensionMap")
            .field("entries", &self.map.len())
            .finish()
    }
}

impl PartialEq for ExtensionMap {
    fn eq(&self, other: &Self) -> bool {
        self.map.keys().collect::<std::collections::HashSet<_>>()
            == other.map.keys().collect::<std::collections::HashSet<_>>()
    }
}

impl Eq for ExtensionMap {}

/// Interface for provider-specific authentication preflight validation.
pub trait PreflightValidator: Send + Sync {
    /// Return the list of required Extension type IDs for successful request authorization.
    fn required_extensions(&self) -> Vec<TypeId>;

    /// Assert that all required extensions are populated in the resolved authentication context.
    fn validate(&self, auth: &ResolvedAuth) -> Result<(), String> {
        for type_id in self.required_extensions() {
            if !auth.extensions.map.contains_key(&type_id) {
                return Err(format!(
                    "preflight auth validation failed: missing required typed extension {type_id:?}"
                ));
            }
        }
        Ok(())
    }

    /// Alias for `validate`.
    fn validate_auth(&self, auth: &ResolvedAuth) -> Result<(), String> {
        self.validate(auth)
    }
}

/// ChatGPT account metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAuthMetadata {
    pub account_id: String,
}

/// Google Cloud project metadata (`cloudaicompanionProject` / `x-goog-user-project`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleAuthMetadata {
    pub project_id: String,
}

/// GitHub Copilot session metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopilotAuthMetadata {
    pub session_id: String,
}

/// Resolved authentication credentials and account metadata for an outbound request.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ResolvedAuth {
    /// Bearer access token or raw API key.
    pub token: SecretString,
    /// User email address if known.
    pub user_email: Option<String>,
    /// Wire injection placement for this credential (ADR-0098).
    pub placement: WirePlacement,
    /// Open-world typed provider extensions (e.g. ChatGptAuthMetadata, GoogleAuthMetadata).
    pub extensions: ExtensionMap,
}

impl ResolvedAuth {
    /// Create a new resolved authentication payload with a token.
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self {
            token: token.into(),
            user_email: None,
            placement: WirePlacement::Bearer,
            extensions: ExtensionMap::new(),
        }
    }

    /// Retrieve a typed provider extension.
    pub fn extension<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions.get::<T>()
    }

    /// Attach a typed provider extension.
    pub fn with_extension<T: Send + Sync + 'static>(mut self, val: T) -> Self {
        self.extensions.insert(val);
        self
    }

    /// Set wire injection placement.
    pub fn with_placement(mut self, placement: WirePlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Retrieve the wire placement configuration.
    pub fn placement(&self) -> &WirePlacement {
        &self.placement
    }

    /// Set the user email.
    pub fn with_user_email(mut self, email: impl Into<String>) -> Self {
        self.user_email = Some(email.into());
        self
    }

    /// Whether this credential token is empty.
    pub fn is_empty(&self) -> bool {
        self.token.expose_secret().trim().is_empty()
    }
}

impl fmt::Debug for ResolvedAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedAuth")
            .field("token", &"[REDACTED]")
            .field("user_email", &self.user_email)
            .field("placement", &self.placement)
            .field("extensions", &self.extensions)
            .finish()
    }
}

/// Dynamic or static source of authentication credentials.
pub trait CredentialSource: Send + Sync + fmt::Debug {
    /// Resolve the current valid auth credentials (token + metadata).
    /// If dynamic (e.g. OAuth), automatically refreshes if expired or expiring.
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>>;

    /// Force a fresh token from upstream (e.g. on 401 Unauthorized self-healing retry).
    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>>;

    /// Refresh after an upstream rejection of one exact access token. Dynamic
    /// sources use the rejected value to detect that another request or process
    /// already rotated it; static sources retain the default force behavior.
    fn force_refresh_after_rejection<'a>(
        &'a self,
        _rejected_access: &'a SecretString,
    ) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        self.force_refresh()
    }

    /// Whether this credential source is ready to be used (e.g. non-empty API key).
    fn is_ready(&self) -> bool {
        true
    }

    /// Whether this credential source represents a dynamic negotiated connection (ADR-0098).
    fn is_negotiated(&self) -> bool {
        false
    }
}

/// A static API key credential source (never expires, 0ms fast-path).
#[derive(Clone, PartialEq, Eq)]
pub struct StaticCredentialSource(pub SecretString);

impl StaticCredentialSource {
    pub fn new(secret: impl Into<SecretString>) -> Self {
        Self(secret.into())
    }
}

impl fmt::Debug for StaticCredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StaticCredentialSource")
            .field(&"[REDACTED]")
            .finish()
    }
}

impl CredentialSource for StaticCredentialSource {
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        let val = ResolvedAuth::new(self.0.clone());
        Box::pin(futures::future::ready(Ok(val)))
    }

    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        let val = ResolvedAuth::new(self.0.clone());
        Box::pin(futures::future::ready(Ok(val)))
    }

    fn is_ready(&self) -> bool {
        !self.0.expose_secret().trim().is_empty()
    }

    fn is_negotiated(&self) -> bool {
        false
    }
}

/// Convenience helper to wrap any secret into an `Arc<dyn CredentialSource>`.
pub fn static_credential(secret: impl Into<SecretString>) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentialSource::new(secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyValidator;
    impl PreflightValidator for DummyValidator {
        fn required_extensions(&self) -> Vec<TypeId> {
            vec![TypeId::of::<GoogleAuthMetadata>()]
                .into_iter()
                .collect()
        }
    }

    #[test]
    fn preflight_asserts_missing_and_present_extensions() {
        let validator = DummyValidator;
        let auth_missing = ResolvedAuth::new("secret");
        assert!(validator.validate(&auth_missing).is_err());

        let auth_present = ResolvedAuth::new("secret").with_extension(GoogleAuthMetadata {
            project_id: "test-proj".to_string(),
        });
        assert!(validator.validate(&auth_present).is_ok());
    }
}
