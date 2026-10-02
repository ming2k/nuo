//! Dynamic authentication credential sources (API key, OAuth token, etc.)
//!
//! Provides a uniform [`CredentialSource`] abstraction for resolving live
//! authentication credentials and associated account metadata just-in-time
//! before sending requests to upstream LLMs.

use crate::SecretString;
use futures::future::BoxFuture;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Type-safe extensible property carrier for runtime provider authentication metadata (ADR-0267).
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
    pub fn contains_id(&self, type_id: &TypeId) -> bool {
        self.map.contains_key(type_id)
    }

    /// Whether this map contains an extension of type `T`.
    pub fn contains<T: 'static>(&self) -> bool {
        self.contains_id(&TypeId::of::<T>())
    }

    /// Whether this map contains no extensions.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Number of extensions stored.
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Preflight contract assertion for components declaring required extension dependencies (ADR-0267).
pub trait PreflightValidator: Send + Sync {
    /// Declare the exact extension [`TypeId`]s required by this component.
    fn required_extensions(&self) -> Vec<TypeId>;

    /// Assert that all required extensions exist in [`ResolvedAuth`].
    fn validate_auth(&self, auth: &ResolvedAuth) -> Result<(), String> {
        for type_id in self.required_extensions() {
            if !auth.extensions.contains_id(&type_id) {
                return Err(format!("missing required auth extension: {type_id:?}"));
            }
        }
        Ok(())
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
        if self.map.len() != other.map.len() {
            return false;
        }
        self.map.keys().all(|k| other.map.contains_key(k))
    }
}

impl Eq for ExtensionMap {}

/// ChatGPT/Codex request metadata (`ChatGPT-Account-Id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAuthMetadata {
    pub account_id: String,
}

/// Google Cloud project metadata (`cloudaicompanionProject` / `x-goog-user-project`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleAuthMetadata {
    pub project_id: String,
}

/// OpenCode Console workspace scoping metadata (`x-opencode-org-id`).
///
/// The Console relay rejects inference without an org selection; presence of
/// this extension is the imperative read site for the org header (ADR-0269).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeAuthMetadata {
    pub org_id: String,
}

/// GitHub Copilot session metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopilotAuthMetadata {
    pub session_id: String,
}

/// Resolved authentication credentials and account metadata for an outbound request.
///
/// Designed per ADR-0267: core struct contains only generic transport authentication
/// fields. All provider-specific identity parameters reside in [`ExtensionMap`].
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ResolvedAuth {
    /// Bearer access token or raw API key.
    pub token: SecretString,
    /// User email address if known.
    pub user_email: Option<String>,
    /// Open-world typed provider extensions (e.g. ChatGptAuthMetadata, GoogleAuthMetadata).
    pub extensions: ExtensionMap,
}

impl ResolvedAuth {
    /// Create a new resolved authentication payload with a token.
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self {
            token: token.into(),
            user_email: None,
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

    /// Whether this credential source represents a dynamic OAuth connection.
    fn is_oauth(&self) -> bool {
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

    fn is_oauth(&self) -> bool {
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
            vec![TypeId::of::<ChatGptAuthMetadata>()]
        }
    }

    #[test]
    fn preflight_asserts_missing_and_present_extensions() {
        let auth_without = ResolvedAuth::new("tok");
        let validator = DummyValidator;
        assert!(validator.validate_auth(&auth_without).is_err());

        let auth_with = auth_without.with_extension(ChatGptAuthMetadata {
            account_id: "acct-99".to_string(),
        });
        assert!(validator.validate_auth(&auth_with).is_ok());
    }

    #[test]
    fn opencode_org_metadata_round_trips() {
        let auth = ResolvedAuth::new("st_token").with_extension(OpencodeAuthMetadata {
            org_id: "wrk_org_1".to_string(),
        });
        assert_eq!(
            auth.extension::<OpencodeAuthMetadata>()
                .map(|m| m.org_id.as_str()),
            Some("wrk_org_1")
        );
        assert!(
            ResolvedAuth::new("st_token")
                .extension::<OpencodeAuthMetadata>()
                .is_none()
        );
    }
}
