//! Provider and credential construction factory ports (ADR-0015).

use std::sync::Arc;
use nuo_model_codec::catalog::Channel;
use nuo_model_codec::{ConnectionAuth, CredentialSource, ProviderDialect, SecretString};

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

static PROVIDER_FACTORY: std::sync::OnceLock<Box<dyn ProviderFactory>> =
    std::sync::OnceLock::new();

/// Register the application-wide ProviderFactory.
pub fn register_provider_factory(factory: Box<dyn ProviderFactory>) {
    let _ = PROVIDER_FACTORY.set(factory);
}

/// Construct a concrete `Provider` for a given channel via the registered factory.
pub fn build_provider_for_channel(
    channel: &Channel,
    entry_id: &str,
    session_id: Option<&str>,
) -> Arc<dyn Provider> {
    if let Some(factory) = PROVIDER_FACTORY.get() {
        factory.build_provider_for_channel(channel, entry_id, session_id)
    } else {
        panic!(
            "no ProviderFactory registered in nuo-provider (call the composition root init(), e.g. nuo_server::provider_registry::init())"
        );
    }
}

/// Construct a dynamic or static credential source for a connection via the registered factory.
pub fn build_credential_source(
    host: &CredentialHost,
    connection_name: &str,
    auth: &ConnectionAuth,
    api_key: SecretString,
    dialect: ProviderDialect,
) -> Arc<dyn CredentialSource> {
    if let Some(factory) = PROVIDER_FACTORY.get() {
        factory.build_credential_source(host, connection_name, auth, api_key, dialect)
    } else if auth.is_oauth() {
        panic!("no OAuth credential source builder registered in nuo-provider");
    } else {
        nuo_model_codec::static_credential(api_key)
    }
}
