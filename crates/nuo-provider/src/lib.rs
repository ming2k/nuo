//! Canonical model-provider specification, capability definitions, and dynamic
//! registry for the Nuo agent ecosystem.
//!
//! `nuo-provider` is a zero-agent-runtime contract crate (ADR-0015). It owns
//! concrete provider specs, credential/OAuth machinery, catalog transport, and
//! the provider factory ports. Provider-neutral vocabulary (`Model`, `Effort`,
//! `Provider`, `ToolSpec`, pricing, wire dialects) is owned by
//! `nuo-protocol`; canonical imports point there
//! ([ADR-0180](../../docs/adr/0180-single-ownership-of-shared-vocabulary.md)).
//!
//! Consumers are the composition root (`nuo-server`), the cognitive runtime
//! (`nuo-engine`), and the concrete `providers/nuo-provider-*` leaves.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod auth;
pub mod catalog;
pub mod credentials;
pub mod effort_ladders;
pub mod endpoint;
pub mod error;
pub mod factory;
pub mod pipeline;
pub mod prompt_cache;

/// Default user agent this project sends to providers.
pub use nuo_protocol::NUO_USER_AGENT;
pub mod response;
pub mod spec;
pub mod transport;
pub mod vision;
pub mod wire;

pub use endpoint::*;
pub use pipeline::{
    BodyCodecPhase, EnvelopePhase, OutboundPlan, PassThroughHeaders, PassThroughStreamTransformer,
    ProviderFault, ProviderPipeline, ProviderPipelineBuilder, RequestSignerPhase, ReshapedEnvelope,
    StreamMetrics, StreamTransformer, TransformedFrame, TransportPipeline,
    TransportPipelineBuilder,
};
pub use prompt_cache::PromptCacheConfig;
pub use response::{
    classify_http_error, decode_response_json, ensure_success, is_context_overflow_payload,
};
pub use vision::project_images_for_route;

// Capability & Core Traits — the contract crate owns the `Provider` cognitive contract
pub use catalog::{
    CatalogDiscovery, CatalogSignature, CatalogSigning, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogUpdate, build_catalog_signer, catalog_root_for_connection, fetch_remote_catalog,
    register_catalog_discovery, register_catalog_signer_builder,
};
pub use credentials::{
    CredentialHost, CredentialSession, CredentialStore, CredentialStoreError, DeviceIdentity,
    FileCredentialStore, FileDeviceIdentity, InMemoryCredentialStore, PerProcessIdentity, TokenSet,
};
pub use factory::{
    ProviderFactory, build_credential_source, build_provider_for_channel, register_provider_factory,
};
pub use spec::{
    DisplayPolicy, ModelProviderSpec, PromptCachePolicy, QuotaPort, endpoint_for,
    model_provider_spec, register_provider_spec, register_provider_specs,
    register_user_declared_provider, route_for_model, sync_user_declared_providers,
    unsupported_prompt_cache, user_declared_provider_spec,
};

pub use auth::*;

pub use transport::{
    Client, Http, HttpResponse, NetTransport, Reply, Request, RequestBuilder, RequestParts,
    Transport, TransportError,
};
pub use wire::{EncodedRequest, WireRequest};
