//! Generative chat completions, reasoning tokens, and streaming wire protocols (System Two).

pub mod json;
pub mod loop_detector;
pub mod protocol;
pub mod stream;
pub mod token_classification;
pub mod types;
pub mod wire_protocol;

pub use json::find_balanced_object;
pub use loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use protocol::*;
pub use stream::{SseDecoder, parse_vendor_chunk};
pub use token_classification::{TokenBreakdown, TokenClass};
pub use types::{
    CacheControl, ContentBlock, StreamAccumulator, WireChunk, WireMessage, WireRequest,
    WireResponse, WireRole, WireTool, WireToolCall, WireToolCallChunk, WireToolResult, WireUsage,
};
pub use wire_protocol::WireProtocol;
