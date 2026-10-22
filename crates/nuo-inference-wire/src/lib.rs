//! The inference wire: vendor dialect codecs, SSE stream demuxing, and transport
//! for external model APIs. Renamed from `nuo-inference-wire` per ADR-0084.
//!
//! Provides clean, zero-overhead wire-level framing for OpenAI, Anthropic, Google Gemini,
//! DeepSeek, and Ollama backends.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod auth;
pub mod catalog;
pub mod client;
pub mod client_identity;
pub mod connection_auth;
pub mod connection_detail;
pub mod credentials;
pub mod discriminative;
pub mod endpoint;
pub mod error;
pub mod generative;
pub mod model;
pub mod model_providers;
pub mod provider_auth;
pub mod provider_error;
pub mod provider_state;
pub mod provider_surface;

// Dual-lane modality separation per ADR-0086
pub use generative::*;
pub use generative::{
    cache, effort, instructions, json, loop_detector, protocol, reasoning, stream,
    token_classification, types, wire_protocol, wire_surface,
};

pub use async_trait::async_trait;
pub use auth::*;
pub use catalog::*;
pub use client::{WireClient, WireStream};
pub use client_identity::*;
pub use connection_auth::*;
pub use connection_detail::*;
pub use credentials::{CredentialsProvider, DynamicCredentials, StaticApiKey};
pub use endpoint::{
    Endpoint, ProviderVendor, TransportObservation, TransportTelemetry, TransportTimings,
};
pub use error::{Result, WireError};
pub use model::{
    Availability, BaselineModels, Model, ModelCapabilities, RemoteModelMetadata,
    WireEntityResolution, resolve, resolve as resolve_model,
};
pub use nuo_host::SecretString;
pub use provider_auth::*;
pub use provider_error::{ProviderError, ProviderErrorKind, RetryDisposition};
pub use provider_state::*;
pub use provider_surface::*;
