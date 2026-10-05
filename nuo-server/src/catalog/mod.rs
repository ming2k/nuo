//! Materializes the runtime `Catalog` from the connection store, the preset
//! registry, and the catalog cache — never from `config.toml`, which holds
//! behavior only.

mod derive;
mod picker;
mod sync;

pub use derive::{
    DerivationInputs, derive_channel, derive_entries, derive_entry, resolve_credential, route_models,
};
use picker::active_model_id_for_entry;
pub use picker::{
    build_picker_state, channel_model_info, prune_stale_models, prune_stale_models_on_disk,
};
pub use sync::{
    CatalogSyncOutcome, ConnectionUpdate, refresh_connection_models_for_etag,
    sync_connection_catalog, sync_fitted_model_registry, sync_remote_catalog,
    sync_remote_catalog_streaming,
};

use nuo_wire::catalog::ProviderEntry;
use nuo_persistence::config::{Config, Credentials, RemoteCatalogCache};
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_persistence::connections::Connections;
use nuo_persistence::model_providers::ModelProviders;
use nuo_persistence::route_settings::RouteSettingsStore;
use nuo_provider::CredentialHost;

#[cfg(test)]
mod tests;

/// The stores the catalog derives from.
///
/// Every field is host-owned state: connections, the discovery cache, route
/// overrides, static credentials, the user's provider declarations, and the
/// credential host dynamic sources read through. This struct is the catalog's
/// single point of I/O — a derivation receives borrowed views of it and resolves
/// nothing on its own (ADR-0300 §1).
pub struct Stores {
    pub connections: Connections,
    pub instances: Connections,
    pub cache: RemoteCatalogCache,
    pub routes: RouteSettingsStore,
    pub creds: Credentials,
    pub providers: ModelProviders,
    pub credentials: CredentialHost,
}

impl Stores {
    pub fn load() -> Self {
        let connections = Connections::load();
        let providers = ModelProviders::load();
        // Populate the provider registry from the declarations we just read:
        // the value that was loaded is the value the registry serves, so the two
        // cannot disagree.
        if let Err(error) = nuo_provider::sync_user_declared_providers(&providers) {
            tracing::warn!(%error, "could not refresh the provider registry");
        }
        Self {
            instances: connections.clone(),
            connections,
            cache: RemoteCatalogCache::load(),
            routes: RouteSettingsStore::load(),
            creds: Credentials::load(),
            providers,
            credentials: credential_host(),
        }
    }

    /// The borrowed view every derivation takes.
    pub fn inputs(&self) -> DerivationInputs<'_> {
        DerivationInputs {
            cache: &self.cache,
            routes: &self.routes,
            creds: &self.creds,
            providers: &self.providers,
            credentials: &self.credentials,
        }
    }
}

/// Where this process keeps OAuth credentials.
///
/// The catalog is application-plane policy (it resolves product state by design)
/// and so is the only place here that names a path. `nuo-server` builds the
/// same host for the daemon's own flows; the duplication collapses when the
/// catalog moves to the application plane (ADR-0300 §5).
pub(crate) fn credential_host() -> CredentialHost {
    let paths = nuo_persistence::paths::get();
    CredentialHost::file_backed(paths.auth_file(), paths.state_dir.join("machine_id"))
}

pub fn default_connection_id(config: &Config) -> &str {
    &config.default_connection
}

pub fn default_provider_id(config: &Config) -> &str {
    default_connection_id(config)
}

/// The effective default connection name.
pub fn effective_default_connection_id(config: &Config, stores: &Stores) -> String {
    stores
        .connections
        .effective_default(&config.default_connection)
        .map(|p| p.name.clone())
        .unwrap_or_default()
}

pub fn effective_default_provider_id(config: &Config, stores: &Stores) -> String {
    effective_default_connection_id(config, stores)
}

pub fn build_catalog() -> Vec<ProviderEntry> {
    let stores = Stores::load();
    derive_entries(&stores.connections, &stores.inputs())
}

pub fn build_provider_for(
    config: &Config,
    id: &str,
) -> Option<std::sync::Arc<dyn nuo_wire::Provider>> {
    build_provider_for_model(config, id, config.default_model.as_deref(), None)
}

pub fn build_provider_for_model(
    config: &Config,
    connection_id: &str,
    model_id: Option<&str>,
    session_id: Option<&str>,
) -> Option<std::sync::Arc<dyn nuo_wire::Provider>> {
    let stores = Stores::load();
    let entry = derive_entries(&stores.connections, &stores.inputs())
        .into_iter()
        .find(|e| e.id == connection_id)?;
    let wanted = model_id.or(config.default_model.as_deref());
    let connection = stores.connections.get(connection_id);
    // The single daemon-side availability gate (ADR-0273
    // `[INV-AVAIL-06]`). A route for a model the account may not run is never
    // built, so no client is the only refusal site: TUI, web, daemon protocol,
    // and session restore all inherit it. `effective_availability` applies the
    // user's sovereign override, so an injected model still resolves.
    let usable = |channel: &&nuo_wire::catalog::Channel| {
        let (availability, overridden) = connection.map_or(
            (nuo_wire::Availability::usable(), false),
            |connection| {
                derive::effective_availability(
                    connection,
                    &channel.model,
                    channel.remote.as_ref(),
                    &stores.providers,
                )
            },
        );
        if !availability.usable {
            tracing::warn!(
                connection = %entry.id,
                model = %channel.model,
                reason = availability.reason.as_deref().unwrap_or(""),
                "refusing to route a model the provider declared unavailable",
            );
        }
        let _ = overridden;
        availability.usable
    };
    // An explicitly requested model that is declared unavailable is refused
    // rather than silently swapped for another; an unrequested connection falls
    // through to the first model that *is* available, so a catalogue whose head
    // happens to be locked still yields a runnable default.
    let channel = match wanted.and_then(|m| entry.channel_for_model(m)) {
        Some(channel) => usable(&channel).then_some(channel),
        None => entry
            .channels
            .iter()
            .find(usable)
            .or_else(|| entry.default_channel()),
    };
    channel
        .map(|channel| nuo_provider::build_provider_for_channel(channel, &entry.id, session_id))
}

pub fn resolved_model_name(config: &Config, id: &str) -> Option<String> {
    resolved_model_name_inner(config, id, &ConnectionUsage::default())
}

pub fn resolved_model_name_with_usage(
    config: &Config,
    id: &str,
    usage: &ConnectionUsage,
) -> Option<String> {
    resolved_model_name_inner(config, id, usage)
}

fn resolved_model_name_inner(config: &Config, id: &str, usage: &ConnectionUsage) -> Option<String> {
    build_catalog()
        .iter()
        .find(|e| e.id == id)
        .and_then(|entry| active_model_id_for_entry(config, entry, usage))
}

pub fn models_for_connection(_config: &Config, connection_id: &str) -> Vec<String> {
    build_catalog()
        .iter()
        .find(|e| e.id == connection_id)
        .map(|entry| entry.channels.iter().map(|c| c.model.clone()).collect())
        .unwrap_or_default()
}

pub fn models_for_provider(config: &Config, provider_id: &str) -> Vec<String> {
    models_for_connection(config, provider_id)
}
