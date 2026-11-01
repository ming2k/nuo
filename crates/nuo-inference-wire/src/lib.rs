//! The external model wire substrate: protocol identity, decoders, and the
//! vocabulary shipped code actually shares.
//!
//! # What this crate is now
//!
//! It is not a codec suite. ADR-0084 shrank it toward "pure wire codec" and left
//! a five-vendor `build_request`/`parse_response` fan-out behind; that fan-out
//! had **zero production callers** — every provider constructs its own request
//! body and parses its own response — so it was a second implementation drifting
//! away from the real one. ADR-0143 finishes the aperture: only symbols with a
//! live consumer remain.
//!
//! Live surface, and every consumer:
//!
//! | Symbol | Consumer |
//! | --- | --- |
//! | [`WireProtocol`] | every provider spec, route derivation, capability registry |
//! | [`find_balanced_object`] | harness tool-call router; two provider stream parsers |
//! | [`loop_detector`] | harness stream governance |
//! | [`token_classification::TokenBreakdown`] | provider pricing |
//! | [`protocol::google_interactions`] | `nuo-provider-google-antigravity` |
//!
//! # What was removed, and why it is not a regression
//!
//! `discriminative`, `credentials`, `endpoint` (`Endpoint`, `ProviderVendor`,
//! `StaticApiKey`, `DynamicCredentials`), `error::WireError`, and the four
//! remaining per-vendor codecs had zero references from any crate's `src`. They
//! were reachable only from this crate's own tests, which is the definition of a
//! self-referential dead layer.
//!
//! `WireClient` is *not* in that list: ADR-0088 deleted it a year earlier when it
//! converged every provider onto `nuo-egress`. This crate did not remove it and
//! does not claim to.
//!
//! A caller who wanted a wire codec would have found a second, unmaintained
//! implementation of what `providers/*` already do correctly. That is a trap,
//! not an API.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod generative;

// The *module* names are part of the live API surface: `nuo-provider` reaches
// `nuo_inference_wire::token_classification::TokenBreakdown`, `nuo-harness`
// reaches `nuo_inference_wire::loop_detector::{..}`, and
// `nuo-provider-google-antigravity` reaches
// `nuo_inference_wire::protocol::google_interactions::{..}`. Flattening them to
// root items only would be a cosmetic break with no structural gain, so the
// modules are re-exported alongside the types they own.
pub use generative::{json, loop_detector, protocol, token_classification, types, wire_protocol};

pub use generative::{
    CacheControl, ContentBlock, DegeneratePattern, StreamAccumulator, StreamLoopDetector,
    TokenBreakdown, TokenClass, WireChunk, WireMessage, WireProtocol, WireRequest, WireResponse,
    WireRole, WireTool, WireToolCall, WireToolCallChunk, WireToolResult, WireUsage,
    find_balanced_object,
};
