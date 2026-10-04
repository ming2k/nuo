//! Canonical model provider specification, capability definitions, and dynamic registry
//! for the Nuo agent ecosystem.
//!
//! Directly mirrors `nuo-tool` as a zero-agent-runtime, zero-heavy-persistence contract crate (ADR-0015).
//! High-level runtimes (`nuo-harness`, `nuo-agent`, `nuo-server`, `nuo-tui`) depend solely on
//! the abstractions and metadata exposed by this crate.

pub mod capability;
pub mod catalog;
pub mod credentials;
pub mod descriptor;
pub mod effort_ladders;
pub mod factory;
pub mod registry;
pub mod spec;

pub mod oauth {
    pub use crate::credentials::*;
}

// Capability & Core Traits
pub use capability::{Provider, QuotaTracker};
pub use catalog::{
    CatalogDiscovery, CatalogSignature, CatalogSigning, DiscoveredModel, ModelListError,
    RemoteCatalogOptions, RemoteCatalogRequest, RemoteCatalogUpdate, build_catalog_signer,
    catalog_root_for_connection, fetch_remote_catalog, register_catalog_discovery,
    register_catalog_signer_builder,
};
pub use credentials::{
    CredentialHost, CredentialSession, CredentialStore, CredentialStoreError, DeviceIdentity,
    FileCredentialStore, FileDeviceIdentity, InMemoryCredentialStore, PerProcessIdentity, TokenSet,
};
pub use descriptor::ProviderDescriptor;
pub use factory::{
    ProviderFactory, build_credential_source, build_provider_for_channel, register_provider_factory,
};
pub use registry::ProviderRegistry;
pub use spec::{
    ModelProviderSpec, PromptCachePolicy, endpoint_for, model_provider_spec,
    register_provider_spec, register_provider_specs, route_for_model, sync_user_declared_providers,
    unsupported_prompt_cache,
};

// Re-export canonical domain vocabulary
pub use nuo_model_codec::model_providers::*;
pub use nuo_model_codec::{
    CatalogShape, ConnectionAuth, CredentialSource, ModelRequest, ProviderCompletion,
    ProviderCompletionMeta, ProviderDialect, ProviderError, ProviderErrorKind, ProviderEventStream,
    ProviderStreamEvent, ProviderTextStream, RemoteCatalogSource, TokenUsage, WireProtocol,
};
