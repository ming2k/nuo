//! Generative chat completions, reasoning tokens, and streaming wire protocols (System Two).

pub mod cache;
pub mod effort;
pub mod instructions;
pub mod json;
pub mod loop_detector;
pub mod protocol;
pub mod reasoning;
pub mod stream;
pub mod token_classification;
pub mod types;
pub mod wire_protocol;
pub mod wire_surface;

pub use cache::*;
pub use effort::{COMMON_LADDER, Effort, EffortLevel};
pub use instructions::{InstructionBundle, InstructionSlice, InstructionTier};
pub use json::find_balanced_object;
pub use loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use protocol::*;
pub use reasoning::{ReasoningMode, ReasoningSupport};
pub use stream::{SseDecoder, parse_vendor_chunk};
pub use token_classification::{TokenBreakdown, TokenClass};
pub use types::{
    CacheControl, ContentBlock, StreamAccumulator, WireChunk, WireMessage, WireRequest,
    WireResponse, WireRole, WireTool, WireToolCall, WireToolCallChunk, WireToolResult, WireUsage,
};
pub use wire_protocol::WireProtocol;
pub use wire_surface::*;

use crate::client::{WireClient, WireStream};
use crate::endpoint::Endpoint;
use crate::error::Result;

/// Executes a generative [`WireRequest`] against `endpoint` using the shared [`WireClient`] transport pool.
pub async fn execute(
    client: &WireClient,
    endpoint: &Endpoint,
    request: &WireRequest,
) -> Result<WireResponse> {
    client.execute(endpoint, request).await
}

/// Executes a streaming generative [`WireRequest`] against `endpoint`.
pub async fn execute_stream(
    client: &WireClient,
    endpoint: &Endpoint,
    request: &WireRequest,
) -> Result<WireStream> {
    client.execute_stream(endpoint, request).await
}
