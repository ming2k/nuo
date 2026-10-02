//! Multi-vendor wire protocol serialization, SSE stream demuxing, and transport for AI model APIs.
//!
//! Provides clean, zero-overhead wire-level framing for OpenAI, Anthropic, Google Gemini,
//! DeepSeek, and Ollama backends.

pub mod client;
pub mod credentials;
pub mod endpoint;
pub mod error;
pub mod identity;
pub mod json;
pub mod loop_detector;
pub mod protocol;
pub mod stream;
pub mod types;

pub use client::{WireClient, WireStream};
pub use credentials::{CredentialsProvider, DynamicCredentials, StaticApiKey};
pub use endpoint::{Endpoint, ProviderVendor};
pub use error::{Result, WireError};
pub use identity::ClientProfile;
pub use json::find_balanced_object;
pub use loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use stream::{SseDecoder, parse_vendor_chunk};
pub use types::{
    CacheControl, ContentBlock, StreamAccumulator, WireChunk, WireMessage, WireRequest,
    WireResponse, WireRole, WireTool, WireToolCall, WireToolCallChunk, WireToolResult, WireUsage,
};
