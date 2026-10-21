//! Provider and credential construction factory ports (ADR-0015).

use nuo_inference_wire::catalog::Channel;
use nuo_inference_wire::{ConnectionAuth, CredentialSource, ProviderDialect, SecretString};
use std::sync::Arc;

use crate::capability::Provider;
use crate::credentials::CredentialHost;

/// Factory port constructing concrete `Provider` and `CredentialSource` instances.
pub trait ProviderFactory: Send + Sync {
    fn build_provider_for_channel(
        &self,
        channel: &Channel,
        entry_id: &str,
        session_id: Option<&str>,
    ) -> Arc<dyn Provider>;

    fn build_credential_source(
        &self,
        host: &CredentialHost,
        connection_name: &str,
        auth: &ConnectionAuth,
        api_key: SecretString,
        dialect: ProviderDialect,
    ) -> Arc<dyn CredentialSource>;
}

static PROVIDER_FACTORY: std::sync::OnceLock<Box<dyn ProviderFactory>> = std::sync::OnceLock::new();

/// Register the application-wide ProviderFactory.
pub fn register_provider_factory(factory: Box<dyn ProviderFactory>) {
    let _ = PROVIDER_FACTORY.set(factory);
}

/// Construct a concrete `Provider` for a given channel via the registered factory.
///
/// Returns `None` when the composition root has not registered a factory
/// (`register_provider_factory`); callers decide whether that is an error in
/// their context. A panic here would take down whatever task touched the
/// registry before the root initialized — the registry is a race the caller
/// is better positioned to arbitrate.
pub fn build_provider_for_channel(
    channel: &Channel,
    entry_id: &str,
    session_id: Option<&str>,
) -> Option<Arc<dyn Provider>> {
    PROVIDER_FACTORY
        .get()
        .map(|factory| factory.build_provider_for_channel(channel, entry_id, session_id))
}

/// Construct a dynamic or static credential source for a connection via the
/// registered factory.
///
/// Without a registered factory, a static key degrades to the static
/// credential source it already is; an OAuth connection cannot be served
/// without the OAuth-aware builder, so that is an explicit `None` the caller
/// must surface (it is an initialization-order bug, not a runtime event).
pub fn build_credential_source(
    host: &CredentialHost,
    connection_name: &str,
    auth: &ConnectionAuth,
    api_key: SecretString,
    dialect: ProviderDialect,
) -> Option<Arc<dyn CredentialSource>> {
    if let Some(factory) = PROVIDER_FACTORY.get() {
        Some(factory.build_credential_source(host, connection_name, auth, api_key, dialect))
    } else if auth.is_oauth() {
        None
    } else {
        Some(nuo_inference_wire::static_credential(api_key))
    }
}
