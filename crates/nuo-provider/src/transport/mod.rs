//! Outbound HTTP transport, connection pooling, and stream reassembly.
//!
//! Substrate layer providing outbound network connectivity across all Nuo provider components.

pub mod client;
pub mod encoding;
pub mod error;
pub mod http;
pub mod network;
pub mod request;
pub mod sse;
pub mod telemetry;

pub use client::{Client, HttpResponse, NetTransport, TraceSink, Transport};
pub use encoding::{encode_component, form_encode, percent_encode, percent_encode_form_pairs};
pub use error::{TransportError, retry_after_ms};
pub use http::{Body, Http, Reply, Request};
pub use network::{direct_client, shared_client};
pub use request::{RequestBuilder, RequestParts};
pub use sse::{data_payloads, payloads_from_chunks};
pub use telemetry::{TransportObservation, TransportTelemetry, TransportTimings};

// Re-export response helpers previously exposed under `transport`
pub use crate::response::{
    decode_response_json, ensure_success, extract_error_message, extract_error_summary,
};
