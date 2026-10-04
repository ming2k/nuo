//! Per-vendor wire-protocol adapters.
//!
//! Each submodule speaks one LLM backend's wire format (request construction +
//! response/stream parsing) as a thin executor over the shared transport in the
//! crate root. Protocols never depend on each other; they share only the
//! substrate (`Endpoint`, `Client`, `sse`, `transport`).

pub mod anthropic;
pub mod google;
pub mod openai;
