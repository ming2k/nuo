//! Multi-vendor wire protocol serialization, SSE stream demuxing, and transport for AI model APIs.
//!
//! Provides clean, zero-overhead wire-level framing for OpenAI, Anthropic, Google Gemini,
//! DeepSeek, and Ollama backends.

pub mod auth;
pub mod cache;
pub mod capability;
pub mod catalog;
pub mod client;
pub mod client_identity;
pub mod connection_auth;
pub mod connection_detail;
pub mod credentials;
pub mod effort;
pub mod endpoint;
pub mod error;
pub mod instructions;
pub mod json;
pub mod loop_detector;
pub mod message;
pub mod model;
pub mod model_providers;
pub mod protocol;
pub mod provider_auth;
pub mod provider_error;
pub mod provider_state;
pub mod provider_surface;
pub mod reasoning;
pub mod stream;
pub mod tokenizer;
pub mod tool_output;
pub mod types;
pub mod usage;
pub mod wire_protocol;
pub mod wire_surface;

pub use auth::*;
pub use cache::*;
pub use capability::*;
pub use catalog::*;
pub use client::{WireClient, WireStream};
pub use client_identity::*;
pub use connection_auth::*;
pub use connection_detail::*;
pub use credentials::{CredentialsProvider, DynamicCredentials, StaticApiKey};
pub use effort::{COMMON_LADDER, Effort, EffortLevel};
pub use async_trait::async_trait;
pub use endpoint::{Endpoint, ProviderVendor, TransportObservation, TransportTelemetry, TransportTimings};
pub use error::{Result, WireError};
pub use instructions::{InstructionBundle, InstructionSlice, InstructionTier};
pub use json::find_balanced_object;
pub use loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use message::*;
pub use model::{Availability, BaselineModels, Model, ModelCapabilities, RemoteModelMetadata, resolve, resolve as resolve_model};
pub use nuo_host::SecretString;
pub use provider_auth::*;
pub use provider_error::{ProviderError, ProviderErrorKind, RetryDisposition};
pub use provider_state::*;
pub use provider_surface::*;
pub use reasoning::{ReasoningMode, ReasoningSupport};
pub use stream::{SseDecoder, parse_vendor_chunk};
pub use tokenizer::*;
pub use tool_output::*;
pub use types::{
    CacheControl, ContentBlock, StreamAccumulator, WireChunk, WireMessage, WireRequest,
    WireResponse, WireRole, WireTool, WireToolCall, WireToolCallChunk, WireToolResult, WireUsage,
};
pub use usage::TokenUsage;
pub use wire_protocol::WireProtocol;
pub use wire_surface::*;
