//! Pluggable dynamic registry for model providers and capabilities (ADR-0015).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::capability::{CatalogDiscovery, Provider, QuotaTracker};
use crate::descriptor::ProviderDescriptor;

/// Thread-safe registry holding instantiated providers and orthogonal capability handlers.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
    catalog_resolvers: RwLock<HashMap<String, Arc<dyn CatalogDiscovery>>>,
    quota_trackers: RwLock<HashMap<String, Arc<dyn QuotaTracker>>>,
    descriptors: RwLock<HashMap<String, ProviderDescriptor>>,
}

impl ProviderRegistry {
    /// Create a new empty provider registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider implementation with a given identifier.
    pub fn register(&self, id: impl Into<String>, provider: Arc<dyn Provider>) {
        let id_str = id.into();
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id_str, provider);
    }

    /// Register static provider descriptor metadata.
    pub fn register_descriptor(&self, descriptor: ProviderDescriptor) {
        self.descriptors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(descriptor.id.clone(), descriptor);
    }

    /// Register a catalog discovery capability for a provider.
    pub fn register_catalog_discovery(
        &self,
        id: impl Into<String>,
        discovery: Arc<dyn CatalogDiscovery>,
    ) {
        self.catalog_resolvers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.into(), discovery);
    }

    /// Register a quota tracking capability for a provider.
    pub fn register_quota_tracker(
        &self,
        id: impl Into<String>,
        tracker: Arc<dyn QuotaTracker>,
    ) {
        self.quota_trackers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.into(), tracker);
    }

    /// Retrieve an inference provider by identifier.
    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Retrieve a catalog discovery capability by provider identifier.
    pub fn get_catalog_discovery(&self, id: &str) -> Option<Arc<dyn CatalogDiscovery>> {
        self.catalog_resolvers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Retrieve a quota tracking capability by provider identifier.
    pub fn get_quota_tracker(&self, id: &str) -> Option<Arc<dyn QuotaTracker>> {
        self.quota_trackers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Retrieve a provider descriptor by identifier.
    pub fn get_descriptor(&self, id: &str) -> Option<ProviderDescriptor> {
        self.descriptors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// List all registered provider identifiers.
    pub fn registered_ids(&self) -> Vec<String> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}
