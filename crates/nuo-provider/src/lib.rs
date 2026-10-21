//! Canonical model provider specification, capability definitions, and dynamic registry
//! for the Nuo agent ecosystem.
//!
//! Directly mirrors `nuo-tool` as a zero-agent-runtime, zero-heavy-persistence contract crate (ADR-0015).
//! High-level runtimes (`nuo-harness`, `nuo-agent`, `nuo-server`, `nuo-tui`) depend solely on
//! the abstractions and metadata exposed by this crate.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod capability;
pub mod catalog;
pub mod credentials;
pub mod effort_ladders;
pub mod endpoint;
pub mod factory;
pub mod pipeline;
pub mod pricing;
pub mod prompt_cache;
pub mod response;
pub mod spec;
pub mod vision;

pub mod transport {
    pub use crate::response::{decode_response_json, ensure_success};
}

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

pub mod oauth {
    pub use crate::credentials::*;
}

// Capability & Core Traits — the contract crate owns the `Provider` cognitive
// contract per ADR-0084 (`nuo-inference-wire` split); it is no longer sourced
// from the wire crate.
pub use capability::{
    ModelRequest, Provider, ProviderEventStream, ProviderPromptHints, ProviderStreamEvent,
    ProviderTextStream, ProviderTurnContext, QuotaTracker, ToolSpec, VariantSelection,
    empty_variant_selection,
};
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
pub use factory::{
    ProviderFactory, build_credential_source, build_provider_for_channel, register_provider_factory,
};
pub use pricing::{
    CivilDate, HolidayRange, ModelPricing, NanoUsd, OffPeakPricing, PeakWindow, PricingTier,
    RateSchedule, RateWindow, RatingInstant, TurnCost, Weekday, WeekdayMask, resolve_model_pricing,
};
pub use spec::{
    DisplayPolicy, ModelProviderSpec, PromptCachePolicy, QuotaPort, endpoint_for,
    model_provider_spec, register_provider_spec, register_provider_specs, route_for_model,
    sync_user_declared_providers, unsupported_prompt_cache,
};

// Re-export canonical domain vocabulary
pub use nuo_inference_wire::model_providers::*;
pub use nuo_inference_wire::{
    CatalogShape, ConnectionAuth, CredentialSource, ProviderCompletion, ProviderCompletionMeta,
    ProviderDialect, ProviderError, ProviderErrorKind, RemoteCatalogSource, WireProtocol,
};
pub use nuo_tool::usage::TokenUsage;
