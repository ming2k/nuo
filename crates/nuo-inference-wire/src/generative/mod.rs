//! Generative lane: the wire vocabulary and decoders that shipped code uses.
//!
//! After the aperture cut (ADR-0084's amendment, ADR-0143) this module carries
//! only what a shipped binary actually reaches:
//!
//! - [`wire_protocol`] — the [`WireProtocol`] identity enum, referenced by 60+
//!   call sites across every provider spec and route derivation.
//! - [`json`] — `find_balanced_object`, the incremental JSON-boundary scan used
//!   by the harness tool-call router and two providers' stream parsers.
//! - [`loop_detector`] — the degenerate-repetition guard the harness runs over
//!   every stream.
//! - [`token_classification`] — `TokenBreakdown`, consumed by provider pricing.
//! - [`protocol::google_interactions`] — the one shared vendor parser.
//! - [`types`] — the payload structs those parsers return.
//!
//! Deleted in this cut, because nothing in any shipped binary reached them:
//! the five per-vendor `build_request` implementations, the whole
//! `discriminative` lane, `Endpoint`/`ProviderVendor`, and the credential
//! providers. `WireClient` was already gone — ADR-0088 removed it. See
//! `docs/adr/0143`.

pub mod json;
pub mod loop_detector;
pub mod protocol;
pub mod token_classification;
pub mod types;
pub mod wire_protocol;

pub use json::find_balanced_object;
pub use loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use token_classification::{TokenBreakdown, TokenClass};
pub use types::{
    CacheControl, ContentBlock, StreamAccumulator, WireChunk, WireMessage, WireRequest,
    WireResponse, WireRole, WireTool, WireToolCall, WireToolCallChunk, WireToolResult, WireUsage,
};
pub use wire_protocol::WireProtocol;
